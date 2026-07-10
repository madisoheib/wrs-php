<?php

namespace App\Events;

use Illuminate\Broadcasting\Channel;
use Illuminate\Contracts\Broadcasting\ShouldBroadcastNow;

class TestEvent implements ShouldBroadcastNow
{
    /** @var string */
    public $msg;

    public function __construct(string $msg)
    {
        $this->msg = $msg;
    }

    public function broadcastOn(): Channel
    {
        return new Channel('test-channel');
    }

    public function broadcastAs(): string
    {
        return 'ping';
    }

    public function broadcastWith(): array
    {
        return ['msg' => $this->msg];
    }
}
