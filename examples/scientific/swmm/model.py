"""Run one synthetic urban catchment with the EPA SWMM engine."""
import csv
import math
from pathlib import Path
from tempfile import TemporaryDirectory

CATCHMENT = Path(__file__).with_name("catchment.inp")
AREA_M2 = 10_000
MINUTES = 360
REPORT_STEP_SECONDS = 60


def simulate_catchment(paved_percent, workspace):
    from swmm.toolkit import output, solver
    from swmm.toolkit.shared_enum import FlowUnits, SubcatchAttribute, Time, UnitSystem

    with TemporaryDirectory(prefix="swmm-", dir=workspace) as directory:
        run = Path(directory)
        source, report, results = (run / name for name in ("case.inp", "case.rpt", "case.out"))
        source.write_text(CATCHMENT.read_text().format(paved_percent=float(paved_percent)))
        solver.swmm_open(str(source), str(report), str(results))
        try:
            solver.swmm_start(True)
            while solver.swmm_step() > 0:
                pass
            # The toolkit returns depths in millimeters for this SI case.
            # Copy the values while the engine still owns the totals.
            totals = solver.system_get_runoff_totals()
            depth = totals.rainfall
            infiltration, runoff, storage = totals.infil, totals.runoff, totals.finalStorage
            if any((totals.evap, totals.runon, totals.initStorage)):
                raise ValueError("Unexpected water source or loss in the synthetic catchment")
            solver.swmm_end()
        finally:
            solver.swmm_close()

        handle = output.init()
        try:
            output.open(handle, str(results))
            count = output.get_times(handle, Time.NUM_PERIODS)
            if (count != MINUTES or output.get_times(handle, Time.REPORT_STEP) != REPORT_STEP_SECONDS
                    or output.get_units(handle)[:2] != [UnitSystem.SI.value, FlowUnits.LPS.value]):
                raise ValueError("Unexpected SWMM reporting grid or units")
            rain = [0.0] + output.get_subcatch_series(handle, 0, SubcatchAttribute.RAINFALL, 0, count - 1)
            flow = [0.0] + output.get_subcatch_series(handle, 0, SubcatchAttribute.RUNOFF_RATE, 0, count - 1)
        finally:
            output.close(handle)

    if not all(math.isfinite(value) and value >= 0 for value in (depth, infiltration, runoff, storage, *rain, *flow)):
        raise ValueError("Invalid SWMM runoff or water balance")
    rainfall_depth = math.fsum(rain[:-1]) / 60
    if (not math.isclose(depth, rainfall_depth, abs_tol=1e-8) or depth <= 0
            or abs(depth - infiltration - runoff - storage) > 1e-4 * depth):
        raise ValueError("SWMM internal runoff water balance does not close")

    rainfall_volume = rainfall_depth * AREA_M2 / 1000
    infiltration_volume, storage_volume = (value * AREA_M2 / 1000 for value in (infiltration, storage))
    runoff_volume = math.fsum((left + right) / 2 for left, right in zip(flow, flow[1:])) * REPORT_STEP_SECONDS / 1000
    balance_error = 100 * (rainfall_volume - runoff_volume - infiltration_volume - storage_volume) / rainfall_volume
    if abs(balance_error) > 0.1:
        raise ValueError("Sampled runoff water balance does not close")
    peak = max(flow)
    return rain, flow, {
        "peak_runoff_lps": peak,
        "peak_time_minutes": flow.index(peak),
        "runoff_volume_m3": runoff_volume,
        "rainfall_depth_mm": rainfall_depth,
        "infiltration_volume_m3": infiltration_volume,
        "final_surface_storage_m3": storage_volume,
        "runoff_balance_error_percent": balance_error,
    }


def simulate(values, workspace):
    import matplotlib.pyplot as plt

    workspace = Path(workspace)
    paved_percent = values["paved_percent"]
    rain, flow, outputs = simulate_catchment(paved_percent, workspace)
    times = range(MINUTES + 1)
    with (workspace / "runoff.csv").open("x", newline="") as stream:
        writer = csv.writer(stream)
        writer.writerow(("time_minutes", "rainfall_mm_per_hour", "runoff_lps"))
        writer.writerows(zip(times, rain, flow))

    figure, (rain_axes, flow_axes) = plt.subplots(2, 1, figsize=(8, 5), sharex=True, layout="constrained")
    rain_axes.step(times, rain, where="post", color="tab:blue")
    rain_axes.set(title=f"SWMM stormwater runoff ({paved_percent:g}% paved)", ylabel="Rain (mm/h)", ylim=(0, 30))
    flow_axes.plot(times, flow, color="tab:blue")
    flow_axes.scatter([outputs["peak_time_minutes"]], [outputs["peak_runoff_lps"]], color="tab:orange",
                      label="Peak runoff", zorder=3)
    flow_axes.set(xlabel="Time (minutes)", ylabel="Runoff (L/s)", xlim=(0, MINUTES), ylim=(0, 75))
    flow_axes.legend(loc="upper right")
    for axes in (rain_axes, flow_axes):
        axes.grid(alpha=0.3)
    figure.savefig(workspace / "runoff.png", dpi=120)
    plt.close(figure)
    return outputs
