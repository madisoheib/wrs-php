<?php

return [
    // Where the resonance server lives (used by the broadcast driver to POST events).
    'host'   => env('RESONANCE_HOST', '127.0.0.1'),
    'port'   => env('RESONANCE_PORT', 8080),
    'scheme' => env('RESONANCE_SCHEME', 'http'), // 'https' if TLS terminates before the server

    // App credentials — must match an [[apps]] entry in the server's config.
    'app_id' => env('RESONANCE_APP_ID', 'app1'),
    'key'    => env('RESONANCE_KEY'),
    'secret' => env('RESONANCE_SECRET'),

    // Local binary (installed by `php artisan resonance:install`).
    'bin' => env('RESONANCE_BIN', base_path('bin/resonance')),

    // GitHub release to pull the binary from.
    'release' => [
        'repo'    => env('RESONANCE_REPO', 'madisoheib/wrs-php'),
        'version' => env('RESONANCE_VERSION', 'latest'),
    ],
];
