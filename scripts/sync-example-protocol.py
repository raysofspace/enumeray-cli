#!/usr/bin/env python3
"""Generate standalone example boundaries from the editable author starter."""
import argparse
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
SOURCES = ROOT / "crates/cli/starter-kit"
COPIES = [(SOURCES / filename, ROOT / "examples" / name / filename)
          for filename in ("protocol.py", "test_protocol.py") for name in ("scalar_model", "collections")]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    stale = [(source, path) for source, path in COPIES
             if not path.is_file() or path.read_bytes() != source.read_bytes()]
    if args.check:
        if stale:
            print("Run python3 scripts/sync-example-protocol.py to refresh standalone protocol copies.", file=sys.stderr)
            return 1
    else:
        for source, path in stale:
            path.write_bytes(source.read_bytes())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
