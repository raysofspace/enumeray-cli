# REBOUND: an isolated Sun–planet orbit

**Hosted model:** `demo-rebound`. **Named example:** `earth-like-orbit`.
**Maintainer:** Enumeray, [enumeray@raysofspace.com](mailto:enumeray@raysofspace.com).

This case uses REBOUND 5.2.2 and its IAS15 integrator to solve an isolated
Newtonian two-body problem. The star has one solar mass and the planet has
`3.003e-6` solar masses. It illustrates how semimajor axis, eccentricity and
elapsed orbital periods change a planar trajectory. The planet starts at
periapse, the initial orbital plane is the xy plane and the calculation uses the
centre-of-mass frame. Reported positions and velocities are relative to the star.
It excludes other planets, relativistic effects, tides and collisions; it is not
an ephemeris or a fitted prediction of the real Earth.

After installing the current CLI and completing browser signup/email verification:

```sh
enumeray auth login
enumeray model get demo-rebound
enumeray analysis init demo-rebound --example earth-like-orbit --directory first-orbit
enumeray analysis submit first-orbit
enumeray analysis wait first-orbit --check-outcome
enumeray analysis collect first-orbit
```

Review the execution location/disclosure before submitting. For editable inputs,
copy this project's `inputs.json`, change its three fields and initialize a fresh
analysis using `--inputs inputs.json`:

| Field | Units | Range | Example |
| --- | --- | --- | --- |
| `semimajor_axis_au` | AU | 0.1–5 | 1 |
| `eccentricity` | dimensionless | 0–0.8 | 0.0167 |
| `orbits` | initial orbital periods | 0.25–10 | 2 |

All fields are required finite JSON numbers. Missing values, strings, booleans,
extra keys and out-of-range values are rejected. Integrator choice and accuracy,
particle count, masses, orientation and the **361** evenly spaced output times
are fixed. Units are set to years, AU and solar masses before adding particles;
REBOUND supplies the corresponding gravitational constant. IAS15 uses epsilon
`1e-9`. No random seed, external ephemeris or runtime download is involved.

The small JSON summary reports `period_years`, `periapse_au`, `apoapse_au`,
`max_relative_energy_error` and `max_relative_angular_momentum_error`.
The period and apsides describe the initial analytic relative orbit. Conservation
errors are the largest sampled fractional changes in specific orbital energy and
specific angular momentum. `bundle.json` maps:

- `trajectory_csv`: 361 rows with `time_years,x_au,y_au,vx_au_per_year,vy_au_per_year`.
- `orbit_plot_png`: the relative planar trajectory, with equal axis scales.

For new results, `bundle.json` points to `trajectory.csv` and `orbit.png` under
their respective `results/files/<artifact-uuid>/` directories.
Retain `results/outputs.json` and `results/provenance.json` with them. A partial
orbit ends before returning to its starting point by design. At high eccentricity,
equal time samples are spaced farther apart around periapse because the planet
moves faster there; the output spacing does not set IAS15's adaptive timestep.

Independent checks solve Kepler's equation `E-e*sin(E)=M` for each saved time.
Maximum relative position error divided by semimajor axis must be at most `1e-8`;
relative specific energy and angular momentum drift must each be at most `1e-9`.
Tests include circular and maximum-eccentricity cases and all parameter corners
in explicit qualification. These checks qualify this isolated problem and its
wrapper; they do not validate additional physics or a real planetary ephemeris.
PNG bytes can vary across plotting-library/font versions.

The wrapper permits one thread, 60 seconds and 8 MiB combined CSV/PNG output per run. A solver error,
nonfinite solution, failed conservation check or exhausted limit produces a
failed result. Preserve its analysis/logs; changed work needs a new analysis
directory. Demo compute is shared and queue limits apply. Collected local evidence
is the retained copy when the disposable demo installation is recreated.

REBOUND is GPL-3.0 licensed. Its installed distribution retains the upstream
license; distributing that runtime must satisfy its source obligations. This
repository's example wrapper is Apache-2.0 licensed. Use the following
scientific/software references for results:

- [REBOUND 5.2.2 source and license](https://github.com/hannorein/rebound/tree/5.2.2)
- [Rein and Liu (2012), REBOUND](https://doi.org/10.1051/0004-6361/201118085)
- [Rein and Spiegel (2015), IAS15](https://doi.org/10.1093/mnras/stu2164)
