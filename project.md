# Spécification technique — Serveur WebSocket Rust pour l'écosystème PHP

**Nom de code provisoire :** `resonance` (à remplacer)
**Version du document :** 0.1 — Juillet 2026
**Objectif :** serveur WebSocket self-hosted, compatible protocole Pusher, framework-agnostique, distribué en binaire unique. Package Composer d'intégration Laravel en premier adaptateur.

---

## 1. Vision et principes directeurs

### 1.1 Le problème
Les développeurs PHP qui veulent du temps réel performant doivent choisir entre : un SaaS payant (Pusher, Ably), un serveur PHP qui sature tôt (Reverb ~1000 connexions à 95% CPU sur petit serveur), ou une solution Node (Soketi) qui impose un runtime supplémentaire. Il n'existe pas de serveur temps réel **compilé, zéro dépendance, pensé pour tout PHP** (Laravel, Symfony, WordPress, vanilla).

### 1.2 Les trois promesses (dans l'ordre)
1. **Compatibilité maximale** — protocole Pusher intégral. Tout client Pusher existant (Laravel Echo, pusher-js, pusher-php-server, clients mobiles) fonctionne sans modification.
2. **Performance sous charge** — densité de connexions par CPU maximale, latence stable quand ça monte. C'est le gain réel vs PHP/Node ; la latence à vide est équivalente partout.
3. **Installation triviale** — un binaire statique, zéro Redis, zéro Node, zéro runtime. `./resonance start` et c'est parti.

### 1.3 Anti-objectifs (v0/v1)
- Pas de scaling horizontal multi-instances en v0 (une instance verticale bien exploitée couvre déjà des dizaines de milliers de connexions).
- Pas de protocole propriétaire maison — Pusher-compat ou rien.
- Pas d'UI d'admin en v0 (endpoint métriques texte suffit).

---

## 2. Architecture globale

```
┌──────────────────────────────────────────────────────────────┐
│  Repo 1 : resonance (Rust)                                   │
│  Le serveur. Binaire unique, protocole Pusher, HTTP API.     │
└──────────────────────────────────────────────────────────────┘
┌──────────────────────────────────────────────────────────────┐
│  Repo 2 : resonance-laravel (PHP)                            │
│  Package Composer : broadcast driver, config, commande       │
│  artisan, auth des canaux. Fin — la logique est dans le cœur.│
└──────────────────────────────────────────────────────────────┘
```

**Flux runtime :**

```
Navigateur (Echo/pusher-js)
    │  WSS (protocole Pusher, port 8080)
    ▼
Serveur resonance (Rust) ◄──── POST /apps/{app_id}/events (HTTP, signé)
    │                              ▲
    │  auth canaux privés          │
    ▼                              │
App PHP  ───────────────────────────┘
(endpoint /broadcasting/auth)   (pusher-php-server ou package)
```

Trois canaux de communication, tous au format Pusher :
1. **Client ↔ serveur** : WebSocket, protocole Pusher (subscribe, events, ping/pong).
2. **App PHP → serveur** : HTTP REST `POST /apps/{app_id}/events`, signature HMAC — c'est l'API que `pusher-php-server` parle déjà.
3. **Serveur → app PHP** : requête d'auth des canaux privés/presence (le client fournit la signature obtenue de l'app), et webhooks optionnels (v1).

---

## 3. Cœur Rust — stack technique

### 3.1 Dépendances (Cargo.toml)

| Crate | Rôle | Justification |
|---|---|---|
| `tokio` (full) | Runtime async | Standard de facto, scheduler work-stealing multi-thread |
| `axum` | HTTP + upgrade WebSocket | Intégration Tokio native, extraction typée, plus simple qu'Actix pour un résultat quasi identique |
| `tokio-tungstenite` / `axum::extract::ws` | WebSocket | Fourni par axum, basé tungstenite |
| `dashmap` | État concurrent | HashMap shardée lock-free en lecture, évite un Mutex global |
| `serde` + `serde_json` | Sérialisation | Messages Pusher = JSON |
| `hmac` + `sha2` | Signatures | Auth API REST + canaux privés (HMAC-SHA256) |
| `tracing` + `tracing-subscriber` | Logs structurés | Observabilité sans coût quand désactivé |
| `clap` | CLI | `resonance start --config ...` |
| `toml` | Config fichier | Lisible, standard |
| `rustls` + `tokio-rustls` | TLS natif | Pas d'OpenSSL — binaire statique portable |

**Interdits :** pas de `openssl` (casse le build statique), pas de dépendance C (cgo-like), pas de Redis en v0.

### 3.2 Modèle de concurrence

```
main
 └── Runtime Tokio multi-thread (workers = nb de cœurs)
      ├── Listener WS (axum) ── 1 task Tokio PAR connexion
      ├── Listener HTTP API (même serveur axum, routes séparées)
      └── Task périodique : ping/pong, éviction connexions mortes
```

**Règle d'or : une connexion = une task Tokio + un canal mpsc sortant.**
Chaque connexion possède :
- Une task de lecture (messages entrants du client : subscribe, unsubscribe, ping, client events).
- Un `tokio::sync::mpsc::Sender<Message>` pour l'écriture. Toute écriture vers ce client passe par ce canal ; une task d'écriture unique draine le canal vers la socket. **Jamais d'écriture directe multi-task sur la socket** (source classique de corruption de trames).

### 3.3 Structures d'état (le cœur de la performance)

```rust
struct AppState {
    // app_id -> App (clé, secret, limites)
    apps: DashMap<AppId, App>,
    // socket_id -> handle de connexion (sender mpsc + métadonnées)
    connections: DashMap<SocketId, ConnectionHandle>,
    // (app_id, channel_name) -> ensemble des socket_id abonnés
    channels: DashMap<(AppId, ChannelName), ChannelState>,
}

struct ChannelState {
    subscribers: HashSet<SocketId>,
    // Pour presence channels uniquement :
    presence: Option<HashMap<SocketId, PresenceMember>>,
}
```

Points critiques :
- `DashMap` shardé → les broadcasts sur des canaux différents ne se contendent pas.
- `ChannelState` derrière un shard : le fan-out d'un event verrouille **un seul shard**, brièvement, pour cloner la liste des sockets, puis relâche AVANT d'envoyer. On n'envoie jamais sous verrou.
- `SocketId` : format Pusher `{u64}.{u64}` généré aléatoirement.

### 3.4 Chemin chaud du broadcast (à optimiser en priorité)

```
POST /apps/{id}/events  (depuis PHP)
  1. Vérifier signature HMAC (rejet précoce, avant tout parsing coûteux)
  2. Parser le body JSON une seule fois
  3. PRÉ-SÉRIALISER le message sortant UNE FOIS → Arc<str> / Bytes
  4. Pour chaque canal cible :
     a. Lire ChannelState, cloner la Vec<Sender> (verrou très court)
     b. Pour chaque sender : try_send(message.clone())  // clone d'Arc = pas de copie
  5. Répondre 200 immédiatement (fire-and-forget vers les clients)
```

**Optimisations non négociables du chemin chaud :**
- Le payload sortant est sérialisé **une seule fois** et partagé par `Arc`/`Bytes` — jamais N sérialisations pour N destinataires. À 10k abonnés, c'est LA différence.
- `try_send` (non bloquant) avec canal borné (ex. 64 messages). Si le buffer d'un client est plein → client trop lent → on le déconnecte (politique "slow consumer kill", comme les brokers sérieux). Un client lent ne doit jamais ralentir les autres.
- `TCP_NODELAY` activé sur toutes les sockets (latence > throughput pour du temps réel).
- Pas d'allocation dans la boucle de fan-out.

### 3.5 Limites et protections (dès la v0)
- Taille max de message client : 10 Ko (défaut Pusher), configurable.
- Nombre max de canaux par connexion : configurable (défaut 100).
- Rate limit sur les client events (`client-*`) : défaut Pusher ~10/s par connexion.
- Timeout d'activité : ping toutes les 120s (défaut protocole Pusher `activity_timeout`), fermeture si pas de pong sous 30s.
- Backpressure : canal mpsc borné par connexion (voir 3.4).

---

## 4. Compatibilité protocole Pusher — la checklist intégrale

C'est la section la plus importante pour la promesse n°1. La compatibilité se joue dans les détails. Référence : Pusher Channels Protocol v7.

### 4.1 Handshake WebSocket
- URL : `ws(s)://host:port/app/{key}?protocol=7&client=...&version=...`
- À la connexion, envoyer immédiatement :
```json
{"event":"pusher:connection_established","data":"{\"socket_id\":\"123.456\",\"activity_timeout\":120}"}
```
- **Piège de compat n°1 :** le champ `data` est une **chaîne JSON encodée**, pas un objet JSON. Tous les events Pusher font ça (double encodage). Les clients cassent silencieusement si tu envoies un objet.

### 4.2 Événements protocole à implémenter

| Événement | Direction | Notes |
|---|---|---|
| `pusher:connection_established` | S→C | Au handshake |
| `pusher:subscribe` | C→S | Avec `channel`, et `auth` + `channel_data` pour privé/presence |
| `pusher:unsubscribe` | C→S | |
| `pusher_internal:subscription_succeeded` | S→C | Pour presence : contient la liste des membres |
| `pusher:ping` / `pusher:pong` | Bidirectionnel | Répondre aux deux sens |
| `pusher:error` | S→C | Avec les codes d'erreur officiels (4000-4299) |
| `pusher_internal:member_added` / `member_removed` | S→C | Presence channels |
| `client-*` (client events) | C→S→C | Uniquement sur canaux privés/presence, jamais renvoyé à l'émetteur, rate-limité |

### 4.3 Types de canaux

| Type | Préfixe | Auth requise | Spécificités |
|---|---|---|---|
| Public | (aucun) | Non | Subscribe direct |
| Privé | `private-` | Oui | Signature HMAC vérifiée au subscribe |
| Privé chiffré | `private-encrypted-` | Oui | v1+ (chiffrement côté client, le serveur relaie) |
| Presence | `presence-` | Oui | `channel_data` = user_id + user_info ; diffuser member_added/removed ; renvoyer la liste au subscribe |

### 4.4 Signature d'auth des canaux privés (le détail qui casse tout)
Le client obtient la signature depuis l'app PHP (`/broadcasting/auth` en Laravel). Le serveur doit **vérifier** :
```
signature = HMAC-SHA256(secret, socket_id + ":" + channel_name)
// presence : socket_id + ":" + channel_name + ":" + channel_data
auth = "{key}:{hex(signature)}"
```
Comparaison en temps constant (`subtle` ou équivalent) pour éviter les timing attacks.

### 4.5 API REST HTTP (ce que pusher-php-server appelle)

| Endpoint | Méthode | Rôle |
|---|---|---|
| `/apps/{app_id}/events` | POST | Publier un event (LE endpoint critique) |
| `/apps/{app_id}/batch_events` | POST | Publier en lot (v1) |
| `/apps/{app_id}/channels` | GET | Lister les canaux occupés (v1) |
| `/apps/{app_id}/channels/{name}` | GET | Info d'un canal (v1) |
| `/apps/{app_id}/channels/{name}/users` | GET | Membres presence (v1) |

**Signature des requêtes REST (Pusher auth scheme) :**
```
string_to_sign = "POST\n/apps/{app_id}/events\n" + query_string_triée
auth_signature = HMAC-SHA256(secret, string_to_sign)
```
Query string : `auth_key`, `auth_timestamp` (tolérance ±600s), `auth_version=1.0`, `body_md5` (MD5 du body), paramètres triés alphabétiquement. Implémenter **exactement** ce schéma — c'est ce que `pusher-php-server` génère. Tester contre la lib officielle, pas contre ta propre implémentation.

### 4.6 Matrice de compatibilité à valider (tests d'intégration)

| Client | Test |
|---|---|
| `pusher-js` (navigateur) | Connexion, subscribe public/privé/presence, réception d'events, client events |
| **Laravel Echo** (wrapper pusher-js) | `Echo.channel()`, `Echo.private()`, `Echo.join()` (presence), whisper |
| `pusher-php-server` | `trigger()`, `triggerBatch()`, auth de canaux |
| Laravel `broadcast()` + driver pusher | Le flux complet Laravel sans le package dédié |
| Reconnexion automatique | Couper la socket, vérifier resubscribe automatique du client |

**Critère de done de la v0 : une app Laravel existante utilisant Reverb bascule vers resonance en changeant UNIQUEMENT les variables d'env (host/port/key/secret). Zéro changement de code.**

### 4.7 Réseau et déploiement (compat infra)
- TLS natif (rustls) OU terminaison TLS au reverse proxy — supporter les deux.
- Fonctionner derrière nginx/Caddy/Traefik : documenter la config d'upgrade WebSocket (`proxy_set_header Upgrade/Connection`).
- Écouter sur un port unique pour WS + API HTTP (routes distinctes) : simplifie firewall et proxy.
- IPv4 + IPv6.
- Header `X-Forwarded-For` pour les logs derrière proxy.

---

## 5. Performance — objectifs chiffrés et méthode

### 5.1 Cibles v0 (sur une machine 2 vCPU / 4 Go type t3.medium)

| Métrique | Cible | Référence concurrente |
|---|---|---|
| Connexions simultanées idle | ≥ 50 000 | Reverb : ~20k rapporté sur t3.medium |
| CPU à 1 000 connexions actives | < 10% | Reverb : ~95% sur serveur 5$ ; Go : ~18% |
| Latence p99 broadcast (1 canal, 1k abonnés) | < 10 ms intra-DC | |
| Mémoire par connexion idle | < 20 Ko | |
| Throughput events entrants (API REST) | ≥ 5 000 req/s | |

### 5.2 Réglages système à documenter (sinon les benchs mentent)
- `ulimit -n` (file descriptors) ≥ 2× connexions visées.
- `net.core.somaxconn`, `net.ipv4.tcp_tw_reuse` selon charge.
- Le binaire doit afficher un avertissement au démarrage si `ulimit` est trop bas.

### 5.3 Méthodologie de benchmark (livrable public)
- Outil : k6 (scénario WS) ou un bencher Rust custom publié dans le repo.
- Scénarios : (a) ramp 0→N connexions idle, (b) 1k connexions + broadcast 100 msg/s sur canal partagé, (c) fan-out extrême : 1 event → 10k abonnés, mesurer p50/p99 de livraison.
- Comparer sur le **même hardware, même scénario** : resonance vs Reverb vs Soketi. Publier scripts + résultats bruts dans `bench/`. La reproductibilité est l'argument de crédibilité.
- Ne jamais publier de chiffre non reproductible par un tiers.

### 5.4 Pièges de perf à éviter (revue de code systématique)
- Sérialiser N fois le même payload (voir 3.4) — le tueur silencieux.
- Envoyer sous verrou DashMap.
- `send().await` bloquant sur un client lent au milieu d'un fan-out — toujours `try_send`.
- Logs en niveau debug dans le chemin chaud (tracing avec filtres compilés).
- Allocations dans la boucle de fan-out (profiler avec `cargo flamegraph`).

---

## 6. Package Composer `resonance-laravel`

### 6.1 Principe : le package le plus fin possible
Comme le serveur est Pusher-compatible, Laravel sait DÉJÀ lui parler via le driver `pusher` existant (config host/port custom). Le package apporte le confort, pas la plomberie :

```
resonance-laravel/
├── composer.json          # require: php ^8.2, laravel ^11|^12 ; suggest rien — zéro extension
├── config/resonance.php   # host, port, app_id, key, secret, TLS
├── src/
│   ├── ResonanceServiceProvider.php   # merge config, enregistre le driver
│   ├── ResonanceBroadcaster.php       # étend PusherBroadcaster (réutilise, ne réécrit pas)
│   └── Console/
│       ├── InstallCommand.php         # télécharge le binaire de la release GitHub
│       │                              #   selon OS/arch, le place dans ./bin, chmod +x
│       └── StartCommand.php           # php artisan resonance:start (lance le binaire)
└── tests/
```

### 6.2 Décisions
- **Pas d'extension PHP, pas de FFI** — inutiles ici, tout passe par réseau. Le package reste du PHP pur → installable partout, aucun frein d'adoption.
- `InstallCommand` détecte OS + architecture (`php_uname`) et télécharge le bon binaire depuis GitHub Releases avec vérification de checksum SHA-256. C'est l'équivalent DX de `php artisan reverb:start`.
- Réutiliser `PusherBroadcaster` de Laravel au lieu de réécrire : moins de code, compat garantie avec les évolutions du framework.
- Versionner la compat : matrice PHP 8.2/8.3/8.4 × Laravel 11/12 en CI.

### 6.3 Adaptateurs futurs (v2+, seulement si traction)
- Bundle Symfony (Mercure est SSE ; positionner sur le bidirectionnel).
- Client PHP générique = documentation d'usage de `pusher-php-server` pointé vers resonance (quasi zéro code).
- Plugin WordPress si demande.

---

## 7. Distribution du binaire

### 7.1 Cibles de compilation (CI GitHub Actions)

| Cible | Priorité |
|---|---|
| `x86_64-unknown-linux-musl` (statique) | P0 — le serveur type |
| `aarch64-unknown-linux-musl` | P0 — ARM (Graviton, Ampère, Raspberry) |
| `x86_64-apple-darwin` + `aarch64-apple-darwin` | P1 — dev local |
| `x86_64-pc-windows-msvc` | P2 — dev local Windows |

**musl + rustls = binaire 100% statique**, aucun `.so` requis, tourne sur n'importe quelle distro et dans une image Docker `scratch`.

### 7.2 Canaux de distribution
1. GitHub Releases (binaires + checksums) — source de vérité.
2. Image Docker officielle (`FROM scratch`, ~10 Mo) sur ghcr.io.
3. `php artisan resonance:install` (voir 6.2).
4. Plus tard : Homebrew tap, AUR.

### 7.3 Config du serveur (fichier TOML + surcharge env)
```toml
[server]
host = "0.0.0.0"
port = 8080

[tls]                    # optionnel — sinon terminer au proxy
cert = "/path/cert.pem"
key = "/path/key.pem"

[[apps]]
id = "app1"
key = "resonance-key"
secret = "resonance-secret"
max_connections = 0      # 0 = illimité
enable_client_events = true

[limits]
max_message_size_kb = 10
activity_timeout_s = 120
```
Toute valeur surchargeable par variable d'env (`RESONANCE_PORT=...`) — indispensable pour Docker.

---

## 8. Sécurité (checklist v0)
- Comparaisons HMAC en temps constant partout.
- `auth_timestamp` REST : rejeter au-delà de ±600 s (anti-replay).
- Secrets jamais loggés, jamais dans les messages d'erreur.
- Origins autorisées configurables (CORS de l'upgrade WS) — vide = tout accepter (dev), à restreindre en prod, documenté.
- Fuzzing basique du parser de trames (cargo-fuzz) avant la v1.
- Pas de `unsafe` dans le code applicatif (autorisé uniquement via dépendances auditées).

---

## 9. Tests

| Niveau | Outil | Couvre |
|---|---|---|
| Unitaires Rust | `cargo test` | Signatures, parsing protocole, state channels |
| Intégration protocole | Tests Rust lançant le serveur + client `tungstenite` | Handshake, subscribe, fan-out, presence, erreurs |
| **Compat clients réels** | Docker Compose : serveur + PHP (pusher-php-server) + Node (pusher-js headless) | La matrice 4.6 — c'est LE filet de sécurité |
| Charge | k6 / bencher custom | Les cibles 5.1, en CI hebdo (pas à chaque commit) |
| Package PHP | Pest/PHPUnit + orchestra/testbench | Driver, commandes, matrice Laravel |

---

## 10. Roadmap

### v0 — « Reverb drop-in » (objectif : 4-6 semaines de soirées)
- Canaux publics + privés, events REST, ping/pong, erreurs protocole.
- Une instance, état mémoire, config TOML, TLS rustls.
- Package Laravel : provider + install + start.
- Test de bascule : app Reverb → resonance par variables d'env uniquement.
- Benchmark publié vs Reverb.

### v1 — « Production-ready »
- Presence channels complets, client events rate-limités, batch events.
- Endpoints d'inspection (channels, users), métriques Prometheus (`/metrics`).
- Webhooks (channel_occupied/vacated, member_added/removed).
- Fuzzing, docs de déploiement (nginx, systemd, Docker), benchmark vs Soketi.

### v2 — selon traction uniquement
- Scaling horizontal (NATS ou Redis pub/sub en option, jamais requis).
- Canaux chiffrés (`private-encrypted-`).
- Bundle Symfony.

---

## 11. Décisions actées (ADR courts)

| # | Décision | Raison |
|---|---|---|
| 1 | Axum plutôt qu'Actix Web | Intégration Tokio native, API plus simple, perf équivalente en pratique ; Actix n'apporte rien de décisif ici |
| 2 | Protocole Pusher, pas de protocole maison | Compat immédiate avec tout l'écosystème (Echo, libs PHP/JS/mobiles) — c'est la promesse n°1 |
| 3 | Pas d'extension PHP / FFI | Un serveur long-running ne rentre pas dans le modèle d'exécution PHP ; le réseau est la seule frontière saine |
| 4 | Pas de Redis en v0 | Zéro dépendance = argument d'adoption ; une instance couvre déjà la cible |
| 5 | musl + rustls | Binaire statique universel, image Docker scratch |
| 6 | Package = surcouche du driver pusher Laravel | Réutiliser > réécrire ; compat garantie |
| 7 | Slow-consumer kill (buffer borné + try_send) | Un client lent ne doit jamais dégrader les autres — condition de la latence stable sous charge |