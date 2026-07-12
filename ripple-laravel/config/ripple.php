<?php

return [
    // Where the ripple server lives (used by the broadcast driver to POST events).
    'host'   => env('RIPPLE_HOST', '127.0.0.1'),
    'port'   => env('RIPPLE_PORT', 8080),
    'scheme' => env('RIPPLE_SCHEME', 'http'), // 'https' if TLS terminates before the server

    // App credentials — must match an [[apps]] entry in the server's config.
    'app_id' => env('RIPPLE_APP_ID', 'app1'),
    'key'    => env('RIPPLE_KEY'),
    'secret' => env('RIPPLE_SECRET'),

    // Local binary (installed by `php artisan ripple:install`).
    'bin' => env('RIPPLE_BIN', base_path('bin/ripple')),

    // GitHub release to pull the binary from.
    'release' => [
        'repo'    => env('RIPPLE_REPO', 'madisoheib/ripple'),
        'version' => env('RIPPLE_VERSION', 'latest'),
    ],

    // Web dashboard (Horizon-style) to watch connections, channels and health.
    'dashboard' => [
        'enabled' => env('RIPPLE_DASHBOARD', true),
        'path'    => env('RIPPLE_DASHBOARD_PATH', 'ripple'),
        'middleware' => ['web'],
        // Server metrics endpoint (unauthenticated by design — keep it internal).
        // Defaults to the same host/port as the server above.
        'metrics_url' => env('RIPPLE_METRICS_URL'),
    ],
];
