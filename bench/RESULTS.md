# Benchmark — Resonance vs Laravel Reverb (Linux)

**Date:** 2026-07-11 · **Host:** AWS c6i.xlarge (4 vCPU Ice Lake, 8 GB), Ubuntu 20.04
**Method:** each server pinned to CPU cores 0-1 (`CPUAffinity` / `--cpuset-cpus`) to emulate
a 2-vCPU box; load generator (Node, `qa/bench/bench.mjs`) pinned to cores 2-3; loopback
networking. Kernel tuned: `somaxconn=65535`, `nf_conntrack_max=1e6`, `nofile=1e6`,
`ip_local_port_range 1025-65000`.

**Setups.** Resonance: the release binary, stock config. Reverb: Laravel 13 +
`laravel/reverb` on PHP 8.3 **with `ext-ev`, `memory_limit=-1` and
`REVERB_APP_MAX_CONNECTIONS=100000`** — without those production tweaks Reverb
dies at ~1,000 connections (ReactPHP `stream_select` fd cap).

## 1. Connection density (idle)

| Connections | Resonance RSS | Reverb RSS |
|---|---|---|
| 10,000 | 131 MiB | — |
| 20,000 | 258 MiB | — |
| 40,000 | **512 MiB** (~12.8 KB/conn) | 834 MiB (~20 KB/conn) |
| 60,000 | **770 MiB — 100% established** | not attempted |

Both idle at 0% CPU. Resonance memory is linear from 1k to 60k. The spec target
(≥50k connections on a 2-vCPU/4GB class machine) is exceeded.

## 2. Fan-out — 1 event → 10,000 subscribers

| | delivered | p50 | p99 |
|---|---|---|---|
| Resonance | 10,000/10,000 | **94 ms** | 139 ms |
| Reverb | 10,000/10,000 | 122 ms | 164 ms |

Caveat: these figures include the load generator draining 10k messages on the
shared box; a control run splitting subscribers across 4 client processes
lowered Resonance's p50 to ~50-70 ms. ~10k socket writes on 2 cores put the
server-side floor around 15-30 ms for any implementation. Treat the *relative*
gap as meaningful, the absolute values as an upper bound.

## 3. Sustained broadcast — 2,000 subscribers × 25 events/s = 50,000 deliveries/s, 15 s

| | delivered | p50 | p99 | server CPU (avg/peak) |
|---|---|---|---|---|
| Resonance | 100% | **14.7 ms** | **32 ms** | 27% / 33% *of 200% (2 cores)* |
| Reverb | 100% | 24.6 ms | 254 ms | 62% / 75% *of 100% (1 core)* |

The tail is the story: Reverb's p99 is **8× worse** and its single-threaded
event loop was already at 62% of its only core. Normalized headroom at this
load: Reverb can grow ~1.6× before saturating; Resonance ~7× — and it can use
additional cores, which Reverb structurally cannot.

## 4. Operational notes

- Resonance's accept backlog was 128 (std default) before this benchmark; ramp
  storms exposed it and it now listens with backlog 8192.
- On a burstable instance (t2.micro), the *ramp* exhausts CPU credits long
  before RAM runs out — use fixed-performance instances for load testing.
- fail2ban + repeated SSH monitoring connections don't mix; sample via one
  persistent connection.

## Reproduce

```bash
# server, pinned to 2 cores:
systemd-run --unit=resonance -p LimitNOFILE=1000000 -p CPUAffinity="0 1" \
  resonance start --config /etc/resonance.toml
# generator (Node ≥16, npm i pusher ws):
BENCH_CONNS=40000 BENCH_HOLD=20000 taskset -c 2,3 node bench.mjs resonance idle
BENCH_CONNS=10000 taskset -c 2,3 node bench.mjs resonance fanout
BENCH_CONNS=2000 BENCH_RATE=25 BENCH_SECONDS=15 taskset -c 2,3 node bench.mjs resonance sustained
```

Raw scenario driver: [`qa/bench/bench.mjs`](../qa/bench/bench.mjs). Numbers you
can't reproduce shouldn't be trusted — including these.
