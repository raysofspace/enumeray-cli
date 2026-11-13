#!/usr/bin/env python3
"""Offline numerical qualification, with sanitized bounded local measurements."""
import argparse
import hashlib
from importlib.metadata import version
import itertools
import json
import os
from pathlib import Path
import sys
import tempfile
import time

from runtime import FILES, PARAMETERS, run_model, worker_environment
from verify_collected import load_model, verify_science

ROOT = Path(__file__).resolve().parent


def environment_receipt(models=tuple(FILES)):
    manifest = json.loads((ROOT / "runtime-manifest.json").read_text())
    if sys.version_info[:3] != (3, 12, 16):
        raise ValueError("Qualification requires the declared Python3.12.16 runtime")
    for package, expected in manifest["solvers"].items():
        if version(package) != expected:
            raise ValueError("Scientific solver version differs from runtime manifest")
    if hashlib.sha256((ROOT / "requirements.lock").read_bytes()).hexdigest() != manifest["requirements_sha256"]:
        raise ValueError("Scientific dependency lock differs from runtime manifest")
    if hashlib.sha256((ROOT / "build-requirements.lock").read_bytes()).hexdigest() != manifest["build_requirements_sha256"]:
        raise ValueError("Native build dependency lock differs from runtime manifest")
    from packaging.requirements import Requirement
    for line in (ROOT / "requirements.lock").read_text().splitlines():
        if not line or line[0].isspace() or line.startswith("#"):
            continue
        requirement = Requirement(line.removesuffix(" \\"))
        if (requirement.marker is None or requirement.marker.evaluate()) and version(requirement.name) not in requirement.specifier:
            raise ValueError("Installed scientific dependency differs from exact lock: " + requirement.name)
    import huxt
    ephemeris = Path(huxt.__file__).parent / "data/ephemeris/ephemeris.hdf5"
    if hashlib.sha256(ephemeris.read_bytes()).hexdigest() != manifest["huxt_ephemeris_sha256"]:
        raise ValueError("Bundled HUXt ephemeris differs from runtime manifest")
    receipt = {"python": "3.12.16", "solvers": manifest["solvers"],
               "huxt_numba_jit_enabled": manifest["huxt_numba_jit_enabled"],
               "requirements_sha256": manifest["requirements_sha256"],
               "build_requirements_sha256": manifest["build_requirements_sha256"],
               "huxt_ephemeris_sha256": manifest["huxt_ephemeris_sha256"]}
    if "swmm" in models:
        from swmm.toolkit import solver
        engine_version = solver.swmm_version_info()
        if engine_version != manifest["swmm_engine_version"]:
            raise ValueError("SWMM engine version differs from runtime manifest")
        catchment = ROOT / "swmm" / "catchment.inp"
        catchment_sha256 = hashlib.sha256(catchment.read_bytes()).hexdigest()
        if catchment_sha256 != manifest["swmm_catchment_sha256"]:
            raise ValueError("Bundled SWMM catchment and rainfall differ from runtime manifest")
        receipt.update(swmm_engine_version=engine_version, swmm_catchment_sha256=catchment_sha256)
    if "lammps" in models:
        lammps = load_model("lammps")
        deck_sha256 = hashlib.sha256(lammps.DECK.read_bytes()).hexdigest()
        if deck_sha256 != manifest["lammps_deck_sha256"]:
            raise ValueError("Bundled LAMMPS liquid input differs from runtime manifest")
        if lammps.ENGINE != manifest["lammps_engine_version"]:
            raise ValueError("LAMMPS engine version differs from runtime manifest")
        receipt.update(lammps_engine_version=lammps.ENGINE, lammps_deck_sha256=deck_sha256)
    return receipt


def uniform_wind_check():
    with tempfile.TemporaryDirectory(prefix="enumeray-analytic-") as temporary:
        environment = worker_environment(temporary, "huxt")
        previous = dict(os.environ)
        try:
            os.environ.update(environment)
            return _uniform_wind_check()
        finally:
            os.environ.clear()
            os.environ.update(previous)


def _uniform_wind_check():
    from astropy.utils import iers
    iers.conf.auto_download = False
    import astropy.units as u
    import numpy as np
    from huxt import huxt as H
    model = H.HUXt(v_boundary=np.ones(128) * 350 * u.km / u.s,
                   cr_num=2080, cr_lon_init=360 * u.deg, lon_out=0 * u.deg,
                   simtime=5 * u.day, dt_scale=8, track_cmes=False)
    model.solve([])
    expected = 350 * (1 + 0.15 * (1 - np.exp((model.r[0] - model.r).to_value(u.solRad) / 50)))
    error = float(np.max(np.abs(model.v_grid[:, :, 0].value - expected) / expected))
    if error > 1e-3:
        raise ValueError("HUXt uniform flow fails upstream analytic tolerance")
    return error


def qualify(models=tuple(FILES), extrema=False):
    environment = environment_receipt(models)
    checks = []
    for model in models:
        defaults = json.loads((ROOT / model / "inputs.json").read_text())
        cases = [("example", defaults)]
        if model == "swmm":
            cases.append(("more-paved", {"paved_percent": 60}))
        if model == "lammps":
            cases.append(("warmer-liquid", {"temperature_reduced": 1.5}))
        if extrema:
            keys = list(PARAMETERS[model])
            cases += [("boundary-" + str(index), dict(zip(keys, values))) for index, values in
                      enumerate(itertools.product(*(PARAMETERS[model][key] for key in keys)))]
        for name, inputs in cases:
            with tempfile.TemporaryDirectory(prefix="enumeray-science-") as temporary:
                started = time.monotonic()
                outputs, files = run_model(model, inputs, Path(temporary))
                elapsed = time.monotonic() - started
                metrics = json.loads((Path(temporary) / ".scientific/metrics.json").read_text())
                elapsed_limit = 60 if name == "example" else 120
                rss = metrics["peak_rss_bytes"]
                rss_limit = 1024 ** 3
                if elapsed > elapsed_limit or rss > rss_limit:
                    raise ValueError(f"{model}/{name} exceeds qualification resource budget "
                                     f"(elapsed {elapsed:.3f} s, limit {elapsed_limit} s; "
                                     f"peak RSS {rss} bytes, limit {rss_limit} bytes)")
                science = verify_science(model, inputs, outputs,
                                         {slot: Path(temporary) / value["path"] for slot, value in files.items()})
                checks.append({"model": model, "case": name, "elapsed_seconds": round(elapsed, 3),
                               **metrics, "checks": science})
    return {"format_version": 1, "qualified": True, "environment": environment,
            "cases": checks, "huxt_uniform_max_fractional_error": uniform_wind_check() if "huxt" in models else None}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", choices=(*FILES, "all"), default="all")
    parser.add_argument("--extrema", action="store_true")
    args = parser.parse_args()
    models = tuple(FILES) if args.model == "all" else (args.model,)
    print(json.dumps(qualify(models, args.extrema), indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
