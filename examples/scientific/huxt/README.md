# HUXt: a synthetic rotating fast stream

What happens when a fast stream of solar wind catches slower wind? This HUXt 5.0.3 example lets you vary the two speeds and the width of the fast sector. It models outward flow between 30 and 240 solar radii along one meridian.

The model starts from a synthetic wind speed profile. It illustrates wind propagation under a fixed set of assumptions. It excludes coronal mass ejections, magnetic fields, density, temperature and the other physics of a full magnetohydrodynamic simulation. Use it to explore how the model responds to different wind profiles.

## Run the fast-stream example

The hosted model ID is `demo-huxt` and the example name is `fast-stream`. Use the first-run guide to check whether the trial is available, install the matching CLI and sign in. The hosted connector supplies the solver and runtime. [Hosted trial](https://doc.enumeray.com/start/first-run/)

```sh
enumeray model get demo-huxt
enumeray analysis init demo-huxt --example fast-stream --directory first-run
```

Open `first-run/inputs.json`. The example sets the slow wind to 350 km/s, the fast stream to 700 km/s and the sector width to 30°. Review those values, the printed execution location and who can access the data. After review, submit and wait.

```sh
enumeray analysis submit first-run
enumeray analysis wait first-run --check-outcome
```

When the run succeeds, collect it with `enumeray analysis collect first-run`.

## Read the speed field and the 1 AU summary

Open `first-run/bundle.json` for the local paths to these outputs.

| Output name | What to open |
| --- | --- |
| `wind_field_csv` | `wind-field.csv` contains 30,597 rows of `time_days,radius_au,speed_kms`, sorted by time then radius. |
| `wind_plot_png` | `wind.png` shows a radius–time speed heatmap and a time series at 1 AU. |

The CLI saves each file under its own `results/files/<artifact-uuid>/` directory. The summary at `first-run/results/outputs.json` contains `min_speed_1au_kms`, `peak_speed_1au_kms` and `peak_time_days`. For the default inputs, expect a minimum near **401.0803 km/s** and a peak near **802.1606 km/s** at **4.283708 days**. The model includes prescribed residual acceleration, which helps explain why the peak near 1 AU is greater than the inner-boundary fast speed.

The summary interpolates speed at 1 AU between radial cells. The reported peak time is the first saved time whose speed is within `1e-9` relative of the maximum. Keep `first-run/results/provenance.json` with the data and plot. Demo data is temporary, so collect the files you need.

## Change the inputs

Edit a copy of this project's `inputs.json` and initialize another analysis directory with `--inputs inputs.json`. Keep all three fields and enter finite JSON numbers.

| Field | Meaning | Allowed range | Default |
| --- | --- | --- | --- |
| `slow_speed_kms` | Slow wind at the inner boundary, in km/s | 250–450 | 350 |
| `fast_speed_kms` | Fast wind at the inner boundary, in km/s | 500–900 | 700 |
| `stream_width_deg` | Fast sector width, in degrees of longitude | 10–45 | 30 |

The schema rejects missing or extra fields, strings, booleans and out-of-range values. The model maps the sector to 128 equal longitude bins, so a small change in width may leave the selected bins the same.

## Numerical setup and checks

Each run covers seven days in the synodic frame. The fast sector is centered at Carrington longitude 330° and the initial Carrington longitude is 360°. The fixed CR2080 rotation makes coordinate and time calculations repeatable. The longitude is the upstream grid meridian nearest zero. The radial step is 1.5 solar radii. The model saves every eight intrinsic CFL timesteps, giving 217 output times and 141 radial samples.

A uniform 350 km/s boundary agrees with the upstream analytic acceleration solution `v(r)=v0*(1+0.15*(1-exp((r0-r)/(50 solar radii))))` to a maximum fractional error of `1e-3`. The default fast-stream case checks the 1 AU values against `reference.json` with relative tolerance `1e-5` and absolute tolerance `1e-3 km/s`. These tests check numerical behavior and the wrapper's mapping of results. Observational accuracy would require a separate validation. Plot bytes may vary with plotting libraries and fonts.

The wrapper permits one thread, 180 seconds and 8 MiB combined CSV and PNG output per run. Solver errors, nonfinite values and exceeded limits produce a failed result. Read the run's failure and logs if that happens. Demo capacity is shared and account queue limits can delay admission. [Troubleshoot a run](https://doc.enumeray.com/troubleshooting/execution/)

## Cite and distribute the example

Upstream HUXt uses the MIT license. Preserve its notices when distributing it. The Enumeray wrapper and case use Apache-2.0. The runtime manifest identifies the packaged ephemeris. This example uses no downloaded observational wind data.

- [HUXt 5.0.3 source and license](https://github.com/University-of-Reading-Space-Science/HUXt/tree/v5.0.3)
- [HUXt software record](https://doi.org/10.5281/zenodo.4889326)
- [Barnard and Owens (2022), HUXt](https://doi.org/10.3389/fphy.2022.1005621)
- [Owens et al. (2020), model formulation](https://doi.org/10.1007/s11207-020-01605-3)

For questions about the example, contact [Enumeray](mailto:enumeray@raysofspace.com).
