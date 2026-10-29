"""Editable protocol-1 boundary. No API, backend, or credential dependencies."""
from decimal import Decimal, InvalidOperation
import json
import re
import os
from pathlib import Path
import tempfile
import uuid

MAX_BYTES = 65536
MAX_FILE_BYTES = 1048576
IDENTITIES = ("run_id", "assignment_id", "attempt_id", "launch_intent_id")


class ModelError(Exception):
    """Fixed diagnostics: never embed scientific values or private paths."""


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ModelError("Duplicate JSON keys are unsupported.")
        result[key] = value
    return result


def finite_number(text):
    try:
        value = Decimal(text)
    except InvalidOperation:
        raise ModelError("Unsupported numeric value.") from None
    if not value.is_finite():
        raise ModelError("Finite numeric values are required.")
    return value


def read_json(path, maximum=MAX_BYTES):
    try:
        with Path(path).open("rb") as source:
            raw = source.read(maximum + 1)
        if len(raw) > maximum:
            raise ModelError("JSON exceeds the declared byte limit.")
        return json.loads(raw.decode("utf-8"), object_pairs_hook=unique_object,
                          parse_int=finite_number, parse_float=finite_number,
                          parse_constant=finite_number)
    except (OSError, UnicodeError, ValueError, RecursionError):
        raise ModelError("Could not read bounded JSON.") from None


def canonical_uuid(value):
    try:
        parsed = uuid.UUID(value) if isinstance(value, str) else None
        return parsed is not None and parsed.int != 0 and str(parsed) == value
    except ValueError:
        return False


def read_request(path, scientific):
    request = read_json(path)
    fields = {"wrapper_protocol_version", *IDENTITIES, "model_id", "publication_id", "inputs", "input_files"}
    if not isinstance(request, dict) or request.keys() != fields:
        raise ModelError("Unsupported request envelope.")
    version = request["wrapper_protocol_version"]
    if not isinstance(version, Decimal) or version != 1 or version.as_tuple().exponent != 0:
        raise ModelError("Protocol version 1 is required.")
    if not all(canonical_uuid(request[key]) for key in IDENTITIES):
        raise ModelError("Canonical nonzero request identities are required.")
    if request["model_id"] != scientific["model"]["id"] or not isinstance(request["publication_id"], str) or not re.fullmatch(r"[a-z][a-z0-9_-]{0,63}", request["publication_id"]):
        raise ModelError("Unsupported publication.")
    if not isinstance(request["inputs"], dict) or not isinstance(request["input_files"], dict):
        raise ModelError("Inputs must be objects.")
    return request


def exact_json(value):
    if isinstance(value, Decimal):
        if not value.is_finite():
            raise ModelError("Nonfinite result number.")
        return str(value)
    if isinstance(value, dict):
        return "{" + ",".join(json.dumps(k) + ":" + exact_json(v) for k, v in value.items()) + "}"
    if isinstance(value, (list, tuple)):
        return "[" + ",".join(exact_json(item) for item in value) + "]"
    return json.dumps(value, allow_nan=False, separators=(",", ":"))


def publish(path, content, maximum=MAX_BYTES):
    """Publish a synced complete file atomically; preserve prior evidence."""
    if len(content) > maximum:
        raise ModelError("Output exceeds the declared byte limit.")
    destination = Path(path)
    temporary = None
    try:
        descriptor, temporary = tempfile.mkstemp(prefix=".model-result-", dir=destination.parent)
        with os.fdopen(descriptor, "wb") as target:
            target.write(content)
            target.flush()
            os.fsync(target.fileno())
        os.link(temporary, destination)
        os.unlink(temporary)
        temporary = None
        directory = os.open(destination.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    except OSError:
        raise ModelError("Could not durably publish output; preserve existing evidence.") from None
    finally:
        if temporary is not None:
            try:
                os.unlink(temporary)
            except OSError:
                pass


def staged_dataset(request, workspace):
    reference = request["inputs"].get("dataset")
    staged = request["input_files"].get("dataset")
    if request["inputs"].keys() != {"dataset"} or request["input_files"].keys() != {"dataset"}:
        raise ModelError("One staged dataset is required.")
    if not isinstance(reference, dict) or reference.keys() != {"artifact_id"} or not canonical_uuid(reference["artifact_id"]):
        raise ModelError("A dataset artifact reference is required.")
    if not isinstance(staged, dict) or staged.get("artifact_id") != reference["artifact_id"] or not isinstance(staged.get("path"), str):
        raise ModelError("Staged dataset identity mismatch.")
    relative = Path(staged["path"])
    if relative.is_absolute() or ".." in relative.parts or not relative.parts:
        raise ModelError("Dataset must be workspace-relative.")
    resolved = (workspace / relative).resolve()
    if not resolved.is_relative_to(workspace.resolve()) or not resolved.is_file() or resolved.stat().st_size > MAX_FILE_BYTES:
        raise ModelError("Dataset exceeds its workspace boundary.")
    return resolved
