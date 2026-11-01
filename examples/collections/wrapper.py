#!/usr/bin/env python3
"""Standalone protocol-1 example: copy verified input members into an output set."""
import argparse
import base64
import hashlib
import os
from pathlib import Path
import sys
import tempfile
import unicodedata

from protocol import (BoundaryError, IDENTITIES, JsonInteger, ModelError, canonical_uuid, confined,
                      exact_json, preflight_result, publish, read_json, read_request,
                      sync_directory, valid_path, validate_storage)

MAX_MEMBERS = 10000
MAX_MANIFEST_BYTES = 16 * 1024 * 1024
MAX_FILE_BYTES = 100 * 1024 ** 3
MAX_TOTAL_BYTES = 1024 ** 4
MEMBER_FIELDS = {"path", "media_type", "scientific_metadata", "size_bytes",
                 "checksum_sha256", "artifact_id", "version_id"}


def integer(value):
    return isinstance(value, JsonInteger)


def alias(path):
    lowered = "".join(c.lower() for c in path)
    uppered = "".join(c.upper() for c in lowered)
    return unicodedata.normalize("NFC", "".join(c.lower() for c in uppered))


def checksum(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return base64.b64encode(digest.digest()).decode("ascii")


def validate_mapping(request, workspace):
    """Validate every member, including late members, before producing any output."""
    if request["input_files"] or request["inputs"].keys() != {"dataset"}:
        raise BoundaryError("One declared dataset collection is required.")
    reference = request["inputs"]["dataset"]
    mappings = request["input_collections"]
    if (not isinstance(reference, dict) or reference.keys() != {"collection_id"}
            or not canonical_uuid(reference["collection_id"])
            or not isinstance(mappings, dict) or mappings.keys() != {"dataset"}):
        raise BoundaryError("A canonical dataset collection reference is required.")
    staged = mappings["dataset"]
    if (not isinstance(staged, dict) or staged.keys() != {"collection_id", "root", "manifest"}
            or staged["collection_id"] != reference["collection_id"]):
        raise BoundaryError("Staged collection identity mismatch.")
    root = confined(workspace, staged["root"], directory=True)
    manifest_path = confined(workspace, staged["manifest"])
    try:
        members = read_json(manifest_path, MAX_MANIFEST_BYTES)
        if not isinstance(members, list) or len(members) > MAX_MEMBERS:
            raise BoundaryError("A bounded collection member list is required.")
        previous, aliases, total, validated = None, set(), 0, []
        for member in members:
            if not isinstance(member, dict) or member.keys() != MEMBER_FIELDS:
                raise BoundaryError("Unsupported pinned member shape.")
            path, size = member["path"], member["size_bytes"]
            version, encoded = member["version_id"], member["checksum_sha256"]
            if (not valid_path(path) or (previous is not None and previous >= path)
                    or alias(path) in aliases or member["media_type"] != "application/octet-stream"
                    or not isinstance(member["scientific_metadata"], dict)
                    or not integer(size) or not 0 <= size <= MAX_FILE_BYTES
                    or not canonical_uuid(member["artifact_id"])
                    or not isinstance(version, str) or not version or len(version.encode()) > 1024
                    or any(unicodedata.category(c) == "Cc" for c in version)
                    or not isinstance(encoded, str)
                    or len(exact_json(member).encode()) > 65536 - 2048):
                raise BoundaryError("Invalid pinned collection member.")
            try:
                decoded = base64.b64decode(encoded, validate=True)
                if len(decoded) != 32 or base64.b64encode(decoded).decode() != encoded:
                    raise ValueError
            except (ValueError, UnicodeError):
                raise BoundaryError("A canonical SHA-256 checksum is required.") from None
            source = confined(root, path)
            validate_storage(member["scientific_metadata"])
            if source.stat().st_size != size or checksum(source) != encoded:
                raise BoundaryError("Staged member integrity mismatch.")
            total += int(size)
            if total > MAX_TOTAL_BYTES:
                raise BoundaryError("Collection total byte limit exceeded.")
            previous = path
            aliases.add(alias(path))
            validated.append((member, source))
        for name in aliases:
            parts = name.split("/")
            if any("/".join(parts[:i]) in aliases for i in range(1, len(parts))):
                raise BoundaryError("Collection member paths conflict.")
        return validated
    except (ModelError, OSError, ValueError, TypeError, OverflowError, RecursionError, UnicodeError):
        raise BoundaryError("Invalid bounded collection mapping; no output was created.") from None


def copy_member(source, target, member):
    """Stream into private scratch, then sync and publish without replacement."""
    temporary = None
    try:
        target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        descriptor, temporary = tempfile.mkstemp(prefix=".collection-member-", dir=target.parent)
        digest, count = hashlib.sha256(), 0
        with os.fdopen(descriptor, "wb") as output, source.open("rb") as content:
            for chunk in iter(lambda: content.read(1024 * 1024), b""):
                count += len(chunk)
                if count > member["size_bytes"]:
                    raise BoundaryError("Staged member changed during execution.")
                digest.update(chunk)
                if output.write(chunk) != len(chunk):
                    raise OSError("Incomplete member write")
            output.flush()
            os.fsync(output.fileno())
        if count != member["size_bytes"] or base64.b64encode(digest.digest()).decode() != member["checksum_sha256"]:
            raise BoundaryError("Staged member changed during execution.")
        os.link(temporary, target)
        os.unlink(temporary)
        temporary = None
        sync_directory(target.parent)
    except OSError:
        raise BoundaryError("Could not publish member; preserve retained output evidence.") from None
    finally:
        if temporary is not None:
            try:
                os.unlink(temporary)
            except OSError:
                pass


def evaluate(members, workspace):
    """Scientific mapping: byte-preserving copy with declared output metadata."""
    declared = []
    for member, source in members:
        copy_member(source, workspace / "snapshots" / member["path"], member)
        declared.append({key: member[key] for key in ("path", "media_type", "scientific_metadata")})
    return declared


def execute(request_path, result_path):
    request = read_request(request_path, {"model": {"id": "collection-example"}}, collections=True)
    destination = preflight_result(result_path)
    if alias(destination.name) in {"snapshots", "snapshots-members.json"}:
        raise BoundaryError("Result and declared output paths must be distinct.")
    workspace = destination.parent.resolve()
    members = validate_mapping(request, workspace)
    sidecar = workspace / "snapshots-members.json"
    if sidecar.exists() or sidecar.is_symlink():
        raise BoundaryError("Preserve existing collection output evidence.")
    # Only the exclusive output-tree owner may publish a result. A competing
    # invocation exits without publishing a failure that could race the owner.
    try:
        (workspace / "snapshots").mkdir(mode=0o700)
        sync_directory(workspace)
    except OSError:
        raise BoundaryError("Preserve the retained output tree; use a fresh workspace.") from None
    result = {"wrapper_protocol_version": 1, **{key: request[key] for key in IDENTITIES},
              "outcome": "succeeded", "outputs": {}, "files": {}, "failure": None}
    try:
        declared = evaluate(members, workspace)
    except BoundaryError:
        raise
    except (ModelError, OSError, ValueError):
        result.update(outcome="failed", failure={"code": "model_failure",
                      "message": "Model could not process its declared collection."})
    else:
        # All member directories must reach storage before the result commits.
        try:
            for directory, _, _ in os.walk(workspace / "snapshots", topdown=False):
                sync_directory(directory)
        except OSError:
            raise BoundaryError("Could not sync output tree; preserve retained evidence.") from None
        publish(sidecar, (exact_json(declared) + "\n").encode(), MAX_MANIFEST_BYTES)
        result.update(outputs={"member_count": len(members)}, collections={"snapshots": {
            "root": "snapshots", "manifest": "snapshots-members.json"}})
    publish(destination, (exact_json(result) + "\n").encode())
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["execute"])
    parser.add_argument("--request", type=Path, required=True)
    parser.add_argument("--result", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        result = execute(args.request, args.result)
    except ModelError:
        print("Collection wrapper: rejected; preserve existing evidence.", file=sys.stderr)
        return 2
    print("Collection wrapper: " + result["outcome"], file=sys.stderr)
    return 0 if result["outcome"] == "succeeded" else 1


if __name__ == "__main__":
    raise SystemExit(main())
