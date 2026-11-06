"""Real scientific checks; run with the exact locked scientific environment."""
import json
import hashlib
import base64
from pathlib import Path
import tempfile
import unittest

from qualify import environment_receipt, uniform_wind_check
from runtime import run_model
from verify_collected import load_model, verify_science, verify_collected

ROOT = Path(__file__).resolve().parent


class ScientificModelTests(unittest.TestCase):
    def test_locked_runtime_and_bundled_dataset(self):
        self.assertEqual(environment_receipt()["solvers"], {"huxt": "5.0.3", "rebound": "5.2.2"})

    def test_uniform_huxt_matches_analytic_acceleration(self):
        self.assertLessEqual(uniform_wind_check(), 1e-3)

    def test_both_example_runs_publish_verified_csv_png_and_numerical_summaries(self):
        for model in ("huxt", "rebound"):
            with self.subTest(model=model), tempfile.TemporaryDirectory() as directory:
                values = json.loads((ROOT / model / "inputs.json").read_text())
                outputs, files = run_model(model, values, Path(directory))
                evidence = verify_science(model, values, outputs,
                                          {key: Path(directory) / item["path"] for key, item in files.items()})
                self.assertTrue(evidence)
                with self.assertRaises(FileExistsError):
                    run_model(model, values, Path(directory))

    def test_rebound_circular_and_eccentric_extremes_match_kepler(self):
        import numpy as np
        module = load_model("rebound")
        for values in ({"semimajor_axis_au": 0.1, "eccentricity": 0, "orbits": 10},
                       {"semimajor_axis_au": 5, "eccentricity": 0.8, "orbits": 10}):
            rows, period, mu = module.trajectory(values)
            expected = module.analytic_xy(values, rows[:, 0], period)
            self.assertLessEqual(np.max(np.linalg.norm(rows[:, 1:3] - expected, axis=1)) /
                                 values["semimajor_axis_au"], 1e-8)
            self.assertTrue(all(error <= 1e-9 for error in module.diagnostics(rows, mu)))

    def test_collected_artifacts_and_numerical_changes_are_checked(self):
        model = "rebound"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            work = root / "work"
            work.mkdir()
            values = json.loads((ROOT / model / "inputs.json").read_text())
            outputs, files = run_model(model, values, work)
            results = root / "results"
            results.mkdir()
            (results / "effective-inputs.json").write_text(json.dumps(values))
            (results / "outputs.json").write_text(json.dumps(outputs))
            items = {}
            for slot, item in files.items():
                path = work / item["path"]
                items[slot] = {"path": str(path.relative_to(root)), "size_bytes": path.stat().st_size,
                               "checksum": {"algorithm": "sha256", "checksum_type": "full_object",
                                            "value": base64.b64encode(hashlib.sha256(path.read_bytes()).digest()).decode("ascii")}}
            (root / "bundle.json").write_text(json.dumps({"complete": True,
                "scientific_state": "succeeded", "outputs": items}))
            self.assertTrue(verify_collected(model, root)["verified"])
            outputs["period_years"] *= 2
            (results / "outputs.json").write_text(json.dumps(outputs))
            with self.assertRaisesRegex(ValueError, "period disagrees"):
                verify_collected(model, root)
            (work / "trajectory.csv").write_text("tampered")
            with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                verify_collected(model, root)


if __name__ == "__main__":
    unittest.main()
