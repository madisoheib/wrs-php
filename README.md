# Resonance

**Self-hosted, Pusher-compatible WebSocket server for the PHP ecosystem — a single static binary.**

[![CI](https://github.com/madisoheib/wrs-php/actions/workflows/ci.yml/badge.svg)](https://github.com/madisoheib/wrs-php/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/madisoheib/wrs-php)](https://github.com/madisoheib/wrs-php/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Resonance speaks the Pusher Channels protocol, so every existing client works
unchanged: **Laravel Echo**, **pusher-js**, **pusher-php-server**, mobile SDKs.
No Redis, no Node, no PHP extensions — download one binary and run it.

```bash
./resonance start --config resonance.toml
```

## How it compares

### The market

| | Pusher / Ably | Laravel Reverb | Soketi | **Resonance** |
|---|---|---|---|---|
| Model | SaaS (paid per connection/message) | self-hosted | self-hosted | **self-hosted** |
| Runtime required | — | PHP + `ext-ev`/`ext-uv` beyond ~1k conns | Node.js | **none — static binary** |
| Language / concurrency | — | PHP, single-threaded event loop | JS (µWebSockets core), 1 worker/core with adapter | **Rust, all cores natively** |
| Horizontal scaling deps | managed | Redis for multi-server | Redis for multi-server | none needed at target scale (v2: optional) |
| Pusher protocol | ✅ origin | ✅ | ✅ | ✅ |
| Install | account + latency to their region | composer + PHP tuning | npm / Docker | **one binary / `FROM scratch` Docker (~5 MB)** |
| Slow-client protection | managed | ❌ unbounded buffering | partial (backpressure config) | ✅ bounded buffers + disconnect |
| Status | commercial | active (Laravel official) | maintenance slowed since 2024 | early (v0) |

### Measured head-to-head — Resonance vs Reverb (Linux, 2 pinned cores each)

AWS c6i.xlarge, servers pinned to 2 cores, full methodology and caveats in
[`bench/RESULTS.md`](bench/RESULTS.md):

| Metric | Resonance | Reverb (tuned¹) |
|---|---|---|
| 60,000 idle connections | ✅ 770 MiB, 100% established | not attempted |
| Memory @ 40k connections | **512 MiB** (~12.8 KB/conn) | 834 MiB (~20 KB/conn) |
| Sustained 50k deliveries/s — p50 / p99 | **14.7 / 32 ms** | 24.6 / **254 ms** |
| CPU at that load (share of its ceiling) | **~13%** (2 cores usable) | 62% (1 core, hard cap) |
| Fan-out 1 event → 10k subs | p50 94 ms | p50 122 ms |

Earlier same-machine Docker runs (1k conns) told the same story: ~2× lower
latency, ~3× less memory, bounded slow-consumer behavior vs unbounded
buffering. Soketi is absent because we haven't run it on this harness yet —
we don't publish numbers we didn't measure.

| Metric | Resonance | Reverb (tuned¹) |
|---|---|---|
| Baseline memory (0 conns) | **0.9 MiB** | 33 MiB |
| Idle memory @ 1k conns | **17 MiB** (~16 KB/conn) | 55 MiB (~22 KB/conn) |
| Idle memory @ 5k conns | **83 MiB** | 142 MiB |
| Fan-out latency p50 / p99 (1k subs) | **21 / 27 ms** | 39 / 45 ms |
| Fan-out latency p50 (5k subs) | **48 ms** | 91 ms |
| CPU @ 20 000 deliveries/s | **22 % avg** | 35 % avg |
| Sustained broadcast p50 | **8.5 ms** (stable at all tested rates) | 13.9 ms |
| Slow consumer under flood | disconnected, memory stays bounded | buffers unbounded — p99 reached 209 s |
| Stock install at 5k conns | ✅ no tuning | ❌ dies at ~1k (`stream_select` fd cap) |

¹ *Reverb needed `ext-ev`, `memory_limit=-1` and a raised connection limit to
complete the 5k test; Resonance ran stock. Absolute numbers are specific to
this hardware (Docker on an 8-core host) — treat them as relative. CPU-core
ceilings (Reverb: one core; Resonance: all cores) only diverge further at
scales this harness can't generate; large-scale Linux results will be
published when available.*

## Quick start

### 1. Get the binary

One-liner (Linux x86_64/ARM64, macOS Intel/Apple Silicon — verifies SHA-256):

```bash
curl -sSL https://raw.githubusercontent.com/madisoheib/wrs-php/main/install.sh | sh
```

Or download manually from [Releases](https://github.com/madisoheib/wrs-php/releases)
(includes Windows), or with Docker:

```bash
docker run -p 8080:8080 ghcr.io/madisoheib/wrs-php:latest
```

Or build from source: `cargo build --release`.

### 2. Configure

```toml
# resonance.toml
[server]
host = "0.0.0.0"
port = 8080

[[apps]]
id = "app1"
key = "my-key"
secret = "my-secret"

[limits]
max_message_size_kb = 10
activity_timeout_s = 120
max_channels_per_connection = 100
```

Every value can be overridden by environment (`RESONANCE_HOST`, `RESONANCE_PORT`).

### 3. Point your app at it

**Laravel** (6 through 13) — use
[`resonance/resonance-laravel`](https://github.com/madisoheib/resonance-laravel):

```bash
composer require resonance/resonance-laravel
php artisan resonance:install   # downloads the binary + configures .env (credentials generated)
php artisan resonance:start
```

**Any PHP** — `pusher-php-server` already speaks the protocol:

```php
$pusher = new Pusher\Pusher('my-key', 'my-secret', 'app1', [
    'host' => '127.0.0.1', 'port' => 8080, 'scheme' => 'http',
]);
$pusher->trigger('my-channel', 'my-event', ['hello' => 'world']);
```

**Browser** — Laravel Echo / pusher-js with `wsHost`/`wsPort` pointed at the server.

## Protocol support

- WebSocket handshake, `pusher:connection_established`, ping/pong, protocol error codes
- Public, private and **presence** channels (HMAC auth, constant-time verification;
  member roster, `member_added` / `member_removed` — `Echo.join()` works)
- **Client events** (`client-*`, Echo `whisper()`): private/presence only,
  never echoed to the sender, rate-limited per connection (default 10/s)
- REST API with the full Pusher auth scheme (`auth_signature`, `auth_timestamp`
  ±600 s anti-replay, mandatory `body_md5` on bodies): `POST events`,
  `POST batch_events`, `GET channels`, `GET channels/{name}`,
  `GET channels/{name}/users`
- Sender exclusion via `socket_id`
- Origin allow-list for browser connections (`allowed_origins`)
- **Webhooks** (`channel_occupied` / `channel_vacated` / `member_added` /
  `member_removed`), Pusher format: signed `X-Pusher-Key` + `X-Pusher-Signature`
- Slow-consumer protection: bounded per-connection buffers, non-blocking fan-out,
  laggards are disconnected instead of degrading everyone else
- Dead-connection eviction (server ping after `activity_timeout`, 30 s grace)
- Prometheus metrics at `GET /metrics` (connections, channels, events in,
  messages out, slow-consumer kills)
- Native TLS (rustls): add a `[tls]` table with `cert`/`key` PEM paths to serve
  `wss://` directly — or omit it and terminate TLS at your proxy

## Deployment

Run behind any reverse proxy — one port serves both WebSocket and the REST API:

```nginx
location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
}
```

Raise `ulimit -n` to at least 2× your target connection count.

## Development

```bash
cargo test                                   # protocol + signature unit tests
cd qa && npm install
node e2e.mjs                                 # end-to-end with real pusher-js/pusher libs
node protocol.mjs                            # raw-wire protocol behaviours
qa/laravel/run.sh                            # real Laravel app broadcast in Docker
qa/bench/run.sh                              # benchmark vs Reverb, same harness
```

Full technical specification: [`project.md`](project.md).

## License

[MIT](LICENSE)
