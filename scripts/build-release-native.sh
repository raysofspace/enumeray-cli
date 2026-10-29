#!/usr/bin/env bash
# Run inside the workflow's native glibc 2.28 build image or macOS 14 host.
set -euo pipefail
release_tools="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
release_source="$release_tools"
release_target=''
release_revision=''
release_output='dist'
while (($#)); do
  case "$1" in
    --target) release_target="${2:?Missing target}"; shift 2 ;;
    --revision) release_revision="${2:?Missing revision}"; shift 2 ;;
    --output) release_output="${2:?Missing output directory}"; shift 2 ;;
    --source) release_source="${2:?Missing source checkout}"; shift 2 ;;
    *) echo 'Usage: build-release-native.sh --target TARGET --revision FULL_SHA [--source DIR] [--output DIR]' >&2; exit 2 ;;
  esac
done
if [[ -z "$release_target" || ! "$release_revision" =~ ^[0-9a-f]{40}$ ]]; then
  echo 'An explicit native target and full qualified source revision are required' >&2
  exit 2
fi
cd "$release_source"
release_source="$PWD"
case "$release_target" in
  x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu)
    [[ "$(uname -s)" == Linux && "$(getconf GNU_LIBC_VERSION)" == 'glibc 2.28' ]] || {
      echo 'Linux release builds require the native glibc 2.28 environment' >&2; exit 2;
    }
    command -v readelf >/dev/null
    ;;
  x86_64-apple-darwin|aarch64-apple-darwin)
    [[ "$(uname -s)" == Darwin && "$(sw_vers -productVersion)" == 14.* ]] || {
      echo 'macOS release builds require the native macOS 14 environment' >&2; exit 2;
    }
    export MACOSX_DEPLOYMENT_TARGET=14.0
    command -v otool >/dev/null
    command -v lipo >/dev/null
    ;;
  *) echo 'Unsupported release target' >&2; exit 2 ;;
esac
case "$release_target:$(uname -m)" in
  x86_64-*:x86_64|aarch64-*:aarch64|aarch64-*:arm64) ;;
  *) echo 'Release builds must execute natively on the target CPU' >&2; exit 2 ;;
esac
release_python="${ENUMERAY_RELEASE_PYTHON:-python3.12}"
"$release_python" -c 'import sys; assert sys.version_info >= (3, 12), "Python 3.12+ is required"'
"$release_python" - "$release_tools" "$release_source" <<'PYTHON'
import importlib.util
from pathlib import Path
import sys
spec = importlib.util.spec_from_file_location('release_artifacts', Path(sys.argv[1]) / 'scripts/release_artifacts.py')
artifacts = importlib.util.module_from_spec(spec)
spec.loader.exec_module(artifacts)
artifacts.verify_toolchain(Path(sys.argv[2]))
PYTHON
export CARGO_PROFILE_RELEASE_STRIP=symbols
export CARGO_INCREMENTAL=0
bash scripts/verify.sh native-clients
cargo build --locked --release --target "$release_target" -p enumeray-cli --bin enumeray
"$release_python" "$release_tools/scripts/package-binaries.py" --source "$release_source" \
  --target "$release_target" --revision "$release_revision" --release --output "$release_output"
"$release_python" "$release_tools/scripts/qualify-binary-archives.py" --archives "$release_output" --target "$release_target" \
  --require-minimum-platform --report "$release_output/qualification-$release_target.json"
if [[ "$release_target" == *-apple-darwin ]]; then
  release_version="$("$release_python" -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' \
    "$release_output/qualification-$release_target.json")"
  release_channel_report="$(mktemp "${TMPDIR:-/tmp}/enumeray-homebrew-qualification.XXXXXX")"
  trap 'rm -f "$release_channel_report"' EXIT
  "$release_python" "$release_tools/scripts/qualify-homebrew.py" \
    --archive "$release_output/enumeray-$release_version-$release_target.tar.gz" \
    --target "$release_target" --version "$release_version" --output "$release_channel_report"
  "$release_python" - "$release_output/qualification-$release_target.json" "$release_channel_report" <<'PYTHON'
import json
from pathlib import Path
import sys
destination = Path(sys.argv[1])
qualification = json.loads(destination.read_text())
qualification.setdefault('channel_qualification', {})['homebrew'] = json.loads(Path(sys.argv[2]).read_text())
destination.write_text(json.dumps(qualification, indent=2, sort_keys=True) + '\n')
PYTHON
fi
