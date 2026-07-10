#!/usr/bin/env bash
# Full compat matrix: build a real Laravel app per version (right PHP for each),
# install the package from the local path repo, broadcast, assert a pusher-js
# subscriber receives it. Usage: ./matrix.sh [versions...]  (default: all)
set -uo pipefail
cd "$(dirname "$0")"
ROOT=../..

# version|php-image|create-project constraint
MATRIX=(
  "6|php:7.4-cli|laravel/laravel:^6.0"
  "7|php:7.4-cli|laravel/laravel:^7.0"
  "8|php:8.0-cli|laravel/laravel:^8.0"
  "9|php:8.1-cli|laravel/laravel:^9.0"
  "10|php:8.2-cli|laravel/laravel:^10.0"
  "11|php:8.3-cli|laravel/laravel:^11.0"
  "12|php:8.3-cli|laravel/laravel:^12.0"
  "13|php:8.4-cli|laravel/laravel"
)

docker image inspect resonance:qa >/dev/null 2>&1 || docker build -t resonance:qa "$ROOT" >/dev/null

cleanup() { docker rm -f mx-resonance >/dev/null 2>&1; docker network rm mxnet >/dev/null 2>&1; }
trap cleanup EXIT

PASS=(); FAIL=()
for entry in "${MATRIX[@]}"; do
  IFS='|' read -r ver php constraint <<< "$entry"
  if [ "$#" -gt 0 ] && ! printf '%s\n' "$@" | grep -qx "$ver"; then continue; fi
  echo ""
  echo "===== Laravel $ver ($php) ====="

  if ! docker build -q -t "laravel${ver}-app" -f Dockerfile \
        --build-arg PHP_IMAGE="$php" --build-arg LARAVEL="$constraint" "$ROOT" >/dev/null 2>/tmp/mx-build.log; then
    echo "BUILD FAILED"; tail -5 /tmp/mx-build.log; FAIL+=("$ver(build)"); continue
  fi

  cleanup
  docker network create mxnet >/dev/null 2>&1
  docker run -d --name mx-resonance --network mxnet --network-alias resonance -p 8080:8080 resonance:qa >/dev/null
  sleep 1
  node subscribe.mjs > /tmp/mx-sub.log 2>&1 &
  SUB=$!
  sleep 4

  docker run --rm --network mxnet \
    -e BROADCAST_DRIVER=resonance -e BROADCAST_CONNECTION=resonance \
    -e RESONANCE_HOST=resonance -e RESONANCE_PORT=8080 -e RESONANCE_SCHEME=http \
    -e RESONANCE_APP_ID=app1 -e RESONANCE_KEY=resonance-key -e RESONANCE_SECRET=resonance-secret \
    "laravel${ver}-app" php artisan tinker --execute "broadcast(new App\Events\TestEvent('hello-from-laravel'));" >/dev/null 2>&1

  if wait $SUB; then
    echo "Laravel $ver: PASS"; PASS+=("$ver")
  else
    echo "Laravel $ver: FAIL"; tail -3 /tmp/mx-sub.log; FAIL+=("$ver")
  fi
  kill $SUB 2>/dev/null
done

echo ""
echo "================ MATRIX RESULT ================"
echo "PASS: ${PASS[*]:-none}"
echo "FAIL: ${FAIL[*]:-none}"
[ "${#FAIL[@]}" -eq 0 ]
