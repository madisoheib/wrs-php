<?php

namespace Resonance\Laravel\Console;

use Illuminate\Console\Command;
use Resonance\Laravel\Platform;

class InstallCommand extends Command
{
    protected $signature = 'resonance:install {--version= : Release tag (default: config/latest)} {--force} {--no-env : Do not touch the .env file}';
    protected $description = 'Download the resonance server binary and configure broadcasting in .env.';

    public function handle(): int
    {
        $target = Platform::target(php_uname('s'), php_uname('m'));
        if ($target === null) {
            $this->error('Unsupported platform: ' . php_uname('s') . '/' . php_uname('m'));
            return self::FAILURE;
        }

        $bin = config('resonance.bin');
        if (is_file($bin) && ! $this->option('force')) {
            $this->info("Already installed at {$bin} (use --force to reinstall).");
            return self::SUCCESS;
        }

        $repo = config('resonance.release.repo');
        $version = $this->option('version') ?: config('resonance.release.version');
        $asset = Platform::assetName($target);
        $base = $version === 'latest'
            ? "https://github.com/{$repo}/releases/latest/download"
            : "https://github.com/{$repo}/releases/download/{$version}";

        $this->info("Downloading {$asset} from {$repo} ({$version})...");
        $binary = $this->fetch("{$base}/{$asset}");
        if ($binary === null) {
            $this->error("Download failed: {$base}/{$asset}");
            return self::FAILURE;
        }

        // Verify against the published .sha256 sidecar (skip only if absent).
        $expected = $this->fetch("{$base}/{$asset}.sha256");
        if ($expected !== null) {
            $expected = trim(explode(' ', trim($expected))[0]);
            $actual = hash('sha256', $binary);
            if (! hash_equals($expected, $actual)) {
                $this->error("Checksum mismatch — refusing to install.");
                return self::FAILURE;
            }
        } else {
            $this->warn('No .sha256 published for this asset; skipping checksum.');
        }

        @mkdir(dirname($bin), 0755, true);
        file_put_contents($bin, $binary);
        chmod($bin, 0755);
        $this->info("Installed resonance to {$bin}");

        if (! $this->option('no-env')) {
            $this->configureEnv();
        }
        $this->info('Done. Run: php artisan resonance:start');

        return self::SUCCESS;
    }

    /**
     * Point broadcasting at resonance and generate app credentials, so
     * `resonance:start` + `broadcast()` work immediately after install.
     * Existing RESONANCE_* values are never overwritten.
     */
    private function configureEnv(): void
    {
        $path = base_path('.env');
        if (! is_file($path)) {
            $this->warn('No .env file found — skipping broadcasting configuration.');
            return;
        }
        $env = file_get_contents($path);

        $set = function (string $key, string $value, bool $overwrite) use (&$env) {
            if (preg_match("/^{$key}=.*$/m", $env)) {
                if ($overwrite) {
                    $env = preg_replace("/^{$key}=.*$/m", "{$key}={$value}", $env);
                    $this->line("  {$key}={$value}");
                }
                return;
            }
            $env = rtrim($env, "\n") . "\n{$key}={$value}\n";
            $this->line("  {$key}={$value}");
        };

        $this->info('Configuring .env:');
        // Both spellings so every Laravel version picks up the driver
        // (BROADCAST_DRIVER <= 10, BROADCAST_CONNECTION >= 11).
        $set('BROADCAST_DRIVER', 'resonance', true);
        $set('BROADCAST_CONNECTION', 'resonance', true);
        $set('RESONANCE_APP_ID', 'app-' . bin2hex(random_bytes(4)), false);
        $set('RESONANCE_KEY', bin2hex(random_bytes(16)), false);
        $set('RESONANCE_SECRET', bin2hex(random_bytes(16)), false);
        $set('RESONANCE_HOST', '127.0.0.1', false);
        $set('RESONANCE_PORT', '8080', false);
        $set('RESONANCE_SCHEME', 'http', false);

        file_put_contents($path, $env);
    }

    /** Download a URL following redirects; null on any non-200. */
    private function fetch(string $url): ?string
    {
        $ctx = stream_context_create(['http' => [
            'follow_location' => 1,
            'timeout' => 120,
            'ignore_errors' => true,
            'header' => "User-Agent: resonance-laravel\r\n",
        ]]);
        $body = @file_get_contents($url, false, $ctx);
        if ($body === false) {
            return null;
        }
        // $http_response_header is set by the stream wrapper.
        $status = isset($http_response_header[0]) ? $http_response_header[0] : '';
        if (strpos($status, ' 200') === false) {
            return null;
        }
        return $body;
    }
}
