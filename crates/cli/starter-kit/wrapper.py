#!/usr/bin/env python3
"""Protocol-1 wrapper. Scheduling, staging, uploads and credentials belong elsewhere."""
import argparse
import csv
from decimal import InvalidOperation
from pathlib import Path
import sys
from implementation import evaluate
from protocol import IDENTITIES, ModelError, exact_json, publish, read_json, read_request

RELEASE = Path(__file__).resolve().parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    execute = commands.add_parser("execute")
    execute.add_argument("--request", required=True)
    execute.add_argument("--result", required=True)
    args = parser.parse_args()
    try:
        scientific = read_json(RELEASE / "publication.json", 1048576)
        request = read_request(args.request, scientific)
        mode = "files" if "dataset" in scientific["input_schema"]["properties"] else "json"
    except (ModelError, KeyError, TypeError):
        print("Wrapper: invalid request or scientific declaration.", file=sys.stderr)
        return 2
    # Refuse prior evidence before scientific work, including native command launch.
    result_path = Path(args.result).absolute()
    if result_path.exists() or result_path.is_symlink():
        print("Wrapper: preserve existing result evidence.", file=sys.stderr)
        return 2
    result = {"wrapper_protocol_version": 1, **{key: request[key] for key in IDENTITIES},
              "outcome": "succeeded", "outputs": {}, "files": {}, "failure": None}
    try:
        result["outputs"], result["files"] = evaluate(request, result_path.parent, mode)
        # Encoding/size failures are scientific failures, never partial results.
        if len((exact_json(result) + "\n").encode()) > 65536:
            raise ModelError("Result exceeds the wrapper byte limit.")
    except (ModelError, OSError, ValueError, InvalidOperation, OverflowError, csv.Error):
        result.update(outcome="failed", outputs={}, files={}, failure={
            "code": "model_failure", "message": "Model could not process its declared inputs or publish outputs."})
    try:
        publish(result_path, (exact_json(result) + "\n").encode())
    except ModelError:
        print("Wrapper: preserve result evidence; publication failed.", file=sys.stderr)
        return 2
    print("Wrapper: " + result["outcome"], file=sys.stderr, flush=True)
    return 0 if result["outcome"] == "succeeded" else 1


if __name__ == "__main__":
    raise SystemExit(main())
