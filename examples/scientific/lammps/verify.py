"""Check the reported self-diffusion and energy without running LAMMPS."""
import json
import math
from pathlib import Path


def verify(inputs, outputs, header, rows):
    import numpy as np

    expected_header = ["time_reduced"] + [
        f"{case}{quantity}_{replica}_reduced"
        for quantity in ("msd", "energy")
        for case in ("baseline_", "")
        for replica in range(1, 4)
    ]
    if header != expected_header or rows.shape != (241, 13):
        raise ValueError("Invalid diffusion CSV shape or units")
    times, msd, energies = rows[:, 0], rows[:, 1:7], rows[:, 7:]
    if not np.allclose(times, np.linspace(0, 36, 241), rtol=0, atol=1e-12):
        raise ValueError("Invalid LAMMPS reporting grid")
    if np.any(msd < 0) or np.any(msd[0] > 1e-12):
        raise ValueError("Invalid mean-squared displacement")

    # An independent OLS calculation from centered sums, with an intercept.
    start, stop = 48, 193
    centered_times = times[start:stop] - times[start:stop].mean()
    centered_msd = msd[start:stop] - msd[start:stop].mean(axis=0)
    slopes = centered_times @ centered_msd / (centered_times @ centered_times)
    coefficients = slopes / 6
    energy_drift = float(np.max(np.abs(energies - energies[0])))
    if np.any(coefficients <= 0) or energy_drift > 0.005:
        raise ValueError("Invalid diffusion slope or excessive energy drift")
    baseline, warmer = coefficients[:3], coefficients[3:]
    expected = {
        "baseline_diffusion_reduced": float(baseline.mean()),
        "diffusion_reduced": float(warmer.mean()),
        "baseline_diffusion_std_reduced": float(baseline.std(ddof=1)),
        "diffusion_std_reduced": float(warmer.std(ddof=1)),
        "max_energy_drift_per_particle_reduced": energy_drift,
    }
    for name, value in expected.items():
        if not math.isclose(float(outputs[name]), value, rel_tol=1e-9, abs_tol=1e-10):
            raise ValueError("Diffusion summary disagrees with collected samples")

    reference = json.loads(Path(__file__).with_name("reference.json").read_text())
    measured_baseline = expected["baseline_diffusion_reduced"]
    measured_warmer = expected["diffusion_reduced"]
    tolerance = reference["absolute_tolerance_reduced"]
    if abs(measured_baseline - reference["baseline_diffusion_reduced"]) > tolerance:
        raise ValueError("Baseline diffusion differs from the qualified reference")
    reference_case = next((case for case in reference["cases"] if case["inputs"] == inputs), None)
    if reference_case is not None and (
            abs(measured_warmer - reference_case["diffusion_reduced"]) > tolerance
            or measured_warmer <= measured_baseline):
        raise ValueError("Warmer diffusion differs from the qualified reference or trend")
    return {"time_samples": len(times), "replicas_per_temperature": len(baseline),
            "max_energy_drift_per_particle_reduced": energy_drift, "reference_checked": reference_case is not None}
