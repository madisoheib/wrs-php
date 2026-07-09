<?php

namespace Resonance\Laravel\Console;

use Illuminate\Console\Command;
use Resonance\Laravel\Platform;

class StartCommand extends Command
{
    protected $signature = 'resonance:start {--config= : Path to a resonance.toml (default: generated from config)}';
    protected $description = 'Run the resonance WebSocket server.';

    public function handle(): int
    {
        $bin = config('resonance.bin');
        if (! is_file($bin)) {
            $this->error("Binary not found at {$bin}. Run: php artisan resonance:install");
            return self::FAILURE;
        }

        $config = $this->option('config');
        if (! $config) {
            $config = storage_path('resonance.toml');
            file_put_contents($config, Platform::toml(config('resonance')));
        }

        $this->info("Starting resonance (config: {$config}) — Ctrl+C to stop.");
        $cmd = escapeshellarg($bin) . ' start --config ' . escapeshellarg($config);
        passthru($cmd, $code);

        return $code === 0 ? self::SUCCESS : self::FAILURE;
    }
}
