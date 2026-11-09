# HUXt: a synthetic rotating fast stream

<a id="run-the-fast-stream-example"></a>

Follow the [HUXt first-run walkthrough](https://doc.enumeray.com/start/first-run/#huxt-solar-wind)
to submit `demo-huxt` with `fast-stream` through Enumeray and collect the speed
plot and CSV.

HUXt 5.0.3 models solar-wind speed between 30 and 240 solar radii along one
meridian. The example starts from an idealized wind speed profile.

The default case sets the slow wind to 350 km/s, the fast stream to 700 km/s
and the sector width to 30°.

## Read the speed field and the 1 AU summary

After collection, open `first-run/bundle.json` to find the plot and CSV paths.
The summary is in `first-run/results/outputs.json`. Save
`first-run/results/provenance.json` with the files you need.

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

<a id="change-the-inputs"></a>

## Model inputs

To try different parameters through Enumeray, edit a copy of the
[example inputs](inputs.json) and [start a new analysis with those inputs](https://doc.enumeray.com/users/analyses/).
All three fields take finite JSON numbers.

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

### Optional reference files

The [HUXt reference files (ZIP)](sample-run.zip) contain a checked result for
the default inputs. Use the plot, CSV, numerical summary and run record to
compare against a collected result.

## Cite and distribute the example

Upstream HUXt uses the MIT license. Preserve its notices when distributing it. The Enumeray wrapper and case use Apache-2.0. The runtime manifest identifies the packaged ephemeris. This example uses no downloaded observational wind data.

- [HUXt 5.0.3 source and license](https://github.com/University-of-Reading-Space-Science/HUXt/tree/v5.0.3)
- [HUXt software record](https://doi.org/10.5281/zenodo.4889326)
- [Barnard and Owens (2022), HUXt](https://doi.org/10.3389/fphy.2022.1005621)
- [Owens et al. (2020), model formulation](https://doi.org/10.1007/s11207-020-01605-3)

For questions about the example, contact [Enumeray](mailto:enumeray@raysofspace.com).
