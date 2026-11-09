# HUXt: a synthetic rotating fast stream

Use HUXt to see how a fast stream of solar wind moves through slower wind.
HUXt 5.0.3 models wind speed along one meridian between 30 and 240 solar radii.
You’ll run an idealized seven-day simulation, read the speed plot at Earth’s
orbital distance, then change the starting speed of the fast stream.

To run the hosted example, [install the CLI and sign in to Enumeray](https://doc.enumeray.com/start/first-run/).
These commands use the default profile. If you signed in with a named profile,
add `--profile NAME` after `enumeray` in each command.

<a id="run-the-fast-stream-example"></a>

## 1. Prepare and review the fast stream

From a directory where the name `huxt-run` is available, look up the model and
prepare an analysis with its `fast-stream` example.

```sh
enumeray model get demo-huxt &&
  enumeray analysis init demo-huxt --directory huxt-run \
    --example fast-stream
```

Open `huxt-run/inputs.json`. At the inner boundary, the slow wind starts at
350 km/s and the fast stream starts at 700 km/s. The stream spans 30° of
longitude. Check the model, publication, execution location and disclosure
before sending the inputs.

```sh
enumeray analysis status huxt-run
```

## 2. Submit and collect the run

```sh
enumeray analysis submit huxt-run &&
  enumeray analysis wait huxt-run --check-outcome --deadline 600 &&
  enumeray analysis collect huxt-run
```

The model can run for up to 180 seconds. Queueing and file transfers can add
time. If you lose the submit reply, repeat `analysis submit` with the same
folder to recover the original request. If the wait reaches its deadline,
run `analysis wait` again with the same folder. If collection stops before
**Collection complete: yes**, run `analysis collect huxt-run` again to finish
downloading the files. If the run fails, [read its logs](https://doc.enumeray.com/troubleshooting/execution/).

<a id="read-the-speed-field-and-the-1-au-summary"></a>
<a id="read-the-result"></a>

## 3. Read the speed field and the 1 AU summary

If you’re following along without running the model, unzip the
[collected sample (ZIP)](https://doc.enumeray.com/examples/huxt-run-bundle.zip)
and use `huxt-run-bundle` wherever the steps below use `huxt-run`. The sample
contains the calculated outputs and illustrative run IDs and execution
details.

<a id="read-a-collected-huxt-result"></a>

In `huxt-run/bundle.json`, look under `outputs` for the paths to the plot
and CSV. These paths are relative to the analysis folder and include artifact
IDs that vary by run. The CLI records whether the run succeeded in
`scientific_state`. The separate `complete` field says whether all the
recorded outputs were collected and verified.

Compare the plot with this example for the same inputs.

<figure class="docs-result-plot">

![A band of fast wind moves outward over time in the heatmap. The line plot at 1 AU rises from about 400 to about 802 kilometers per second before falling again.](https://doc.enumeray.com/examples/huxt-wind.png)

<figcaption>The top panel shows wind speed by distance and time. The bottom panel follows the speed at 1 AU, approximately Earth’s orbital distance.</figcaption>
</figure>

The wind speed at 1 AU ranges from **401.0803 km/s** to a peak of
**802.1606 km/s** on **day 4.283708**. The model adds residual acceleration as
the wind travels outward, so the peak exceeds the 700 km/s starting speed.
It estimates the speed at 1 AU by interpolating between neighboring radial
cells and reports the first saved time within `1e-9` relative of the maximum.

| File | What to look for |
| --- | --- |
| `wind.png` (`wind_plot_png`) | The wind speed over distance and time, and a time series at 1 AU. |
| `wind-field.csv` (`wind_field_csv`) | 30,597 rows of time in days, distance in AU and speed in km/s, sorted by time and then distance. |
| `results/outputs.json` | `min_speed_1au_kms`, `peak_speed_1au_kms` and `peak_time_days`. |
| `results/effective-inputs.json` and `results/provenance.json` | The inputs the model used, plus its publication, version and execution details. |

The CSV contains 217 saved times with 141 radial samples each. The solver
writes at fixed intervals, so the last saved time is day 6.957.

<a id="verify-the-downloaded-files"></a>

### Verify the files

Save the [result checker](https://doc.enumeray.com/examples/check-demo-result.py)
in the directory that contains `huxt-run`. Run it with Python 3.9 or later.

```sh
python3 check-demo-result.py huxt-run
```

The checker confirms that both files match the sizes and SHA-256 checksums
in `bundle.json`. It also interpolates the CSV at 1 AU and compares the peak
and its time with `results/outputs.json`.

For the solver’s plot, CSV, summary and original check record in one folder,
use the [local solver reference (ZIP)](sample-run.zip).

<a id="try-a-faster-solar-wind-stream"></a>

## 4. Try a faster solar wind stream

Keep the original analysis so you can compare the results. From its parent
directory, prepare a new analysis with the same example.

```sh
enumeray analysis init demo-huxt --directory huxt-faster --example fast-stream
```

In `huxt-faster/inputs.json`, change `fast_speed_kms` from `700` to `750`.
Review the new analysis with `enumeray analysis status huxt-faster`, then
[submit and collect](#2-submit-and-collect-the-run) using `huxt-faster` in
place of `huxt-run`.

Compare `results/outputs.json` in the two folders. The higher starting speed
raises the peak at 1 AU to about 859.46 km/s and brings it forward to day
4.123, compared with 802.16 km/s on day 4.284 for the original run.

<a id="change-the-inputs"></a>

## Model inputs

All three fields are required and take finite JSON numbers. You can start
with the [example inputs](inputs.json) and [run with your own inputs](https://doc.enumeray.com/users/run-model/).

| Field | Meaning | Allowed range | Named example value |
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
and saves every eight solver time steps. It checks that computed speeds are
finite.

The test with uniform 350 km/s wind agrees with the analytic acceleration
solution `v(r)=v0*(1+0.15*(1-exp((r0-r)/(50 solar radii))))` to a maximum
fractional error of `1e-3`. For the default fast stream, the offline scientific
verifier compares speeds at 1 AU with the [saved reference](reference.json).
Each comparison allows a difference of `1e-3 km/s` plus `1e-5` times the
reference speed.

For both default and edited inputs, the verifier recomputes the summary from
the CSV. The peak time must agree within `1e-8` days or a relative difference
of `1e-9`, whichever is larger. Observational accuracy requires a separate
comparison with measurements. The wrapper uses one thread and allows up to
8 MiB of combined CSV and PNG output.

<a id="full-scientific-checks"></a>

### Repeat the scientific checks

Set `RESULT_DIR="$PWD/huxt-run"` from the directory containing your analysis
folder, or substitute the path to the unzipped sample. In the same shell,
[set up the locked Python environment](../README.md#set-up-the-solvers) in
`examples/scientific` and run the offline verifier.

```sh
.venv/bin/python verify_collected.py --model huxt --analysis "$RESULT_DIR"
```

Look for `"verified": true`. The verifier checks file hashes and compares the
speed summary with the CSV. For the original inputs, it also compares the
speed series with the saved numerical reference.

## Cite and distribute the example

Upstream HUXt uses the MIT license. Preserve its notices when distributing it.
The Enumeray wrapper and case use Apache-2.0. The runtime manifest identifies
the packaged ephemeris. This example uses no downloaded observational wind
data.

- [HUXt 5.0.3 source and license](https://github.com/University-of-Reading-Space-Science/HUXt/tree/v5.0.3)
- [HUXt software record](https://doi.org/10.5281/zenodo.4889326)
- [Barnard and Owens (2022), HUXt](https://doi.org/10.3389/fphy.2022.1005621)
- [Owens et al. (2020), model formulation](https://doi.org/10.1007/s11207-020-01605-3)

For questions about the example, contact [Enumeray](mailto:enumeray@raysofspace.com).
