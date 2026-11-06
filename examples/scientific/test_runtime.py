"""Dependency-free supervision, offline, protocol and evidence regressions."""
import errno
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock
import uuid

from runtime import ExecutionFailure, PARAMETERS, run_model, supervise, validate_inputs, worker_environment

ROOT = Path(__file__).resolve().parent


def request(model="huxt"):
    return {"wrapper_protocol_version": 1, **{key: str(uuid.uuid4()) for key in
            ("run_id", "assignment_id", "attempt_id", "launch_intent_id")},
            "model_id": "demo-" + model, "publication_id": "test-publication",
            "inputs": json.loads((ROOT / model / "inputs.json").read_text()), "input_files": {}}


class RuntimeTests(unittest.TestCase):
    def test_parameter_domains_are_closed_finite_and_bounded(self):
        for model, parameters in PARAMETERS.items():
            defaults = request(model)["inputs"]
            self.assertEqual(set(validate_inputs(model, defaults)), set(parameters))
            for key, (low, high) in parameters.items():
                for bad in (low - 1, high + 1, True, None, "1", float("nan"), float("inf")):
                    with self.subTest(model=model, key=key, bad=bad), self.assertRaises(ExecutionFailure):
                        validate_inputs(model, {**defaults, key: bad})
            with self.assertRaises(ExecutionFailure):
                validate_inputs(model, {**defaults, "command": "anything"})

    def test_worker_environment_confines_writes_and_drops_credentials(self):
        with tempfile.TemporaryDirectory() as directory:
            env = worker_environment(directory)
            self.assertNotIn("ENUMERAY_TOKEN", env)
            self.assertNotIn("AWS_SECRET_ACCESS_KEY", env)
            for key in ("HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "MPLCONFIGDIR", "NUMBA_CACHE_DIR", "TMPDIR"):
                self.assertTrue(Path(env[key]).is_relative_to(directory))
            self.assertEqual(env["OPENBLAS_NUM_THREADS"], "1")

    def test_timeout_reaps_the_original_scientific_child(self):
        with tempfile.TemporaryDirectory() as directory:
            pid_file = Path(directory) / "pid"
            command = [sys.executable, "-c", "import os,time;open('pid','w').write(str(os.getpid()));time.sleep(30)"]
            with self.assertRaisesRegex(ExecutionFailure, "runtime_timeout"):
                supervise(command, cwd=directory, env=dict(os.environ), timeout=0.2)
            pid = int(pid_file.read_text())
            with self.assertRaises(ProcessLookupError):
                os.kill(pid, 0)

    def test_cancellation_terminates_and_reaps_child(self):
        with tempfile.TemporaryDirectory() as directory:
            pid_file = Path(directory) / "pid"
            code = ("import sys;sys.path.insert(0,sys.argv[1]);from runtime import supervise,ExecutionFailure;"
                    "supervise([sys.executable,'-c',\"import os,time;open('pid','w').write(str(os.getpid()));time.sleep(30)\"],"
                    "cwd=sys.argv[2],env={},timeout=30)")
            parent = subprocess.Popen([sys.executable, "-c", code, str(ROOT), directory],
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                deadline = time.monotonic() + 5
                while not pid_file.exists() and time.monotonic() < deadline:
                    time.sleep(0.02)
                self.assertTrue(pid_file.exists())
                child = int(pid_file.read_text())
                parent.terminate()
                parent.wait(timeout=5)
                with self.assertRaises(ProcessLookupError):
                    os.kill(child, 0)
            finally:
                if parent.poll() is None:
                    parent.kill()
                    parent.wait()

    def test_solver_network_is_disabled_without_scientific_dependencies(self):
        code = ("import os,socket,sys;from worker import isolate;isolate(os.getppid());"
                "socket.create_connection(('example.org',443))")
        result = subprocess.run([sys.executable, "-c", code], cwd=ROOT,
                                capture_output=True, text=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("bundled offline inputs only", result.stderr)

    def test_combined_outputs_are_bounded_even_when_each_file_fits(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def produce(*_args, **_kwargs):
                (root / ".scientific/outputs.json").write_text('{"peak_speed_1au_kms":700}')
                (root / "wind-field.csv").write_bytes(b"x" * (5 * 1024 * 1024))
                (root / "wind.png").write_bytes(b"\x89PNG\r\n\x1a\n" + b"x" * (5 * 1024 * 1024))

            with mock.patch("runtime.supervise", side_effect=produce):
                with self.assertRaisesRegex(ExecutionFailure, "output_limit"):
                    run_model("huxt", request()["inputs"], root)

    def test_invalid_inputs_publish_failure_without_starting_a_solver(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "request.json"
            body = request()
            body["inputs"]["fast_speed_kms"] = 1000000
            source.write_text(json.dumps(body))
            target = Path(directory) / "result.json"
            result = subprocess.run([sys.executable, str(ROOT / "wrapper.py"), "execute", "--model", "huxt",
                                     "--request", str(source), "--result", str(target)], capture_output=True, timeout=5)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(json.loads(target.read_text())["failure"]["code"], "invalid_inputs")
            self.assertFalse((Path(directory) / ".scientific").exists())

    def test_existing_outputs_and_malformed_envelopes_preserve_evidence(self):
        for bad in ("existing", "identity", "input-files"):
            with self.subTest(bad=bad), tempfile.TemporaryDirectory() as directory:
                source = Path(directory) / "request.json"
                body = request()
                if bad == "identity":
                    body["run_id"] = "not-a-uuid"
                elif bad == "input-files":
                    body["input_files"] = {"unreviewed": {"path": "/tmp/anything"}}
                else:
                    (Path(directory) / "wind.png").write_bytes(b"retained")
                source.write_text(json.dumps(body))
                target = Path(directory) / "result.json"
                result = subprocess.run([sys.executable, str(ROOT / "wrapper.py"), "execute", "--model", "huxt",
                                         "--request", str(source), "--result", str(target)], capture_output=True, timeout=5)
                self.assertEqual(result.returncode, 2)
                self.assertFalse(target.exists())
                if bad == "existing":
                    self.assertEqual((Path(directory) / "wind.png").read_bytes(), b"retained")


if __name__ == "__main__":
    unittest.main()
