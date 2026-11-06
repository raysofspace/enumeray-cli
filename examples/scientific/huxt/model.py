"""Synthetic ambient fast-stream case using the unmodified HUXt solver."""
import csv
from pathlib import Path


def solve(values):
    from astropy.utils import iers
    iers.conf.auto_download = False
    import astropy.units as u
    import numpy as np
    from huxt import huxt as H
    longitude = (np.arange(128) + 0.5) * 360 / 128
    distance = np.abs((longitude - 330 + 180) % 360 - 180)
    boundary = np.where(distance <= values["stream_width_deg"] / 2,
                        values["fast_speed_kms"], values["slow_speed_kms"])
    model = H.HUXt(v_boundary=boundary * u.km / u.s, cr_num=2080,
                   cr_lon_init=360 * u.deg, lon_out=0 * u.deg,
                   r_min=30 * u.solRad, r_max=240 * u.solRad,
                   simtime=7 * u.day, dt_scale=8, frame="synodic", track_cmes=False)
    model.solve([])
    return model


def simulate(values, workspace):
    import astropy.units as u
    import matplotlib.pyplot as plt
    import numpy as np
    model = solve(values)
    times = model.time_out.to_value(u.day)
    radii = model.r.to_value(u.au)
    speed = model.v_grid[:, :, 0].to_value(u.km / u.s)
    if not np.all(np.isfinite(speed)):
        raise ValueError("Nonfinite wind solution")
    at_au = np.array([np.interp(1, radii, row) for row in speed])
    with (Path(workspace) / "wind-field.csv").open("x", newline="") as output:
        writer = csv.writer(output)
        writer.writerow(("time_days", "radius_au", "speed_kms"))
        for time, row in zip(times, speed):
            writer.writerows((f"{time:.15g}", f"{radius:.15g}", f"{value:.15g}")
                             for radius, value in zip(radii, row))
    figure, axes = plt.subplots(2, 1, figsize=(8, 7), layout="constrained")
    mesh = axes[0].pcolormesh(times, radii, speed.T, shading="auto", cmap="viridis")
    axes[0].set(xlabel="Elapsed time (days)", ylabel="Heliocentric radius (AU)",
                title="HUXt synthetic rotating fast stream")
    figure.colorbar(mesh, ax=axes[0], label="Solar wind speed (km/s)")
    axes[1].plot(times, at_au)
    axes[1].set(xlabel="Elapsed time (days)", ylabel="Speed at 1 AU (km/s)")
    axes[1].grid(alpha=0.3)
    figure.savefig(Path(workspace) / "wind.png", dpi=120)
    plt.close(figure)
    first_peak = int(np.flatnonzero(at_au >= at_au.max() * (1 - 1e-9))[0])
    return {"min_speed_1au_kms": float(at_au.min()), "peak_speed_1au_kms": float(at_au.max()),
            "peak_time_days": float(times[first_peak])}
