# Resonance for Laravel

**Laravel broadcasting driver for [Resonance](https://github.com/madisoheib/wrs-php)
— a self-hosted, Pusher-compatible WebSocket server shipped as a single static binary.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

No Redis, no Node, no PHP extensions. The server speaks the Pusher protocol, so
Laravel Echo and your existing broadcasting code work unchanged — this package
just wires Laravel to it and manages the binary for you.

## Requirements

- PHP ≥ 7.4
- Laravel 6, 7, 8, 9, 10, 11, 12 or 13 — one package version covers all.
  Every version is verified end-to-end (real app, real broadcast, real
  pusher-js subscriber) by [`qa/laravel/matrix.sh`](https://github.com/madisoheib/wrs-php/blob/main/qa/laravel/matrix.sh):

| Laravel | PHP tested | Status |
|---|---|---|
| 6 / 7 | 7.4 | ✅ |
| 8 | 8.0 | ✅ |
| 9 | 8.1 | ✅ |
| 10 | 8.2 | ✅ |
| 11 / 12 | 8.3 | ✅ |
| 13 | 8.4 | ✅ |

## Installation — two commands, any Laravel project

```bash
composer require resonance/resonance-laravel
php artisan resonance:install
```

`resonance:install` does everything:
- detects your OS/architecture (Linux x86_64/ARM64, macOS Intel/Apple Silicon,
  Windows) and downloads the matching server binary from GitHub Releases
  (SHA-256 verified) into `./bin`
- points broadcasting at resonance in your `.env` (both `BROADCAST_DRIVER`
  and `BROADCAST_CONNECTION`, so every Laravel version picks it up)
- generates random `RESONANCE_APP_ID` / `RESONANCE_KEY` / `RESONANCE_SECRET`
  credentials (existing values are never overwritten; `--no-env` skips this)

Then start broadcasting:

```bash
php artisan resonance:start
```

That's it — `broadcast(new MyEvent())` and Laravel Echo work.

## Configuration

Switching an existing Pusher/Reverb app is **environment-only** — no code changes:

```dotenv
BROADCAST_CONNECTION=resonance   # Laravel 11+
BROADCAST_DRIVER=resonance       # Laravel 6-10 read this variable instead

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
