#!/usr/bin/env python3
"""Protocol-1 file wrapper for the independently runnable scalar model.

The wrapper handles only the local request/result boundary. It has no API,
credential, assignment journal, or backend reconciliation responsibilities.
"""

import argparse
from decimal import Decimal, InvalidOperation
import json
import re
import os
from pathlib import Path
import sys
import tempfile
import uuid

from model import double


MAX_BYTES = 64 * 1024
IDENTITIES = ("run_id", "assignment_id", "attempt_id", "launch_intent_id")
REQUEST_FIELDS = {
    "wrapper_protocol_version", *IDENTITIES, "model_id", "publication_id", "inputs", "input_files"
}


class WrapperError(Exception):
    """A bounded diagnostic that never incorporates request values or paths."""


def unique_object(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise WrapperError("The request contains duplicate JSON keys.")
        value[key] = item
    return value


def finite_number(text):
    try:
        value = Decimal(text)
    except InvalidOperation as error:
        raise WrapperError("The request contains an unsupported number.") from error
    if not value.is_finite():
        raise WrapperError("The request contains a non-finite number.")
    return value


def read_request(path):
    try:
        with Path(path).open("rb") as source:
            raw = source.read(MAX_BYTES + 1)
        if len(raw) > MAX_BYTES:
            raise WrapperError("The request exceeds 64 KiB.")
        value = json.loads(
            raw.decode("utf-8"), object_pairs_hook=unique_object,
            parse_int=finite_number, parse_float=finite_number,
            parse_constant=finite_number,
        )
    except (OSError, UnicodeError, ValueError, RecursionError) as error:
        raise WrapperError("The request could not be read as bounded JSON.") from error
    if not isinstance(value, dict) or value.keys() != REQUEST_FIELDS:
        raise WrapperError("The request envelope has unsupported or missing fields.")
    version = value["wrapper_protocol_version"]
    if not isinstance(version, Decimal) or version != 1 or version.as_tuple().exponent != 0:
        raise WrapperError("The wrapper requires protocol version 1.")
    for field in IDENTITIES:
        identifier = value[field]
        try:
            parsed = uuid.UUID(identifier) if isinstance(identifier, str) else None
        except ValueError:
            parsed = None
        if parsed is None or parsed.int == 0 or str(parsed) != identifier:
            raise WrapperError("The request requires canonical nonzero UUID identities.")
    if value["model_id"] != "scalar-double" or not isinstance(value["publication_id"], str) or not re.fullmatch(r"[a-z][a-z0-9_-]{0,63}", value["publication_id"]):
        raise WrapperError("The wrapper does not support this publication.")
    if value["input_files"] != {}:
        raise WrapperError("The scalar wrapper does not declare file inputs.")
    if not isinstance(value["inputs"], dict):
        raise WrapperError("The request inputs must be an object.")
    return value


def exact_json(value):
    """Serialize decimal tokens directly instead of converting them to floats."""
    if isinstance(value, Decimal):
        if not value.is_finite():
            raise WrapperError("The result contains a non-finite number.")
        return str(value)
    if isinstance(value, dict):
        return "{" + ",".join(
            json.dumps(key) + ":" + exact_json(item) for key, item in value.items()
        ) + "}"
    return json.dumps(value, allow_nan=False, separators=(",", ":"))


def failed_result(request, code, message):
    return {
        "wrapper_protocol_version": 1,
        **{field: request[field] for field in IDENTITIES},
        "outcome": "failed", "outputs": {}, "files": {},
        "failure": {"code": code, "message": message},
    }


def evaluate(request):
    inputs = request["inputs"]
    if inputs.keys() != {"value"} or not isinstance(inputs["value"], Decimal):
        return failed_result(request, "invalid_inputs", "A single finite numeric value is required.")
    try:
        doubled = double(inputs["value"])
    except (InvalidOperation, ValueError, OverflowError):
        return failed_result(request, "scalar_overflow", "The exact doubled value is unsupported.")
    result = {
        "wrapper_protocol_version": 1,
        **{field: request[field] for field in IDENTITIES},
        "outcome": "succeeded", "outputs": {"doubled": doubled}, "failure": None, "files": {},
    }
    if len(exact_json(result).encode("utf-8")) + 1 > MAX_BYTES:
        return failed_result(request, "scalar_overflow", "The exact doubled value exceeds the result limit.")
    return result


def publish_result(path, result):
    """Publish one complete synced file atomically, never replace prior evidence."""
    destination = Path(path)
    content = (exact_json(result) + "\n").encode("utf-8")
    if len(content) > MAX_BYTES:
        raise WrapperError("The result exceeds 64 KiB.")
    temporary = None
    try:
        descriptor, temporary = tempfile.mkstemp(prefix=".scalar-result-", dir=destination.parent)
        with os.fdopen(descriptor, "wb") as output:
            output.write(content)
            output.flush()
            os.fsync(output.fileno())
        # The source and destination share a filesystem. link publishes the fully
        # written inode atomically and fails if a prior result already exists.
        os.link(temporary, destination)
        os.unlink(temporary)
        temporary = None
        directory = os.open(destination.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    except OSError as error:
        raise WrapperError("The result could not be durably published; preserve existing evidence.") from error
    finally:
        if temporary is not None:
            try:
                os.unlink(temporary)
            except OSError:
                pass


def main(argv=None):
    parser = argparse.ArgumentParser(description="Execute the local scalar wrapper using protocol 1.")
    commands = parser.add_subparsers(dest="command", required=True)
    execute = commands.add_parser("execute", help="Read a request and publish a result")
    execute.add_argument("--request", required=True)
    execute.add_argument("--result", required=True)
    args = parser.parse_args(argv)
    try:
        request = read_request(args.request)
        result = evaluate(request)
        publish_result(args.result, result)
    except WrapperError as error:
        print(f"Scalar wrapper: {error}", file=sys.stderr)
        return 2
    print("Scalar wrapper completed.", file=sys.stderr)
    return 0 if result["outcome"] == "succeeded" else 1


if __name__ == "__main__":
    raise SystemExit(main())
