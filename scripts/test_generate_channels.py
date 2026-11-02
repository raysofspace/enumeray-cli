"""Qualified-source channel rendering and the documented installer-free path."""
import copy
import io
import tarfile
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


def manifest_fixture(version="0.1.0", status="released"):
    files, targets = [], {}
    for index, (target, minimum) in enumerate(channels.TARGETS.items()):
        name = "enumeray-%s-%s.tar.gz" % (version, target)
        files.append({"name": name, "sha256": str(index + 1) * 64, "kind": "binary", "target": target})
        targets[target] = {**minimum, "archive": name, "qualification": "qualification-" + target + ".json"}
    files.append({"name": "enumeray-" + version + "-source.tar.gz", "sha256": "5" * 64, "kind": "source"})
    return {"schema_version": 1, "version": version, "tag": "v" + version,
            "public_repository": channels.PUBLIC_REPOSITORY, "public_revision": "a" * 40,
            "source_tree": "b" * 40, "lockfile_sha256": "c" * 64,
            "rust_toolchain": "1.98.1",
            "builder_workflow_revision": "d" * 40,
            "baseline": {"id": "persistent-v1", "sha256": "e" * 64, "status": status},
            "files": files, "targets": targets}


def config_fixture():
    config = json.loads((ROOT / "distribution/channels.json").read_text())
    return config


class GenerateChannelTests(unittest.TestCase):
    def test_alpha_formula_uses_exact_prerelease_and_candidate_identity(self):
        manifest = manifest_fixture('0.1.0-alpha.1', 'candidate')
        artifacts, plan = channels.generate(manifest, config_fixture())
        formula = artifacts['homebrew/Formula/enumeray.rb'].decode()
        self.assertIn('version "0.1.0-alpha.1"', formula)
        self.assertIn('/releases/download/v0.1.0-alpha.1/', formula)
        self.assertEqual(plan['release']['baseline']['status'], 'candidate')
        self.assertEqual(set(plan['channels']), {'homebrew'})
        self.assertEqual(list(artifacts), ['homebrew/Formula/enumeray.rb'])

    def test_removed_channels_and_unsafe_channel_configuration_are_rejected(self):
        for config in ({**config_fixture(), 'conda': {}},
                       {**config_fixture(), 'homebrew': {'repository': 'owner/repo\nother'}},
                       {**config_fixture(), 'homebrew': {'base_branch': '../../escape'}}):
            with self.subTest(config=config), self.assertRaises(ValueError):
                channels.generate(manifest_fixture(), config)
        with self.assertRaises(ValueError):
            channels.generate(manifest_fixture(), config_fixture(), channel='conda')

    def test_formula_selects_verified_mac_archives_and_starts_no_service(self):
        artifacts, plan = channels.generate(manifest_fixture(), config_fixture())
        formula = artifacts["homebrew/Formula/enumeray.rb"].decode()
        for target in ("aarch64-apple-darwin", "x86_64-apple-darwin"):
            self.assertIn("enumeray-0.1.0-" + target + ".tar.gz", formula)
        self.assertIn("/releases/download/v0.1.0/", formula)
        self.assertNotIn("unknown-linux", formula)
        self.assertIn("depends_on macos: :sequoia", formula)
        self.assertNotIn("service do", formula)
        self.assertIn("journal_id", formula)
        self.assertEqual(plan["channels"]["homebrew"]["state"], "ready")
        if subprocess.run(["sh", "-c", "command -v ruby"], capture_output=True).returncode == 0:
            result = subprocess.run(["ruby", "-c"], input=formula, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)



    def test_wrong_source_target_version_or_unsafe_asset_is_rejected(self):
        for mutate in (
            lambda item: item.update(tag="v0.2.0"),
            lambda item: item.update(tag="cli-v0.1.0"),
            lambda item: item.update(tag="binaries-v0.1.0"),
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


class DocumentedAcquisitionTests(unittest.TestCase):
    def recipe(self, marker):
        guide = (ROOT / "crates/cli/INSTALL.md").read_text()
        return re.search(r"<!-- " + marker + r" -->\n```sh\n(.*?)\n```", guide, re.DOTALL)[1]

    def test_unselected_source_revision_stops_before_any_network_or_build_command(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            tools = directory / 'tools'
            tools.mkdir()
            calls = directory / 'calls'
            for name in ('git', 'rustup', 'cargo'):
                executable = tools / name
                executable.write_text('#!/bin/sh\nprintf "%s\\n" invoked >> "$CALLS"\nexit 99\n')
                executable.chmod(0o755)
            result = subprocess.run(['sh', '-c', self.recipe('pinned-source-acquisition')], cwd=directory,
                                    env={**os.environ, 'PATH': str(tools) + os.pathsep + os.environ['PATH'],
                                         'CALLS': str(calls)}, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('approved full public commit', result.stderr)
            self.assertFalse(calls.exists())
            self.assertFalse((directory / 'enumeray-cli-evaluation').exists())

    def test_failed_release_manifest_signature_stops_before_parsing_reports_or_checking(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            tools = directory / 'tools'
            tools.mkdir()
            calls = directory / 'calls'
            curl = tools / 'curl'
            curl.write_text('#!/bin/sh\nfor argument do last=$argument; done\nprintf "curl %s\\n" "$last" >> "$CALLS"\nexit 0\n')
            cosign = tools / 'cosign'
            cosign.write_text('#!/bin/sh\nprintf "cosign\\n" >> "$CALLS"\nexit 7\n')
            for name in ('jq', 'enumeray'):
                executable = tools / name
                executable.write_text('#!/bin/sh\nprintf "' + name + '\\n" >> "$CALLS"\nexit 99\n')
            for executable in tools.iterdir():
                executable.chmod(0o755)
            version = '0.1.0-alpha.1'
            tag = 'v' + version
            base = 'https://github.com/raysofspace/enumeray-cli/releases/download/' + tag
            result = subprocess.run(['sh', '-c', self.recipe('release-evidence-acquisition')], cwd=directory,
                                    env={**os.environ, 'PATH': str(tools) + os.pathsep + os.environ['PATH'],
                                         'CALLS': str(calls), 'VERSION': version, 'TAG': tag,
                                         'REPOSITORY': 'raysofspace/enumeray-cli', 'BASE_URL': base,
                                         'EXTRACTED': str(tools), 'API_ORIGIN': 'https://service.example.invalid'},
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 7, result.stderr)
            self.assertEqual(calls.read_text().splitlines(), [
                'curl ' + base + '/' + name for name in (
                    'release-manifest.json', 'release-manifest.json.sigstore.json',
                    'service-publish.json', 'service-publish.sigstore.json')
            ] + ['cosign'])
            self.assertFalse((directory / ('release-evidence-' + tag) / 'installation-receipt.json').exists())

    def test_archive_signature_or_checksum_failure_never_extracts_untrusted_bytes(self):
        for signature_failure, tamper in ((True, False), (False, True), (False, False)):
            with self.subTest(signature_failure=signature_failure, tamper=tamper), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                archive = directory / 'enumeray-fixture.tar.gz'
                with tarfile.open(archive, 'w:gz') as stream:
                    info = tarfile.TarInfo('extracted/enumeray')
                    data = b'fixture binary bytes'
                    info.size = len(data)
                    stream.addfile(info, io.BytesIO(data))
                checksum = archive.with_name(archive.name + '.sha256')
                checksum.write_text(hashlib.sha256(archive.read_bytes()).hexdigest() + '  ' + archive.name + '\n')
                if tamper:
                    archive.write_bytes(archive.read_bytes() + b'changed after signing')
                tools = directory / 'tools'
                tools.mkdir()
                cosign = tools / 'cosign'
                cosign.write_text('#!/bin/sh\nexit ' + ('7' if signature_failure else '0') + '\n')
                cosign.chmod(0o755)
                result = subprocess.run(['sh', '-c', self.recipe('archive-verification')], cwd=directory,
                                        env={**os.environ, 'PATH': str(tools) + os.pathsep + os.environ['PATH'],
                                             'ARCHIVE': archive.name, 'CHECKSUM': checksum.name,
                                             'REPOSITORY': 'raysofspace/enumeray-cli'}, capture_output=True, text=True)
                self.assertEqual(result.returncode == 0, not signature_failure and not tamper, result.stderr)
                self.assertEqual((directory / 'extracted/enumeray').exists(), not signature_failure and not tamper)

    def test_interrupted_staged_copy_preserves_previous_executable_and_private_state(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            source = directory / 'source'
            source.mkdir()
            (source / 'enumeray').write_text('#!/bin/sh\nexit 0\n')
            prefix = directory / 'prefix'
            (prefix / 'bin').mkdir(parents=True)
            original = prefix / 'bin/enumeray'
            original.write_bytes(b'previous installed executable')
            private = directory / 'journal'
            private.write_bytes(b'original private durable evidence')
            tools = directory / 'tools'
            tools.mkdir()
            install = tools / 'install'
            install.write_text('#!/bin/sh\nfor destination do :; done\nprintf partial > "$destination"\nkill -TERM "$PPID"\nexit 143\n')
            install.chmod(0o755)
            result = subprocess.run(['sh', '-c', self.recipe('manual-install-copy')],
                                    env={**os.environ, 'PATH': str(tools) + os.pathsep + os.environ['PATH'],
                                         'EXTRACTED': str(source), 'PREFIX': str(prefix)}, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(original.read_bytes(), b'previous installed executable')
            self.assertEqual(private.read_bytes(), b'original private durable evidence')
            self.assertEqual(list((prefix / 'bin').glob('.enumeray.*')), [])


if __name__ == "__main__":
    unittest.main()
