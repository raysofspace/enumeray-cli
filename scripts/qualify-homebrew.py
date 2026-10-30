#!/usr/bin/env python3
"""Exercise a native candidate in a unique, keg-only temporary Homebrew formula.

Only macOS 15 on the archive's native CPU establishes this channel qualification.
The upgrade changes the temporary formula revision while retaining the exact
candidate bytes; it proves packaging mechanics, not cross-release state safety.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile
import uuid


ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("generate_channels", ROOT / "scripts/generate-channels.py")
generation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(generation)
platform_spec = importlib.util.spec_from_file_location("release_platform", ROOT / "scripts/release_platform.py")
platform_audit = importlib.util.module_from_spec(platform_spec)
platform_spec.loader.exec_module(platform_audit)


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def native_platform(target):
    machines = {"aarch64-apple-darwin": "arm64", "x86_64-apple-darwin": "x86_64"}
    if (target not in machines or platform.system() != "Darwin"
            or platform.machine() != machines[target] or platform.mac_ver()[0].split(".")[0] != "15"):
        raise ValueError("Homebrew qualification requires actual macOS 15 on the candidate's native CPU")
    platform_audit.require_untranslated_macos(platform.machine(), run=subprocess.check_output)


def snapshot_tree(root):
    return {str(path.relative_to(root)): sha256(path)
            for path in sorted(Path(root).rglob("*")) if path.is_file()}


def snapshot_file(path):
    path = Path(path)
    if path.is_symlink():
        return {"symlink": os.readlink(path), "sha256": sha256(path) if path.is_file() else None}
    return {"sha256": sha256(path)} if path.is_file() else None


def archive_binary_sha256(archive):
    member_name = archive.name.removesuffix(".tar.gz") + "/enumeray"
    with tarfile.open(archive, "r:gz") as contents:
        matches = [member for member in contents if member.name == member_name]
        if len(matches) != 1 or not matches[0].isfile():
            raise ValueError("Candidate archive must contain exactly one regular unified executable")
        digest = hashlib.sha256()
        with contents.extractfile(matches[0]) as binary:
            for block in iter(lambda: binary.read(1024 * 1024), b""):
                digest.update(block)
        return digest.hexdigest()


def failure_diagnostics(result, inherited=None, limit=4000):
    """Retain useful tool errors without terminal controls or credential values."""
    parts = []
    for name in ("stderr", "stdout"):
        value = getattr(result, name, "") or ""
        if isinstance(value, bytes):
            value = value.decode("utf-8", errors="replace")
        if value.strip():
            parts.append(name + ": " + value.strip())
    text = "\n".join(parts)
    for key, value in (inherited if inherited is not None else os.environ).items():
        if (isinstance(value, str) and len(value) >= 4
                and re.search(r"TOKEN|PASSWORD|SECRET|CREDENTIAL|PRIVATE_KEY|AUTH|API_KEY", key, re.I)):
            text = text.replace(value, "[redacted]")
    text = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", text)
    text = re.sub(r"[\x00-\x08\x0b-\x1f\x7f]", "", text)
    return ("[earlier output omitted]\n" + text[-limit:]) if len(text) > limit else text


def isolated_environment(temporary, environment=None):
    inherited = environment if environment is not None else os.environ
    allowed = ("PATH", "USER", "LOGNAME", "SHELL", "LANG", "LC_ALL", "DEVELOPER_DIR", "SDKROOT")
    result = {name: inherited[name] for name in allowed if name in inherited}
    for name, child in (("HOME", "home"), ("HOMEBREW_CACHE", "cache"),
                        ("HOMEBREW_LOGS", "logs"), ("HOMEBREW_TEMP", "tmp"), ("TMPDIR", "tmp")):
        directory = Path(temporary) / child
        directory.mkdir(parents=True, exist_ok=True)
        result[name] = str(directory)
    result.update({name: "1" for name in (
        "HOMEBREW_NO_AUTO_UPDATE", "HOMEBREW_NO_ANALYTICS", "HOMEBREW_NO_INSTALL_FROM_API",
        "HOMEBREW_NO_INSTALL_CLEANUP", "HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK",
        "HOMEBREW_NO_INSTALL_UPGRADE", "HOMEBREW_NO_ENV_HINTS", "HOMEBREW_NO_ASK",
    )})
    return result


def qualify(archive, target, version, run=subprocess.run, check_platform=native_platform,
            root=ROOT, token=None, environment=None):
    generation.artifacts_policy.validate_version(version)
    if Path(archive).is_symlink():
        raise ValueError("Candidate archive must be a regular file")
    archive = Path(archive).resolve()
    if (archive.name != "enumeray-%s-%s.tar.gz" % (version, target)
            or not archive.is_file() or archive.is_symlink()):
        raise ValueError("Expected the exact native candidate archive and version")
    check_platform(target)
    binary_digest = archive_binary_sha256(archive)
    token = token or uuid.uuid4().hex
    if not re.fullmatch(r"[a-f0-9]{32}", token):
        raise ValueError("Invalid qualification namespace")
    formula_name = "enumeray-qualification-" + token
    tap_name = "enumeray-qualification/" + token
    formula = tap_name + "/" + formula_name
    class_name = "EnumerayQualification" + token.capitalize()
    report = {"schema_version": 1, "channel": "homebrew", "target": target,
              "version": version, "archive": archive.name, "sha256": sha256(archive),
              "state": "failed", "lifecycle": [],
              "binary_sha256": binary_digest,
              "runtime": {"system": platform.system(), "macos": platform.mac_ver()[0],
                          "machine": platform.machine()},
              "minimum_platform_qualified": platform.system() == "Darwin"
                  and platform.mac_ver()[0].split(".")[0] == "15"
                  and platform.machine() == ("arm64" if target == "aarch64-apple-darwin" else "x86_64"),
              "upgrade_evidence": "same-candidate-formula-revision; installation mechanics only"}
    with tempfile.TemporaryDirectory(prefix="enumeray-homebrew-") as temporary:
        env = isolated_environment(temporary, environment)
        cleanup_diagnostics = []
        brew = shutil.which("brew", path=env.get("PATH"))
        if brew is None:
            raise ValueError("Homebrew must be available on the native qualification host")

        def command(arguments, capture=False, cleanup=False):
            try:
                result = run(arguments, env=env, text=True, capture_output=True, check=False)
            except OSError as error:
                if cleanup:
                    cleanup_diagnostics.append(arguments[1] + ": " + str(error.strerror))
                    return 1
                raise ValueError("Cannot execute the scoped Homebrew qualification command: " + str(error.strerror)) from None
            if result.returncode:
                details = (arguments[1] + " (exit " + str(result.returncode) + ")\n"
                           + failure_diagnostics(result, environment))
                if cleanup:
                    cleanup_diagnostics.append(details)
                else:
                    raise ValueError("Homebrew qualification command failed: " + details)
            return result.stdout.strip() if capture else result.returncode

        repository = Path(command([brew, "--repository"], capture=True)).resolve()
        cellar = Path(command([brew, "--cellar"], capture=True)).resolve()
        prefix = Path(command([brew, "--prefix"], capture=True)).resolve()
        tap = repository / "Library/Taps/enumeray-qualification" / ("homebrew-" + token)
        rack = cellar / formula_name
        real_binary = prefix / "bin/enumeray"
        real_before = snapshot_file(real_binary)
        if tap.exists() or rack.exists():
            raise ValueError("Temporary qualification namespace already exists; nothing was changed")
        template = generation.render(root / "distribution/homebrew/enumeray.rb.in", {
            "VERSION": version, "ARM_URL": archive.as_uri(), "ARM_SHA256": report["sha256"],
            "INTEL_URL": archive.as_uri(), "INTEL_SHA256": report["sha256"],
        }).decode()
        template = template.replace("class Enumeray < Formula", "class " + class_name + " < Formula", 1)
        template = template.replace("  def install\n", '  keg_only "Temporary native package qualification"\n\n  def install\n', 1)
        tap_attempted = install_attempted = False
        failure = None
        cleanup_failures = []
        try:
            tap_attempted = True
            command([brew, "tap-new", "--no-git", tap_name])
            definition = tap / "Formula" / (formula_name + ".rb")
            if not definition.parent.is_dir() or definition.exists():
                raise ValueError("Homebrew did not create the expected isolated tap directory")
            definition.write_text(template)
            install_attempted = True
            command([brew, "install", "--formula", "--build-from-source", "--skip-link", formula])

            def installed_binary(revision):
                installed = Path(command([brew, "--prefix", formula], capture=True)).resolve()
                expected = rack / (version + ("_1" if revision else ""))
                if installed != expected or not installed.is_relative_to(rack):
                    raise ValueError("Homebrew package escaped its isolated qualification keg")
                binary = installed / "bin/enumeray"
                if command([str(binary), "--version"], capture=True) != "enumeray " + version:
                    raise ValueError("Homebrew installed the wrong executable version")
                if sha256(binary) != binary_digest:
                    raise ValueError("Homebrew did not retain the exact candidate executable bytes")
                if sha256(archive) != report["sha256"]:
                    raise ValueError("Candidate archive changed during Homebrew qualification")
                return binary

            binary = installed_binary(0)
            command([brew, "test", "--verbose", formula])
            report["lifecycle"].append("clean-install-and-formula-test")
            state = Path(env["HOME"]) / ".config/enumeray"
            state.mkdir(parents=True)
            for name in ("profiles.json", "credentials"):
                (state / name).write_text("private original qualification state\n")
            journal = state / "journal"
            initialized = json.loads(command([str(binary), "connector", "init", "--journal", str(journal),
                "--api-base-url", "http://127.0.0.1:8080", "--connector-id", "homebrew-qualification"], capture=True))
            before = snapshot_tree(state)

            definition.write_text(template.replace('  version "' + version + '"\n',
                '  version "' + version + '"\n  revision 1\n', 1))
            command([brew, "upgrade", "--formula", "--build-from-source", formula])
            binary = installed_binary(1)
            command([brew, "test", "--verbose", formula])
            inspected = json.loads(command([str(binary), "connector", "inspect", "--journal", str(journal)], capture=True))
            if initialized.get("journal_id") != inspected.get("journal_id") or snapshot_tree(state) != before:
                raise ValueError("Homebrew replacement changed private configuration, credentials or journal identity")
            report["lifecycle"].append("same-candidate-revision-upgrade-and-formula-test")
            command([brew, "uninstall", "--formula", "--force", formula])
            install_attempted = False
            if rack.exists() or snapshot_tree(state) != before:
                raise ValueError("Homebrew removal left a keg or changed private state")
            report["lifecycle"].append("uninstall-preserving-private-state")
        except (ValueError, OSError, KeyError, TypeError) as error:
            failure = error
        finally:
            if install_attempted and rack.exists() and command([brew, "uninstall", "--formula", "--force", formula], cleanup=True):
                cleanup_failures.append("temporary qualification keg")
            if tap_attempted and tap.exists() and command([brew, "untap", tap_name], cleanup=True):
                cleanup_failures.append("temporary qualification tap")
            if snapshot_file(real_binary) != real_before:
                cleanup_failures.append("the existing global enumeray executable changed")
            if rack.exists() or tap.exists():
                cleanup_failures.append("scoped Homebrew files remain")
        if cleanup_failures:
            details = "\n".join(cleanup_diagnostics)[-4000:]
            raise ValueError("Homebrew qualification cleanup failed: " + "; ".join(cleanup_failures)
                             + ("\n" + details if details else ""))
        if failure:
            raise failure
    report["state"] = "passed"
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--target", choices=("aarch64-apple-darwin", "x86_64-apple-darwin"), required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        report = qualify(args.archive, args.target, args.version)
    except (ValueError, OSError, tarfile.TarError, subprocess.SubprocessError) as error:
        parser.exit(1, str(error) + "\n")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()
