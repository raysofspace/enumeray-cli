#!/usr/bin/env bash
# The daemon supplies the nested mount; the test container needs no privileges.
set -euo pipefail
image=${1:-}
if [ -z "$image" ]; then
  cd "$(dirname "${BASH_SOURCE[0]}")/.."
  image=$(docker compose -p enumeray-backend-acceptance -f crates/connector/tests/real-backends/compose.yaml images -q slurm)
fi
[ -n "$image" ] || { printf '%s\n' 'Build the backend driver fixture before its mount-confinement check.' >&2; exit 1; }
scratch=$(mktemp -d "${TMPDIR:-/tmp}/enumeray-cleanup-mounts.XXXXXXXX")
cleanup() {
  status=$?
  set +e
  # A container-hosted runner may not own Docker's nested mount target. Remove
  # fixture directories as root after that mount is gone, using only the parent
  # bind mount so the sentinel is never removed through the readonly test mount.
  docker run --rm --network none --user 0:0 \
    --mount "type=bind,source=$scratch,target=/fixture" \
    --entrypoint /bin/sh "$image" -c 'rm -rf /fixture/owned /fixture/outside' \
    || { [ "$status" -ne 0 ] || status=1; }
  rm -rf -- "$scratch" || { [ "$status" -ne 0 ] || status=1; }
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -p "$scratch/owned/payload/nested" "$scratch/outside"
printf '%s\n' enumeray-cleanup-bind-mount-v1 > "$scratch/fixture-kind"
printf '%s\n' preserve > "$scratch/outside/sentinel"
docker run --rm --network none \
  --mount "type=bind,source=$scratch,target=/fixture" \
  --mount "type=bind,source=$scratch/outside,target=/fixture/owned/payload/nested,readonly" \
  --entrypoint /usr/local/bin/driver-tests "$image" \
  cleanup::tests::cleanup_rejects_same_filesystem_bind_mount \
  --ignored --exact --nocapture --test-threads=1 | tee "$scratch/result.log"
grep -q 'test result: ok\. 1 passed;' "$scratch/result.log"
