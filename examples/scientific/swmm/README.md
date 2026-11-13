# SWMM stormwater runoff

Use the EPA Storm Water Management Model (SWMM) to see how paving changes
runoff during a rainstorm. You’ll run the same synthetic storm over a one-hectare
catchment with 25% and 60% paved area, then compare the peak flow and total
runoff volume.

To run the hosted example, [install the CLI and sign in to Enumeray](https://doc.enumeray.com/start/first-run/).
These commands use the default profile. If you signed in with a named profile,
add `--profile NAME` after `enumeray` in each command.

## 1. Prepare and review the rainstorm

From a directory where the name `swmm-run` is available, look up the model and
prepare an analysis with its `synthetic-rainstorm` example.

```sh
enumeray model get demo-swmm &&
  enumeray analysis init demo-swmm --directory swmm-run \
    --example synthetic-rainstorm
```

Open `swmm-run/inputs.json`. The example sets `paved_percent` to `25`. The
model supplies the rainfall and catchment. Check the model, publication,
execution location and disclosure before sending the inputs.

```sh
enumeray analysis status swmm-run
```

## 2. Submit and collect the run

```sh
enumeray analysis submit swmm-run &&
  enumeray analysis wait swmm-run --check-outcome --deadline 600 &&
  enumeray analysis collect swmm-run
```

The model can run for up to 60 seconds. Queueing and file transfers can add
time. If you lose the submit reply, repeat `analysis submit` with the same
folder to recover the original request. If the wait reaches its deadline,
run `analysis wait` again with the same folder. If collection stops before
**Collection complete: yes**, run `analysis collect swmm-run` again to finish
downloading the files. If the run fails, [read its logs](https://doc.enumeray.com/troubleshooting/execution/).

<a id="read-a-collected-swmm-result"></a>

## 3. Read the rainfall and runoff

If you’re following along without running the model, unzip the
[collected sample (ZIP)](https://doc.enumeray.com/examples/swmm-run-bundle.zip)
and use `swmm-run-bundle` wherever the steps below use `swmm-run`. The sample
contains the calculated outputs and illustrative run IDs and execution details.

In `swmm-run/bundle.json`, look under `outputs` for the paths to the plot
and CSV. These paths are relative to the analysis folder.

Compare the plot with this example for the same inputs.

<figure class="docs-result-plot">

![Rain falls for one hour. Runoff rises during the storm and falls after the rain ends.](https://doc.enumeray.com/examples/swmm-runoff.png)

<figcaption>The top panel shows rainfall intensity. The bottom panel shows runoff leaving the catchment over the six-hour simulation.</figcaption>
</figure>

The storm delivers **25 mm** of rain over one hour. At 25% paved area, runoff
peaks at **39.50 L/s** at minute **75**, when the rain ends. The catchment
produces **102.61 m³** of runoff over the full six hours.

| File | What to look for |
| --- | --- |
| `runoff.png` (`runoff_plot_png`) | Rainfall intensity and runoff flow by time. |
| `runoff.csv` (`runoff_csv`) | 361 samples with time in minutes, rainfall intensity in mm/h and runoff in L/s. |
| `results/outputs.json` | Rainfall depth, peak runoff, its time, total runoff volume, infiltration, remaining surface storage and the runoff balance error. |
| `results/effective-inputs.json` and `results/provenance.json` | The inputs the model used, plus its publication, version and execution details. |

Each rainfall value applies to the minute beginning at that sample. The peak
is the largest sampled flow. The runoff volume is the area under the sampled
flow curve. The runoff balance error compares the rain that fell with the
water that ran off, soaked into the ground or remained on the surface.

### Verify the files

Save the [result checker](https://doc.enumeray.com/examples/check-demo-result.py)
in the directory that contains `swmm-run`. Run it with Python 3.9 or later.

```sh
python3 check-demo-result.py swmm-run
```

The checker confirms that both files match the sizes and SHA-256 checksums
in `bundle.json`. It also calculates the rainfall depth, peak runoff, peak time
and runoff volume from the CSV. It uses SWMM’s infiltration and surface storage
totals in `results/outputs.json` to check the runoff balance.

For the solver’s plot, CSV, summary and original check record in one folder,
use the [local solver reference (ZIP)](sample-run.zip).

## 4. Try a catchment with more paving

Keep the original analysis so you can compare the results. From its parent
directory, prepare a new analysis with the same example.

```sh
enumeray analysis init demo-swmm --directory swmm-more-paved \
  --example synthetic-rainstorm
```

In `swmm-more-paved/inputs.json`, change `paved_percent` from `25` to `60`.
Review the new analysis with `enumeray analysis status swmm-more-paved`, then
[submit and collect](#2-submit-and-collect-the-run) using `swmm-more-paved` in
place of `swmm-run`.

Compare the plots and `results/outputs.json` in the two folders. With more
paved area, less rain can soak into the ground, so more water leaves the
catchment as runoff.

| Paved area | Rainfall depth | Peak runoff | Total runoff volume |
| --- | --- | --- | --- |
| 25% | 25 mm | 39.50 L/s at minute 75 | 102.61 m³ |
| 60% | 25 mm | 57.76 L/s at minute 75 | 171.62 m³ |

<a id="change-the-inputs"></a>

## Model inputs

Supply one finite JSON number. You can start with the [example inputs](inputs.json)
and [run with your own inputs](https://doc.enumeray.com/users/run-model/).

| Field | Meaning | Allowed range | Named example value |
| --- | --- | --- | --- |
| `paved_percent` | Share of the catchment that is impervious, as a percentage. | 0–100 | 25 |

## Numerical setup and checks

The wrapper uses the SWMM 5.2.4 engine through SWMM Toolkit 0.17.0. It simulates
a one-hectare catchment with a 1% slope and uses Horton infiltration for the
unpaved area. The [model configuration](catchment.inp) includes the storm, which
starts at minute 15 and ends at minute 75. The simulation runs for six hours
and saves flow once a minute. The model reports runoff leaving the catchment
before any routing through streets or pipes.

The offline verifier compares the 25% and 60% hydrographs with saved
references. Each flow comparison allows `1e-4 L/s` plus `1e-5` times the
reference flow. For any allowed paved fraction, the verifier recomputes the
summary from the CSV and checks that the sampled runoff water balance closes
within `0.1%`. The wrapper also checks SWMM’s internal water balance to within
`0.01%`. It uses one thread and allows up to 8 MiB of combined CSV and PNG
output. Estimating runoff at an actual site would require site measurements
and a model set up for that catchment.

### Repeat the scientific checks

Set `RESULT_DIR="$PWD/swmm-run"` from the directory containing your analysis
folder, or substitute the path to the unzipped sample. In the same shell,
[set up the locked Python environment](../README.md#set-up-the-solvers) in
`examples/scientific` and run the offline verifier.

```sh
.venv/bin/python verify_collected.py --model swmm --analysis "$RESULT_DIR"
```

Look for `"verified": true` and the runoff checks.

## Cite and distribute the example

The Enumeray example wrapper uses Apache-2.0. Refer to the solver and toolkit
sources for their notices.

- [EPA Storm Water Management Model](https://www.epa.gov/water-research/storm-water-management-model-swmm)
- [EPA SWMM 5.2 user’s manual](https://www.epa.gov/system/files/documents/2022-04/swmm-users-manual-version-5.2.pdf)
- [SWMM Toolkit 0.17.0 source and licenses](https://github.com/pyswmm/swmm-python/tree/v0.17.0)

For questions about the example, contact [Enumeray](mailto:enumeray@raysofspace.com).
