#!/bin/sh
# Readiness failures must stop setup before dependent services or tests run.
wait_ready() {
  wait_ready_attempts=$1
  wait_ready_description=$2
  shift 2
  for wait_ready_attempt in $(seq 1 "$wait_ready_attempts"); do
    if "$@" >/dev/null 2>&1; then return 0; fi
    if [ "$wait_ready_attempt" -lt "$wait_ready_attempts" ]; then sleep 1; fi
  done
  printf 'Timed out waiting for %s after %s attempts.\n' "$wait_ready_description" "$wait_ready_attempts" >&2
  return 1
}
