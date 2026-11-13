"""Check the sampled hydrograph and water balance without running SWMM."""
import json
import math
from pathlib import Path


def verify(inputs, outputs, header, rows):
    import numpy as np

    if header != ["time_minutes", "rainfall_mm_per_hour", "runoff_lps"] or rows.shape != (361, 3):
        raise ValueError("Invalid runoff CSV shape or units")
    times, rainfall, flow = rows.T
    if not np.array_equal(times, np.arange(361)) or not np.array_equal(rainfall, 25 * ((times >= 15) & (times < 75))):
        raise ValueError("Runoff CSV differs from the fixed storm or reporting grid")
    if np.any(flow < 0) or np.any(flow[:16]) or flow[-1] != 0:
        raise ValueError("Invalid runoff before the storm or after drainage")

    peak_index = int(flow.argmax())
    depth = math.fsum(rainfall) / 60
    volume = float(np.trapezoid(flow, times)) * 0.06
    rainfall_volume = depth * 10  # One millimeter over one hectare is 10 cubic meters.
    infiltration = float(outputs["infiltration_volume_m3"])
    storage = float(outputs["final_surface_storage_m3"])
    if (not 0 < volume <= rainfall_volume or not 0 <= infiltration <= rainfall_volume
            or not 0 <= storage <= rainfall_volume):
        raise ValueError("Invalid runoff water balance volumes")
    error = 100 * (1 - (volume + infiltration + storage) / rainfall_volume)
    for name, expected in (("peak_runoff_lps", flow[peak_index]), ("peak_time_minutes", times[peak_index]),
                           ("runoff_volume_m3", volume), ("rainfall_depth_mm", depth)):
        if not math.isclose(float(outputs[name]), expected, rel_tol=1e-9, abs_tol=1e-8):
            raise ValueError("Runoff summary disagrees with collected hydrograph")
    if not math.isclose(float(outputs["runoff_balance_error_percent"]), error, rel_tol=1e-9, abs_tol=1e-8):
        raise ValueError("Reported runoff water balance differs from the collected outputs")
    if abs(error) > 0.1:
        raise ValueError("Sampled runoff water balance does not close")

    references = json.loads(Path(__file__).with_name("reference.json").read_text())["cases"]
    reference = next((case for case in references if case["inputs"] == inputs), None)
    if reference is not None:
        if not np.allclose(flow, reference["runoff_lps"], rtol=1e-5, atol=1e-4):
            raise ValueError("Runoff differs from the qualified storm reference")
        for name in ("infiltration_volume_m3", "final_surface_storage_m3"):
            if not math.isclose(float(outputs[name]), reference[name], rel_tol=1e-5, abs_tol=1e-3):
                raise ValueError("Water balance volumes differ from the qualified storm reference")
    return {"time_samples": len(times), "runoff_balance_error_percent": error, "reference_checked": reference is not None}
