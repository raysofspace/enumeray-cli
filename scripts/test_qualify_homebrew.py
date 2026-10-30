"""Native Homebrew lifecycle qualification stays in its temporary namespace."""
import importlib.util
import io
import json
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("qualify_homebrew", ROOT / "scripts/qualify-homebrew.py")
qualification = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qualification)


class FakeHomebrew:
    def __init__(self, root, fail=None):
        self.root = root
        self.repository = root / "brew-repository"
        self.cellar = root / "Cellar"
        self.prefix = root / "brew-prefix"
        for directory in (self.repository, self.cellar, self.prefix / "bin"):
            directory.mkdir(parents=True)
        self.real_binary = self.prefix / "bin/enumeray"
        self.real_binary.write_bytes(b"existing real installation must remain untouched")
        self.calls = []
        self.fail = fail
        self.revision = 0
        self.tap = self.rack = self.formula = None

    def result(self, command, stdout="", code=0):
        return subprocess.CompletedProcess(command, code, stdout=stdout, stderr="private provider diagnostic" if code else "")

    def __call__(self, command, **kwargs):
        self.calls.append((command, kwargs))
        if command[0].endswith("/brew"):
            action = command[1]
            if action == "--repository":
                return self.result(command, str(self.repository))
            if action == "--cellar":
                return self.result(command, str(self.cellar))
            if action == "--prefix" and len(command) == 2:
                return self.result(command, str(self.prefix))
            if action == "tap-new":
                owner, token = command[-1].split("/")
                self.tap = self.repository / "Library/Taps" / owner / ("homebrew-" + token)
                (self.tap / "Formula").mkdir(parents=True)
                return self.result(command)
            if action in ("install", "upgrade"):
                self.formula = command[-1]
                short = self.formula.rsplit("/", 1)[1]
                if not short.startswith("enumeray-qualification-"):
                    raise AssertionError("Touched an unscoped formula")
                self.rack = self.cellar / short
                self.revision = 1 if action == "upgrade" else 0
                keg = self.rack / ("0.1.0_1" if self.revision else "0.1.0")
                (keg / "bin").mkdir(parents=True)
                (keg / "bin/enumeray").write_bytes(b"wrong binary" if self.fail == "wrong-binary" else b"exact candidate executable")
                if action == "upgrade" and self.fail == "state-change":
                    (Path(kwargs["env"]["HOME"]) / ".config/enumeray/credentials").write_bytes(b"changed")
                definition = (self.tap / "Formula" / (short + ".rb")).read_text()
                if 'keg_only "Temporary native package qualification"' not in definition:
                    raise AssertionError("Qualification formula could link the global binary")
                if "file://" not in definition or "https://github.com/" in definition:
                    raise AssertionError("Qualification attempted a remote release download")
                if self.revision and "  revision 1\n" not in definition:
                    raise AssertionError("Upgrade did not select a new formula revision")
                return self.result(command, code=2 if self.fail == action else 0)
            if action == "--prefix":
                return self.result(command, str(self.rack / ("0.1.0_1" if self.revision else "0.1.0")))
            if action == "test":
                return self.result(command, code=2 if self.fail == "test" else 0)
            if action == "uninstall":
                if command[-1] != self.formula:
                    raise AssertionError("Uninstall escaped its scoped formula")
                if self.fail == "uninstall":
                    return self.result(command, code=2)
                shutil.rmtree(self.rack)
                return self.result(command)
            if action == "untap":
                if command[-1] != self.formula.rsplit("/", 1)[0]:
                    raise AssertionError("Untap escaped its scoped namespace")
                shutil.rmtree(self.tap)
                return self.result(command)
            raise AssertionError("Unexpected Homebrew command " + str(command))
        if command[0].endswith("/enumeray"):
            if command[1:] == ["--version"]:
                return self.result(command, "enumeray 0.1.0\n")
            journal = Path(command[command.index("--journal") + 1])
            if command[1:3] == ["connector", "init"]:
                journal.mkdir()
                (journal / "record.json").write_text('{"journal_id":"original-journal"}\n')
            elif command[1:3] != ["connector", "inspect"]:
                raise AssertionError("Unexpected candidate action")
            return self.result(command, (journal / "record.json").read_text())
        raise AssertionError("Unexpected executable " + str(command))


class HomebrewQualificationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.archive = self.root / "enumeray-0.1.0-aarch64-apple-darwin.tar.gz"
        with tarfile.open(self.archive, "w:gz") as archive:
            member = tarfile.TarInfo(self.archive.name.removesuffix(".tar.gz") + "/enumeray")
            contents = b"exact candidate executable"
            member.size = len(contents)
            member.mode = 0o755
            archive.addfile(member, io.BytesIO(contents))
        self.fake = FakeHomebrew(self.root)

    def qualify(self, **kwargs):
        with mock.patch.object(qualification.shutil, "which", return_value="/test/bin/brew"):
            return qualification.qualify(self.archive, "aarch64-apple-darwin", "0.1.0",
                                         run=self.fake, check_platform=lambda target: None,
                                         token="a" * 32, environment={"PATH": "/usr/bin:/bin",
                                         "GH_TOKEN": "must-not-reach-candidate", "ENUMERAY_TOKEN": "private"}, **kwargs)

    def test_install_revision_upgrade_formula_tests_and_uninstall_use_only_scoped_kegs(self):
        original = self.fake.real_binary.read_bytes()
        report = self.qualify()
        self.assertEqual(report["state"], "passed")
        self.assertEqual(report["sha256"], qualification.sha256(self.archive))
        self.assertEqual(report["binary_sha256"], qualification.archive_binary_sha256(self.archive))
        self.assertIn("macos", report["runtime"])
        self.assertEqual(len(report["lifecycle"]), 3)
        self.assertIn("installation mechanics only", report["upgrade_evidence"])
        self.assertEqual(sum(call[0][1] == "test" for call in self.fake.calls), 2)
        self.assertFalse(self.fake.rack.exists())
        self.assertFalse(self.fake.tap.exists())
        self.assertEqual(self.fake.real_binary.read_bytes(), original)
        for command, kwargs in self.fake.calls:
            env = kwargs["env"]
            self.assertNotIn("GH_TOKEN", env)
            self.assertNotIn("ENUMERAY_TOKEN", env)
            self.assertEqual(env["HOMEBREW_NO_AUTO_UPDATE"], "1")
            self.assertEqual(env["HOMEBREW_NO_INSTALL_CLEANUP"], "1")
            self.assertEqual(env["HOMEBREW_NO_INSTALLED_DEPENDENTS_CHECK"], "1")
            self.assertNotIn("services", command)
            self.assertNotIn("run", command)
        install = next(command for command, _ in self.fake.calls if command[1] == "install")
        self.assertIn("--skip-link", install)

    def test_partial_install_or_upgrade_or_formula_test_failure_cleans_up_exact_namespace(self):
        for point in ("install", "upgrade", "test"):
            with self.subTest(point=point), tempfile.TemporaryDirectory() as temporary:
                self.fake = FakeHomebrew(Path(temporary), fail=point)
                original = self.fake.real_binary.read_bytes()
                with self.assertRaisesRegex(ValueError, "command failed"):
                    self.qualify()
                self.assertFalse(self.fake.rack.exists())
                self.assertFalse(self.fake.tap.exists())
                self.assertEqual(self.fake.real_binary.read_bytes(), original)

    def test_cleanup_failure_remains_visible_and_still_attempts_tap_cleanup(self):
        self.fake.fail = "uninstall"
        with self.assertRaisesRegex(ValueError, "cleanup failed"):
            self.qualify()
        self.assertTrue(self.fake.rack.exists())
        self.assertFalse(self.fake.tap.exists())
        self.assertTrue(any(command[1] == "untap" for command, _ in self.fake.calls))

    def test_wrong_binary_bytes_cannot_pass_on_version_reporting_alone(self):
        self.fake.fail = "wrong-binary"
        with self.assertRaisesRegex(ValueError, "exact candidate executable"):
            self.qualify()
        self.assertFalse(self.fake.rack.exists())
        self.assertFalse(self.fake.tap.exists())

    def test_changed_private_state_fails_qualification_and_cleans_kegs(self):
        self.fake.fail = "state-change"
        with self.assertRaisesRegex(ValueError, "changed private configuration"):
            self.qualify()
        self.assertFalse(self.fake.rack.exists())
        self.assertFalse(self.fake.tap.exists())

    def test_existing_namespace_is_rejected_before_any_mutation(self):
        rack = self.fake.cellar / ("enumeray-qualification-" + "a" * 32)
        rack.mkdir()
        (rack / "existing-marker").write_bytes(b"must remain")
        with self.assertRaisesRegex(ValueError, "already exists"):
            self.qualify()
        self.assertEqual((rack / "existing-marker").read_bytes(), b"must remain")
        self.assertTrue(all(command[1].startswith("--") for command, _ in self.fake.calls))

    def test_minimum_platform_refusal_happens_before_homebrew_is_touched(self):
        with mock.patch.object(qualification.platform, "system", return_value="Darwin"), \
             mock.patch.object(qualification.platform, "machine", return_value="arm64"), \
             mock.patch.object(qualification.platform, "mac_ver", return_value=("26.0", (), "")):
            with self.assertRaisesRegex(ValueError, "actual macOS 15"):
                qualification.qualify(self.archive, "aarch64-apple-darwin", "0.1.0", run=self.fake)
        self.assertEqual(self.fake.calls, [])

    def test_archive_identity_mismatch_fails_before_commands(self):
        with self.assertRaisesRegex(ValueError, "exact native candidate"):
            qualification.qualify(self.archive, "aarch64-apple-darwin", "0.2.0", run=self.fake)
        self.assertEqual(self.fake.calls, [])

    def test_rosetta_is_rejected_before_homebrew_is_touched(self):
        archive = self.root / "enumeray-0.1.0-x86_64-apple-darwin.tar.gz"
        archive.write_bytes(self.archive.read_bytes())
        with mock.patch.object(qualification.platform, "system", return_value="Darwin"), \
             mock.patch.object(qualification.platform, "machine", return_value="x86_64"), \
             mock.patch.object(qualification.platform, "mac_ver", return_value=("15.8", (), "")), \
             mock.patch.object(qualification.subprocess, "check_output", return_value="1\n") as sysctl:
            with self.assertRaisesRegex(ValueError, "Rosetta"):
                qualification.qualify(archive, "x86_64-apple-darwin", "0.1.0", run=self.fake)
        sysctl.assert_called_once_with(["sysctl", "-n", "sysctl.proc_translated"],
                                       text=True, stderr=subprocess.PIPE)
        self.assertEqual(self.fake.calls, [])

    def test_command_failure_retains_bounded_diagnostics_without_credentials_or_terminal_controls(self):
        result = subprocess.CompletedProcess([], 2, stdout="useful output", stderr=(
            "x" * 6000 + "\x1b[31mError: failed to install; GH_TOKEN=sensitive-value\x1b[0m\n"))
        details = qualification.failure_diagnostics(result, {"GH_TOKEN": "sensitive-value"})
        self.assertIn("Error: failed to install", details)
        self.assertIn("useful output", details)
        self.assertNotIn("sensitive-value", details)
        self.assertNotIn("\x1b", details)
        self.assertLessEqual(len(details), 4050)

    def test_install_failure_diagnostics_reach_the_operator_after_cleanup(self):
        self.fake.fail = "install"
        with self.assertRaisesRegex(ValueError, "exit 2") as failure:
            self.qualify()
        self.assertIn("provider diagnostic", str(failure.exception))
        self.assertFalse(self.fake.rack.exists())
        self.assertFalse(self.fake.tap.exists())


if __name__ == "__main__":
    unittest.main()
