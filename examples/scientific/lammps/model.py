"""Estimate self-diffusion in a bounded Lennard-Jones liquid."""
import csv
import ctypes
from importlib.metadata import distribution, files
import math
import os
from pathlib import Path
import signal
import subprocess
import sys
from tempfile import TemporaryDirectory

ENGINE = "22 Jul 2025 - Update 4"
DECK = Path(__file__).with_name("liquid.in").resolve()
BASELINE_TEMPERATURE = 1.0
SEEDS = ((513841, 738191, 246803), (829013, 306481, 957827))
PRODUCTION_STEPS = 12000
SAMPLE_STEPS = 50
TIMESTEP = 0.003
ATOM_COUNT = 864
FIT_START = (PRODUCTION_STEPS // SAMPLE_STEPS + 4) // 5
FIT_STOP = 4 * (PRODUCTION_STEPS // SAMPLE_STEPS) // 5 + 1
CSV_HEADER = (
    "time_reduced",
    "baseline_msd_1_reduced", "baseline_msd_2_reduced", "baseline_msd_3_reduced",
    "msd_1_reduced", "msd_2_reduced", "msd_3_reduced",
    "baseline_energy_1_reduced", "baseline_energy_2_reduced", "baseline_energy_3_reduced",
    "energy_1_reduced", "energy_2_reduced", "energy_3_reduced",
)


def executable():
    binary = distribution("lammps").locate_file("lammps/lmp")
    mpi_filename = "libmpi.so.12" if sys.platform == "linux" else "libmpi.12.dylib"
    mpi_library = next((entry.locate() for entry in files("mpich") or () if entry.name == mpi_filename), None)
    if not binary.is_file() or mpi_library is None or not mpi_library.is_file():
        raise ValueError("Missing pinned LAMMPS executable or MPICH library")
    loader_variable = "LD_LIBRARY_PATH" if sys.platform == "linux" else "DYLD_LIBRARY_PATH"
    # The wheel is built with MPI. Launch the engine directly, with one rank.
    environment = dict(os.environ, FI_PROVIDER="tcp", OMP_NUM_THREADS="1")
    environment[loader_variable] = str(mpi_library.resolve().parent)
    return binary, environment


def run_engine(binary, environment, directory, temperature, seed):
    bind_lifetime = None
    if sys.platform == "linux":
        parent = os.getpid()
        prctl = ctypes.CDLL(None, use_errno=True).prctl

        def bind_lifetime():
            # A supervisor cancellation or SIGKILL of the Python worker must
            # also stop this native child. The parent check closes the fork race.
            if prctl(1, signal.SIGKILL, 0, 0, 0) or os.getppid() != parent:
                os._exit(125)

    def cancel(signum, _frame):
        raise InterruptedError(f"LAMMPS run interrupted by signal {signum}")

    original = signal.signal(signal.SIGTERM, cancel)
    try:
        result = subprocess.run(
            [binary, "-log", "none", "-in", DECK, "-var", "temperature", str(temperature), "-var", "seed", str(seed)],
            cwd=directory, env=environment, stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, preexec_fn=bind_lifetime,
        )
    finally:
        signal.signal(signal.SIGTERM, original)
    if (result.returncode or f"LAMMPS ({ENGINE})" not in result.stdout
            or f"for {PRODUCTION_STEPS} steps with {ATOM_COUNT} atoms" not in result.stdout):
        raise ValueError("LAMMPS did not complete the declared one-rank liquid run")


def read_samples(path, temperature):
    samples = []
    with path.open() as stream:
        for line in stream:
            if line.startswith("#"):
                continue
            step, msd, energy, kinetic_temperature = line.split()
            values = tuple(map(float, (msd, energy, kinetic_temperature)))
            if int(step) != SAMPLE_STEPS * len(samples) or not all(map(math.isfinite, values)):
                raise ValueError("Invalid LAMMPS reporting grid or sample")
            samples.append(values)
    if len(samples) != PRODUCTION_STEPS // SAMPLE_STEPS + 1:
        raise ValueError("Incomplete LAMMPS displacement series")
    measured_temperature = math.fsum(row[2] for row in samples) / len(samples)
    if not math.isclose(measured_temperature, temperature, rel_tol=0.1):
        raise ValueError("Production temperature differs from the equilibrated target")
    return samples


def simulate(values, workspace):
    temperature = values["temperature_reduced"]
    temperatures = (BASELINE_TEMPERATURE, temperature)
    binary, environment = executable()
    runs = []
    with TemporaryDirectory(prefix="lammps-", dir=workspace) as temporary:
        for case_temperature, seeds in zip(temperatures, SEEDS):
            for seed in seeds:
                directory = Path(temporary) / str(seed)
                directory.mkdir()
                run_engine(binary, environment, directory, case_temperature, seed)
                runs.append(read_samples(directory / "samples.txt", case_temperature))

    import matplotlib.pyplot as plt
    import numpy as np

    samples = np.asarray(runs)
    msd, energy = samples[:, :, 0].T, samples[:, :, 1].T
    times = np.arange(len(msd)) * SAMPLE_STEPS * TIMESTEP
    fit = slice(FIT_START, FIT_STOP)
    coefficients = np.polyfit(times[fit], msd[fit], 1)[0] / 6
    drift = float(np.max(np.abs(energy - energy[0])))
    if np.any(coefficients <= 0) or not 0 <= drift <= 0.005:
        raise ValueError("LAMMPS production run failed the diffusion or energy check")
    with (workspace / "msd.csv").open("x", newline="") as stream:
        writer = csv.writer(stream)
        writer.writerow(CSV_HEADER)
        writer.writerows(np.column_stack((times, msd, energy)))

    figure, axes = plt.subplots(figsize=(8, 4.5), layout="constrained")
    axes.axvspan(times[FIT_START], times[FIT_STOP - 1], color="0.92", label="Fit window")
    for index, (case_temperature, color) in enumerate(zip(temperatures, ("tab:blue", "tab:orange"))):
        replicate_msd = msd[:, index * 3:(index + 1) * 3]
        average = replicate_msd.mean(axis=1)
        spread = replicate_msd.std(axis=1, ddof=1)
        axes.plot(times, average, color=color, label=f"Temperature {case_temperature:g}")
        axes.fill_between(times, average - spread, average + spread, color=color, alpha=0.17)
    axes.set(title="Self-diffusion in a Lennard-Jones liquid", xlabel="Time (reduced units)",
             ylabel="Mean-squared displacement (reduced units)", xlim=(times[0], times[-1]), ylim=(0, None))
    axes.grid(alpha=0.25)
    axes.legend(loc="upper left")
    figure.savefig(workspace / "diffusion.png", dpi=120)
    plt.close(figure)
    return {
        "baseline_diffusion_reduced": float(coefficients[:3].mean()),
        "diffusion_reduced": float(coefficients[3:].mean()),
        "baseline_diffusion_std_reduced": float(coefficients[:3].std(ddof=1)),
        "diffusion_std_reduced": float(coefficients[3:].std(ddof=1)),
        "max_energy_drift_per_particle_reduced": drift,
    }
