<?php

namespace Resonance\Laravel;

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

        // Register the broadcast connection so users only set
        // BROADCAST_CONNECTION=resonance — no editing of config/broadcasting.php.
        $config = $this->app['config'];
        $config->set('broadcasting.connections.resonance', array_merge(
            ['driver' => 'resonance'],
            $config->get('broadcasting.connections.resonance', []),
        ));
    }

    public function boot(): void
    {
        $this->publishes([
            __DIR__ . '/../config/resonance.php' => config_path('resonance.php'),
        ], 'resonance-config');

        if ($this->app->runningInConsole()) {
            $this->commands([InstallCommand::class, StartCommand::class]);
        }

        // The server speaks Pusher, so we build on Laravel's PusherBroadcaster —
        // ResonanceBroadcaster only normalizes the pusher lib's trigger()
        // signature so Laravel 6 through 13 all work with one package version.
        Broadcast::extend('resonance', function ($app, $config) {
            $c = config('resonance');
            $pusher = new Pusher($c['key'], $c['secret'], $c['app_id'], [
                'host'    => $c['host'],
                'port'    => (int) $c['port'],
                'scheme'  => $c['scheme'],
                'useTLS'  => $c['scheme'] === 'https',
                'encrypted' => $c['scheme'] === 'https',
            ]);

            return new ResonanceBroadcaster($pusher);
        });
    }
}
