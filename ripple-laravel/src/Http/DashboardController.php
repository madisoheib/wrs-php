<?php

namespace Ripple\Laravel\Http;

use Illuminate\Http\Request;
use Illuminate\Routing\Controller;
use Illuminate\Support\Facades\Gate;

/**
 * Ripple dashboard — a Horizon-style page to watch the WebSocket server while
 * developing: is it reachable, how many connections/channels are live, how many
 * slow consumers got dropped, and a button to fire a test broadcast end-to-end.
 */
class DashboardController extends Controller
{
    public function index()
    {
        $this->authorizeAccess();

        return view('ripple::dashboard', [
            'statsUrl' => route('ripple.stats'),
            'testUrl'  => route('ripple.test'),
            'server'   => $this->serverLabel(),
        ]);
    }

    /** JSON snapshot: health + metrics + channels. Polled by the page. */
    public function stats()
    {
        $this->authorizeAccess();
        $c = config('ripple');

        $health = ['reachable' => false, 'error' => null];
        $metrics = [];
        $url = $c['dashboard']['metrics_url'] ?: "{$c['scheme']}://{$c['host']}:{$c['port']}/metrics";
        [$status, $body] = $this->http('GET', $url);
        if ($status === 200) {
            $health['reachable'] = true;
            $metrics = $this->parseMetrics($body);
        } else {
            $health['error'] = $status ? "metrics returned HTTP {$status}" : 'server unreachable';
        }

        $channels = [];
        [$cstatus, $cbody] = $this->http('GET', $this->signedUrl($c, 'GET', '/channels', ['info' => 'subscription_count,user_count']));
        if ($cstatus === 200) {
            $decoded = json_decode($cbody, true);
            foreach ((array) ($decoded['channels'] ?? []) as $name => $info) {
                $channels[] = [
                    'name'  => $name,
                    'type'  => $this->channelType($name),
                    'subscription_count' => $info['subscription_count'] ?? null,
                    'user_count' => $info['user_count'] ?? null,
                ];
            }
            usort($channels, function ($a, $b) {
                return ($b['subscription_count'] ?? 0) <=> ($a['subscription_count'] ?? 0);
            });
        } elseif (! $health['error']) {
            $health['error'] = "channels returned HTTP {$cstatus} (check key/secret/app_id)";
        }

        return response()->json([
            'health'   => $health,
            'metrics'  => $metrics,
            'channels' => $channels,
            'ts'       => now()->toIso8601String(),
        ]);
    }

    /** Fire a test broadcast so the dev can confirm the full path works. */
    public function test(Request $request)
    {
        $this->authorizeAccess();
        $c = config('ripple');
        $channel = 'ripple-dashboard-test';
        $body = json_encode([
            'name' => 'ping',
            'channel' => $channel,
            'data' => json_encode(['at' => now()->toIso8601String()]),
        ]);
        [$status, $rbody] = $this->http('POST', $this->signedUrl($c, 'POST', '/events', ['body_md5' => md5($body)]), $body);
        $ok = $status === 200;
        return response()->json(
            $ok ? ['sent' => true, 'channel' => $channel]
                : ['sent' => false, 'status' => $status, 'error' => $rbody ?: 'unreachable'],
            $ok ? 200 : 502
        );
    }

    // --- helpers ------------------------------------------------------------

    protected function authorizeAccess(): void
    {
        if (Gate::has('viewRipple')) {
            Gate::authorize('viewRipple');
            return;
        }
        // Safe default: dashboard is dev-only unless a gate is defined.
        abort_unless(app()->environment('local') || config('app.debug'), 403);
    }

    protected function channelType(string $name): string
    {
        if (strpos($name, 'presence-') === 0) return 'presence';
        if (strpos($name, 'private-') === 0) return 'private';
        return 'public';
    }

    /** Dependency-free HTTP (works on Laravel 6-13). Returns [status, body]. */
    protected function http(string $method, string $url, ?string $body = null): array
    {
        $opts = ['http' => [
            'method' => $method,
            'timeout' => 3,
            'ignore_errors' => true,
            'header' => "Accept: application/json\r\n" . ($body !== null ? "Content-Type: application/json\r\n" : ''),
        ]];
        if ($body !== null) $opts['http']['content'] = $body;
        $resp = @file_get_contents($url, false, stream_context_create($opts));
        $status = 0;
        if (isset($http_response_header[0]) && preg_match('/\s(\d{3})\s/', $http_response_header[0], $m)) {
            $status = (int) $m[1];
        }
        return [$status, $resp === false ? '' : $resp];
    }

    /** Build a full Pusher-signed URL (version-independent, no pusher lib). */
    protected function signedUrl(array $c, string $method, string $path, array $extra = []): string
    {
        $fullPath = "/apps/{$c['app_id']}{$path}";
        $params = array_merge($extra, [
            'auth_key' => $c['key'],
            'auth_timestamp' => (string) time(),
            'auth_version' => '1.0',
        ]);
        ksort($params);
        $qs = implode('&', array_map(function ($k) use ($params) {
            return "{$k}={$params[$k]}";
        }, array_keys($params)));
        $params['auth_signature'] = hash_hmac('sha256', "{$method}\n{$fullPath}\n{$qs}", $c['secret']);
        $query = implode('&', array_map(function ($k) use ($params) {
            return "{$k}=" . rawurlencode($params[$k]);
        }, array_keys($params)));

        return "{$c['scheme']}://{$c['host']}:{$c['port']}{$fullPath}?{$query}";
    }

    protected function parseMetrics(string $body): array
    {
        $out = [];
        foreach (explode("\n", $body) as $line) {
            $line = trim($line);
            if ($line === '' || $line[0] === '#') continue;
            $parts = preg_split('/\s+/', $line);
            if (count($parts) >= 2 && is_numeric($parts[1])) {
                $out[$parts[0]] = 0 + $parts[1];
            }
        }
        return $out;
    }

    protected function serverLabel(): string
    {
        $c = config('ripple');
        return "{$c['scheme']}://{$c['host']}:{$c['port']}";
    }
}
