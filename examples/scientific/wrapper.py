#!/opt/enumeray/scientific/bin/python
"""Protocol-1 entrypoint for the reviewed, bounded HUXt and REBOUND cases."""
import argparse
from pathlib import Path
import sys

from protocol import (BoundaryError, IDENTITIES, ModelError, claim_workspace, exact_json,
                      preflight_result, publish, read_json, read_request, validate_storage)
from runtime import ExecutionFailure, run_model

ROOT = Path(__file__).resolve().parent


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    execute = commands.add_parser("execute")
    execute.add_argument("--model", choices=("huxt", "rebound"), required=True)
    execute.add_argument("--request", required=True)
    execute.add_argument("--result", required=True)
    args = parser.parse_args(argv)
    try:
        scientific = read_json(ROOT / args.model / "publication.json", 1048576)
        request = read_request(args.request, scientific)
        if request["input_files"]:
            raise BoundaryError("This demonstration accepts bounded JSON inputs only.")
        destination = preflight_result(args.result)
        reserved = {"wind-field.csv", "wind.png", "trajectory.csv", "orbit.png", ".model-workspace"}
        if destination.name in reserved or (destination.parent / ".scientific").exists():
            raise BoundaryError("Preserve previous scientific evidence.")
        if any((destination.parent / name).exists() or (destination.parent / name).is_symlink()
               for name in reserved - {".model-workspace"}):
            raise BoundaryError("Preserve previous scientific evidence.")
        claim_workspace(destination.parent)
    except (ModelError, OSError, ValueError, KeyError, TypeError):
        print("Scientific wrapper: invalid request or existing workspace evidence.", file=sys.stderr)
        return 2
    result = {"wrapper_protocol_version": 1, **{key: request[key] for key in IDENTITIES},
              "outcome": "succeeded", "outputs": {}, "files": {}, "failure": None}
    try:
        result["outputs"], result["files"] = run_model(args.model, request["inputs"], destination.parent)
        validate_storage(result)
    except (ExecutionFailure, OSError, ValueError, ModelError) as error:
        code = error.code if isinstance(error, ExecutionFailure) else "model_failure"
        result.update(outcome="failed", outputs={}, files={}, failure={
            "code": code, "message": "The bounded scientific demonstration did not complete successfully."})
    try:
        publish(destination, (exact_json(result) + "\n").encode())
    except ModelError:
        print("Scientific wrapper: preserve result evidence.", file=sys.stderr)
        return 2
    print("Scientific wrapper: " + result["outcome"], file=sys.stderr, flush=True)
    return 0 if result["outcome"] == "succeeded" else 1


if __name__ == "__main__":
    raise SystemExit(main())
