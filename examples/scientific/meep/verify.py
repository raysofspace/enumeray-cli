"""Check the saved cavity spectrum without running Meep."""
import json
import math
from pathlib import Path


def verify(inputs, outputs, header, rows):
    import numpy as np

    expected_header = ["frequency_c_over_a", "wavelength_over_a", "reference_flux", "cavity_flux", "transmission"]
    if header != expected_header or rows.shape != (401, 5):
        raise ValueError("Invalid Meep spectrum CSV shape or units")
    frequencies, wavelengths, reference, cavity, transmission = rows.T
    if not np.allclose(frequencies, np.linspace(0.195, 0.275, 401), rtol=0, atol=1e-12):
        raise ValueError("Invalid Meep reporting grid")
    if (np.any(reference <= 0) or np.any(cavity < 0) or np.any(transmission > 1.02)
            or not np.allclose(wavelengths * frequencies, 1, rtol=0, atol=1e-12)
            or not np.allclose(transmission, cavity / reference, rtol=1e-10, atol=1e-12)):
        raise ValueError("Invalid Meep wavelength, flux or transmission")
    # The fixed reporting grid puts the inclusive resonance search at rows 75–375.
    search = range(75, 376)
    peak = max(search, key=transmission.__getitem__)
    if peak in (search.start, search.stop - 1):
        raise ValueError("Meep peak is outside the interior search band")
    for name, expected in (("peak_frequency_c_over_a", frequencies[peak]),
                           ("peak_wavelength_over_a", wavelengths[peak]),
                           ("peak_transmission", transmission[peak])):
        if not math.isclose(float(outputs[name]), expected, rel_tol=1e-10, abs_tol=1e-12):
            raise ValueError("Meep summary disagrees with the collected spectrum")

    reference_cases = json.loads(Path(__file__).with_name("reference.json").read_text())
    case = next((case for case in reference_cases["cases"] if case["inputs"] == inputs), None)
    if case and (abs(frequencies[peak] - case["peak_frequency_c_over_a"]) > 0.0004
                 or abs(transmission[peak] - case["peak_transmission"]) > 0.03):
        raise ValueError("Meep peak differs from the qualified reference")
    return {"frequency_samples": len(frequencies), "reference_checked": case is not None}
