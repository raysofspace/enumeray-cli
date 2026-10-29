#!/usr/bin/env bash
# Run only public workspace checks; backend fixtures require explicit profiles.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
exec python3 scripts/verify.py "$@"
