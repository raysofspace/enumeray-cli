#!/usr/bin/env python3
"""Package the unified executable and reject server dependencies.

No service, database, AWS SDK or repository checkout is needed on the target host.
The release workflow supplies the signature, after this script creates checksums.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

_spec = importlib.util.spec_from_file_location('release_artifacts', Path(__file__).with_name('release_artifacts.py'))
artifacts = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(artifacts)

_bundle_spec = importlib.util.spec_from_file_location('example_bundle', Path(__file__).with_name('example_bundle.py'))
example_bundle = importlib.util.module_from_spec(_bundle_spec)
_bundle_spec.loader.exec_module(example_bundle)
bundle_guide = example_bundle.bundle_guide
copy_examples = example_bundle.copy_examples
bundle_example_guides = example_bundle.bundle_example_guides


def assert_source_unchanged(root, revision, original):
    if artifacts.source_identity(root, revision, require_clean=True) != original:
        raise ValueError('Public source identity changed during binary packaging')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True)
    parser.add_argument("--output", type=Path, default=Path("dist"))
    parser.add_argument("--version")
    parser.add_argument("--source", type=Path, help="Public candidate source checkout; defaults to this repository")
    parser.add_argument("--revision", help="Expected full public source commit")
    parser.add_argument("--release", action="store_true", help="Require clean exact source and complete license notices")
    args = parser.parse_args()
    root = args.source.resolve() if args.source else Path(__file__).resolve().parents[1]
    if not re.fullmatch(r"[A-Za-z0-9_-]+", args.target):
        parser.error("target must be a Rust target triple")
    if args.release and args.target not in artifacts.MINIMUM_PLATFORMS:
        parser.error('Release packaging requires a supported qualified target')
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    try:
        source_identity = artifacts.source_identity(root, args.revision, require_clean=args.release)
    except ValueError as error:
        parser.error(str(error))
    # Cargo resolves inherited workspace metadata as well as explicit versions.
    metadata_command = ["cargo", "metadata", "--locked", "--format-version", "1"]
    if args.target in artifacts.MINIMUM_PLATFORMS:
        metadata_command.extend(['--filter-platform', args.target])
    metadata = json.loads(subprocess.check_output(
        metadata_command,
        cwd=root, text=True,
    ))
    packages = {package["name"]: package for package in metadata["packages"]}
    package, binary = "enumeray-cli", "enumeray"
    version = packages[package]["version"]
    if args.version and args.version != version:
        raise SystemExit(f"Tag version {args.version} does not match {package} {version}")
    tree = subprocess.check_output(
        ["cargo", "tree", "--locked", "--edges", "normal", "--prefix", "none", "-p", package],
        cwd=root, text=True,
    )
    names = {line.split()[0] for line in tree.splitlines() if line.strip()}
    forbidden = sorted(name for name in names if name in (
        "enumeray-api", "aws-config", "aws-types", "aws-credential-types",
        "aws-runtime", "aws-sigv4",
    ) or name.startswith(("sqlx", "aws-sdk-", "aws-smithy-", "axum")))
    if forbidden:
        raise SystemExit(f"{package} depends on server libraries: {', '.join(forbidden)}")
    try:
        notices = artifacts.dependency_notices(metadata, strict=args.release)
    except ValueError as error:
        parser.error(str(error))
    executable = Path(metadata["target_directory"]) / args.target / "release" / binary
    if not executable.is_file():
        raise SystemExit(f"Build the independent target binary first: {executable}")
    artifacts.validate_version(version)
    archive_name = f"{binary}-{version}-{args.target}"
    with tempfile.TemporaryDirectory(prefix="enumeray-package-") as temporary:
        contents = Path(temporary) / archive_name
        contents.mkdir()
        shutil.copy2(executable, contents / binary)
        (contents / binary).chmod(0o755)
        shutil.copy2(root / "LICENSE", contents / "LICENSE")
        artifacts.write_json(contents / 'THIRD_PARTY_NOTICES.json', notices)
        example_bundle.assemble_examples(root, contents, contents / binary, source_identity, version, release=args.release)
        protocol = {
            "package": package, "binary": binary, "version": version,
            "target": args.target, "api": "v1", "connector_protocol": 1,
            "wrapper_protocols": [1, 2], "requires_database": False,
            "capabilities": ["cli", "connector", "compute_runner"],
            "requires_openssh_for_ssh": True,
            "source": source_identity,
            "minimum_platform": artifacts.MINIMUM_PLATFORMS.get(args.target),
        }
        artifacts.write_json(contents / 'compatibility.json', protocol)
        archive = output / f"{archive_name}.tar.gz"
        if args.release:
            try:
                assert_source_unchanged(root, args.revision, source_identity)
            except ValueError as error:
                parser.error(str(error))
        artifacts.archive_directory(contents, archive)
    print(archive)


if __name__ == "__main__":
    main()
