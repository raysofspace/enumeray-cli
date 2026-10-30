#!/usr/bin/env python3
"""Render channel updates from an already verified published-release manifest.

This pure generator neither authenticates a release nor writes to GitHub. The
release delivery gate verifies the Sigstore signature, qualification and public
release before invoking it. Publishing uses the separately scoped helper.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import tempfile


ROOT = Path(__file__).resolve().parents[1]
_spec = importlib.util.spec_from_file_location("release_artifacts", ROOT / "scripts/release_artifacts.py")
artifacts_policy = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(artifacts_policy)
PUBLIC_REPOSITORY = "raysofspace/enumeray-cli"
TARGETS = {
    "x86_64-unknown-linux-gnu": {"minimum_glibc": "2.28"},
    "aarch64-unknown-linux-gnu": {"minimum_glibc": "2.28"},
    "x86_64-apple-darwin": {"minimum_macos": "15.0"},
    "aarch64-apple-darwin": {"minimum_macos": "15.0"},
}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def encoded(value):
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def validate_manifest(manifest):
    """Reject ambiguous or noncanonical package identities before rendering code."""
    version = artifacts_policy.validate_version(manifest.get("version"))
    if (manifest.get("schema_version") != 1 or manifest.get("tag") != "v" + version
            or manifest.get("public_repository") != PUBLIC_REPOSITORY):
        raise ValueError("Expected the canonical public release identity")
    for name, size in (("public_revision", 40), ("source_tree", 40),
                       ("lockfile_sha256", 64), ("builder_workflow_revision", 40)):
        if not re.fullmatch(r"[0-9a-f]{%s}" % size, str(manifest.get(name, ""))):
            raise ValueError("Invalid release identity field: " + name)
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", str(manifest.get("rust_toolchain", ""))):
        raise ValueError("The signed source identity must pin a numeric Rust toolchain")
    artifacts_policy.validate_baseline(manifest.get("baseline"), version)
    files = {}
    for item in manifest.get("files", []):
        name = item.get("name", "")
        if (not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", name)
                or name in files
                or not re.fullmatch(r"[0-9a-f]{64}", str(item.get("sha256", "")))):
            raise ValueError("Duplicate or invalid release asset identity")
        files[name] = item
    if set(manifest.get("targets", {})) != set(TARGETS):
        raise ValueError("The qualified release must cover all four supported targets")
    for target, minimum in TARGETS.items():
        platform = manifest["targets"][target]
        archive = "enumeray-%s-%s.tar.gz" % (version, target)
        if (any(platform.get(key) != value for key, value in minimum.items())
                or platform.get("archive") != archive
                or files.get(archive, {}).get("kind") != "binary"
                or files.get(archive, {}).get("target") != target):
            raise ValueError("Archive or minimum-platform mismatch: " + target)
    sources = [item for item in files.values() if item.get("kind") == "source"]
    if len(sources) != 1 or not sources[0]["name"].endswith(".tar.gz"):
        raise ValueError("Expected one versioned source archive")
    return files, sources[0]


def validate_config(config):
    if (not isinstance(config, dict) or config.get("schema_version") != 1
            or set(config) - {"schema_version", "homebrew"}):
        raise ValueError("Only the Homebrew distribution channel is supported")
    item = config.get("homebrew", {})
    if not isinstance(item, dict) or set(item) - {"repository", "base_branch"}:
        raise ValueError("Invalid Homebrew configuration")
    repository = item.get("repository")
    if repository is not None and (not isinstance(repository, str)
            or not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository)):
        raise ValueError("Invalid channel repository: homebrew")
    branch = item.get("base_branch", "main")
    if not isinstance(branch, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._/-]*", branch) or ".." in branch:
        raise ValueError("Invalid channel base branch: homebrew")


def render(path, replacements):
    contents = path.read_text()
    for key, value in replacements.items():
        contents = contents.replace("@" + key + "@", value)
    if re.search(r"@[A-Z_]+@", contents):
        raise ValueError("Unresolved channel template field: " + str(path))
    return contents.encode()


def generate(manifest, config, channel="homebrew", root=ROOT):
    if channel != "homebrew":
        raise ValueError("Only the Homebrew distribution channel is supported")
    files, _ = validate_manifest(manifest)
    validate_config(config)
    url = "https://github.com/%s/releases/download/%s/" % (PUBLIC_REPOSITORY, manifest["tag"])
    identity = {key: manifest[key] for key in (
        "version", "tag", "public_repository", "public_revision", "source_tree",
        "lockfile_sha256", "rust_toolchain", "baseline")}
    settings = config.get("homebrew", {})
    entry = {"state": "ready", "files": [], "repository": settings.get("repository"),
             "base_branch": settings.get("base_branch", "main"),
             "branch": "codex/enumeray-v" + manifest["version"]}
    plan = {"schema_version": 1, "release": identity, "channels": {"homebrew": entry}}
    if not entry["repository"]:
        entry.update(state="blocked-configuration", reason="Configure the channel repository")
        return {}, plan
    arm = files[manifest["targets"]["aarch64-apple-darwin"]["archive"]]
    intel = files[manifest["targets"]["x86_64-apple-darwin"]["archive"]]
    contents = render(root / "distribution/homebrew/enumeray.rb.in", {
        "VERSION": manifest["version"], "ARM_URL": url + arm["name"], "ARM_SHA256": arm["sha256"],
        "INTEL_URL": url + intel["name"], "INTEL_SHA256": intel["sha256"],
    })
    output = "homebrew/Formula/enumeray.rb"
    entry["files"].append({"path": output, "destination": "Formula/enumeray.rb", "sha256": digest(contents)})
    return {output: contents}, plan


def write_delivery(output, artifacts, report):
    """Rerunning the same version is harmless; conflicting rendered bytes fail."""
    output = Path(output)
    report_bytes = encoded(report)
    planned = {**artifacts, "channel-plan.json": report_bytes}
    for relative, contents in planned.items():
        destination = output / relative
        if destination.exists() and destination.read_bytes() != contents:
            raise ValueError("Refusing to replace conflicting generated delivery: " + relative)
    for relative, contents in planned.items():
        destination = output / relative
        if destination.exists():
            continue
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as temporary:
            temporary.write(contents)
            temporary_path = Path(temporary.name)
        temporary_path.replace(destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", required=True, type=Path)
    parser.add_argument("--config", type=Path, default=ROOT / "distribution/channels.json")
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--channel", choices=("homebrew",), default="homebrew")
    args = parser.parse_args()
    try:
        artifacts, report = generate(json.loads(args.manifest.read_text()),
                                     json.loads(args.config.read_text()), args.channel)
        report["manifest_sha256"] = digest(args.manifest.read_bytes())
        write_delivery(args.output, artifacts, report)
    except (ValueError, OSError, KeyError, TypeError) as error:
        parser.exit(1, "Channel generation failed: " + str(error) + "\n")
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()
