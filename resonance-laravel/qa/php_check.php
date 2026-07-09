<?php
// Plain-php self-check for the framework-free bits. Run: php qa/php_check.php
require __DIR__ . '/../src/Platform.php';

use Resonance\Laravel\Platform;

function eq($got, $want, $msg)
{
    if ($got !== $want) {
        fwrite(STDERR, "FAIL {$msg}\n  got:  " . var_export($got, true) . "\n  want: " . var_export($want, true) . "\n");
        exit(1);
    }
    echo "  ✓ {$msg}\n";
}

// Target triple mapping across the release matrix (spec §7.1).
eq(Platform::target('Linux', 'x86_64'), 'x86_64-unknown-linux-musl', 'linux x86_64');
eq(Platform::target('Linux', 'aarch64'), 'aarch64-unknown-linux-musl', 'linux arm64');
eq(Platform::target('Darwin', 'arm64'), 'aarch64-apple-darwin', 'macos arm64');
eq(Platform::target('Darwin', 'x86_64'), 'x86_64-apple-darwin', 'macos x86_64');
eq(Platform::target('Windows NT', 'AMD64'), 'x86_64-pc-windows-msvc', 'windows x86_64');
eq(Platform::target('Linux', 'mips'), null, 'unsupported arch -> null');

eq(Platform::assetName('x86_64-pc-windows-msvc'), 'resonance-x86_64-pc-windows-msvc.exe', 'windows asset .exe');
eq(Platform::assetName('aarch64-apple-darwin'), 'resonance-aarch64-apple-darwin', 'unix asset no ext');

// Generated toml is parseable and carries the credentials.
$toml = Platform::toml(['app_id' => 'app1', 'key' => 'k', 'secret' => 's"x', 'port' => 9001]);
if (! str_contains($toml, 'port = 9001')) { fwrite(STDERR, "FAIL toml port\n"); exit(1); }
if (! str_contains($toml, 'id = "app1"')) { fwrite(STDERR, "FAIL toml app_id\n"); exit(1); }
if (! str_contains($toml, 'secret = "s\\"x"')) { fwrite(STDERR, "FAIL toml quote escaping\n"); exit(1); }
echo "  ✓ toml render + quote escaping\n";

echo "\nall php checks passed\n";
