<?php

namespace Resonance\Laravel;

use Illuminate\Broadcasting\Broadcasters\PusherBroadcaster;
use Illuminate\Support\Facades\Broadcast;
use Illuminate\Support\ServiceProvider;
use Pusher\Pusher;
use Resonance\Laravel\Console\InstallCommand;
use Resonance\Laravel\Console\StartCommand;

class ResonanceServiceProvider extends ServiceProvider
{
    public function register(): void
    {
        $this->mergeConfigFrom(__DIR__ . '/../config/resonance.php', 'resonance');
    }

    public function boot(): void
    {
        $this->publishes([
            __DIR__ . '/../config/resonance.php' => config_path('resonance.php'),
        ], 'resonance-config');

        if ($this->app->runningInConsole()) {
            $this->commands([InstallCommand::class, StartCommand::class]);
        }

        // The server speaks Pusher, so we reuse Laravel's PusherBroadcaster
        // verbatim — just point its client at resonance. Users set
        // BROADCAST_CONNECTION and a 'driver' => 'resonance' connection.
        Broadcast::extend('resonance', function ($app, $config) {
            $c = config('resonance');
            $pusher = new Pusher($c['key'], $c['secret'], $c['app_id'], [
                'host'    => $c['host'],
                'port'    => (int) $c['port'],
                'scheme'  => $c['scheme'],
                'useTLS'  => $c['scheme'] === 'https',
                'encrypted' => $c['scheme'] === 'https',
            ]);

            return new PusherBroadcaster($pusher);
        });
    }
}
