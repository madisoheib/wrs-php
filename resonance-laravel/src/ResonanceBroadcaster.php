<?php

namespace Resonance\Laravel;

use Illuminate\Broadcasting\BroadcastException;
use Illuminate\Broadcasting\Broadcasters\PusherBroadcaster;
use Illuminate\Support\Arr;

/**
 * Thin shim over Laravel's PusherBroadcaster that normalizes the
 * pusher-php-server trigger() signature across major versions:
 * <= 5.x takes a socket_id string as the 4th argument, >= 6.x takes an
 * array of params. Old Laravel (6/7) passes the old shape to new pusher
 * libs and fatals — this shim lets one package version cover Laravel 6-13
 * with whatever pusher version Composer resolves.
 */
class ResonanceBroadcaster extends PusherBroadcaster
{
    /** @var bool|null Lazily-detected: does trigger() want an array 4th arg? */
    private $wantsParamsArray;

    public function broadcast(array $channels, $event, array $payload = [])
    {
        $socket = Arr::pull($payload, 'socket');

        if ($this->wantsParamsArray === null) {
            $param = (new \ReflectionMethod($this->pusher, 'trigger'))->getParameters()[3] ?? null;
            $type = $param && $param->getType() ? $param->getType()->getName() : null;
            $this->wantsParamsArray = ($type === 'array');
        }

        $fourth = $this->wantsParamsArray
            ? ($socket !== null ? ['socket_id' => $socket] : [])
            : $socket;

        try {
            $result = $this->pusher->trigger($this->formatChannels($channels), $event, $payload, $fourth);
        } catch (\Throwable $e) {
            throw new BroadcastException($e->getMessage());
        }

        // pusher-php-server <= 4 returns false on failure instead of throwing.
        if ($result === false) {
            throw new BroadcastException('Failed to send event to the resonance server.');
        }
    }
}
