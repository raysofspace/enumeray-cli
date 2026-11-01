#!/usr/bin/env python3
"""Independent, lossless implementation of Enumeray collection identity v1.

Use only this file and the published collection identity guide. No Rust package,
service, installed CLI or third-party Python dependency is required.
"""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import re
import unicodedata


class Number(str):
    """A validated JSON number token, never a binary floating point value."""


def number(token):
    token = re.sub(r"[eE]([0-9])", r"e+\1", token).replace("E", "e")
    if token == "-0":
        token = "0"
    return Number(token)


def object_pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate object key")
        result[key] = value
    return result


def loads(source):
    def invalid(value):
        raise ValueError("non-JSON numeric constant: " + value)
    return json.loads(source, parse_int=number, parse_float=number,
                      parse_constant=invalid, object_pairs_hook=object_pairs)


def encode(value):
    if isinstance(value, Number):
        return str(value)
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, str):
        # Like the v1 serializer: escape U+0000..001F, quote and backslash;
        # retain all other Unicode scalars, including U+007F and non-BMP text.
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    if isinstance(value, int):
        return str(value)
    if isinstance(value, list):
        return "[" + ",".join(encode(item) for item in value) + "]"
    if isinstance(value, dict):
        return "{" + ",".join(encode(key) + ":" + encode(value[key])
                                for key in sorted(value)) + "}"
    raise ValueError("unsupported JSON value; floats lose scientific precision")


FIELDS = ("path", "media_type", "scientific_metadata", "size_bytes", "checksum_sha256")


def manifest_bytes(members, sealed=False):
    """Serialize an already validated, strictly path-ordered manifest.

    This specifies identity, not the service's complete admission/path policy.
    The service also rejects aliases and enforces declared bounds.
    """
    fields = FIELDS + (("artifact_id", "version_id") if sealed else ())
    previous = None
    encoded = []
    for member in members:
        if set(member) != set(fields):
            raise ValueError("manifest member has missing or unknown fields")
        path = member["path"]
        if not isinstance(path, str) or isinstance(path, Number):
            raise ValueError("path must be a string")
        if unicodedata.normalize("NFC", path) != path:
            raise ValueError("paths must already be NFC")
        if previous is not None and previous >= path:
            raise ValueError("members must be strictly ordered by Unicode path")
        previous = path
        if not isinstance(member["scientific_metadata"], dict):
            raise ValueError("scientific_metadata must be an object")
        checksum = member["checksum_sha256"]
        try:
            decoded = base64.b64decode(checksum, validate=True)
        except (ValueError, TypeError) as error:
            raise ValueError("invalid checksum") from error
        if len(decoded) != 32 or base64.b64encode(decoded).decode("ascii") != checksum:
            raise ValueError("checksum must be canonical padded standard Base64")
        size = str(member["size_bytes"])
        if not re.fullmatch(r"0|[1-9][0-9]*", size) or int(size) > 2**64 - 1:
            raise ValueError("size_bytes must be a nonnegative u64 integer")
        encoded.append("{" + ",".join(encode(key) + ":" + encode(member[key])
                                      for key in fields) + "}")
    return ("[" + ",".join(encoded) + "]").encode("utf-8")


def digest(data):
    return base64.b64encode(hashlib.sha256(data).digest()).decode("ascii")


def verify_vectors(directory):
    directory = Path(directory)
    catalogue = json.loads((directory / "vectors.json").read_text())
    if catalogue["schema_version"] != 1:
        raise ValueError("unsupported vector version")
    for vector in catalogue["vectors"]:
        source = (directory / vector["input"]).read_bytes()
        actual = manifest_bytes(loads(source), vector["sealed"])
        expected = (directory / vector["bytes"]).read_bytes()
        if actual != expected or digest(actual) != vector["sha256_base64"]:
            raise ValueError("identity mismatch: " + vector["name"])
    return len(catalogue["vectors"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", nargs="?", type=Path)
    parser.add_argument("--sealed", action="store_true")
    parser.add_argument("--bytes", type=Path, help="write exact digest input without a newline")
    parser.add_argument("--verify-vectors", type=Path)
    args = parser.parse_args()
    if args.verify_vectors:
        print(json.dumps({"verified_vectors": verify_vectors(args.verify_vectors)}))
        return
    if args.manifest is None:
        parser.error("provide a manifest or --verify-vectors")
    data = manifest_bytes(loads(args.manifest.read_bytes()), args.sealed)
    if args.bytes:
        args.bytes.write_bytes(data)
    print(json.dumps({"sha256_base64": digest(data), "size_bytes": len(data)}))


if __name__ == "__main__":
    main()
