"""Dependency-free supervision, offline, protocol and evidence regressions."""
import base64
import errno
import hashlib
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
from zipfile import ZipFile

import qualify
from runtime import ExecutionFailure, FILES, PARAMETERS, run_model, supervise, validate_inputs, worker_environment

ROOT = Path(__file__).resolve().parent


def request(model="huxt"):
    return {"wrapper_protocol_version": 1, **{key: str(uuid.uuid4()) for key in
            ("run_id", "assignment_id", "attempt_id", "launch_intent_id")},
            "model_id": "demo-" + model, "publication_id": "test-publication",
            "inputs": json.loads((ROOT / model / "inputs.json").read_text()), "input_files": {}}


class RuntimeTests(unittest.TestCase):
    def test_sample_run_bundle_links_real_output_bytes_and_example_run_records(self):
        model = ROOT / "rebound"
        prefix = "rebound-run-bundle/"
        with ZipFile(model / "sample-run-bundle.zip") as collection, ZipFile(model / "sample-run.zip") as sample:
            bundle = json.loads(collection.read(prefix + "bundle.json"))
            inspection = json.loads(collection.read(prefix + bundle["inspection"]))
            provenance = json.loads(collection.read(prefix + bundle["provenance"]))
            inputs = json.loads(collection.read(prefix + bundle["effective_inputs"]))
            outputs = json.loads(collection.read(prefix + bundle["scientific_outputs"]))
            self.assertIs(bundle["complete"], True)
            self.assertEqual(bundle["scientific_state"], "succeeded")
            for key in ("run_id", "publication_id"):
                self.assertEqual(bundle[key], inspection[key])
                self.assertEqual(bundle[key], provenance["run"][key])
            self.assertEqual(provenance["publication"]["context_id"], inspection["context_id"])
            self.assertEqual(inspection["effective_inputs"], inputs)
            self.assertEqual(inputs, request("rebound")["inputs"])
            self.assertEqual(inspection["result"], provenance["result"])
            self.assertEqual(inspection["result"]["outputs"], outputs)
            expected_paths = [prefix + name for name in ("bundle.json", bundle["inspection"],
                                                         bundle["provenance"], bundle["effective_inputs"],
                                                         bundle["scientific_outputs"])]
            self.assertEqual(bundle["outputs"].keys(), FILES["rebound"].keys())
            for pin in provenance["output_artifacts"]:
                item = bundle["outputs"][pin["name"]]
                expected_paths.append(prefix + item["path"])
                data = collection.read(prefix + item["path"])
                self.assertEqual(data, sample.read("rebound-sample/" + pin["filename"]))
                self.assertEqual(len(data), item["size_bytes"])
                self.assertEqual(item["artifact_id"], outputs[pin["name"]]["artifact_id"])
                self.assertEqual(pin["checksum"], item["checksum"])
                self.assertEqual(item["checksum"]["value"], base64.b64encode(hashlib.sha256(data).digest()).decode("ascii"))
            self.assertCountEqual(collection.namelist(), expected_paths)

    def test_sample_downloads_match_the_default_models_and_recorded_files(self):
        manifest = json.loads((ROOT / "runtime-manifest.json").read_text())
        for model, artifacts in FILES.items():
            with self.subTest(model=model), ZipFile(ROOT / model / "sample-run.zip") as archive:
                prefix = f"{model}-sample/"
                expected = {"inputs.json", "outputs.json", *(name for name, _ in artifacts.values())}
                self.assertCountEqual(archive.namelist(), [prefix + name for name in expected | {"run-record.json"}])
                self.assertEqual(json.loads(archive.read(prefix + "inputs.json")), request(model)["inputs"])
                record = json.loads(archive.read(prefix + "run-record.json"))
                self.assertEqual(record["kind"], "local-scientific-example")
                self.assertEqual(record["model"], model)
                self.assertEqual(record["model_sha256"], hashlib.sha256((ROOT / model / "model.py").read_bytes()).hexdigest())
                self.assertEqual(record["runtime"], {key: manifest[key] for key in record["runtime"]})
                self.assertEqual(record["files"].keys(), expected)
                for name, identity in record["files"].items():
                    data = archive.read(prefix + name)
                    self.assertEqual(identity, {"size_bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})

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
            env = worker_environment(directory, "huxt")
            self.assertNotIn("ENUMERAY_TOKEN", env)
            self.assertNotIn("AWS_SECRET_ACCESS_KEY", env)
            for key in ("HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME", "MPLCONFIGDIR", "NUMBA_CACHE_DIR", "TMPDIR"):
                self.assertTrue(Path(env[key]).is_relative_to(directory))
            self.assertEqual(env["OPENBLAS_NUM_THREADS"], "1")
            self.assertEqual(env["NUMBA_DISABLE_JIT"], "1")
            manifest = json.loads((ROOT / "runtime-manifest.json").read_text())
            self.assertIs(manifest["huxt_numba_jit_enabled"], False)
        with tempfile.TemporaryDirectory() as directory:
            self.assertNotIn("NUMBA_DISABLE_JIT", worker_environment(directory, "rebound"))

    def test_qualification_reports_case_and_both_resource_measurements(self):
        rss_limit = 1024 ** 3
        for elapsed, rss in ((61, rss_limit), (60, rss_limit + 1)):
            with self.subTest(elapsed=elapsed, rss=rss):
                def run_case(_model, _inputs, workspace):
                    metrics = workspace / ".scientific" / "metrics.json"
                    metrics.parent.mkdir()
                    metrics.write_text(json.dumps({"peak_rss_bytes": rss, "offline": True}))
                    return {}, {}

                with mock.patch.object(qualify, "environment_receipt", return_value={}), \
                        mock.patch.object(qualify, "run_model", side_effect=run_case), \
                        mock.patch.object(qualify.time, "monotonic", side_effect=[0, elapsed]), \
                        self.assertRaisesRegex(ValueError,
                            rf"huxt/example .*elapsed {elapsed}\.000 s, limit 60 s.*"
                            rf"peak RSS {rss} bytes, limit {rss_limit} bytes"):
                    qualify.qualify(("huxt",))

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
