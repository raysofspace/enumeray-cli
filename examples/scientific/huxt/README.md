# HUXt: a synthetic rotating fast stream

**Hosted model:** `demo-huxt`. **Named example:** `fast-stream`.
**Maintainer:** Enumeray, [enumeray@raysofspace.com](mailto:enumeray@raysofspace.com).

This example uses HUXt 5.0.3 to show how faster solar wind catches slower wind
between 30 and 240 solar radii. It solves the upstream reduced-physics radial
flow model on one meridian. The inner boundary is a prescribed synthetic
longitude profile, rather than a magnetogram-derived or observed boundary.
It is useful for exploring the effect of wind speeds and sector width. It is
an illustrative simulation with no observational forecast validation, CME,
magnetic field, plasma density, temperature or full MHD solution.

After installing the current CLI and completing browser signup/email verification:

```sh
enumeray auth login
enumeray model get demo-huxt
enumeray analysis init demo-huxt --example fast-stream --directory first-wind
enumeray analysis submit first-wind
enumeray analysis wait first-wind --check-outcome
enumeray analysis collect first-wind
```

Review the model's execution location and disclosure before submitting. To change
the science, edit a copy of this project's `inputs.json` and initialize a new
analysis with `--inputs inputs.json`. Keep all three required fields:

| Field | Units | Range | Example |
| --- | --- | --- | --- |
| `slow_speed_kms` | km/s at the inner boundary | 250–450 | 350 |
| `fast_speed_kms` | km/s at the inner boundary | 500–900 | 700 |
| `stream_width_deg` | degrees of longitude | 10–45 | 30 |

Values must be finite JSON numbers. Missing values, strings, booleans, extra keys
and out-of-range values are rejected. Positive fast-wind contrast is guaranteed
by the separate ranges. Sector width is discretized onto 128 equal longitude
bins: changing the requested width slightly may select the same bins.

The run always covers seven days in the synodic frame, with a fast sector centred
at Carrington longitude 330°, initial Carrington longitude 360° and fixed CR2080
for deterministic coordinate/time calculations. Longitude is the upstream grid
meridian nearest zero. The radial step is 1.5 solar radii; output is saved every
eight intrinsic CFL timesteps. There are 217 output times and 141 radial samples.
The date is a fixed numerical setup, not a claim to reproduce a historical event.

`results/outputs.json` contains `min_speed_1au_kms`, `peak_speed_1au_kms` and
`peak_time_days`. Speed at 1 AU is linearly interpolated between radial cells;
time of peak is the first saved value within `1e-9` relative of the maximum. `bundle.json` maps:

- `wind_field_csv` to a CSV of `time_days,radius_au,speed_kms`, ordered by time then
  radius. All values are finite; there are 30,597 data rows.
- `wind_plot_png` to a radius/time speed heatmap and interpolated 1-AU time series.

The downloaded artifacts are stored under `results/files/<artifact-uuid>`; use
the bundle mapping to locate them. Preserve `results/provenance.json` with the
scientific files. A larger speed near 1 AU than at the boundary includes HUXt's
prescribed residual acceleration; these quantities are not interchangeable.
The default reference has a minimum near **401.0803 km/s**, a peak near
**802.1606 km/s**, first saved at **4.283708 days**.

Numerical validation has two checks. The uniform 350-km/s boundary agrees with
the upstream analytic acceleration solution
`v(r)=v0*(1+0.15*(1-exp((r0-r)/(50 solar radii))))` to a maximum fractional error
of `1e-3`. The default structured case is compared with the maintained 1-AU
reference in `reference.json` using `rtol=1e-5, atol=1e-3 km/s`.
These establish numerical regression and the wrapper mapping; they do not establish
observational accuracy. PNG bytes may vary with plotting libraries and fonts.

The wrapper permits one thread, 180 seconds and 8 MiB combined CSV/PNG output per run. A failed solver,
nonfinite output, exhausted limit or timeout produces a failed scientific result;
inspect its failure and logs. Cancellation is a service operation. Shared demo
capacity and account queue limits can delay admission. Demo data is disposable
and disappears when the installation is recreated; collect anything to retain.

Upstream HUXt is MIT licensed; preserve its notices when distributing it.
Enumeray's wrapper and case use this repository's Apache-2.0 license. The packaged
ephemeris is identified in the runtime manifest; this synthetic case consumes no
downloaded observational wind data. Cite the software and scientific model when
using results:

- [HUXt 5.0.3 source and license](https://github.com/University-of-Reading-Space-Science/HUXt/tree/v5.0.3)
- [HUXt software record](https://doi.org/10.5281/zenodo.4889326)
- [Barnard and Owens (2022), HUXt](https://doi.org/10.3389/fphy.2022.1005621)
- [Owens et al. (2020), model formulation](https://doi.org/10.1007/s11207-020-01605-3)
