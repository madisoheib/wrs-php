<?php

namespace Ripple\Laravel;

use Illuminate\Support\Facades\Broadcast;
use Illuminate\Support\Facades\Route;
use Illuminate\Support\ServiceProvider;
use Pusher\Pusher;
use Ripple\Laravel\Console\InstallCommand;
use Ripple\Laravel\Console\StartCommand;
use Ripple\Laravel\Http\DashboardController;

class RippleServiceProvider extends ServiceProvider
{
    public function register(): void
    {
        $this->mergeConfigFrom(__DIR__ . '/../config/ripple.php', 'ripple');

        // Register the broadcast connection so users only set
        // BROADCAST_CONNECTION=ripple — no editing of config/broadcasting.php.
        $config = $this->app['config'];
        $config->set('broadcasting.connections.ripple', array_merge(
            ['driver' => 'ripple'],
            $config->get('broadcasting.connections.ripple', []),
        ));
    }

    public function boot(): void
    {
        $this->publishes([
            __DIR__ . '/../config/ripple.php' => config_path('ripple.php'),
        ], 'ripple-config');

        if ($this->app->runningInConsole()) {
            $this->commands([InstallCommand::class, StartCommand::class]);
        }

        // Dashboard (Horizon-style debug page).
        $this->loadViewsFrom(__DIR__ . '/../resources/views', 'ripple');
        if (config('ripple.dashboard.enabled')) {
            Route::group([
                'prefix' => config('ripple.dashboard.path', 'ripple'),
                'middleware' => config('ripple.dashboard.middleware', ['web']),
            ], function () {
                Route::get('/', [DashboardController::class, 'index'])->name('ripple.dashboard');
                Route::get('/stats', [DashboardController::class, 'stats'])->name('ripple.stats');
                Route::post('/test', [DashboardController::class, 'test'])->name('ripple.test');
            });
        }

        // The server speaks Pusher, so we build on Laravel's PusherBroadcaster —
        // RippleBroadcaster only normalizes the pusher lib's trigger()
        // signature so Laravel 6 through 13 all work with one package version.
        Broadcast::extend('ripple', function ($app, $config) {
            $c = config('ripple');
            $pusher = new Pusher($c['key'], $c['secret'], $c['app_id'], [
                'host'    => $c['host'],
                'port'    => (int) $c['port'],
                'scheme'  => $c['scheme'],
                'useTLS'  => $c['scheme'] === 'https',
                'encrypted' => $c['scheme'] === 'https',
            ]);

            return new RippleBroadcaster($pusher);
        });
    }
}
