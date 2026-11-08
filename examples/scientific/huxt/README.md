# HUXt: a synthetic rotating fast stream

How does a faster stream of solar wind change the speed measured at Earth's
distance from the Sun? This HUXt 5.0.3 example models outward flow between 30
and 240 solar radii along one meridian. You can change the speed of either wind
or the width of the fast sector.

The model starts from an idealized wind speed profile. Use it to explore how
the model responds to that starting profile. It models wind speed alone.

## Run the fast-stream example

The default case sets the slow wind to 350 km/s, the fast stream to 700 km/s
and the sector width to 30°. [Download the sample result](sample-run.zip) to see
its data, plot and checks before installing anything. The extracted
examples archive includes `huxt/sample-run.zip` under `examples/scientific`.

To run the calculation yourself, [set up the solvers](https://doc.enumeray.com/models/local/#set-up-the-solvers)
and run this command from `examples/scientific`.

```sh
.venv/bin/python run_example.py huxt --directory huxt-default
```

If you have access to the [hosted trial](https://doc.enumeray.com/start/hosted-release/),
you can also run [`demo-huxt` with `fast-stream`](https://doc.enumeray.com/start/first-run/#huxt-solar-wind).

## Read the speed field and the 1 AU summary

Open these files in the sample ZIP or in `huxt-default` after a local run.

| File | What to look for |
| --- | --- |
| `wind.png` | The heatmap shows wind speed over distance and time. The line plot shows speed over time at 1 AU. |
| `wind-field.csv` | The 30,597 data rows give time in days, distance in AU and speed in km/s, sorted by time and then distance. |
| `outputs.json` | `min_speed_1au_kms`, `peak_speed_1au_kms` and `peak_time_days` summarize the line plot. |

In the default run, speed at 1 AU ranges from **401.0803 km/s** to a peak near
**802.1606 km/s** at **4.283708 days**. The model adds residual acceleration as
the wind travels outward, so the peak at 1 AU exceeds the starting fast speed.

The model estimates speed at 1 AU by interpolating between neighboring radial
cells. It reports the first saved time whose speed is within `1e-9` relative
of the maximum.

For a hosted run, `first-run/bundle.json` lists the local paths to the plot
and data. The summary is in `first-run/results/outputs.json`. Save
`first-run/results/provenance.json` with the files you need.

## Change the inputs

From `examples/scientific` in the extracted archive, open `huxt/inputs.json`.
Save a copy as `huxt-faster.json` beside `run_example.py`. Change only
`fast_speed_kms` from `700` to `750`, then run the comparison.

```sh
.venv/bin/python run_example.py huxt --inputs huxt-faster.json --directory huxt-faster
```

Compare `outputs.json` in the two directories. The higher starting speed raises
the peak at 1 AU from about **802.16 km/s** to **859.46 km/s** and moves it from
about **4.284 days** to **4.123 days**. Open both plots to see the change in the
speed over time. To use the edited file with a hosted model,
[start a new analysis](https://doc.enumeray.com/users/analyses/) with `--inputs huxt-faster.json`.

Keep all three fields and use finite JSON numbers in these ranges.

| Field | Meaning | Allowed range | Default |
| --- | --- | --- | --- |
| `slow_speed_kms` | Slow wind at the inner boundary, in km/s | 250–450 | 350 |
| `fast_speed_kms` | Fast wind at the inner boundary, in km/s | 500–900 | 700 |
| `stream_width_deg` | Fast sector width, in degrees of longitude | 10–45 | 30 |

The model maps the sector to 128 equal longitude bins, so a small change in
width may leave the selected bins the same.

## Numerical setup and checks

Each run covers seven days in the synodic frame. The fast sector is centered
at Carrington longitude 330° and the initial Carrington longitude is 360°.
The fixed CR2080 rotation makes coordinate and time calculations repeatable.
The model uses the grid meridian nearest zero, a radial step of 1.5 solar radii
and saves every eight solver time steps. This gives 217 saved times and 141
radial samples.

The test with uniform 350 km/s wind agrees with the analytic acceleration
solution `v(r)=v0*(1+0.15*(1-exp((r0-r)/(50 solar radii))))` to a maximum
fractional error of `1e-3`. For the default fast stream, the runner compares
the speeds at 1 AU with the [saved reference](reference.json). Each comparison
allows a difference of `1e-3 km/s` plus `1e-5` times the reference speed.

For both default and edited inputs, the runner recomputes the summary from the
CSV. The peak time must agree within `1e-8` days or a relative difference of
`1e-9`, whichever is larger. These checks test the numerical result for this
idealized setup. Observational accuracy requires a separate comparison with
measurements.

The wrapper allows one thread, 180 seconds and 8 MiB of combined CSV and PNG
output per run. For a hosted failure, [read the run logs](https://doc.enumeray.com/troubleshooting/execution/).

## Cite and distribute the example

Upstream HUXt uses the MIT license. Preserve its notices when distributing it. The Enumeray wrapper and case use Apache-2.0. The runtime manifest identifies the packaged ephemeris. This example uses no downloaded observational wind data.

- [HUXt 5.0.3 source and license](https://github.com/University-of-Reading-Space-Science/HUXt/tree/v5.0.3)
- [HUXt software record](https://doi.org/10.5281/zenodo.4889326)
- [Barnard and Owens (2022), HUXt](https://doi.org/10.3389/fphy.2022.1005621)
- [Owens et al. (2020), model formulation](https://doi.org/10.1007/s11207-020-01605-3)

For questions about the example, contact [Enumeray](mailto:enumeray@raysofspace.com).
