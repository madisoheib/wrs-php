# Technical Specification — Rust WebSocket Server for the PHP Ecosystem

**Working code name:** `resonance`
**Document version:** 0.2 — July 2026
**Goal:** a self-hosted, Pusher-protocol-compatible, framework-agnostic WebSocket server, distributed as a single binary. A Composer package for Laravel integration as the first adapter.

---

## 1. Vision and guiding principles

### 1.1 The problem
PHP developers who want performant real-time features must choose between: a paid SaaS (Pusher, Ably), a PHP server that saturates early (Reverb: ~1,000 connections at 95% CPU on a small server), or a Node solution (Soketi) that imposes an extra runtime. There is no **compiled, zero-dependency real-time server designed for all of PHP** (Laravel, Symfony, WordPress, vanilla).

### 1.2 The three promises (in order)
1. **Maximum compatibility** — the full Pusher protocol. Every existing Pusher client (Laravel Echo, pusher-js, pusher-php-server, mobile clients) works without modification.
2. **Performance under load** — maximum connection density per CPU, stable latency as load grows. That's the real gain vs PHP/Node; idle latency is equivalent everywhere.
3. **Trivial installation** — one static binary, zero Redis, zero Node, zero runtime. `./resonance start` and you're live.

### 1.3 Non-goals (v0/v1)
- No horizontal multi-instance scaling in v0 (one well-utilized vertical instance already covers tens of thousands of connections).
- No home-grown proprietary protocol — Pusher-compatible or nothing.
- No admin UI in v0 (a text metrics endpoint is enough).

---

## 2. Overall architecture

```
┌──────────────────────────────────────────────────────────────┐
│  Repo 1: resonance (Rust)                                    │
│  The server. Single binary, Pusher protocol, HTTP API.       │
└──────────────────────────────────────────────────────────────┘
┌──────────────────────────────────────────────────────────────┐
│  Repo 2: resonance-laravel (PHP)                             │
│  Composer package: broadcast driver, config, artisan         │
│  command, channel auth. Thin — the logic lives in the core.  │
└──────────────────────────────────────────────────────────────┘
```

**Runtime flow:**

```
Browser (Echo/pusher-js)
    │  WSS (Pusher protocol, port 8080)
    ▼
resonance server (Rust) ◄──── POST /apps/{app_id}/events (HTTP, signed)
    │                              ▲
    │  private channel auth        │
    ▼                              │
PHP app ───────────────────────────┘
(/broadcasting/auth endpoint)   (pusher-php-server or the package)
```

Three communication channels, all in Pusher format:
1. **Client ↔ server**: WebSocket, Pusher protocol (subscribe, events, ping/pong).
2. **PHP app → server**: HTTP REST `POST /apps/{app_id}/events`, HMAC signature — the API `pusher-php-server` already speaks.
3. **Server → PHP app**: private/presence channel auth requests (the client supplies the signature obtained from the app), and optional webhooks (v1).

---

## 3. Rust core — technical stack

### 3.1 Dependencies (Cargo.toml)

| Crate | Role | Rationale |
|---|---|---|
| `tokio` (full) | Async runtime | De-facto standard, multi-threaded work-stealing scheduler |
| `axum` | HTTP + WebSocket upgrade | Native Tokio integration, typed extraction, simpler than Actix for a near-identical result |
| `tokio-tungstenite` / `axum::extract::ws` | WebSocket | Provided by axum, built on tungstenite |
| `dashmap` | Concurrent state | Sharded HashMap, lock-free reads, avoids a global Mutex |
| `serde` + `serde_json` | Serialization | Pusher messages = JSON |
| `hmac` + `sha2` | Signatures | REST API auth + private channels (HMAC-SHA256) |
| `tracing` + `tracing-subscriber` | Structured logs | Observability with zero cost when disabled |
| `clap` | CLI | `resonance start --config ...` |
| `toml` | Config file | Readable, standard |
| `rustls` + `tokio-rustls` | Native TLS | No OpenSSL — portable static binary |

**Forbidden:** no `openssl` (breaks static builds), no C dependencies, no Redis in v0.

### 3.2 Concurrency model

```
main
 └── Multi-threaded Tokio runtime (workers = CPU cores)
      ├── WS listener (axum) ── 1 Tokio task PER connection
      ├── HTTP API listener (same axum server, separate routes)
      └── Periodic task: ping/pong, dead-connection eviction
```

**Golden rule: one connection = one Tokio task + one outbound mpsc channel.**
Each connection owns:
- A read task (inbound client messages: subscribe, unsubscribe, ping, client events).
- A `tokio::sync::mpsc::Sender<Message>` for writes. Every write to this client goes through that channel; a single writer task drains the channel into the socket. **Never multi-task direct writes to the socket** (the classic source of frame corruption).

### 3.3 State structures (the heart of performance)

```rust
struct AppState {
    // app_id -> App (key, secret, limits)
    apps: DashMap<AppId, App>,
    // socket_id -> connection handle (mpsc sender + metadata)
    connections: DashMap<SocketId, ConnectionHandle>,
    // (app_id, channel_name) -> set of subscribed socket_ids
    channels: DashMap<(AppId, ChannelName), ChannelState>,
}

struct ChannelState {
    subscribers: HashSet<SocketId>,
    // Presence channels only:
    presence: Option<HashMap<SocketId, PresenceMember>>,
}
```

Critical points:
- Sharded `DashMap` → broadcasts on different channels don't contend.
- `ChannelState` behind a shard: fanning out an event locks **a single shard**, briefly, to clone the socket list, then releases BEFORE sending. Never send while holding a lock.
- `SocketId`: Pusher format `{u64}.{u64}`, randomly generated.

### 3.4 Broadcast hot path (optimize first)

```
POST /apps/{id}/events  (from PHP)
  1. Verify HMAC signature (early rejection, before any costly parsing)
  2. Parse the JSON body once
  3. PRE-SERIALIZE the outbound message ONCE → Arc<str> / Bytes
  4. For each target channel:
     a. Read ChannelState, clone the Vec<Sender> (very short lock)
     b. For each sender: try_send(message.clone())  // Arc clone = no copy
  5. Reply 200 immediately (fire-and-forget toward clients)
```

**Non-negotiable hot-path optimizations:**
- The outbound payload is serialized **once** and shared via `Arc`/`Bytes` — never N serializations for N recipients. At 10k subscribers, this is THE difference.
- `try_send` (non-blocking) with a bounded channel (e.g. 64 messages). If a client's buffer is full → client too slow → disconnect it ("slow consumer kill" policy, like serious brokers). A slow client must never slow the others down.
- `TCP_NODELAY` enabled on all sockets (latency > throughput for real-time).
- No allocation inside the fan-out loop.

### 3.5 Limits and protections (from v0)
- Max client message size: 10 KB (Pusher default), configurable.
- Max channels per connection: configurable (default 100).
- Rate limit on client events (`client-*`): Pusher default ~10/s per connection.
- Activity timeout: ping every 120 s (Pusher protocol default `activity_timeout`), close if no pong within 30 s.
- Backpressure: bounded per-connection mpsc channel (see 3.4).

---

## 4. Pusher protocol compatibility — the complete checklist

This is the most important section for promise #1. Compatibility lives in the details. Reference: Pusher Channels Protocol v7.

### 4.1 WebSocket handshake
- URL: `ws(s)://host:port/app/{key}?protocol=7&client=...&version=...`
- On connection, immediately send:
```json
{"event":"pusher:connection_established","data":"{\"socket_id\":\"123.456\",\"activity_timeout\":120}"}
```
- **Compat trap #1:** the `data` field is a **JSON-encoded string**, not a JSON object. Every Pusher event does this (double encoding). Clients break silently if you send an object.

### 4.2 Protocol events to implement

| Event | Direction | Notes |
|---|---|---|
| `pusher:connection_established` | S→C | On handshake |
| `pusher:subscribe` | C→S | With `channel`, plus `auth` + `channel_data` for private/presence |
| `pusher:unsubscribe` | C→S | |
| `pusher_internal:subscription_succeeded` | S→C | For presence: contains the member list |
| `pusher:ping` / `pusher:pong` | Bidirectional | Answer in both directions |
| `pusher:error` | S→C | With the official error codes (4000-4299) |
| `pusher_internal:member_added` / `member_removed` | S→C | Presence channels |
| `client-*` (client events) | C→S→C | Private/presence channels only, never echoed to the sender, rate-limited |

### 4.3 Channel types

| Type | Prefix | Auth required | Specifics |
|---|---|---|---|
| Public | (none) | No | Direct subscribe |
| Private | `private-` | Yes | HMAC signature verified at subscribe |
| Private encrypted | `private-encrypted-` | Yes | v1+ (client-side encryption, server relays) |
| Presence | `presence-` | Yes | `channel_data` = user_id + user_info; broadcast member_added/removed; return the member list at subscribe |

### 4.4 Private channel auth signature (the detail that breaks everything)
The client obtains the signature from the PHP app (`/broadcasting/auth` in Laravel). The server must **verify**:
```
signature = HMAC-SHA256(secret, socket_id + ":" + channel_name)
// presence: socket_id + ":" + channel_name + ":" + channel_data
auth = "{key}:{hex(signature)}"
```
Constant-time comparison (`subtle` or equivalent) to prevent timing attacks.

### 4.5 HTTP REST API (what pusher-php-server calls)

| Endpoint | Method | Role |
|---|---|---|
| `/apps/{app_id}/events` | POST | Publish an event (THE critical endpoint) |
| `/apps/{app_id}/batch_events` | POST | Publish in batch (v1) |
| `/apps/{app_id}/channels` | GET | List occupied channels (v1) |
| `/apps/{app_id}/channels/{name}` | GET | Info on one channel (v1) |
| `/apps/{app_id}/channels/{name}/users` | GET | Presence members (v1) |

**REST request signing (Pusher auth scheme):**
```
string_to_sign = "POST\n/apps/{app_id}/events\n" + sorted_query_string
auth_signature = HMAC-SHA256(secret, string_to_sign)
```
Query string: `auth_key`, `auth_timestamp` (±600 s tolerance), `auth_version=1.0`, `body_md5` (MD5 of the body), parameters sorted alphabetically. Implement **exactly** this scheme — it's what `pusher-php-server` generates. Test against the official library, not your own implementation.

### 4.6 Compatibility matrix to validate (integration tests)

| Client | Test |
|---|---|
| `pusher-js` (browser) | Connect, subscribe public/private/presence, receive events, client events |
| **Laravel Echo** (pusher-js wrapper) | `Echo.channel()`, `Echo.private()`, `Echo.join()` (presence), whisper |
| `pusher-php-server` | `trigger()`, `triggerBatch()`, channel auth |
| Laravel `broadcast()` + pusher driver | The full Laravel flow without the dedicated package |
| Automatic reconnection | Cut the socket, verify the client resubscribes automatically |

**v0 definition of done: an existing Laravel app using Reverb switches to resonance by changing ONLY environment variables (host/port/key/secret). Zero code changes.**

### 4.7 Network and deployment (infra compat)
- Native TLS (rustls) OR TLS termination at the reverse proxy — support both.
- Work behind nginx/Caddy/Traefik: document the WebSocket upgrade config (`proxy_set_header Upgrade/Connection`).
- Listen on a single port for WS + HTTP API (separate routes): simplifies firewall and proxy.
- IPv4 + IPv6.
- `X-Forwarded-For` header for logs behind a proxy.

---

## 5. Performance — numeric targets and method

### 5.1 v0 targets (on a 2 vCPU / 4 GB machine, t3.medium class)

| Metric | Target | Competitor reference |
|---|---|---|
| Simultaneous idle connections | ≥ 50,000 | Reverb: ~20k reported on t3.medium |
| CPU at 1,000 active connections | < 10% | Reverb: ~95% on a $5 server; Go: ~18% |
| p99 broadcast latency (1 channel, 1k subscribers) | < 10 ms intra-DC | |
| Memory per idle connection | < 20 KB | |
| Inbound event throughput (REST API) | ≥ 5,000 req/s | |

### 5.2 System tuning to document (otherwise benchmarks lie)
- `ulimit -n` (file descriptors) ≥ 2× target connections.
- `net.core.somaxconn`, `net.ipv4.tcp_tw_reuse` depending on load.
- The binary must print a warning at startup if `ulimit` is too low.

### 5.3 Benchmark methodology (public deliverable)
- Tool: k6 (WS scenario) or a custom Rust bencher published in the repo.
- Scenarios: (a) ramp 0→N idle connections, (b) 1k connections + 100 msg/s broadcast on a shared channel, (c) extreme fan-out: 1 event → 10k subscribers, measure p50/p99 delivery.
- Compare on the **same hardware, same scenario**: resonance vs Reverb vs Soketi. Publish scripts + raw results in `bench/`. Reproducibility is the credibility argument.
- Never publish a number a third party cannot reproduce.

### 5.4 Performance traps to avoid (systematic code review)
- Serializing the same payload N times (see 3.4) — the silent killer.
- Sending while holding a DashMap lock.
- A blocking `send().await` on a slow client mid-fan-out — always `try_send`.
- Debug-level logs in the hot path (tracing with compile-time filters).
- Allocations in the fan-out loop (profile with `cargo flamegraph`).

---

## 6. Composer package `resonance-laravel`

### 6.1 Principle: the thinnest possible package
Since the server is Pusher-compatible, Laravel ALREADY knows how to talk to it via the existing `pusher` driver (custom host/port config). The package adds comfort, not plumbing:

```
resonance-laravel/
├── composer.json          # require: php ^8.2, laravel ^11|^12; suggests nothing — zero extensions
├── config/resonance.php   # host, port, app_id, key, secret, TLS
├── src/
│   ├── ResonanceServiceProvider.php   # merges config, registers the driver
│   ├── ResonanceBroadcaster.php       # extends PusherBroadcaster (reuse, don't rewrite)
│   └── Console/
│       ├── InstallCommand.php         # downloads the binary from the GitHub release
│       │                              #   by OS/arch, drops it in ./bin, chmod +x
│       └── StartCommand.php           # php artisan resonance:start (runs the binary)
└── tests/
```

### 6.2 Decisions
- **No PHP extension, no FFI** — pointless here, everything goes over the network. The package stays pure PHP → installable everywhere, zero adoption friction.
- `InstallCommand` detects OS + architecture (`php_uname`) and downloads the right binary from GitHub Releases with SHA-256 checksum verification. It's the DX equivalent of `php artisan reverb:start`.
- Reuse Laravel's `PusherBroadcaster` instead of rewriting: less code, guaranteed compat with framework evolution.
- Version the compat: PHP 8.2/8.3/8.4 × Laravel 11/12 matrix in CI.

### 6.3 Future adapters (v2+, only if traction)
- Symfony bundle (Mercure is SSE; position on bidirectional).
- Generic PHP client = usage documentation for `pusher-php-server` pointed at resonance (near-zero code).
- WordPress plugin if demand exists.

---

## 7. Binary distribution

### 7.1 Compilation targets (GitHub Actions CI)

| Target | Priority |
|---|---|
| `x86_64-unknown-linux-musl` (static) | P0 — the typical server |
| `aarch64-unknown-linux-musl` | P0 — ARM (Graviton, Ampere, Raspberry) |
| `x86_64-apple-darwin` + `aarch64-apple-darwin` | P1 — local dev |
| `x86_64-pc-windows-msvc` | P2 — local Windows dev |

**musl + rustls = 100% static binary**, no `.so` required, runs on any distro and in a `scratch` Docker image.

### 7.2 Distribution channels
1. GitHub Releases (binaries + checksums) — source of truth.
2. Official Docker image (`FROM scratch`, ~10 MB) on ghcr.io.
3. `php artisan resonance:install` (see 6.2).
4. Later: Homebrew tap, AUR.

### 7.3 Server config (TOML file + env overrides)
```toml
[server]
host = "0.0.0.0"
port = 8080

[tls]                    # optional — otherwise terminate at the proxy
cert = "/path/cert.pem"
key = "/path/key.pem"

[[apps]]
id = "app1"
key = "resonance-key"
secret = "resonance-secret"
max_connections = 0      # 0 = unlimited
enable_client_events = true

[limits]
max_message_size_kb = 10
activity_timeout_s = 120
```
Every value overridable via environment variable (`RESONANCE_PORT=...`) — essential for Docker.

---

## 8. Security (v0 checklist)
- Constant-time HMAC comparisons everywhere.
- REST `auth_timestamp`: reject beyond ±600 s (anti-replay).
- Secrets never logged, never in error messages.
- Configurable allowed origins (CORS of the WS upgrade) — empty = accept all (dev), restrict in prod, documented.
- Basic fuzzing of the frame parser (cargo-fuzz) before v1.
- No `unsafe` in application code (allowed only via audited dependencies).

---

## 9. Testing

| Level | Tool | Covers |
|---|---|---|
| Rust unit tests | `cargo test` | Signatures, protocol parsing, channel state |
| Protocol integration | Rust tests launching the server + `tungstenite` client | Handshake, subscribe, fan-out, presence, errors |
| **Real client compat** | Docker Compose: server + PHP (pusher-php-server) + Node (headless pusher-js) | The 4.6 matrix — THE safety net |
| Load | k6 / custom bencher | The 5.1 targets, weekly CI (not per-commit) |
| PHP package | Pest/PHPUnit + orchestra/testbench | Driver, commands, Laravel matrix |

---

## 10. Roadmap

### v0 — "Reverb drop-in" (goal: 4-6 weeks of evenings)
- Public + private channels, REST events, ping/pong, protocol errors.
- Single instance, in-memory state, TOML config, rustls TLS.
- Laravel package: provider + install + start.
- Switch test: Reverb app → resonance via environment variables only.
- Published benchmark vs Reverb.

### v1 — "Production-ready"
- Full presence channels, rate-limited client events, batch events.
- Inspection endpoints (channels, users), Prometheus metrics (`/metrics`).
- Webhooks (channel_occupied/vacated, member_added/removed).
- Fuzzing, deployment docs (nginx, systemd, Docker), benchmark vs Soketi.

### v2 — traction-dependent only
- Horizontal scaling (optional NATS or Redis pub/sub, never required).
- Encrypted channels (`private-encrypted-`).
- Symfony bundle.

---

## 11. Recorded decisions (short ADRs)

| # | Decision | Reason |
|---|---|---|
| 1 | Axum over Actix Web | Native Tokio integration, simpler API, equivalent perf in practice; Actix brings nothing decisive here |
| 2 | Pusher protocol, no custom protocol | Immediate compat with the whole ecosystem (Echo, PHP/JS/mobile libs) — promise #1 |
| 3 | No PHP extension / FFI | A long-running server doesn't fit PHP's execution model; the network is the only sane boundary |
| 4 | No Redis in v0 | Zero dependencies = adoption argument; one instance already covers the target |
| 5 | musl + rustls | Universal static binary, scratch Docker image |
| 6 | Package = thin layer over Laravel's pusher driver | Reuse > rewrite; guaranteed compat |
| 7 | Slow-consumer kill (bounded buffer + try_send) | A slow client must never degrade the others — the condition for stable latency under load |
