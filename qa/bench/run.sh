#!/usr/bin/env bash
# Compare resonance vs Reverb on the same host: idle footprint, fan-out latency,
# sustained broadcast. Relative numbers only (Docker Desktop/Mac, not a t3.medium).
set -uo pipefail
cd "$(dirname "$0")"
ROOT=../..
CONNS="${BENCH_CONNS:-1000}"
export BENCH_CONNS="$CONNS"

cleanup() { docker compose down -v >/dev/null 2>&1; }
trap cleanup EXIT

docker image inspect resonance:qa >/dev/null 2>&1 || docker build -t resonance:qa "$ROOT" >/dev/null
if ! docker image inspect laravel-app >/dev/null 2>&1; then
  echo "building laravel-app image first..."
  docker build -t laravel-app -f "$ROOT/qa/laravel/Dockerfile" "$ROOT" >/dev/null
fi

echo "== building reverb image =="
docker compose build reverb >/dev/null

echo "== starting both servers (conns=$CONNS) =="
docker compose up -d
for port in 8080 8081; do
  for i in $(seq 1 60); do
    nc -z 127.0.0.1 "$port" 2>/dev/null && break
    sleep 1
    [ "$i" = 60 ] && { echo "port $port never came up"; docker compose logs; exit 1; }
  done
done
sleep 4  # let reverb finish booting its event loop
echo "servers up."

metric() { echo "$1" | grep -o "\"$2\":[0-9.]*" | cut -d: -f2; }

# echoes "mem|cpu|conns" for an idle hold; progress goes to stderr.
idle_stat() {
  local log; log=$(mktemp)
  node bench.mjs "$1" idle >"$log" 2>&1 &
  local pid=$!
  local i; for i in $(seq 1 40); do grep -q '^READY' "$log" && break; sleep 1; done
  sleep 2
  local stat; stat=$(docker stats --no-stream --format '{{.MemUsage}}|{{.CPUPerc}}' "$2")
  local conns; conns=$(grep '^READY' "$log" | head -1 | awk '{print $2}')
  kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null; rm -f "$log"
  echo "  $1 idle: ${conns:-0} conns | ${stat}" >&2
  echo "${stat}|${conns:-0}"
}

echo ""
echo "== SCENARIO A: idle connections =="
RES_IDLE=$(idle_stat resonance bench-resonance-1)
REV_IDLE=$(idle_stat reverb    bench-reverb-1)
MEM_RES=$(echo "$RES_IDLE" | cut -d'|' -f1); CPU_RES=$(echo "$RES_IDLE" | cut -d'|' -f2)
MEM_REV=$(echo "$REV_IDLE" | cut -d'|' -f1); CPU_REV=$(echo "$REV_IDLE" | cut -d'|' -f2)

echo ""
echo "== SCENARIO C: fan-out latency (1 event -> $CONNS subs) =="
FO_RES=$(node bench.mjs resonance fanout 2>&1 | grep '^RESULT' | sed 's/^RESULT //'); echo "  resonance: $FO_RES"
FO_REV=$(node bench.mjs reverb    fanout 2>&1 | grep '^RESULT' | sed 's/^RESULT //'); echo "  reverb:    $FO_REV"

echo ""
echo "== SCENARIO B: sustained broadcast (100 msg/s x 5s) =="
SU_RES=$(node bench.mjs resonance sustained 2>&1 | grep '^RESULT' | sed 's/^RESULT //'); echo "  resonance: $SU_RES"
SU_REV=$(node bench.mjs reverb    sustained 2>&1 | grep '^RESULT' | sed 's/^RESULT //'); echo "  reverb:    $SU_REV"

echo ""
echo "=================== COMPARISON (relative, same host) ==================="
printf "%-22s | %-22s | %-22s\n" "metric" "resonance" "reverb"
printf "%-22s-+-%-22s-+-%-22s\n" "----------------------" "----------------------" "----------------------"
printf "%-22s | %-22s | %-22s\n" "idle mem ($CONNS conns)" "$MEM_RES" "$MEM_REV"
printf "%-22s | %-22s | %-22s\n" "idle cpu" "$CPU_RES" "$CPU_REV"
printf "%-22s | %-22s | %-22s\n" "fanout p50 (ms)"  "$(metric "$FO_RES" p50_ms)" "$(metric "$FO_REV" p50_ms)"
printf "%-22s | %-22s | %-22s\n" "fanout p99 (ms)"  "$(metric "$FO_RES" p99_ms)" "$(metric "$FO_REV" p99_ms)"
printf "%-22s | %-22s | %-22s\n" "fanout delivered" "$(metric "$FO_RES" delivered)/$(metric "$FO_RES" conns)" "$(metric "$FO_REV" delivered)/$(metric "$FO_REV" conns)"
printf "%-22s | %-22s | %-22s\n" "sustained p50 (ms)" "$(metric "$SU_RES" p50_ms)" "$(metric "$SU_REV" p50_ms)"
printf "%-22s | %-22s | %-22s\n" "sustained p99 (ms)" "$(metric "$SU_RES" p99_ms)" "$(metric "$SU_REV" p99_ms)"
printf "%-22s | %-22s | %-22s\n" "sustained delivery %" "$(metric "$SU_RES" delivery_pct)" "$(metric "$SU_REV" delivery_pct)"
echo "======================================================================="
