#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cargo_target_dir="${FLUVORA_CARGO_TARGET_DIR:-$repo_dir/target}"
python_command="${FLUVORA_PYTHON:-python3}"
run_dir="$(mktemp -d "${TMPDIR:-/tmp}/fluvora-browser.XXXXXXXX")"
media_pid=""
api_pid=""
web_pid=""
token_refresh_pid=""
netem_enabled=""

cleanup() {
  exit_code=$?
  if [[ "$exit_code" -ne 0 ]]; then
    for log in media api web; do
      if [[ -f "$run_dir/$log.log" ]]; then
        echo "===== $log.log"
        tail -n 200 "$run_dir/$log.log"
      fi
    done
  fi
  for pid in "$token_refresh_pid" "$web_pid" "$api_pid" "$media_pid"; do
    if [[ -n "$pid" ]]; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
  if [[ -n "$netem_enabled" ]]; then
    sudo tc qdisc del dev lo root 2>/dev/null || true
  fi
  case "$run_dir" in
    "${TMPDIR:-/tmp}"/fluvora-browser.*) rm -rf -- "$run_dir" ;;
  esac
  exit "$exit_code"
}
trap cleanup EXIT INT TERM

wait_for_url() {
  url=$1
  for _ in $(seq 1 100); do
    if curl --fail --silent --show-error "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.1
  done
  echo "timed out waiting for $url" >&2
  return 1
}

MSYS2_ARG_CONV_EXCL="/CN=" openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 \
  -sha256 -nodes -subj "/CN=localhost" -days 1 \
  -keyout "$run_dir/key.pem" -out "$run_dir/cert.pem" >/dev/null 2>&1

if [[ "${FLUVORA_NETEM:-false}" == "true" ]]; then
  sudo tc qdisc add dev lo root netem \
    delay 80ms 20ms distribution normal loss 5% reorder 1% 50%
  netem_enabled=true
fi

FLUVORA_MEDIA_UDP_BIND=127.0.0.1:51000 \
FLUVORA_MEDIA_CONTROL_BIND=127.0.0.1:18092 \
FLUVORA_MEDIA_CONTROL_TOKEN=browser-e2e-media-control-token \
FLUVORA_DTLS_CERT_PEM="$run_dir/cert.pem" \
FLUVORA_DTLS_KEY_PEM="$run_dir/key.pem" \
FLUVORA_DTLS_FINGERPRINT_FILE="$run_dir/fingerprint.txt" \
  "$cargo_target_dir/debug/fluvora-media-node" >"$run_dir/media.log" 2>&1 &
media_pid=$!
wait_for_url http://127.0.0.1:18092/health/live

mkdir "$run_dir/state"
FLUVORA_API_BIND=127.0.0.1:18080 \
FLUVORA_TOKEN_SECRET=browser-e2e-access-token-secret-32-bytes-minimum \
FLUVORA_MEDIA_CONTROL_TOKEN=browser-e2e-media-control-token \
FLUVORA_GATEWAY_TOKEN=browser-e2e-gateway-token \
FLUVORA_WORKER_TOKEN=browser-e2e-worker-token \
FLUVORA_TURN_REST_SECRET=browser-e2e-turn-rest-secret-32-bytes-minimum \
FLUVORA_GIFT_WEBHOOK_SECRET=browser-e2e-gift-webhook-secret-32-bytes-minimum \
FLUVORA_DTLS_FINGERPRINT_FILE="$run_dir/fingerprint.txt" \
FLUVORA_ICE_CANDIDATE="1 1 UDP 2130706431 127.0.0.1 51000 typ host" \
FLUVORA_MEDIA_CONTROL_URL=http://127.0.0.1:18092 \
FLUVORA_GATEWAY_URL=http://127.0.0.1:18193 \
FLUVORA_WORKER_URL=http://127.0.0.1:18191 \
FLUVORA_STATE_DIR="$run_dir/state" \
FLUVORA_CORS_ORIGINS=http://127.0.0.1:18000 \
  "$cargo_target_dir/debug/fluvora-api-server" >"$run_dir/api.log" 2>&1 &
api_pid=$!
wait_for_url http://127.0.0.1:18080/health/live

"$python_command" -m http.server 18000 --bind 127.0.0.1 \
  --directory "$repo_dir" >"$run_dir/web.log" 2>&1 &
web_pid=$!
wait_for_url http://127.0.0.1:18000/tests/browser/

token_ttl="${FLUVORA_TOKEN_TTL:-900}"
issue_token() {
  subject=$1
  FLUVORA_TOKEN_SECRET=browser-e2e-access-token-secret-32-bytes-minimum \
    "$cargo_target_dir/debug/fluvora-admin" token \
      --subject "$subject" --room "*" --ttl "$token_ttl" --scopes all
}
token="$(issue_token 1)"
second_token="$(issue_token 2)"

if [[ "${FLUVORA_SKIP_LOAD:-false}" != "true" ]]; then
  if [[ -n "${FLUVORA_SOAK_SECONDS:-}" ]]; then
    token_file="$run_dir/load.token"
    printf '%s' "$token" >"$token_file"
    token_rotate_seconds="${FLUVORA_TOKEN_ROTATE_SECONDS:-$((token_ttl / 3))}"
    if (( token_rotate_seconds < 1 )); then token_rotate_seconds=1; fi
    (
      while true; do
        sleep "$token_rotate_seconds"
        issue_token 1 >"$token_file.next"
        mv -f -- "$token_file.next" "$token_file"
      done
    ) &
    token_refresh_pid=$!
    FLUVORA_LOAD_TOKEN_FILE="$token_file" \
      node "$repo_dir/scripts/load-control-plane.mjs" \
        --concurrency "${FLUVORA_SOAK_CONCURRENCY:-16}" \
        --iterations 10000000 \
        --duration-seconds "$FLUVORA_SOAK_SECONDS" \
        --maximum-p95-ms "${FLUVORA_SOAK_MAXIMUM_P95_MS:-1000}"
    kill "$token_refresh_pid" 2>/dev/null || true
    wait "$token_refresh_pid" 2>/dev/null || true
    token_refresh_pid=""
  else
    FLUVORA_LOAD_TOKEN="$token" \
      node "$repo_dir/scripts/load-control-plane.mjs" --profile quick
  fi
  curl --fail --silent http://127.0.0.1:18080/metrics |
    grep -Eq 'fluvora_control_processing_micros_count [1-9][0-9]*'
fi

if [[ "${FLUVORA_SKIP_BROWSER:-false}" != "true" ]]; then
  cd "$repo_dir/tests/browser"
  if [[ -n "${FLUVORA_BROWSER_PROJECT:-}" ]]; then
    FLUVORA_BROWSER_TOKEN="$token" FLUVORA_BROWSER_TOKEN_2="$second_token" \
      npx playwright test --project="$FLUVORA_BROWSER_PROJECT"
  else
    FLUVORA_BROWSER_TOKEN="$token" FLUVORA_BROWSER_TOKEN_2="$second_token" npm test
  fi
  curl --fail --silent http://127.0.0.1:18092/metrics |
    grep -Eq 'fluvora_packet_processing_micros_count [1-9][0-9]*'
fi
