#!/usr/bin/env python3
"""Bounded raw HTTP integrations using published inputs, with retained retry state.

Requires Python 3.9+ and the adjacent collection-identity.py. ENUMERAY_TOKEN is
read from the environment, never sent to storage or written to the state files.
"""
import argparse
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

SPEC = importlib.util.spec_from_file_location("collection_identity", Path(__file__).with_name("collection-identity.py"))
identity = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(identity)
MAX_EXAMPLE_BYTES = 1024 * 1024


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, newurl):
        return None


def checked_url(url):
    parsed = urllib.parse.urlsplit(url)
    if parsed.username or parsed.password or parsed.fragment:
        raise ValueError("transfer URL contains unsupported credentials or fragment")
    if parsed.scheme != "https" and not (parsed.scheme == "http" and parsed.hostname in ("127.0.0.1", "localhost", "::1")):
        raise ValueError("HTTPS is required except for a disposable loopback service")
    return url


def transfer(method, url, headers=None, body=None):
    # A fresh request has only the explicitly supplied headers. Redirects are
    # never followed here, including a redirect issued by object storage.
    request = urllib.request.Request(checked_url(url), data=body, headers=dict(headers or {}), method=method)
    try:
        response = urllib.request.build_opener(NoRedirect()).open(request, timeout=30)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        data = response.read(MAX_EXAMPLE_BYTES + 1)
        if len(data) > MAX_EXAMPLE_BYTES:
            raise ValueError("response exceeds the small-example budget")
        return response.code, response.headers, data


def sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def durable_directory(path):
    if path.exists():
        if path.is_symlink() or not path.is_dir():
            raise ValueError("operation state requires a real private directory")
        sync_directory(path.parent)
        return
    durable_directory(path.parent)
    try:
        path.mkdir(mode=0o700)
    except FileExistsError:
        if path.is_symlink() or not path.is_dir():
            raise ValueError("operation directory was replaced")
    sync_directory(path.parent)


def write_private(path, data, immutable=False):
    durable_directory(path.parent)
    if immutable and path.exists():
        if path.read_bytes() != data:
            raise ValueError("saved operation differs; retain it and choose a new directory for new intent")
        sync_directory(path.parent)
        return
    descriptor, temporary_name = tempfile.mkstemp(prefix=".http-operation-", dir=path.parent)
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        if immutable:
            try:
                # No replacement under competing writers; only a complete,
                # fsynced file can acquire the durable operation's final name.
                os.link(temporary, path)
            except FileExistsError:
                if path.read_bytes() != data:
                    raise ValueError("saved operation differs; preserve its original intent")
        else:
            os.replace(temporary, path)
        sync_directory(path.parent)
    finally:
        temporary.unlink(missing_ok=True)


def document(value):
    return identity.encode(value).encode("utf-8")


class Api:
    def __init__(self, origin, directory):
        self.origin = checked_url(origin.rstrip("/"))
        parsed = urllib.parse.urlsplit(self.origin)
        if parsed.path or parsed.query:
            raise ValueError("--api must be an origin without a path or query")
        self.token = os.environ["ENUMERAY_TOKEN"]
        self.directory = directory
        durable_directory(directory)
        account = self.request("GET", "/api/v1/me")
        self.user_id = account["user_id"]
        write_private(directory / "identity.json", document({"schema_version": 1, "api": self.origin, "user_id": account["user_id"]}), True)

    def raw(self, method, path, body=None, key=None):
        if not path.startswith("/") or path.startswith("//"):
            raise ValueError("API paths must stay on the selected origin")
        headers = {"Authorization": "Bearer " + self.token, "Accept": "application/json"}
        if body is not None:
            headers["Content-Type"] = "application/json"
        if key:
            headers["Idempotency-Key"] = key
        return transfer(method, self.origin + path, headers, body)

    def request(self, method, path, body=None, key=None, status=(200,)):
        code, _, data = self.raw(method, path, None if body is None else document(body), key)
        value = identity.loads(data)
        if code not in status:
            # Do not print signed URLs or arbitrary server/scientific messages.
            raise ValueError("HTTP %s: %s" % (code, value.get("code", "unexpected_response")))
        return value

    def operation(self, directory, body, filename="request.json"):
        durable_directory(directory)
        envelope_file = directory / "operation.json"
        key_file = directory / "idempotency-key.txt"
        if envelope_file.exists():
            envelope = identity.loads(envelope_file.read_bytes())
            if str(envelope.get("schema_version")) != "1" or envelope.get("api") != self.origin or envelope.get("user_id") != self.user_id:
                raise ValueError("saved operation belongs to another contract, API or account")
            if envelope.get("request_base64") != base64.b64encode(body).decode("ascii"):
                raise ValueError("saved operation differs; preserve its original intent")
            key = envelope["idempotency_key"]
            if not isinstance(key, str) or str(uuid.UUID(key)) != key or uuid.UUID(key).int == 0:
                raise ValueError("saved operation has an invalid original key")
        else:
            if (directory / filename).exists() or key_file.exists():
                raise ValueError("saved operation is incomplete; preserve its files and recover the original operation envelope")
            key = str(uuid.uuid4())
            envelope = {"schema_version": 1, "api": self.origin, "user_id": self.user_id,
                        "request_base64": base64.b64encode(body).decode("ascii"), "idempotency_key": key}
            write_private(envelope_file, document(envelope), True)
        # Human-readable sidecars can be reconstructed from the one atomic
        # body/key/account envelope; missing state never allocates another key.
        write_private(directory / filename, body, True)
        write_private(key_file, key.encode("ascii"), True)
        sync_directory(directory)
        return key


def small_file(path):
    if path.stat().st_size > MAX_EXAMPLE_BYTES:
        raise ValueError("this recipe is limited to 1 MiB files; use the documented multipart workflow for larger data")
    return path.read_bytes()


def checksum(data):
    return base64.b64encode(hashlib.sha256(data).digest()).decode("ascii")


def download(api, artifact_id, expected_bytes):
    code, headers, _ = api.raw("GET", "/api/v1/artifacts/%s/content" % artifact_id)
    if code != 303:
        raise ValueError("expected the checked artifact 303 capability")
    # Deliberately separate from Api.raw: no API Authorization on this request.
    code, _, data = transfer("GET", headers["Location"])
    if code != 200 or len(data) != len(expected_bytes) or checksum(data) != checksum(expected_bytes):
        raise ValueError("downloaded bytes do not match the retained source checksum")
    return data


def artifact(api, path, directory, metadata=None, media_type="application/octet-stream"):
    data = small_file(path)
    request = {"logical_name": path.name, "media_type": media_type,
               "scientific_metadata": metadata if metadata is not None else {},
               "expected_size_bytes": len(data), "checksum": {"algorithm": "sha256", "checksum_type": "full_object", "value": checksum(data)}}
    key = api.operation(directory, document(request))
    view = api.request("POST", "/api/v1/uploads", request, key, (201,))
    write_private(directory / "reservation.json", document(view["reservation"]), True)
    upload_id = view["reservation"]["upload_id"]
    completion_file = directory / "completion-request.json"
    if view["state"] == "completed":
        receipt = view["completion"]
    else:
        if not completion_file.exists():
            deadline = time.monotonic() + 30
            while view["transfer"]["status"] == "unavailable":
                if time.monotonic() >= deadline:
                    raise ValueError("storage unavailable; resume this same directory later")
                time.sleep(0.05)
                view = api.request("GET", "/api/v1/uploads/" + upload_id)
            if view["transfer"]["status"] != "available":
                raise ValueError("this small recipe requires its original single-PUT transport")
            scoped = view["transfer"]["request"]
            if scoped["method"] != "PUT":
                raise ValueError("unexpected storage transfer method")
            headers = dict(scoped["headers"])
            if any(key.lower() == "authorization" for key in headers):
                raise ValueError("unexpected storage Authorization header")
            code, headers, _ = transfer("PUT", scoped["url"], headers, data)
            if code not in (200, 201) or not headers.get("x-amz-version-id") or headers["x-amz-version-id"] == "null":
                raise ValueError("storage did not acknowledge an exact version; retain reservation and unchanged bytes")
            write_private(completion_file, document({"version_id": headers["x-amz-version-id"]}), True)
        receipt = api.request("PUT", "/api/v1/uploads/%s/completion" % upload_id, identity.loads(completion_file.read_bytes()))
    if receipt["checksum"]["value"] != checksum(data) or int(receipt["size_bytes"]) != len(data):
        raise ValueError("completion differs from the saved upload intent")
    write_private(directory / "completion.json", document(receipt), True)
    api.request("GET", "/api/v1/artifacts/" + receipt["artifact_id"])
    download(api, receipt["artifact_id"], data)
    return receipt


def collection(api, manifest_path, source_root, directory, reuse_path=None):
    source = manifest_path.read_bytes()
    members = identity.loads(source)
    canonical = identity.manifest_bytes(members)
    request = {"logical_name": "http-example", "scientific_metadata": {},
               "member_count": len(members), "total_bytes": sum(int(m["size_bytes"]) for m in members),
               "manifest_sha256": identity.digest(canonical)}
    key = api.operation(directory, document(request))
    write_private(directory / "requested-manifest.json", canonical, True)
    reuse_file = directory / "reuse-artifacts.json"
    reuse = identity.loads(reuse_path.read_bytes()) if reuse_path else (
        identity.loads(reuse_file.read_bytes()) if reuse_file.exists() else {})
    if not isinstance(reuse, dict) or not set(reuse).issubset({member["path"] for member in members}):
        raise ValueError("reuse map must name only members of the original manifest")
    for artifact_id in reuse.values():
        if not isinstance(artifact_id, str) or str(uuid.UUID(artifact_id)) != artifact_id or uuid.UUID(artifact_id).int == 0:
            raise ValueError("reuse map requires canonical non-nil artifact UUIDs")
    write_private(reuse_file, document(reuse), True)
    receipt = api.request("POST", "/api/v1/collections", request, key)
    collection_id = receipt["collection_id"]
    write_private(directory / "receipt.json", document(receipt), True)
    base = "/api/v1/collections/" + collection_id
    # One member per metadata page makes exact replay observable and stays bounded.
    for index, member in enumerate(members):
        page_path = base + "/members/" + str(index)
        first = api.request("PUT", page_path, [member])
        if api.request("PUT", page_path, [member]) != first:
            raise ValueError("member-page replay changed its receipt")
    api.request("PUT", base + "/freeze", {})
    pins = []
    root = source_root.resolve()
    for index, member in enumerate(members):
        path = (root / member["path"]).resolve()
        if root not in path.parents:
            raise ValueError("member source escapes the selected root")
        data = small_file(path)
        if len(data) != int(member["size_bytes"]) or checksum(data) != member["checksum_sha256"]:
            raise ValueError("member source changed; retain original operation and sources")
        if member["path"] in reuse:
            artifact_id = reuse[member["path"]]
            view = api.request("GET", "/api/v1/artifacts/" + artifact_id)
            version_id = view["content"]["version_id"]
            download(api, artifact_id, data)
        else:
            uploaded = artifact(api, path, directory / ("member-%04d" % index), member["scientific_metadata"], member["media_type"])
            artifact_id, version_id = uploaded["artifact_id"], uploaded["version_id"]
        api.request("PUT", base + "/members/%s/artifact" % index, {"artifact_id": artifact_id})
        pins.append(dict(member, artifact_id=artifact_id, version_id=version_id))
    sealed = api.request("PUT", base + "/completion", {})
    expected = identity.digest(identity.manifest_bytes(pins, sealed=True))
    if sealed["state"] != "ready" or sealed["manifest_digest"] != expected:
        raise ValueError("sealed collection differs from the verified member pins")
    write_private(directory / "sealed-identity.json", document({"collection_id": collection_id, "manifest_digest": expected}), True)
    write_private(directory / "sealed.json", document(sealed))
    observed = api.request("GET", base)
    inspected = []
    cursor = None
    while True:
        page = api.request("GET", base + "/members" + ("?cursor=" + str(cursor) if cursor is not None else ""))
        inspected.extend(page["items"])
        next_cursor = page["next_cursor"]
        if next_cursor is None:
            break
        if int(next_cursor) <= (int(cursor) if cursor is not None else 0) or len(inspected) > len(members):
            raise ValueError("member page cursor did not advance within the saved manifest")
        cursor = next_cursor
    if observed["manifest_digest"] != expected or len(inspected) != len(members):
        raise ValueError("inspection differs from the sealed small collection")
    for actual, pinned in zip(inspected, pins):
        if actual["artifact_id"] != pinned["artifact_id"] or actual["version_id"] != pinned["version_id"]:
            raise ValueError("inspected member differs from the original immutable pin")
    return {"collection_id": collection_id, "manifest_sha256": request["manifest_sha256"],
            "manifest_digest": expected, "reference": {"collection_id": collection_id}, "member_count": len(members)}


def submit(api, request_path, directory, lose_reply=False, changed_path=None):
    body = request_path.read_bytes()
    identity.loads(body)  # Validate without reserializing the saved wire bytes.
    key = api.operation(directory, body)
    if lose_reply:
        code, _, _ = api.raw("POST", "/api/v1/runs", body, key)
        if code != 201:
            raise ValueError("initial submission was not accepted")
        # Intentionally discard that reply. Reconcile only with the saved body/key.
    code, _, raw = api.raw("POST", "/api/v1/runs", body, key)
    if code != 201:
        raise ValueError("submission was not accepted; preserve the saved body/key")
    receipt = identity.loads(raw)
    write_private(directory / "receipt.json", document(receipt), True)
    if changed_path:
        changed = changed_path.read_bytes()
        code, _, raw = api.raw("POST", "/api/v1/runs", changed, key)
        if code != 409 or identity.loads(raw)["code"] != "idempotency_conflict":
            raise ValueError("changed content did not conflict with the original run key")
    observed = api.request("GET", "/api/v1/runs/" + receipt["run_id"])
    if observed["run_id"] != receipt["run_id"]:
        raise ValueError("inspection identity differs from the original receipt")
    return {"receipt": receipt, "changed_body_conflicted": changed_path is not None}


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api", required=True)
    parser.add_argument("--directory", type=Path, required=True)
    commands = parser.add_subparsers(dest="command", required=True)
    upload = commands.add_parser("artifact")
    upload.add_argument("--file", type=Path, required=True)
    upload.add_argument("--media-type", default="application/octet-stream")
    upload.add_argument("--metadata", type=Path, help="JSON scientific-metadata object, preserving exact numeric tokens")
    group = commands.add_parser("collection")
    group.add_argument("--manifest", type=Path, required=True)
    group.add_argument("--source-root", type=Path, required=True)
    group.add_argument("--reuse-artifacts", type=Path)
    run = commands.add_parser("run")
    run.add_argument("--request", type=Path, required=True)
    run.add_argument("--lose-reply", action="store_true")
    run.add_argument("--changed-request", type=Path)
    args = parser.parse_args()
    api = Api(args.api, args.directory)
    if args.command == "artifact":
        metadata = identity.loads(args.metadata.read_bytes()) if args.metadata else {}
        if not isinstance(metadata, dict):
            raise ValueError("artifact scientific metadata must be a JSON object")
        result = artifact(api, args.file, args.directory / "artifact", metadata, args.media_type)
    elif args.command == "collection":
        result = collection(api, args.manifest, args.source_root, args.directory / "collection", args.reuse_artifacts)
    else:
        result = submit(api, args.request, args.directory / "run", args.lose_reply, args.changed_request)
    print(identity.encode(result))


if __name__ == "__main__":
    main()
