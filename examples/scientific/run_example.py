#!/usr/bin/env python3
"""Run and check a scientific example locally in a new directory."""
import argparse
import hashlib
import json
from pathlib import Path
import sys

from protocol import ModelError, publish, read_json
from qualify import environment_receipt
from runtime import ExecutionFailure, LIMITS, run_model, validate_inputs
from verify_collected import verify_science

ROOT = Path(__file__).resolve().parent


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, data):
    publish(path, (json.dumps(data, indent=2, sort_keys=True, allow_nan=False) + "\n").encode("utf-8"))


def run_example(model, directory, inputs_file=None):
    inputs = validate_inputs(model, read_json(inputs_file or ROOT / model / "inputs.json"))
    runtime = environment_receipt()
    try:
        directory.mkdir(mode=0o700)
    except FileExistsError:
        raise ValueError(f"Choose a new output directory. {directory} already exists.") from None

    write_json(directory / "inputs.json", inputs)
    outputs, files = run_model(model, inputs, directory)
    artifacts = {slot: directory / item["path"] for slot, item in files.items()}
    checks = verify_science(model, inputs, outputs, artifacts)
    write_json(directory / "outputs.json", outputs)
    paths = [directory / "inputs.json", directory / "outputs.json", *artifacts.values()]
    write_json(directory / "run-record.json", {
        "format_version": 1,
        "kind": "local-scientific-example",
        "model": model,
        "model_sha256": sha256(ROOT / model / "model.py"),
        "runtime": runtime,
        "scientific_checks": checks,
        "files": {path.name: {"size_bytes": path.stat().st_size, "sha256": sha256(path)} for path in paths},
    })


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("model", choices=sorted(LIMITS))
    parser.add_argument("--directory", required=True, type=Path, help="new directory for results")
    parser.add_argument("--inputs", type=Path, help="JSON input file (defaults to the model's inputs.json)")
    args = parser.parse_args(argv)
    try:
        run_example(args.model, args.directory, args.inputs)
    except (ExecutionFailure, ImportError, ModelError, OSError, ValueError) as error:
        print(f"Local {args.model} example failed: {error}", file=sys.stderr)
        return 1
    print(f"Saved checked {args.model} results in {args.directory}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
