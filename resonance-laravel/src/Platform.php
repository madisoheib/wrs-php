<?php

namespace Resonance\Laravel;

/**
 * Framework-free helpers so they can be unit-tested with plain `php` (no
 * composer/testbench needed) — see qa/php_check.php.
 */
class Platform
{
    /** Map php_uname() os/machine to a Rust release target triple, or null if unsupported. */
    public static function target(string $os, string $machine): ?string
    {
        $os = strtolower($os);
        $m = strtolower($machine);

        $arch = match (true) {
            in_array($m, ['x86_64', 'amd64'], true) => 'x86_64',
            in_array($m, ['aarch64', 'arm64'], true) => 'aarch64',
            default => null,
        };
        if ($arch === null) {
            return null;
        }

        if (str_contains($os, 'linux')) {
            return "{$arch}-unknown-linux-musl";
        }
        if (str_contains($os, 'darwin')) {
            return "{$arch}-apple-darwin";
        }
        if (str_contains($os, 'windows') || str_contains($os, 'winnt')) {
            return $arch === 'x86_64' ? 'x86_64-pc-windows-msvc' : null;
        }
        return null;
    }

    /** Asset file name for a target (Windows binaries carry .exe). */
    public static function assetName(string $target): string
    {
        return str_contains($target, 'windows') ? "resonance-{$target}.exe" : "resonance-{$target}";
    }

    /** Render a server resonance.toml from the Laravel config array. */
    public static function toml(array $c): string
    {
        $esc = fn ($v) => str_replace(['\\', '"'], ['\\\\', '\\"'], (string) $v);
        $port = (int) ($c['port'] ?? 8080);

        return <<<TOML
        [server]
        host = "0.0.0.0"
        port = {$port}

        [[apps]]
        id = "{$esc($c['app_id'] ?? 'app1')}"
        key = "{$esc($c['key'] ?? '')}"
        secret = "{$esc($c['secret'] ?? '')}"
        max_connections = 0

        [limits]
        max_message_size_kb = 10
        activity_timeout_s = 120
        max_channels_per_connection = 100

        TOML;
    }
}
