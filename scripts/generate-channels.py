#!/usr/bin/env python3
"""Render channel updates from an already verified published-release manifest.

This pure generator neither authenticates a release nor writes to GitHub. The
release delivery gate verifies the Sigstore signature, qualification and public
release before invoking it. Publishing uses the separately scoped helper.
"""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import tempfile


ROOT = Path(__file__).resolve().parents[1]
PUBLIC_REPOSITORY = "raysofspace/enumeray-cli"
TARGETS = {
    "x86_64-unknown-linux-gnu": {"minimum_glibc": "2.28"},
    "aarch64-unknown-linux-gnu": {"minimum_glibc": "2.28"},
    "x86_64-apple-darwin": {"minimum_macos": "14.0"},
    "aarch64-apple-darwin": {"minimum_macos": "14.0"},
}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def encoded(value):
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def validate_manifest(manifest):
    """Reject ambiguous or noncanonical package identities before rendering code."""
    version = manifest.get("version", "")
    if (manifest.get("schema_version") != 1
            or not isinstance(version, str)
            or not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", version)
            or manifest.get("tag") != "binaries-v" + version
            or manifest.get("public_repository") != PUBLIC_REPOSITORY):
        raise ValueError("Expected a stable version and the canonical public release identity")
    for name, size in (("public_revision", 40), ("source_tree", 40),
                       ("lockfile_sha256", 64), ("builder_workflow_revision", 40)):
        if not re.fullmatch(r"[0-9a-f]{%s}" % size, str(manifest.get(name, ""))):
            raise ValueError("Invalid release identity field: " + name)
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", str(manifest.get("rust_toolchain", ""))):
        raise ValueError("The signed source identity must pin a numeric Rust toolchain")
    baseline = manifest.get("baseline", {})
    if (not isinstance(baseline, dict)
            or not re.fullmatch(r"persistent-v[1-9][0-9]*", str(baseline.get("id", "")))
            or not re.fullmatch(r"[0-9a-f]{64}", str(baseline.get("sha256", "")))):
        raise ValueError("Missing released compatibility baseline identity")
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
    if config.get("schema_version") != 1:
        raise ValueError("Unsupported channel configuration")
    for channel in ("homebrew", "conda"):
        item = config.get(channel, {})
        for key in ("repository", "fork_repository"):
            value = item.get(key)
            if value is not None and not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", value):
                raise ValueError("Invalid channel repository: " + channel)
        branch = item.get("base_branch", "main")
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._/-]*", branch) or ".." in branch:
            raise ValueError("Invalid channel base branch: " + channel)
    conda = config.get("conda", {})
    directory = conda.get("recipe_directory", "recipe")
    path = PurePosixPath(directory)
    if path.is_absolute() or ".." in path.parts or not path.parts:
        raise ValueError("Conda recipe directory must remain within its repository")
    if conda.get("mode") not in ("staged-recipes", "feedstock"):
        raise ValueError("Conda mode must be staged-recipes or feedstock")
    for maintainer in conda.get("maintainers", []):
        if not isinstance(maintainer, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9-]{0,38}", maintainer):
            raise ValueError("Invalid consenting Conda recipe maintainer")


def render(path, replacements):
    contents = path.read_text()
    for key, value in replacements.items():
        contents = contents.replace("@" + key + "@", value)
    if re.search(r"@[A-Z_]+@", contents):
        raise ValueError("Unresolved channel template field: " + str(path))
    return contents.encode()


def generate(manifest, config, channel="all", root=ROOT):
    files, source_archive = validate_manifest(manifest)
    validate_config(config)
    url = "https://github.com/%s/releases/download/%s/" % (PUBLIC_REPOSITORY, manifest["tag"])
    artifacts = {}
    channels = {}
    selected = ("homebrew", "conda") if channel == "all" else (channel,)
    identity = {
        "version": manifest["version"], "tag": manifest["tag"],
        "public_repository": PUBLIC_REPOSITORY,
        "public_revision": manifest["public_revision"],
        "source_tree": manifest["source_tree"],
        "lockfile_sha256": manifest["lockfile_sha256"],
        "rust_toolchain": manifest["rust_toolchain"],
        "baseline": manifest["baseline"],
    }
    for name in selected:
        settings = config.get(name, {})
        entry = {"state": "ready", "files": [], "repository": settings.get("repository"),
                 "base_branch": settings.get("base_branch", "main"),
                 "branch": "codex/enumeray-v" + manifest["version"]}
        channels[name] = entry
        if not settings.get("repository"):
            entry.update(state="blocked-configuration", reason="Configure the channel repository")
            continue
        if name == "homebrew":
            arm = files[manifest["targets"]["aarch64-apple-darwin"]["archive"]]
            intel = files[manifest["targets"]["x86_64-apple-darwin"]["archive"]]
            outputs = {"Formula/enumeray.rb": render(root / "distribution/homebrew/enumeray.rb.in", {
                "VERSION": manifest["version"], "ARM_URL": url + arm["name"],
                "ARM_SHA256": arm["sha256"], "INTEL_URL": url + intel["name"],
                "INTEL_SHA256": intel["sha256"],
            })}
        else:
            if not settings.get("maintainers"):
                entry.update(state="blocked-configuration", reason="Configure consenting recipe maintainers")
                continue
            # The signer binds this compiler to the released source. The trusted
            # delivery workflow's current checkout may use a different compiler.
            rust_version = manifest["rust_toolchain"]
            replacements = {
                "VERSION": manifest["version"], "SOURCE_URL": url + source_archive["name"],
                "SOURCE_SHA256": source_archive["sha256"], "RUST_VERSION": rust_version,
                "LOCKFILE_SHA256": manifest["lockfile_sha256"],
                "MAINTAINERS": "\n".join("    - " + login for login in settings["maintainers"]),
            }
            outputs = {
                "recipe/recipe.yaml": render(root / "distribution/conda/recipe.yaml.in", replacements),
                "recipe/build.sh": render(root / "distribution/conda/build.sh.in", replacements),
                "recipe/conda_build_config.yaml": render(
                    root / "distribution/conda/conda_build_config.yaml.in", replacements),
                "recipe/source.json": encoded(identity),
                "recipe/upstream-source.json": encoded({
                    "repository": "https://github.com/" + PUBLIC_REPOSITORY,
                    "revision": manifest["public_revision"], "tree": manifest["source_tree"],
                    "lockfile_sha256": manifest["lockfile_sha256"], "clean": True,
                }),
                "recipe/compatibility.json": encoded({
                    "package": "enumeray-cli", "binary": "enumeray", "version": manifest["version"],
                    "api": "v1", "connector_protocol": 1, "wrapper_protocols": [1, 2],
                    "capabilities": ["cli", "connector", "compute_runner"],
                    "requires_database": False, "requires_openssh_for_ssh": True,
                    "source": identity,
                }),
            }
            entry["mode"] = settings["mode"]
            entry["maintainers"] = settings["maintainers"]
            entry["fork_repository"] = settings.get("fork_repository")
            entry["recipe_directory"] = settings.get("recipe_directory", "recipe")
        for relative, contents in sorted(outputs.items()):
            output = name + "/" + relative
            artifacts[output] = contents
            destination = relative if name == "homebrew" else str(
                PurePosixPath(entry["recipe_directory"]) / PurePosixPath(relative).relative_to("recipe"))
            entry["files"].append({"path": output, "destination": destination, "sha256": digest(contents)})
    return artifacts, {"schema_version": 1, "release": identity, "channels": channels}


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
    parser.add_argument("--channel", choices=("homebrew", "conda", "all"), default="all")
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
