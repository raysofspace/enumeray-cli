"""Qualified-source channel rendering and the documented installer-free path."""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("generate_channels", ROOT / "scripts/generate-channels.py")
channels = importlib.util.module_from_spec(spec)
spec.loader.exec_module(channels)


def manifest_fixture():
    version = "0.1.0"
    files, targets = [], {}
    for index, (target, minimum) in enumerate(channels.TARGETS.items()):
        name = "enumeray-%s-%s.tar.gz" % (version, target)
        files.append({"name": name, "sha256": str(index + 1) * 64, "kind": "binary", "target": target})
        targets[target] = {**minimum, "archive": name, "qualification": "qualification-" + target + ".json"}
    files.append({"name": "enumeray-0.1.0-source.tar.gz", "sha256": "5" * 64, "kind": "source"})
    return {"schema_version": 1, "version": version, "tag": "binaries-v" + version,
            "public_repository": channels.PUBLIC_REPOSITORY, "public_revision": "a" * 40,
            "source_tree": "b" * 40, "lockfile_sha256": "c" * 64,
            "rust_toolchain": "1.98.1",
            "builder_workflow_revision": "d" * 40,
            "baseline": {"id": "persistent-v1", "sha256": "e" * 64},
            "files": files, "targets": targets}


def config_fixture():
    config = json.loads((ROOT / "distribution/channels.json").read_text())
    config["conda"].update(maintainers=["consenting-test-maintainer"], fork_repository="maintainer/staged-recipes")
    return config


class GenerateChannelTests(unittest.TestCase):
    def test_formula_selects_verified_mac_archives_and_starts_no_service(self):
        artifacts, plan = channels.generate(manifest_fixture(), config_fixture())
        formula = artifacts["homebrew/Formula/enumeray.rb"].decode()
        for target in ("aarch64-apple-darwin", "x86_64-apple-darwin"):
            self.assertIn("enumeray-0.1.0-" + target + ".tar.gz", formula)
        self.assertNotIn("unknown-linux", formula)
        self.assertIn("depends_on macos: :sonoma", formula)
        self.assertNotIn("service do", formula)
        self.assertIn("journal_id", formula)
        self.assertEqual(plan["channels"]["homebrew"]["state"], "ready")
        if subprocess.run(["sh", "-c", "command -v ruby"], capture_output=True).returncode == 0:
            result = subprocess.run(["ruby", "-c"], input=formula, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_conda_builds_pinned_source_and_retains_dependency_license_evidence(self):
        artifacts, plan = channels.generate(manifest_fixture(), config_fixture())
        recipe = artifacts["conda/recipe/recipe.yaml"].decode()
        build = artifacts["conda/recipe/build.sh"].decode()
        self.assertIn("enumeray-0.1.0-source.tar.gz", recipe)
        self.assertNotIn("apple-darwin.tar.gz", recipe)
        self.assertIn("rust ==1.98.1", recipe)
        self.assertIn("cargo-bundle-licenses", recipe)
        self.assertIn("cargo-auditable", recipe)
        self.assertIn("cargo auditable install --locked --offline --no-track", build)
        self.assertIn("--path crates/cli", build)
        self.assertIn("THIRDPARTY.yml", recipe)
        self.assertIn("THIRD_PARTY_NOTICES.json", recipe)
        self.assertIn('cmp "$RECIPE_DIR/upstream-source.json" release-source.json', build)
        self.assertNotIn("rustup", build.split("case", 1)[1])
        self.assertNotIn("python", recipe)
        self.assertEqual(json.loads(artifacts["conda/recipe/source.json"])["public_revision"], "a" * 40)
        self.assertEqual(plan["channels"]["conda"]["files"][0]["destination"].split("/")[:2],
                         ["recipes", "enumeray"])

    def test_missing_maintainers_blocks_only_conda_without_fabricating_an_identity(self):
        config = config_fixture()
        config["conda"]["maintainers"] = []
        artifacts, plan = channels.generate(manifest_fixture(), config)
        self.assertEqual(plan["channels"]["conda"]["state"], "blocked-configuration")
        self.assertEqual(plan["channels"]["homebrew"]["state"], "ready")
        self.assertEqual(list(artifacts), ["homebrew/Formula/enumeray.rb"])

    def test_wrong_source_target_version_or_unsafe_asset_is_rejected(self):
        for mutate in (
            lambda item: item.update(tag="binaries-v0.2.0"),
            lambda item: item.update(public_repository="other/project"),
            lambda item: item["targets"].pop("aarch64-apple-darwin"),
            lambda item: item["targets"]["x86_64-unknown-linux-gnu"].update(minimum_glibc="2.39"),
            lambda item: item["files"][0].update(name="../escape.tar.gz"),
            lambda item: item["files"].append(copy.deepcopy(item["files"][0])),
            lambda item: item.update(version="0.1.0-rc1"),
            lambda item: item.update(lockfile_sha256="wrong"),
            lambda item: item.update(rust_toolchain="stable"),
        ):
            with self.subTest(mutate=mutate):
                manifest = manifest_fixture()
                mutate(manifest)
                with self.assertRaises(ValueError):
                    channels.generate(manifest, config_fixture())

    def test_rust_compiler_pin_comes_from_signed_source_rather_than_current_checkout(self):
        manifest = manifest_fixture()
        manifest["rust_toolchain"] = "1.97.2"
        artifacts, _ = channels.generate(manifest, config_fixture())
        self.assertIn(b"rust ==1.97.2", artifacts["conda/recipe/recipe.yaml"])
        self.assertIn(b'"1.97.2"', artifacts["conda/recipe/conda_build_config.yaml"])
        self.assertIn(b"rustc 1.97.2 ", artifacts["conda/recipe/build.sh"])

    def test_generation_retries_are_identical_and_do_not_replace_existing_version(self):
        artifacts, plan = channels.generate(manifest_fixture(), config_fixture())
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            channels.write_delivery(output, artifacts, plan)
            before = {p.relative_to(output): p.read_bytes() for p in output.rglob("*") if p.is_file()}
            channels.write_delivery(output, artifacts, plan)
            self.assertEqual(before, {p.relative_to(output): p.read_bytes() for p in output.rglob("*") if p.is_file()})
            altered = {**artifacts, "homebrew/Formula/enumeray.rb": b"different version content"}
            with self.assertRaisesRegex(ValueError, "conflicting"):
                channels.write_delivery(output, altered, plan)
            self.assertEqual((output / "homebrew/Formula/enumeray.rb").read_bytes(),
                             artifacts["homebrew/Formula/enumeray.rb"])

    def test_recipe_destination_cannot_escape_or_inject_a_repository(self):
        for key, value in (("recipe_directory", "../../escape"), ("repository", "owner/repo\nother"),
                           ("maintainers", ["name\ncode"])):
            config = config_fixture()
            config["conda"][key] = value
            with self.assertRaises(ValueError):
                channels.generate(manifest_fixture(), config)


class DocumentedCopyTests(unittest.TestCase):
    def run_copy(self, root, version, exit_code=0):
        source = root / "verified extraction"
        source.mkdir(exist_ok=True)
        executable = source / "enumeray"
        executable.write_text("#!/bin/sh\nprintf 'enumeray " + version + "\\n'\nexit " + str(exit_code) + "\n")
        executable.chmod(0o755)
        guide = (ROOT / "crates/cli/INSTALL.md").read_text()
        script = re.search(r"<!-- manual-install-copy -->\n```sh\n(.*?)\n```", guide, re.DOTALL)[1]
        return subprocess.run(["sh", "-c", script], env={**os.environ, "HOME": str(root / "home"),
                              "PREFIX": str(root / "chosen prefix"), "EXTRACTED": str(source)},
                              capture_output=True, text=True)

    def test_clean_install_upgrade_and_removal_preserve_private_state(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            state = root / "home/.config/enumeray"
            state.mkdir(parents=True)
            sentinels = {name: (name + " private original bytes\n").encode()
                         for name in ("profiles.json", "credentials", "journal")}
            for name, content in sentinels.items():
                (state / name).write_bytes(content)
            for version in ("0.1.0", "0.2.0"):
                result = self.run_copy(root, version)
                self.assertEqual(result.returncode, 0, result.stderr)
                binary = root / "chosen prefix/bin/enumeray"
                self.assertEqual(subprocess.check_output([str(binary), "--version"], text=True).strip(),
                                 "enumeray " + version)
                self.assertEqual(list(binary.parent.glob(".enumeray.*")), [])
            binary.unlink()
            self.assertEqual({p.name: p.read_bytes() for p in state.iterdir()}, sentinels)

    def test_failed_staged_binary_does_not_replace_the_installed_executable(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.assertEqual(self.run_copy(root, "0.1.0").returncode, 0)
            result = self.run_copy(root, "broken", exit_code=7)
            self.assertEqual(result.returncode, 7)
            binary = root / "chosen prefix/bin/enumeray"
            self.assertEqual(subprocess.check_output([str(binary)], text=True).strip(), "enumeray 0.1.0")
            self.assertEqual(list(binary.parent.glob(".enumeray.*")), [])


class CondaBuildBoundaryTests(unittest.TestCase):
    def test_changed_lockfile_fails_before_cargo_or_dependency_network_access(self):
        artifacts, _ = channels.generate(manifest_fixture(), config_fixture())
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            tools = root / "tools"
            tools.mkdir()
            (tools / "rustc").write_text("#!/bin/sh\necho 'rustc 1.98.1 (test)'\n")
            trace = root / "cargo-was-called"
            (tools / "cargo").write_text("#!/bin/sh\ntouch '" + str(trace) + "'\n")
            for path in tools.iterdir():
                path.chmod(0o755)
            (root / "Cargo.lock").write_text("changed source lockfile")
            build = root / "build.sh"
            build.write_bytes(artifacts["conda/recipe/build.sh"])
            result = subprocess.run(["bash", str(build)], cwd=root,
                                    env={**os.environ, "PATH": str(tools) + ":/usr/bin:/bin"},
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(trace.exists())


if __name__ == "__main__":
    unittest.main()
