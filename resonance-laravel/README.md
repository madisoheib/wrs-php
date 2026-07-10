# Resonance for Laravel

**Laravel broadcasting driver for [Resonance](https://github.com/madisoheib/wrs-php)
— a self-hosted, Pusher-compatible WebSocket server shipped as a single static binary.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

No Redis, no Node, no PHP extensions. The server speaks the Pusher protocol, so
Laravel Echo and your existing broadcasting code work unchanged — this package
just wires Laravel to it and manages the binary for you.

## Requirements

- PHP ≥ 7.4
- Laravel 6, 7, 8, 9, 10, 11, 12 or 13 — one package version covers all
  (verified end-to-end on Laravel 6/PHP 7.4 and Laravel 13/PHP 8.3)

## Installation

```bash
composer require resonance/resonance-laravel
php artisan resonance:install
```

`resonance:install` detects your OS and architecture (Linux x86_64/ARM64,
macOS Intel/Apple Silicon, Windows), downloads the matching binary from GitHub
Releases, verifies its SHA-256 checksum and drops it in `./bin`.

## Configuration

Switching an existing Pusher/Reverb app is **environment-only** — no code changes:

```dotenv
BROADCAST_CONNECTION=resonance   # Laravel 11+
# BROADCAST_DRIVER=resonance     # Laravel 6-10 use this variable instead

RESONANCE_APP_ID=app1
RESONANCE_KEY=my-key
RESONANCE_SECRET=my-secret
RESONANCE_HOST=127.0.0.1
RESONANCE_PORT=8080
RESONANCE_SCHEME=http        # https if TLS terminates before the server
```

The service provider registers both the `resonance` driver and the broadcasting
connection automatically. Optionally publish the config:

```bash
php artisan vendor:publish --tag=resonance-config
```

| Key | Env | Default | |
|---|---|---|---|
| `host` | `RESONANCE_HOST` | `127.0.0.1` | Server host |
| `port` | `RESONANCE_PORT` | `8080` | Server port (WS + REST) |
| `scheme` | `RESONANCE_SCHEME` | `http` | `https` behind TLS |
| `app_id` | `RESONANCE_APP_ID` | `app1` | Must match the server config |
| `key` / `secret` | `RESONANCE_KEY` / `RESONANCE_SECRET` | — | Must match the server config |
| `bin` | `RESONANCE_BIN` | `base_path('bin/resonance')` | Binary location |

## Usage

Start the server (generates a `resonance.toml` from your config):

```bash
php artisan resonance:start
# or with a hand-written config:
php artisan resonance:start --config /etc/resonance.toml
```

Broadcast as usual:

```php
broadcast(new OrderShipped($order));
```

Frontend via Laravel Echo (`pusher-js` transport):

```js
const echo = new Echo({
    broadcaster: 'pusher',
    key: import.meta.env.VITE_RESONANCE_KEY,
    wsHost: import.meta.env.VITE_RESONANCE_HOST,
    wsPort: import.meta.env.VITE_RESONANCE_PORT,
    forceTLS: false,
    enabledTransports: ['ws', 'wss'],
});
```

Private channels authenticate through the standard `/broadcasting/auth`
endpoint — nothing to change.

## How it works

The package is intentionally thin: the server is Pusher-compatible, so the
driver extends Laravel's own `PusherBroadcaster` and points it at Resonance.
All the heavy lifting (connection handling, fan-out, backpressure) lives in
the compiled server — your PHP app only sends signed HTTP requests to it.

## License

[MIT](LICENSE)
