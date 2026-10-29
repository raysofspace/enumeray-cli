#!/usr/bin/env python3
"""Collection protocol example: copy every input member into a declared output set."""
import argparse
import json
from pathlib import Path
import shutil


def unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("Duplicate JSON member")
        result[key] = value
    return result


def confined(root, relative):
    parts = relative.split("/")
    if not relative or any(p in ("", ".", "..") for p in parts) or "\\" in relative:
        raise ValueError("Unsafe member path")
    current = root
    for part in parts:
        current /= part
        if current.is_symlink():
            raise ValueError("Symlinks are not collection members")
    if not current.is_file() or not current.resolve().is_relative_to(root.resolve()):
        raise ValueError("Member is not a confined regular file")
    return current


def execute(request_path, result_path):
    request = json.loads(request_path.read_text(), object_pairs_hook=unique)
    expected = {"wrapper_protocol_version", "run_id", "assignment_id", "attempt_id",
                "launch_intent_id", "model_id", "publication_id", "inputs", "input_files",
                "input_collections"}
    if set(request) != expected or request["wrapper_protocol_version"] != 1:
        raise ValueError("Invalid protocol-1 collection request")
    workspace = request_path.resolve().parent
    collection = request["input_collections"]["dataset"]
    if request["inputs"]["dataset"]["collection_id"] != collection["collection_id"]:
        raise ValueError("Input identity differs")
    manifest = json.loads(confined(workspace, collection["manifest"]).read_text(), object_pairs_hook=unique)
    if len(manifest) > 10000:
        raise ValueError("Member limit exceeded")
    root = workspace / collection["root"]
    output = workspace / "snapshots"
    output.mkdir(mode=0o700)
    members = []
    for member in manifest:
        spec = member  # Pinned manifest fields are flattened.
        source = confined(root, spec["path"])
        target = output / spec["path"]
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        members.append({"path": spec["path"], "media_type": spec["media_type"],
                        "scientific_metadata": spec["scientific_metadata"]})
    (workspace / "snapshots-members.json").write_text(json.dumps(members))
    result = {key: request[key] for key in ("wrapper_protocol_version", "run_id",
              "assignment_id", "attempt_id", "launch_intent_id")}
    result.update(outcome="succeeded", outputs={"member_count": len(members)}, files={},
                  failure=None, collections={"snapshots": {"root": "snapshots",
                  "manifest": "snapshots-members.json"}})
    result_path.write_text(json.dumps(result))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=["execute"])
    parser.add_argument("--request", type=Path, required=True)
    parser.add_argument("--result", type=Path, required=True)
    args = parser.parse_args()
    execute(args.request, args.result)
