"""Isolated planar Sun/planet problem with an analytic Kepler reference."""
import csv
import math
from pathlib import Path

SAMPLES = 361
PLANET_MASS = 3.003e-6


def trajectory(values):
    import numpy as np
    import rebound
    simulation = rebound.Simulation()
    simulation.units = ("yr", "AU", "Msun")
    simulation.integrator = "ias15"
    simulation.integrator.epsilon = 1e-9
    simulation.add(m=1)
    simulation.add(m=PLANET_MASS, a=values["semimajor_axis_au"], e=values["eccentricity"],
                   inc=0, Omega=0, omega=0, f=0)
    simulation.move_to_com()
    mu = simulation.G * (1 + PLANET_MASS)
    period = 2 * math.pi * math.sqrt(values["semimajor_axis_au"] ** 3 / mu)
    times = np.linspace(0, values["orbits"] * period, SAMPLES)
    rows = []
    for time in times:
        simulation.integrate(float(time), exact_finish_time=1)
        sun, planet = simulation.particles
        rows.append((time, planet.x - sun.x, planet.y - sun.y,
                     planet.vx - sun.vx, planet.vy - sun.vy))
    return np.asarray(rows), period, mu


def diagnostics(rows, mu):
    import numpy as np
    radius = np.hypot(rows[:, 1], rows[:, 2])
    energy = (rows[:, 3] ** 2 + rows[:, 4] ** 2) / 2 - mu / radius
    momentum = rows[:, 1] * rows[:, 4] - rows[:, 2] * rows[:, 3]
    return float(np.max(np.abs((energy - energy[0]) / energy[0]))), float(
        np.max(np.abs((momentum - momentum[0]) / momentum[0])))


def analytic_xy(values, times, period):
    import numpy as np
    mean = np.remainder(2 * np.pi * times / period, 2 * np.pi)
    eccentric = mean.copy()
    e = values["eccentricity"]
    for _ in range(30):
        delta = (eccentric - e * np.sin(eccentric) - mean) / (1 - e * np.cos(eccentric))
        eccentric -= delta
        if np.max(np.abs(delta)) < 1e-14:
            break
    a = values["semimajor_axis_au"]
    return np.column_stack((a * (np.cos(eccentric) - e),
                            a * math.sqrt(1 - e * e) * np.sin(eccentric)))


def simulate(values, workspace):
    import matplotlib.pyplot as plt
    import numpy as np
    rows, period, mu = trajectory(values)
    energy, momentum = diagnostics(rows, mu)
    if not np.all(np.isfinite(rows)) or energy > 1e-9 or momentum > 1e-9:
        raise ValueError("Orbit fails conservation checks")
    with (Path(workspace) / "trajectory.csv").open("x", newline="") as output:
        writer = csv.writer(output)
        writer.writerow(("time_years", "x_au", "y_au", "vx_au_per_year", "vy_au_per_year"))
        writer.writerows(tuple(f"{value:.15g}" for value in row) for row in rows)
    figure, axes = plt.subplots(figsize=(7, 6), layout="constrained")
    axes.plot(rows[:, 1], rows[:, 2], label="Planet relative to Sun")
    axes.scatter([0], [0], color="orange", marker="*", s=150, label="Sun")
    axes.set(xlabel="x (AU)", ylabel="y (AU)", title="REBOUND isolated two-body orbit", aspect="equal")
    axes.grid(alpha=0.3)
    axes.legend()
    figure.savefig(Path(workspace) / "orbit.png", dpi=120)
    plt.close(figure)
    a, e = values["semimajor_axis_au"], values["eccentricity"]
    return {"period_years": period, "periapse_au": a * (1 - e), "apoapse_au": a * (1 + e),
            "max_relative_energy_error": energy, "max_relative_angular_momentum_error": momentum}
