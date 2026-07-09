<?php

namespace Resonance\Laravel\Console;

use Illuminate\Console\Command;
use Resonance\Laravel\Platform;

class InstallCommand extends Command
{
    protected $signature = 'resonance:install {--version= : Release tag (default: config/latest)} {--force}';
    protected $description = 'Download the resonance server binary for this OS/architecture.';

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

        return self::SUCCESS;
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
        if (! str_contains($status, ' 200')) {
            return null;
        }
        return $body;
    }
}
