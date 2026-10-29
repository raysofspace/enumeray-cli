"""Explicit, cheap local demonstration checks for the editable author starter."""
from decimal import Decimal, localcontext
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock
import model
import protocol

RELEASE = Path(__file__).resolve().parent
SCIENTIFIC = json.loads((RELEASE / "publication.json").read_text())
FILES = "dataset" in SCIENTIFIC["input_schema"]["properties"]


class StarterTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.request = json.loads((RELEASE / "fixtures/request.json").read_text())
        self.request["model_id"] = SCIENTIFIC["model"]["id"]
        self.request["publication_id"] = "starter-publication"
        if FILES:
            (self.root / "inputs").mkdir()
            shutil.copyfile(RELEASE / "fixtures/dataset.csv", self.root / "inputs/dataset.csv")

    def execute(self, raw=None):
        path = self.root / "request.json"
        path.write_text(raw if raw is not None else json.dumps(self.request))
        return subprocess.run([sys.executable, str(RELEASE / "wrapper.py"), "execute",
                               "--request", str(path), "--result", str(self.root / "result.json")],
                              cwd=self.root, capture_output=True, text=True, timeout=10)

    def result(self):
        return protocol.read_json(self.root / "result.json")

    def test_expected_fixture_and_file_bytes(self):
        done = self.execute()
        self.assertEqual(done.returncode, 0, done.stderr)
        expected = protocol.read_json(RELEASE / "fixtures/expected-result.json")
        self.assertEqual(self.result(), expected)
        if FILES:
            self.assertEqual((self.root / "processed.csv").read_bytes(), (RELEASE / "fixtures/expected.csv").read_bytes())
        for key in protocol.IDENTITIES:
            self.assertEqual(self.result()[key], self.request[key])

    def test_exact_numbers_independent_of_decimal_context(self):
        with localcontext() as context:
            context.prec = 2
            self.assertEqual(model.double(Decimal("9007199254740993.123456789")), Decimal("18014398509481986.246913578"))
        if FILES:
            (self.root / "inputs/dataset.csv").write_text("index,value\n0,9007199254740993.123456789\n")
        else:
            self.request["inputs"] = {"value": "TOKEN"}
        raw = json.dumps(self.request).replace('"TOKEN"', "9007199254740993.123456789")
        done = self.execute(raw)
        self.assertEqual(done.returncode, 0, done.stderr)
        if FILES:
            self.assertIn(b"18014398509481986.246913578", (self.root / "processed.csv").read_bytes())
        else:
            self.assertEqual(self.result()["outputs"]["doubled"], Decimal("18014398509481986.246913578"))

    def test_malformed_envelopes_never_publish(self):
        original = dict(self.request)
        for patch in ({"wrapper_protocol_version": True}, {"wrapper_protocol_version": 2},
                      {"run_id": "invalid"}, {"publication_id": "unsupported!"},
                      {"model_revision": "retired"}, {"unknown": "private-sentinel"}):
            self.request = {**original, **patch}
            done = self.execute()
            self.assertEqual(done.returncode, 2)
            self.assertFalse((self.root / "result.json").exists())
            self.assertNotIn("private-sentinel", done.stderr)
        for raw in ('{"x":1,"x":2}', "NaN", "x" * 65537):
            self.assertEqual(self.execute(raw).returncode, 2)
            self.assertFalse((self.root / "result.json").exists())

    def test_retained_installation_accepts_a_new_rollback_publication_id(self):
        self.request["publication_id"] = "fresh-rollback_2"
        done = self.execute()
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.result()["outcome"], "succeeded")

    def test_scientific_failure_is_safe_and_explicit(self):
        self.request["inputs"] = {"unexpected": "private-sentinel"}
        done = self.execute()
        self.assertEqual(done.returncode, 1)
        result = self.result()
        self.assertEqual(result["outcome"], "failed")
        self.assertEqual(result["outputs"], {})
        self.assertEqual(result["files"], {})
        self.assertNotIn("private-sentinel", done.stderr + (self.root / "result.json").read_text())

    def test_preserve_prior_result_and_declared_output(self):
        (self.root / "result.json").write_bytes(b"retained")
        self.assertEqual(self.execute().returncode, 2)
        self.assertEqual((self.root / "result.json").read_bytes(), b"retained")
        self.assertFalse((self.root / "processed.csv").exists())
        if FILES:
            (self.root / "result.json").unlink()
            (self.root / "processed.csv").write_bytes(b"retained-file")
            self.assertEqual(self.execute().returncode, 1)
            self.assertEqual((self.root / "processed.csv").read_bytes(), b"retained-file")

    @unittest.skipUnless(FILES, "file starter only")
    def test_staged_identity_and_path_confinement(self):
        staged = self.request["input_files"]["dataset"]
        for path in ("../escape.csv", str(self.root / "inputs/dataset.csv"), "inputs/missing.csv"):
            staged["path"] = path
            self.assertEqual(self.execute().returncode, 1)
            (self.root / "result.json").unlink()
        staged["path"] = "inputs/dataset.csv"
        staged["artifact_id"] = "66666666-6666-4666-8666-666666666666"
        self.assertEqual(self.execute().returncode, 1)
        (self.root / "result.json").unlink()
        staged["artifact_id"] = self.request["inputs"]["dataset"]["artifact_id"]
        with tempfile.TemporaryDirectory() as outside:
            escape = Path(outside) / "secret.csv"
            escape.write_text("index,value\n0,3\n")
            (self.root / "inputs/link.csv").symlink_to(escape)
            staged["path"] = "inputs/link.csv"
            self.assertEqual(self.execute().returncode, 1)
        self.assertFalse((self.root / "processed.csv").exists())

    def test_sync_failure_never_publishes_partial_result(self):
        destination = self.root / "new.json"
        with mock.patch.object(protocol.os, "fsync", side_effect=OSError("failure")):
            with self.assertRaises(protocol.ModelError):
                protocol.publish(destination, b"complete")
        self.assertFalse(destination.exists())
        self.assertEqual(list(self.root.glob(".model-result-*")), [])

    def test_existing_executable_failure(self):
        if not (RELEASE / "demo_solver.py").exists():
            self.skipTest("executable starter only")
        import implementation
        with mock.patch.object(implementation, "SOLVER_ARGV", ["/nonexistent/author-solver"]):
            with self.assertRaises(OSError):
                implementation.evaluate(protocol.read_json(self._request_file()), self.root, "files" if FILES else "json")
        self.assertFalse((self.root / "result.json").exists())

    def _request_file(self):
        path = self.root / "native-request.json"
        path.write_text(json.dumps(self.request))
        return path


if __name__ == "__main__":
    unittest.main()
