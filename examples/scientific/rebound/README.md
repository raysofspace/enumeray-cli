# REBOUND: an isolated Sun–planet orbit

Use REBOUND to simulate two orbits of an Earth-mass planet around a one-solar-mass
star. You’ll collect the trajectory, check that the calculation conserves
energy and angular momentum, then make the orbit more elongated and compare
the distances.

To run the hosted example, [install the CLI and sign in to Enumeray](https://doc.enumeray.com/start/first-run/).
These commands use the default profile. If you signed in with a named profile,
add `--profile NAME` after `enumeray` in each command.

<a id="run-the-earth-like-orbit-example"></a>

## 1. Prepare and review the orbit

From a directory where the name `rebound-run` is available, look up the model
and prepare an analysis with its `earth-like-orbit` example.

```sh
enumeray model get demo-rebound &&
  enumeray analysis init demo-rebound --directory rebound-run \
    --example earth-like-orbit
```

Open `rebound-run/inputs.json`. The example sets the semimajor axis to 1
astronomical unit (AU), the eccentricity to 0.0167 and the duration to two
orbital periods. Check the model, publication, execution location and
disclosure before sending the inputs.

```sh
enumeray analysis status rebound-run
```

## 2. Submit and collect the run

```sh
enumeray analysis submit rebound-run &&
  enumeray analysis wait rebound-run --check-outcome --deadline 600 &&
  enumeray analysis collect rebound-run
```

The model can run for up to 60 seconds. Queueing and file transfers can add
time. If you lose the submit reply, repeat `analysis submit` with the same
folder to recover the original request. If the wait reaches its deadline,
run `analysis wait` again with the same folder. If collection stops before
**Collection complete: yes**, run `analysis collect rebound-run` again to finish
downloading the files. If the run fails, [read its logs](https://doc.enumeray.com/troubleshooting/execution/).

<a id="read-the-trajectory-and-check-the-orbit"></a>
<a id="check-the-result"></a>

## 3. Read the trajectory and check the orbit

If you’re following along without running the model, unzip the
[collected sample (ZIP)](sample-run-bundle.zip) and use `rebound-run-bundle`
wherever the steps below use `rebound-run`. The sample contains the
calculated outputs and illustrative run IDs and execution details.

In `rebound-run/bundle.json`, look under `outputs` for the paths to the plot
and CSV. These paths are relative to the analysis folder.

Compare the plot with this example for the same inputs.

<figure class="docs-result-plot">

![A nearly circular trajectory centered on the star. Both axes are in astronomical units.](https://doc.enumeray.com/examples/rebound-orbit.png)

<figcaption>The plot traces the planet’s position relative to the star, with equal scales on both axes.</figcaption>
</figure>

The planet starts at periapse, its closest point to the star. In this example
the orbit is nearly circular, with a period of about one year. The closest
distance is **0.9833 AU** and the farthest is **1.0167 AU**.

| File | What to look for |
| --- | --- |
| `orbit.png` (`orbit_plot_png`) | The planet’s path around the star. |
| `trajectory.csv` (`trajectory_csv`) | 361 samples with time in years, position in AU and velocity in AU per year. |
| `results/outputs.json` | The expected orbital period, the closest and farthest distances and the relative drift in energy and angular momentum. |
| `results/effective-inputs.json` and `results/provenance.json` | The inputs the model used, plus its publication, version and execution details. |

The fields `periapse_au` and `apoapse_au` in `outputs.json` come from the
initial orbital parameters. The closest and farthest points in the CSV also
depend on which times were saved. For instance, a quarter-orbit run ends
before the planet reaches its farthest point.

In `results/provenance.json`, look under `publication` for the execution
location and under `installation.model_identity` within that publication
for the solver version.

| Check | Example value |
| --- | --- |
| Expected orbital period | 1.000017 years, calculated from the semimajor axis and both masses. |
| Relative energy drift | `7.20e-16`, below the example’s `1e-9` tolerance. |
| Relative angular momentum drift | `2.83e-16`, below the same `1e-9` tolerance. |

<a id="verify-the-downloaded-files"></a>

### Verify the files

Save the [result checker](https://doc.enumeray.com/examples/check-demo-result.py)
in the directory that contains `rebound-run`. Run it with Python 3.9 or later.

```sh
python3 check-demo-result.py rebound-run
```

For the default two-orbit case, the checker confirms that both files match
the sizes and SHA-256 checksums in `bundle.json`. It calculates the distances
from the star using the x and y coordinates at each saved time. For these
inputs, it finds closest and farthest distances of 0.9833 and 1.0167 AU to
four decimal places.

For the solver’s plot, CSV, summary and original check record in one folder,
use the [local solver reference (ZIP)](sample-run.zip).

<a id="try-a-different-orbit"></a>

## 4. Try a different orbit

Keep the original analysis so you can compare the results. From its parent
directory, prepare a new analysis with the same example.

```sh
enumeray analysis init demo-rebound --directory rebound-eccentric --example earth-like-orbit
```

In `rebound-eccentric/inputs.json`, change `eccentricity` from `0.0167` to
`0.1`. Review the new analysis with `enumeray analysis status rebound-eccentric`,
then [submit and collect](#2-submit-and-collect-the-run) using `rebound-eccentric`
in place of `rebound-run`.

Compare the plots and `results/outputs.json` in the two folders. The closest
and farthest distances change to 0.9 and 1.1 AU. The period stays near one
year because the semimajor axis and both masses stay fixed. The samples are
evenly spaced in time, so their positions are farther apart where the planet
moves faster near periapse.

<a id="change-the-inputs"></a>

## Model inputs

All three fields are required and take finite JSON numbers. You can start
with the [example inputs](inputs.json) and [run with your own inputs](https://doc.enumeray.com/users/run-model/).

| Field | Meaning | Allowed range | Named example value |
| --- | --- | --- | --- |
| `semimajor_axis_au` | Half the long axis of the orbit, in AU | 0.1–5 | 1 |
| `eccentricity` | How far the shape departs from a circle, dimensionless | 0–0.8 | 0.0167 |
| `orbits` | Duration in initial orbital periods | 0.25–10 | 2 |

An `orbits` value less than one ends the calculation before the planet
completes one orbit.

## Numerical setup and checks

REBOUND 5.2.2 assumes an isolated Newtonian system and reports positions and
velocities relative to the star in the plane of the orbit. It uses years, AU
and solar masses, with a planet mass of `3.003e-6` solar masses. The IAS15
integrator uses epsilon `1e-9`. It integrates in the center-of-mass frame with
adaptive time steps. The output sampling interval is fixed separately.

The offline scientific verifier solves Kepler’s equation `E-e*sin(E)=M`
independently at each saved time and compares the position with the CSV.
The largest position difference, divided by the semimajor axis, must be at
most `1e-8`. The verifier computes the drift from the rounded CSV samples.
Relative drift in specific orbital energy and angular momentum must each be
at most `1e-9`. Predicting the position of the real Earth requires a model
that includes other influences. The wrapper uses one thread and allows up to
8 MiB of combined CSV and PNG output.

<a id="repeat-the-numerical-checks"></a>

### Repeat the scientific checks

Set `RESULT_DIR="$PWD/rebound-run"` from the directory containing your analysis
folder, or substitute the path to the unzipped sample. In the same shell,
[set up the locked Python environment](../README.md#set-up-the-solvers) in
`examples/scientific` and run the offline verifier.

```sh
.venv/bin/python verify_collected.py --model rebound --analysis "$RESULT_DIR"
```

Look for `"verified": true` and the measured Kepler, energy and angular
momentum errors.

## Cite and distribute the example

Upstream REBOUND uses GPL-3.0. Its installed distribution retains the upstream
license and redistribution carries its source obligations. The Enumeray example
wrapper uses Apache-2.0.

- [REBOUND 5.2.2 source and license](https://github.com/hannorein/rebound/tree/5.2.2)
- [Rein and Liu (2012), REBOUND](https://doi.org/10.1051/0004-6361/201118085)
- [Rein and Spiegel (2015), IAS15](https://doi.org/10.1093/mnras/stu2164)

For questions about the example, contact [Enumeray](mailto:enumeray@raysofspace.com).
