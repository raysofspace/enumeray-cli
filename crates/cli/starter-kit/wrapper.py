#!/usr/bin/env python3
"""Protocol-1 wrapper. Scheduling, staging, uploads and credentials belong elsewhere."""
import argparse
import csv
from decimal import InvalidOperation
from pathlib import Path
import sys
from implementation import evaluate
from protocol import (BoundaryError, IDENTITIES, ModelError, claim_workspace, exact_json,
                      preflight_result, publish, read_json, read_request, staged_dataset,
                      validate_storage)

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
    try:
        result_path = preflight_result(args.result)
        if result_path.name.lower() == ".model-workspace" or (mode == "files" and result_path.name.lower() == "processed.csv"):
            raise BoundaryError("Result and declared output paths must be distinct.")
        if mode == "json" and request["input_files"]:
            raise BoundaryError("The scalar model declares no staged file mappings.")
        if mode == "files":
            staged_dataset(request, result_path.parent)
            output = result_path.parent / "processed.csv"
            if output.exists() or output.is_symlink():
                raise BoundaryError("Preserve existing scientific outputs.")
        if mode == "files" or (RELEASE / "demo_solver.py").exists():
            claim_workspace(result_path.parent)
    except (BoundaryError, OSError):
        print("Wrapper: preserve workspace evidence; use a fresh workspace.", file=sys.stderr)
        return 2
    result = {"wrapper_protocol_version": 1, **{key: request[key] for key in IDENTITIES},
              "outcome": "succeeded", "outputs": {}, "files": {}, "failure": None}
    try:
        result["outputs"], result["files"] = evaluate(request, result_path.parent, mode)
        # Encoding/size failures are scientific failures, never partial results.
        if len((exact_json(result) + "\n").encode()) > 65536:
            raise ModelError("Result exceeds the wrapper byte limit.")
        validate_storage(result)
    except BoundaryError:
        print("Wrapper: preserve output evidence; publication failed.", file=sys.stderr)
        return 2
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
