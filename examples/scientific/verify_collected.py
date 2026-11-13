"""Check collected scientific artifacts, not just a successful service state."""
import argparse
import base64
import csv
import hashlib
import importlib.util
import json
import math
from pathlib import Path

from runtime import FILES, MAX_FILE_BYTES, MAX_OUTPUT_BYTES, validate_inputs

ROOT = Path(__file__).resolve().parent


def load_model(model, source="model.py"):
    spec = importlib.util.spec_from_file_location("checked_model", ROOT / model / source)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def verify_science(model, inputs, outputs, files):
    import numpy as np
    values = validate_inputs(model, inputs)
    slots_by_media_type = {media_type: slot for slot, (_, media_type) in FILES[model].items()}
    png_slot = slots_by_media_type["image/png"]
    csv_slot = slots_by_media_type["text/csv"]
    if Path(files[png_slot]).read_bytes()[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("Invalid scientific plot")
    from PIL import Image
    with Image.open(files[png_slot]) as image:
        if image.format != "PNG" or not all(64 <= size <= 2048 for size in image.size):
            raise ValueError("Invalid scientific plot dimensions")
        image.verify()
    with Path(files[csv_slot]).open(newline="") as source:
        reader = csv.reader(source)
        header = next(reader)
        rows = np.array([[float(value) for value in row] for row in reader])
    if not np.all(np.isfinite(rows)):
        raise ValueError("Nonfinite scientific CSV")
    if model in ("swmm", "lammps", "meep"):
        return load_model(model, "verify.py").verify(values, outputs, header, rows)
    if model == "rebound":
        module = load_model(model)
        if header != ["time_years", "x_au", "y_au", "vx_au_per_year", "vy_au_per_year"] or rows.shape != (361, 5):
            raise ValueError("Invalid orbit CSV shape or units")
        period = float(outputs["period_years"])
        import rebound
        mu = rebound.units.convert_G(("au", "yr", "msun")) * (1 + module.PLANET_MASS)
        expected_period = 2 * math.pi * math.sqrt(values["semimajor_axis_au"] ** 3 / mu)
        if not math.isclose(period, expected_period, rel_tol=1e-12):
            raise ValueError("Orbit period disagrees with fixed masses and units")
        if period <= 0 or not math.isclose(rows[-1, 0], values["orbits"] * period, rel_tol=1e-12):
            raise ValueError("Invalid orbit duration")
        error = float(np.max(np.linalg.norm(rows[:, 1:3] - module.analytic_xy(values, rows[:, 0], period), axis=1))
                      / values["semimajor_axis_au"])
        energy, momentum = module.diagnostics(rows, mu)
        if error > 1e-8 or energy > 1e-9 or momentum > 1e-9:
            raise ValueError("Orbit fails independent Kepler/conservation checks")
        for name, measured in (("max_relative_energy_error", energy),
                               ("max_relative_angular_momentum_error", momentum)):
            reported = float(outputs[name])
            if not 0 <= reported <= 1e-9 or not math.isclose(reported, measured, abs_tol=1e-12):
                raise ValueError("Orbit conservation summary differs from collected samples")
        for name, expected in (("periapse_au", values["semimajor_axis_au"] * (1 - values["eccentricity"])),
                               ("apoapse_au", values["semimajor_axis_au"] * (1 + values["eccentricity"]))):
            if not math.isclose(float(outputs[name]), expected, rel_tol=1e-12):
                raise ValueError("Orbit summary disagrees with initial conditions")
        return {"normalized_kepler_error": error, "relative_energy_error": energy,
                "relative_angular_momentum_error": momentum}
    if header != ["time_days", "radius_au", "speed_kms"] or rows.ndim != 2 or rows.shape[1] != 3:
        raise ValueError("Invalid wind CSV shape or units")
    times = np.unique(rows[:, 0])
    radii = np.unique(rows[:, 1])
    if len(times) != 217 or len(radii) != 141 or len(rows) != len(times) * len(radii):
        raise ValueError("Invalid fixed HUXt grid")
    wind = rows[:, 2].reshape((len(times), len(radii)))
    if not np.array_equal(rows[:, 0], np.repeat(times, len(radii))) or not np.array_equal(
            rows[:, 1], np.tile(radii, len(times))) or np.min(wind) <= 0:
        raise ValueError("Invalid wind grid order or speeds")
    at_au = np.array([np.interp(1, radii, row) for row in wind])
    for name, expected in (("min_speed_1au_kms", float(at_au.min())),
                           ("peak_speed_1au_kms", float(at_au.max())),
                           ("peak_time_days", float(times[int(np.flatnonzero(at_au >= at_au.max() * (1 - 1e-9))[0])]))):
        if not math.isclose(float(outputs[name]), expected, rel_tol=1e-9, abs_tol=1e-8):
            raise ValueError("Wind summary disagrees with collected numerical field")
    reference = json.loads((ROOT / "huxt/reference.json").read_text())
    reference_checked = values == reference["inputs"]
    if reference_checked and not np.allclose(at_au, reference["speed_1au_kms"], rtol=1e-5, atol=1e-3):
        raise ValueError("Wind differs from the qualified fast-stream reference")
    return {"time_samples": len(times), "radial_samples": len(radii),
            "reference_checked": reference_checked}


def verify_collected(model, analysis):
    analysis = Path(analysis).resolve()
    bundle = json.loads((analysis / "bundle.json").read_text())
    if bundle.get("complete") is not True or bundle.get("scientific_state") != "succeeded":
        raise ValueError("Scientific collection is incomplete")
    inputs = json.loads((analysis / "results/effective-inputs.json").read_text())
    outputs = json.loads((analysis / "results/outputs.json").read_text())
    files = {}
    total_bytes = 0
    for slot in FILES[model]:
        item = bundle["outputs"][slot]
        path = (analysis / item["path"]).resolve()
        if not path.is_relative_to(analysis) or not path.is_file() or not 0 < path.stat().st_size <= MAX_FILE_BYTES:
            raise ValueError("Invalid collected artifact path or length")
        checksum = item["checksum"]
        measured = base64.b64encode(hashlib.sha256(path.read_bytes()).digest()).decode("ascii")
        if (checksum.get("algorithm") != "sha256" or checksum.get("checksum_type") != "full_object"
                or path.stat().st_size != item["size_bytes"] or measured != checksum["value"]):
            raise ValueError("Collected artifact checksum mismatch")
        files[slot] = path
        total_bytes += path.stat().st_size
    if total_bytes > MAX_OUTPUT_BYTES:
        raise ValueError("Combined scientific artifacts exceed run output limit")
    return {"model": model, "verified": True,
            "checks": verify_science(model, inputs, outputs, files)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", required=True, choices=FILES)
    parser.add_argument("--analysis", required=True)
    args = parser.parse_args()
    print(json.dumps(verify_collected(args.model, args.analysis), sort_keys=True))


if __name__ == "__main__":
    main()
