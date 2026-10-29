#!/usr/bin/env python3
"""Package the unified executable and reject server dependencies.

No service, database, AWS SDK or repository checkout is needed on the target host.
The release workflow supplies the signature, after this script creates checksums.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
from urllib.parse import unquote


def bundle_guide(source, destination, repository, guide_names):
    """Keep packaged guides navigable without their original checkout layout."""
    prefix = "https://github.com/raysofspace/enumeray-cli/blob/main/"

    def rewrite(match):
        target, separator, fragment = match[1].partition("#")
        if target.startswith(prefix):
            resolved = repository / unquote(target[len(prefix):])
        elif re.match(r"[A-Za-z][A-Za-z0-9+.-]*:", target) or target.startswith(("/", "#")) or not target:
            return match[0]
        else:
            resolved = source.parent / unquote(target)
        resolved = resolved.resolve()
        if resolved in guide_names:
            url = guide_names[resolved]
        else:
            try:
                url = prefix + resolved.relative_to(repository.resolve()).as_posix()
            except ValueError:
                raise SystemExit(f"Guide link escapes the source repository: {source}: {target}")
        return "](" + url + (separator + fragment if separator else "") + ")"

    destination.write_text(re.sub(r"\]\(([^\s)]+)\)", rewrite, source.read_text()))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True)
    parser.add_argument("--output", type=Path, default=Path("dist"))
    parser.add_argument("--version")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    if not re.fullmatch(r"[A-Za-z0-9_-]+", args.target):
        parser.error("target must be a Rust target triple")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    # Cargo resolves inherited workspace metadata as well as explicit versions.
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
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
    executable = root / "target" / args.target / "release" / binary
    if not executable.is_file():
        raise SystemExit(f"Build the independent target binary first: {executable}")
    archive_name = f"{binary}-{version}-{args.target}"
    with tempfile.TemporaryDirectory(prefix="enumeray-package-") as temporary:
        contents = Path(temporary) / archive_name
        contents.mkdir()
        shutil.copy2(executable, contents / binary)
        (contents / binary).chmod(0o755)
        shutil.copy2(root / "LICENSE", contents / "LICENSE")
        guide_names = {
            (root / "crates/cli/INSTALL.md").resolve(): "INSTALL.md",
            (root / "crates/cli/AUTHOR_GUIDE.md").resolve(): "AUTHOR_GUIDE.md",
            (root / "crates/cli/ANALYSIS_GUIDE.md").resolve(): "ANALYSIS_GUIDE.md",
            (root / "crates/connector/README.md").resolve(): "CONNECTOR_GUIDE.md",
            (root / "crates/connector/EXTENDING.md").resolve(): "EXTENDING.md",
        }
        for source, name in guide_names.items():
            if name not in ("AUTHOR_GUIDE.md", "ANALYSIS_GUIDE.md"):
                bundle_guide(source, contents / name, root, guide_names)
        shutil.copytree(root / "crates" / "cli" / "examples", contents / "examples")
        shutil.copy2(root / "crates" / "cli" / "AUTHOR_GUIDE.md", contents / "AUTHOR_GUIDE.md")
        shutil.copy2(root / "crates" / "cli" / "ANALYSIS_GUIDE.md", contents / "ANALYSIS_GUIDE.md")
        # Release jobs package native binaries. Generate complete projects
        # through the embedded assets so archive examples cannot drift.
        starters = contents / "starter-kit"
        starters.mkdir()
        for kind in ("python", "executable"):
            for io in ("json", "files"):
                subprocess.run([
                    str(contents / binary), "model", "init", f"starter-{kind}-{io}",
                    "--directory", str(starters / f"{kind}-{io}"),
                    "--kind", kind, "--io", io, "--driver", "slurm",
                ], check=True, stdout=subprocess.DEVNULL)
        protocol = {
            "package": package, "binary": binary, "version": version,
            "target": args.target, "api": "v1", "connector_protocol": 1,
            "wrapper_protocols": [1, 2], "requires_database": False,
            "capabilities": ["cli", "connector", "compute_runner"],
            "requires_openssh_for_ssh": True,
        }
        (contents / "compatibility.json").write_text(json.dumps(protocol, indent=2) + "\n")
        archive = output / f"{archive_name}.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            tar.add(contents, arcname=archive_name)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (output / f"{archive.name}.sha256").write_text(f"{digest}  {archive.name}\n")
    print(archive)


if __name__ == "__main__":
    main()
