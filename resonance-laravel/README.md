# resonance-laravel

Laravel broadcasting driver + CLI for the [resonance](../) WebSocket server.
The server speaks the Pusher protocol, so this package is deliberately thin —
it reuses Laravel's own `PusherBroadcaster` and just points it at resonance.

## Install

```bash
composer require resonance/resonance-laravel
php artisan resonance:install     # downloads the server binary for your OS/arch
```

## Configure (the whole switch is env vars)

An existing Reverb/Pusher app moves over by changing **only** `.env`:

```dotenv
BROADCAST_CONNECTION=resonance

RESONANCE_APP_ID=app1
RESONANCE_KEY=resonance-key
RESONANCE_SECRET=resonance-secret
RESONANCE_HOST=127.0.0.1
RESONANCE_PORT=8080
RESONANCE_SCHEME=http          # https if TLS terminates before the server
```

Add the connection in `config/broadcasting.php`:

```php
'connections' => [
    'resonance' => ['driver' => 'resonance'],
],
```

Laravel Echo / `pusher-js` on the frontend point at the same host/port/key — no
code changes.

## Run the server

```bash
php artisan resonance:start        # generates a resonance.toml from config and runs the binary
php artisan resonance:start --config /path/to/resonance.toml
```

## Publish the config (optional)

```bash
php artisan vendor:publish --tag=resonance-config
```
