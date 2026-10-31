#!/usr/bin/env python3
"""Verify and smoke-test release archives outside the checkout with a clean environment.

Signature verification is performed by release consumers before this check.
This local build check never signs or publishes anything.
"""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tarfile
import tempfile

_artifacts_spec = importlib.util.spec_from_file_location('release_artifacts', Path(__file__).with_name('release_artifacts.py'))
artifacts = importlib.util.module_from_spec(_artifacts_spec)
_artifacts_spec.loader.exec_module(artifacts)
_platform_spec = importlib.util.spec_from_file_location('release_platform', Path(__file__).with_name('release_platform.py'))
release_platform = importlib.util.module_from_spec(_platform_spec)
_platform_spec.loader.exec_module(release_platform)


def check_guide_links(root, guides):
    """Shipped guides must not require the source checkout for local links."""
    for guide in guides:
        document = root / guide
        if not document.is_file():
            raise SystemExit(f"Archive lacks the workflow guide {guide}")
        for target in re.findall(r"\[[^\]\n]+\]\(([^\s)]+)\)", document.read_text()):
            if target.startswith(("https:", "http:", "#")):
                continue
            destination = (document.parent / target.split("#", 1)[0]).resolve()
            if not destination.is_relative_to(root.resolve()) or not destination.is_file():
                raise SystemExit(f"Broken bundled guide link in {guide}: {target}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--archives", required=True, type=Path)
    parser.add_argument("--target", required=True)
    parser.add_argument("--report", type=Path, help="Write the successful native qualification record")
    parser.add_argument("--require-minimum-platform", action="store_true",
                        help="Require execution on macOS 15 or glibc 2.28, not a newer runtime")
    args = parser.parse_args()
    archives = sorted(args.archives.glob(f"*-{args.target}.tar.gz"))
    if len(archives) != 1:
        parser.error("expected one unified Enumeray archive for the target")
    fresh = Path(tempfile.mkdtemp(prefix="enumeray-package-qualification-"))
    environment = {"PATH": "/usr/bin:/bin", "HOME": str(fresh)}
    result = {"schema_version": 1, "target": args.target}
    archive = archives[0]
    expected, filename = archive.with_suffix(archive.suffix + ".sha256").read_text().split()
    if filename != archive.name or hashlib.sha256(archive.read_bytes()).hexdigest() != expected:
        raise SystemExit(f"Archive checksum failed: {archive.name}")
    with tarfile.open(archive, "r:gz") as tar:
        prefix = archive.name.removesuffix('.tar.gz')
        seen = set()
        for member in tar.getmembers():
            path = PurePosixPath(member.name)
            if (not (member.isfile() or member.isdir()) or path.is_absolute() or '..' in path.parts
                    or not path.parts or path.parts[0] != prefix or member.name in seen):
                raise SystemExit('Archive entries must be unique regular files/directories under the package root')
            seen.add(member.name)
        tar.extractall(fresh, filter="data")
    contents = fresh / archive.name.removesuffix(".tar.gz")
    compatibility = json.loads((contents / "compatibility.json").read_text())
    artifacts.validate_version(compatibility.get("version"))
    name = compatibility["binary"]
    if name != "enumeray" or compatibility.get("capabilities") != ["cli", "connector", "compute_runner"]:
        raise SystemExit("Unknown binary identity")
    if (compatibility["target"] != args.target or compatibility["requires_database"]
            or compatibility["package"] != "enumeray-cli"
            or compatibility["api"] != "v1" or compatibility["connector_protocol"] != 1
            or compatibility["wrapper_protocols"] != [1, 2]
            or not compatibility["requires_openssh_for_ssh"]
            or archive.name != f"enumeray-{compatibility['version']}-{args.target}.tar.gz"):
        raise SystemExit("Package compatibility contract mismatch")
    binary = contents / name
    minimum = artifacts.MINIMUM_PLATFORMS.get(args.target)
    if minimum is None or compatibility.get('minimum_platform') != minimum:
        raise SystemExit('Package lacks the supported minimum-platform contract')
    if not (contents / 'LICENSE').is_file() or not (contents / 'THIRD_PARTY_NOTICES.json').is_file():
        raise SystemExit('Package lacks project or dependency license notices')
    notices = json.loads((contents / 'THIRD_PARTY_NOTICES.json').read_text())
    source = compatibility.get('source', {})
    if notices.get('schema') != 1 or (source.get('clean') and not notices.get('dependencies')):
        raise SystemExit('Package dependency license notices are incomplete')
    try:
        platform_report = release_platform.audit_platform(binary, args.target, minimum)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from error
    if args.require_minimum_platform and not platform_report['platform_qualified']:
        raise SystemExit('This host does not provide native execution on the supported minimum platform')
    result.update(archive=archive.name, sha256=expected, version=compatibility['version'],
                  source=source, minimum_platform=minimum, **platform_report)
    executables = sorted(p.name for p in contents.iterdir() if p.is_file() and p.stat().st_mode & 0o111)
    if executables != ["enumeray"]:
        raise SystemExit("Expected exactly one installed executable")
    guides = {"INSTALL.md", "SETUP_GUIDE.md", "AUTHOR_GUIDE.md", "ANALYSIS_GUIDE.md",
              "CONNECTOR_GUIDE.md", "EXTENDING.md", "examples/README.md"}
    guides.update(path.relative_to(contents).as_posix() for path in contents.rglob('*.md'))
    check_guide_links(contents, sorted(guides))
    subprocess.run([str(binary), "--help"], cwd=fresh, env=environment,
                   check=True, stdout=subprocess.DEVNULL)
    for command in (["connector", "--help"], ["connector", "run", "--help"],
                    ["compute-runner", "--help"], ["local-worker", "--help"],
                    ["transfer-worker", "--help"]):
        subprocess.run([str(binary), *command], cwd=fresh, env=environment,
                       check=True, stdout=subprocess.DEVNULL)
    sentinel = "archive-private-argument-must-not-be-reflected"
    rejected = subprocess.run([str(binary), "compute-runner", sentinel, "--directory", str(fresh)],
                              cwd=fresh, env=environment, capture_output=True, text=True)
    if rejected.returncode != 2 or sentinel in rejected.stdout + rejected.stderr:
        raise SystemExit("Runner argument diagnostics exposed private input")
    version = subprocess.check_output([str(binary), "--version"], cwd=fresh, env=environment, text=True).strip()
    if version != f"enumeray {compatibility['version']}":
        raise SystemExit("Binary and package versions differ")
    journal = fresh / "qualification-journal"
    initialized = json.loads(subprocess.check_output([
        str(binary), "connector", "init", "--journal", str(journal),
        "--api-base-url", "http://127.0.0.1:8080", "--connector-id", "archive-qualification",
    ], cwd=fresh, env=environment))
    inspected = json.loads(subprocess.check_output([
        str(binary), "connector", "inspect", "--journal", str(journal),
    ], cwd=fresh, env=environment))
    if initialized["journal_id"] != inspected["journal_id"]:
        raise SystemExit("Archive host commands changed journal identity")
    subprocess.run([str(binary), "--json", "model", "validate", "--file",
                    str(contents / "examples" / "scalar-publication.json")],
                   cwd=fresh, env=environment, check=True, stdout=subprocess.DEVNULL)
    if not (contents / "AUTHOR_GUIDE.md").is_file():
        raise SystemExit("CLI archive lacks the author guide")
    for kind in ("python", "executable"):
        for io in ("json", "files"):
            bundled = contents / "starter-kit" / f"{kind}-{io}"
            check_guide_links(bundled, ("README.md", "AUTHOR_GUIDE.md", "ANALYSIS_GUIDE.md"))
            readme = (bundled / "README.md").read_text()
            model_id = json.loads((bundled / "publication.json").read_text())["model"]["id"]
            if f"model create {model_id} --file publication.json" not in readme or "{{" in readme:
                raise SystemExit("Bundled starter instructions do not match its model")
            if ("--file dataset=fixtures/dataset.csv" in readme) != (io == "files"):
                raise SystemExit("Bundled starter instructions do not match its I/O")
            if (bundled / "ANALYSIS_GUIDE.md").read_bytes() != (contents / "ANALYSIS_GUIDE.md").read_bytes():
                raise SystemExit("Bundled analysis guides differ")
            subprocess.run([sys.executable, "-B", "-m", "unittest", "discover",
                            "-s", ".", "-p", "test_starter.py"],
                           cwd=bundled, env={**environment, "PYTHONDONTWRITEBYTECODE": "1"}, check=True)
            for driver in ("process", "slurm"):
                project = fresh / f"{kind}-{io}-{driver}"
                subprocess.run([str(binary), "model", "init", "qualification",
                                "--directory", str(project), "--kind", kind,
                                "--io", io, "--driver", driver], cwd=fresh,
                               env=environment, check=True, stdout=subprocess.DEVNULL)
                check_guide_links(project, ("README.md", "AUTHOR_GUIDE.md", "ANALYSIS_GUIDE.md"))
                publication_file = project / "publication.json"
                publication = json.loads(publication_file.read_text())
                publication["execution"].update(context_id="qualification", release_path=str(project))
                publication_file.write_text(json.dumps(publication))
                subprocess.run([str(binary), "model", "validate", "--file", str(project / "publication.json")],
                               cwd=fresh, env=environment, check=True, stdout=subprocess.DEVNULL)
                subprocess.run([sys.executable, "-B", "-m", "unittest", "discover",
                                "-s", ".", "-p", "test_starter.py"], cwd=project,
                               env={**environment, "PYTHONDONTWRITEBYTECODE": "1"}, check=True)
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        artifacts.write_json(args.report, result)
    # Keep the extracted path in stdout for local service harnesses, not in signed metadata.
    result["binary"] = str(binary)
    result["extraction_root"] = str(fresh)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
