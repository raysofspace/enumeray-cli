#!/usr/bin/env python3
"""Protocol-1 file wrapper for the independently runnable scalar model.

The wrapper handles only the local request/result boundary. It has no API,
credential, assignment journal, or backend reconciliation responsibilities.
"""

import argparse
from decimal import Decimal, InvalidOperation
import os
from pathlib import Path
import sys

from model import double


from protocol import (BoundaryError, IDENTITIES, MAX_BYTES, ModelError, exact_json, preflight_result,
                      publish, read_request as protocol_request, validate_storage)

WrapperError = BoundaryError


def read_request(path):
    value = protocol_request(path, {"model": {"id": "scalar-double"}})
    if value["input_files"] != {}:
        raise BoundaryError("The scalar wrapper does not declare file inputs.")
    return value


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
    try:
        validate_storage(result)
    except ModelError:
        return failed_result(request, "scalar_overflow", "The exact doubled value exceeds the result limit.")
    return result


def publish_result(path, result):
    publish(path, (exact_json(result) + "\n").encode("utf-8"))


def main(argv=None):
    parser = argparse.ArgumentParser(description="Execute the local scalar wrapper using protocol 1.")
    commands = parser.add_subparsers(dest="command", required=True)
    execute = commands.add_parser("execute", help="Read a request and publish a result")
    execute.add_argument("--request", required=True)
    execute.add_argument("--result", required=True)
    args = parser.parse_args(argv)
    try:
        request = read_request(args.request)
        preflight_result(args.result)
        result = evaluate(request)
        publish_result(args.result, result)
    except WrapperError as error:
        print(f"Scalar wrapper: {error}", file=sys.stderr)
        return 2
    print("Scalar wrapper completed.", file=sys.stderr)
    return 0 if result["outcome"] == "succeeded" else 1


if __name__ == "__main__":
    raise SystemExit(main())
