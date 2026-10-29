from decimal import Decimal, localcontext
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import model
import wrapper


PACKAGE = Path(__file__).resolve().parent
REQUEST = json.loads((PACKAGE / "request.json").read_text())


class ModelTests(unittest.TestCase):
    def test_exact_calculation_does_not_depend_on_decimal_context(self):
        examples = {
            "9007199254740993.123456789": "18014398509481986.246913578",
            "-0.0000000000000000000000000000000000000001": "-2E-40",
            "9.999999999999999999999999999999999999999": "19.999999999999999999999999999999999999998",
            "1e100000": "2e100000",
            "1e-100000": "2e-100000",
            "-0": "-0",
        }
        with localcontext() as context:
            context.prec = 2
            context.Emax = 9
            context.Emin = -9
            for value, expected in examples.items():
                with self.subTest(value=value):
                    self.assertEqual(model.double(Decimal(value)), Decimal(expected))

    def test_non_finite_and_non_decimal_values_are_rejected(self):
        for value in (Decimal("NaN"), Decimal("Infinity"), Decimal("-Infinity"), 2, True, "2"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                model.double(value)

    def test_standalone_model_has_no_wrapper_runtime_requirement(self):
        with tempfile.TemporaryDirectory() as directory:
            installed = Path(directory) / "model.py"
            shutil.copyfile(PACKAGE / "model.py", installed)
            result = subprocess.run(
                [sys.executable, str(installed), "9007199254740993.123456789"],
                cwd=directory, capture_output=True, text=True, timeout=5,
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "18014398509481986.246913578")


class WrapperTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.request_path = self.directory / "request.json"
        self.result_path = self.directory / "result.json"

    def run_wrapper(self, raw=None, executable=None):
        self.request_path.write_text(raw or json.dumps(REQUEST))
        return subprocess.run(
            [sys.executable, str(executable or PACKAGE / "wrapper.py"), "execute",
             "--request", str(self.request_path), "--result", str(self.result_path)],
            cwd=self.directory, capture_output=True, text=True, timeout=5,
        )

    def read_result(self):
        return json.loads(self.result_path.read_text(), parse_int=Decimal, parse_float=Decimal)

    def test_external_wrapper_copied_independently_echoes_identity_and_exact_value(self):
        for filename in ("model.py", "wrapper.py"):
            shutil.copyfile(PACKAGE / filename, self.directory / filename)
        raw = json.dumps(REQUEST).replace('"value": 2', '"value": 9007199254740993.123456789')
        completed = self.run_wrapper(raw, self.directory / "wrapper.py")
        self.assertEqual(completed.returncode, 0, completed.stderr)
        result = self.read_result()
        self.assertEqual(result["outcome"], "succeeded")
        self.assertEqual(result["outputs"], {"doubled": Decimal("18014398509481986.246913578")})
        self.assertIsNone(result["failure"])
        self.assertEqual(result["files"], {})
        for field in wrapper.IDENTITIES:
            self.assertEqual(result[field], REQUEST[field])
        self.assertEqual(completed.stdout, "")
        self.assertNotIn("900719925", completed.stderr)
        self.assertEqual(os.stat(self.result_path).st_mode & 0o777, 0o600)

    def test_long_coefficient_is_not_limited_by_python_integer_string_conversion(self):
        digits = "1" * 5000
        raw = json.dumps(REQUEST).replace('"value": 2', '"value": ' + digits)
        completed = self.run_wrapper(raw)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(self.read_result()["outputs"]["doubled"], Decimal("2" * 5000))

    def test_invalid_scientific_input_publishes_safe_failure(self):
        for inputs in ({}, {"value": "private-sentinel"}, {"value": True}, {"value": 2, "other": 3}):
            with self.subTest(inputs=inputs):
                self.result_path.unlink(missing_ok=True)
                request = {**REQUEST, "inputs": inputs}
                completed = self.run_wrapper(json.dumps(request))
                self.assertEqual(completed.returncode, 1, completed.stderr)
                result = self.read_result()
                self.assertEqual(result["outcome"], "failed")
                self.assertEqual(result["outputs"], {})
                self.assertEqual(result["failure"]["code"], "invalid_inputs")
                self.assertNotIn("private-sentinel", self.result_path.read_text() + completed.stderr)

    def test_invalid_envelopes_publish_no_result_or_private_diagnostic(self):
        valid = json.dumps(REQUEST)
        invalid = [
            valid.replace('"value": 2', '"value": 2, "value": 3'),
            valid.replace('"wrapper_protocol_version": 1', '"wrapper_protocol_version": 1, "wrapper_protocol_version": 1'),
            valid.replace('"value": 2', '"value": NaN'),
            valid.replace('"value": 2', '"value": Infinity'),
            valid.replace('"value": 2', '"value": 1e99999999999999999999999999999999'),
            json.dumps({**REQUEST, "wrapper_protocol_version": 2}),
            json.dumps({**REQUEST, "wrapper_protocol_version": True}),
            json.dumps({**REQUEST, "wrapper_protocol_version": 1.0}),
            json.dumps({**REQUEST, "unexpected": "private-sentinel"}),
            json.dumps({key: value for key, value in REQUEST.items() if key != "attempt_id"}),
            json.dumps({**REQUEST, "attempt_id": "private-sentinel"}),
            json.dumps({**REQUEST, "attempt_id": "00000000-0000-0000-0000-000000000000"}),
            json.dumps({**REQUEST, "publication_id": "invalid/publication"}),
            json.dumps({**REQUEST, "inputs": []}),
            json.dumps({**REQUEST, "input_files": None}),
            json.dumps({key: value for key, value in REQUEST.items() if key != "input_files"}),
            "[]",
            "{",
            valid + " " * wrapper.MAX_BYTES,
        ]
        for index, raw in enumerate(invalid):
            with self.subTest(case=index, raw=raw[:100]):
                completed = self.run_wrapper(raw)
                self.assertEqual(completed.returncode, 2, completed.stderr)
                self.assertFalse(self.result_path.exists())
                self.assertNotIn("private-sentinel", completed.stderr)
                self.assertNotIn(str(self.directory), completed.stderr)
                self.assertLess(len(completed.stderr), 256)
                self.assertNotIn("Traceback", completed.stderr)

    def test_existing_result_is_never_overwritten(self):
        self.result_path.write_text("existing durable evidence")
        completed = self.run_wrapper()
        self.assertEqual(completed.returncode, 2)
        self.assertEqual(self.result_path.read_text(), "existing durable evidence")
        self.assertEqual(list(self.directory.glob(".scalar-result-*")), [])

    def test_failed_file_sync_does_not_publish_partial_output(self):
        with mock.patch.object(wrapper.os, "fsync", side_effect=OSError("private-sentinel")):
            with self.assertRaisesRegex(wrapper.WrapperError, "could not be durably published"):
                wrapper.publish_result(self.result_path, {"example": Decimal("1.2")})
        self.assertFalse(self.result_path.exists())
        self.assertEqual(list(self.directory.glob(".scalar-result-*")), [])

    def test_help_does_not_require_request_files(self):
        completed = subprocess.run(
            [sys.executable, str(PACKAGE / "wrapper.py"), "execute", "--help"],
            capture_output=True, text=True, timeout=5,
        )
        self.assertEqual(completed.returncode, 0)
        self.assertIn("--request", completed.stdout)


if __name__ == "__main__":
    unittest.main()
