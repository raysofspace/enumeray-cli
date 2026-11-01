"""Editable protocol-1 boundary. No API, backend, or credential dependencies."""
from decimal import Decimal, InvalidOperation
import json
import re
import os
from pathlib import Path
import tempfile
import uuid
import stat
import unicodedata

MAX_BYTES = 65536
MAX_FILE_BYTES = 1048576
IDENTITIES = ("run_id", "assignment_id", "attempt_id", "launch_intent_id")


class ModelError(Exception):
    """Fixed diagnostics: never embed scientific values or private paths."""


class BoundaryError(ModelError):
    """Reject before science, or preserve uncertain publication without a result."""


class JsonInteger(Decimal):
    """Keep JSON integer tokens distinct from integral-looking float tokens."""


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        key.encode("utf-8")
        if "\0" in key or key in ("$serde_json::private::Number", "$serde_json::private::RawValue"):
            raise ModelError("Unsupported JSON object key.")
        if key in result:
            raise ModelError("Duplicate JSON keys are unsupported.")
        result[key] = value
    return result


def number_storage_bytes(text):
    """Match the public decimal magnitude/scale and expanded storage limits."""
    unsigned = text.lstrip("-").lower()
    mantissa, separator, power = unsigned.partition("e")
    exponent = int(power) if separator else 0
    if abs(exponent) > 147455:
        raise ModelError("Unsupported numeric exponent.")
    whole, _, fraction = mantissa.partition(".")
    coefficient = whole + fraction
    leading = len(coefficient) - len(coefficient.lstrip("0"))
    integer_places = len(whole) + exponent - leading
    fractional_places = len(fraction) - exponent
    if integer_places > 131072 or fractional_places > 16383:
        raise ModelError("Unsupported numeric magnitude or scale.")
    return max(integer_places, 1) + max(fractional_places, 0) + int(fractional_places > 0) + int(text.startswith("-"))


def finite_number(text):
    try:
        value = Decimal(text)
    except InvalidOperation:
        raise ModelError("Unsupported numeric value.") from None
    if not value.is_finite():
        raise ModelError("Finite numeric values are required.")
    number_storage_bytes(text)
    return value


def read_json(path, maximum=MAX_BYTES):
    try:
        with Path(path).open("rb") as source:
            raw = source.read(maximum + 1)
        if len(raw) > maximum:
            raise ModelError("JSON exceeds the declared byte limit.")
        value = json.loads(raw.decode("utf-8"), object_pairs_hook=unique_object,
                           parse_int=lambda text: JsonInteger(finite_number(text)), parse_float=finite_number,
                           parse_constant=finite_number)
        pending = [(value, 0)]
        while pending:
            item, parent_depth = pending.pop()
            if isinstance(item, (dict, list)):
                depth = parent_depth + 1
                if depth > 64:
                    raise ModelError("JSON exceeds the supported container depth.")
                pending.extend((child, depth) for child in (item.values() if isinstance(item, dict) else item))
            elif isinstance(item, str):
                if "\0" in item:
                    raise ModelError("Unsupported JSON string.")
                item.encode("utf-8")
        return value
    except (OSError, UnicodeError, ValueError, RecursionError):
        raise ModelError("Could not read bounded JSON.") from None


def canonical_uuid(value):
    try:
        parsed = uuid.UUID(value) if isinstance(value, str) else None
        return parsed is not None and parsed.int != 0 and str(parsed) == value
    except ValueError:
        return False


def read_request(path, scientific, collections=False):
    try:
        request = read_json(path)
    except ModelError:
        raise BoundaryError("Could not read the bounded request.") from None
    fields = {"wrapper_protocol_version", *IDENTITIES, "model_id", "publication_id", "inputs", "input_files"}
    if collections:
        fields.add("input_collections")
    if not isinstance(request, dict) or request.keys() != fields:
        raise BoundaryError("Unsupported request envelope.")
    version = request["wrapper_protocol_version"]
    if not isinstance(version, JsonInteger) or version != 1:
        raise BoundaryError("Protocol version 1 is required.")
    if not all(canonical_uuid(request[key]) for key in IDENTITIES):
        raise BoundaryError("Canonical nonzero request identities are required.")
    if request["model_id"] != scientific["model"]["id"] or not isinstance(request["publication_id"], str) or not re.fullmatch(r"[a-z][a-z0-9_-]{0,63}", request["publication_id"]):
        raise BoundaryError("Unsupported publication.")
    if not isinstance(request["inputs"], dict) or not isinstance(request["input_files"], dict):
        raise BoundaryError("Inputs must be objects.")
    try:
        validate_storage(request["inputs"])
    except ModelError:
        raise BoundaryError("Inputs exceed the supported JSON storage boundary.") from None
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


def validate_storage(value, maximum=MAX_BYTES):
    """Budget compact JSON plus each decimal's expanded storage representation."""
    used = len(exact_json(value).encode("utf-8"))
    pending = [value]
    while pending:
        item = pending.pop()
        if isinstance(item, dict):
            pending.extend(item.values())
        elif isinstance(item, list):
            pending.extend(item)
        elif isinstance(item, Decimal):
            text = str(item)
            used += max(number_storage_bytes(text) - len(text), 0)
        if used > maximum:
            raise ModelError("JSON exceeds the expanded decimal storage budget.")


def publish(path, content, maximum=MAX_BYTES):
    """Publish a synced complete file atomically; preserve prior evidence."""
    if len(content) > maximum:
        raise ModelError("Output exceeds the declared byte limit.")
    destination = Path(path)
    temporary = None
    try:
        descriptor, temporary = tempfile.mkstemp(prefix=".model-result-", dir=destination.parent)
        with os.fdopen(descriptor, "wb") as target:
            if target.write(content) != len(content):
                raise OSError("Incomplete output write")
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
        raise BoundaryError("Could not durably publish output; preserve existing evidence.") from None
    finally:
        if temporary is not None:
            try:
                os.unlink(temporary)
            except OSError:
                pass


def preflight_result(path):
    destination = Path(path).absolute()
    if destination.exists() or destination.is_symlink() or not destination.parent.is_dir():
        raise BoundaryError("Preserve existing result evidence.")
    return destination


def claim_workspace(workspace):
    """Exclusive, retained fence for file/native work; interruption never grants replay."""
    try:
        (workspace / ".model-workspace").mkdir(mode=0o700)
        sync_directory(workspace)
    except OSError:
        raise BoundaryError("Preserve the retained workspace; use a fresh workspace.") from None


def sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def valid_path(value):
    if not isinstance(value, str) or not value:
        return False
    try:
        encoded = value.encode("utf-8")
    except UnicodeError:
        return False
    if len(encoded) > 1024:
        return False
    parts = value.split("/")
    return (len(parts) <= 32 and unicodedata.normalize("NFC", value) == value
            and not any(c in value for c in ("\\", ":"))
            and not any(unicodedata.category(c) == "Cc" for c in value)
            and all(p not in ("", ".", "..") and len(p.encode("utf-8")) <= 255
                    and p.strip() == p and not p.endswith(".")
                    and not p.startswith(".enumeray-download-") for p in parts))


def confined(workspace, relative, directory=False):
    """Walk lexical components without accepting symlink aliases, even inside root."""
    if not valid_path(relative):
        raise BoundaryError("A portable workspace-relative path is required.")
    current = Path(workspace)
    try:
        parts = relative.split("/")
        for index, part in enumerate(parts):
            current = current / part
            mode = current.lstat().st_mode
            wanted_directory = index < len(parts) - 1 or directory
            if not (stat.S_ISDIR(mode) if wanted_directory else stat.S_ISREG(mode)):
                raise BoundaryError("A confined regular file or directory is required.")
    except OSError:
        raise BoundaryError("A confined regular file or directory is required.") from None
    return current


def staged_dataset(request, workspace):
    reference = request["inputs"].get("dataset")
    staged = request["input_files"].get("dataset")
    if request["inputs"].keys() != {"dataset"} or request["input_files"].keys() != {"dataset"}:
        raise BoundaryError("One staged dataset is required.")
    if not isinstance(reference, dict) or reference.keys() != {"artifact_id"} or not canonical_uuid(reference["artifact_id"]):
        raise BoundaryError("A dataset artifact reference is required.")
    if not isinstance(staged, dict) or staged.keys() != {"artifact_id", "path"} or staged.get("artifact_id") != reference["artifact_id"]:
        raise BoundaryError("Staged dataset identity mismatch.")
    resolved = confined(workspace, staged.get("path"))
    if resolved.stat().st_size > MAX_FILE_BYTES:
        raise BoundaryError("Dataset exceeds its workspace boundary.")
    return resolved
