# wrs-php — resonance

Self-hosted, Pusher-compatible WebSocket server in Rust, built for the PHP
ecosystem. Single static binary, zero Redis, zero Node. Any existing Pusher
client (Laravel Echo, pusher-js, pusher-php-server) works unchanged.

See [`project.md`](project.md) for the full spec.

## Layout

- **`/` (Rust)** — the `resonance` server: Pusher protocol over WebSocket + the
  signed HTTP events API. `cargo run -- start --config resonance.toml`.
- **`resonance-laravel/` (PHP)** — thin Laravel package: broadcast driver +
  `resonance:install` / `resonance:start` commands.

## Quick start

```bash
cp resonance.toml.example resonance.toml
cargo run --release -- start --config resonance.toml
# or the static container:
docker build -t resonance . && docker run -p 8080:8080 resonance
```

## Tests

```bash
cargo test                              # protocol + signature unit tests
cd qa && npm install && node e2e.mjs    # end-to-end with real pusher-js + pusher libs
cd resonance-laravel && php qa/php_check.php
```
