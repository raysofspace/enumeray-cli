"""Calculate normalized transmission through a bounded Meep cavity."""
import csv
import ctypes
import math
import os
from pathlib import Path
import signal
import subprocess
from tempfile import TemporaryDirectory

CONTROL = Path(__file__).with_name("cavity.ctl").resolve()
ENGINE = "1.25.0"
EXECUTABLE = "/usr/bin/meep"
SAMPLES = 401
FREQUENCY_START = 0.195
FREQUENCY_STEP = 0.0002
PEAK_WINDOW = (0.21, 0.27)
MAX_TRANSMISSION = 1.02
CSV_HEADER = ("frequency_c_over_a", "wavelength_over_a", "reference_flux", "cavity_flux", "transmission")


def engine_version():
    result = subprocess.run([EXECUTABLE, "--version"], stdin=subprocess.DEVNULL,
                            capture_output=True, text=True, timeout=10, check=True)
    prefix = f"Meep {ENGINE},"
    if not result.stdout.startswith(prefix):
        raise ValueError("Unexpected Meep engine version")
    return ENGINE


def run_engine(directory, spacing, grid, reference):
    parent = os.getpid()
    prctl = ctypes.CDLL(None, use_errno=True).prctl

    def bind_lifetime():
        if prctl(1, signal.SIGKILL, 0, 0, 0) or os.getppid() != parent:
            os._exit(125)

    def cancel(signum, _frame):
        raise InterruptedError(f"Meep run interrupted by signal {signum}")

    environment = dict(os.environ, GUILE_AUTO_COMPILE="0", GC_MARKERS="1")
    command = [EXECUTABLE, f"spacing={spacing}", f"grid={grid}",
               f"reference?={'true' if reference else 'false'}", CONTROL]
    original = signal.signal(signal.SIGTERM, cancel)
    try:
        result = subprocess.run(command, cwd=directory, env=environment, stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
                                preexec_fn=bind_lifetime)
    finally:
        signal.signal(signal.SIGTERM, original)
    if result.returncode:
        raise ValueError("Meep did not complete the declared serial cavity run")

    rows = []
    for line in result.stdout.splitlines():
        if line.startswith("flux1:,"):
            _, frequency, flux = line.split(",")
            rows.append((float(frequency), float(flux)))
    if len(rows) != SAMPLES:
        raise ValueError("Incomplete Meep flux spectrum")
    for index, (frequency, flux) in enumerate(rows):
        expected = FREQUENCY_START + index * FREQUENCY_STEP
        if not math.isclose(frequency, expected, rel_tol=0, abs_tol=1e-12) or not math.isfinite(flux):
            raise ValueError("Invalid Meep frequency grid or flux")
    return rows


def read_field(directory, grid):
    import h5py
    import numpy as np

    arrays = []
    for pattern, name in (("cavity-eps-*.h5", "eps"), ("cavity-hz-*.h5", "hz")):
        matches = list(directory.glob(pattern))
        if len(matches) != 1:
            raise ValueError("Missing Meep field snapshot")
        with h5py.File(matches[0], "r") as handle:
            dataset = handle[name]
            if dataset.shape != (14 * grid, 6 * grid):
                raise ValueError("Invalid Meep field dimensions")
            data = dataset[:].T
            if not np.isfinite(data).all() or np.ptp(data) <= 0:
                raise ValueError("Invalid Meep field data")
            arrays.append(data)
    return arrays


def simulate(values, workspace):
    import matplotlib.pyplot as plt
    import numpy as np

    spacing = values["cavity_spacing_periods"]
    grid = int(values["grid_cells_per_period"])
    with TemporaryDirectory(prefix="meep-", dir=workspace) as temporary:
        directory = Path(temporary)
        frequencies, reference_flux = np.asarray(run_engine(directory, spacing, grid, reference=True)).T
        _, cavity_flux = np.asarray(run_engine(directory, spacing, grid, reference=False)).T
        permittivity, magnetic_field = read_field(directory, grid)

    if np.any(reference_flux <= 0) or np.any(cavity_flux < 0):
        raise ValueError("Invalid Meep forward flux")
    wavelengths = 1 / frequencies
    transmission = cavity_flux / reference_flux
    if np.any(transmission > MAX_TRANSMISSION):
        raise ValueError("Meep transmission exceeds the numerical passivity tolerance")
    interior = np.flatnonzero((frequencies >= PEAK_WINDOW[0] - 1e-12)
                              & (frequencies <= PEAK_WINDOW[1] + 1e-12))
    peak = interior[transmission[interior].argmax()]
    if peak in (interior[0], interior[-1]):
        raise ValueError("Meep resonance is outside the interior search band")

    with (workspace / "spectrum.csv").open("x", newline="") as stream:
        writer = csv.writer(stream)
        writer.writerow(CSV_HEADER)
        writer.writerows(zip(frequencies, wavelengths, reference_flux, cavity_flux, transmission))

    figure, (spectrum, field) = plt.subplots(2, 1, figsize=(8, 6), layout="constrained")
    spectrum.plot(wavelengths, transmission, color="tab:blue")
    spectrum.scatter(wavelengths[peak], transmission[peak], color="tab:orange", zorder=3,
                     label=f"Peak wavelength {wavelengths[peak]:.3f} a")
    spectrum.set(title=f"Photonic-crystal filter at cavity spacing {spacing:g} a",
                 xlabel="Wavelength / a", ylabel="Transmitted fraction", ylim=(0, 1.05))
    spectrum.legend(loc="upper right")
    spectrum.grid(alpha=0.25)

    # Cell-centered samples from the physical interior, inside the absorbing layers.
    x = (np.arange(14 * grid) + 0.5) / grid - 7
    y = (np.arange(6 * grid) + 0.5) / grid - 3
    magnitude = np.max(np.abs(magnetic_field))
    colors = field.imshow(magnetic_field / magnitude, origin="lower", cmap="RdBu_r",
                          extent=(-7, 7, -3, 3), vmin=-1, vmax=1)
    field.contour(x, y, permittivity, levels=[7], colors="0.2", linewidths=0.65)
    field.set(title="Magnetic field snapshot after excitation",
              xlabel="Distance along waveguide / a", ylabel="Across guide / a",
              xlim=(-6, 6), ylim=(-1.6, 1.6))
    figure.colorbar(colors, ax=field, label="Normalized magnetic field", shrink=0.84)
    figure.savefig(workspace / "transmission.png", dpi=120)
    plt.close(figure)

    return {"peak_frequency_c_over_a": float(frequencies[peak]),
            "peak_wavelength_over_a": float(wavelengths[peak]),
            "peak_transmission": float(transmission[peak])}
