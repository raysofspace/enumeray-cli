# LAMMPS liquid self-diffusion

Use LAMMPS to see how temperature changes the motion of atoms in a model
liquid. You’ll compare how far atoms travel from their starting positions at
two temperatures, then raise the second temperature and compare the
estimated diffusion coefficients.

To run the hosted example, [install the CLI and sign in to Enumeray](https://doc.enumeray.com/start/first-run/).
These commands use the default profile. If you signed in with a named profile,
add `--profile NAME` after `enumeray` in each command.

## 1. Prepare and review the liquid model

From a directory where the name `lammps-run` is available, look up the model
and prepare an analysis with its `liquid-diffusion` example.

```sh
enumeray model get demo-lammps &&
  enumeray analysis init demo-lammps --directory lammps-run \
    --example liquid-diffusion
```

Open `lammps-run/inputs.json`. The example sets `temperature_reduced` to
`1.2` and compares it with a fixed baseline of `1.0`. Both values are in
reduced Lennard-Jones units. Check the model, publication, execution location
and disclosure before sending the inputs.

```sh
enumeray analysis status lammps-run
```

## 2. Submit and collect the run

```sh
enumeray analysis submit lammps-run &&
  enumeray analysis wait lammps-run --check-outcome --deadline 600 &&
  enumeray analysis collect lammps-run
```

The model can run for up to 120 seconds. Queueing and file transfers can add
time. If you lose the submit reply, repeat `analysis submit` with the same
folder to recover the original request. If the wait reaches its deadline,
run `analysis wait` again with the same folder. If collection stops before
**Collection complete: yes**, run `analysis collect lammps-run` again to
finish downloading the files. If the run fails,
[read its logs](https://doc.enumeray.com/troubleshooting/execution/).

<a id="read-a-collected-lammps-result"></a>

## 3. Read the diffusion estimate

If you’re following along without running the model, unzip the
[collected sample (ZIP)](https://doc.enumeray.com/examples/lammps-run-bundle.zip)
and use `lammps-run-bundle` wherever the steps below use `lammps-run`. The
sample contains the calculated outputs and illustrative run IDs and execution
details.

In `lammps-run/bundle.json`, look under `outputs` for the paths to the plot
and CSV. These paths are relative to the analysis folder.

Compare the plot with this example for the same inputs.

<figure class="docs-result-plot">

![Mean squared displacement rises faster at the higher temperature. The middle of the time axis is shaded to mark the fit interval.](https://doc.enumeray.com/examples/lammps-diffusion.png)

<figcaption>Each line is the mean across three replicas. Its band shows one sample standard deviation in displacement. The gray region marks the interval used for the diffusion fit.</figcaption>
</figure>

Mean squared displacement is the average, over the atoms, of the squared
distance from each atom's starting position. In three dimensions, the
diffusion coefficient is one sixth of the slope fitted to mean squared
displacement over time. The summary reports the mean and standard deviation
of the three replica estimates at each temperature.

The sample estimates a diffusion coefficient of **0.0596** at the baseline
temperature and **0.0869** at 1.2, in reduced units. The corresponding sample
standard deviations are 0.0034 and 0.0018. In `results/outputs.json`, compare
`baseline_diffusion_reduced` with `diffusion_reduced`.

| File | What to look for |
| --- | --- |
| `diffusion.png` (`diffusion_plot_png`) | Mean squared displacement by time for the two temperatures. |
| `msd.csv` (`msd_csv`) | 241 time samples with mean squared displacement and total energy per atom for three independent runs at each temperature. |
| `results/outputs.json` | Mean diffusion estimates, their sample standard deviations and the largest drift in total energy per atom. |
| `results/effective-inputs.json` and `results/provenance.json` | The input the model used, plus its publication, version and execution details. |

### Verify the files

Save the [result checker](https://doc.enumeray.com/examples/check-demo-result.py)
in the directory that contains `lammps-run`. Run it with Python 3.9 or later.

```sh
python3 check-demo-result.py lammps-run
```

The checker confirms that both files match the sizes and SHA-256 checksums in
`bundle.json`. It recomputes the diffusion estimates, their standard
deviations and the largest drift in total energy per atom from the CSV.

For the solver’s plot, CSV, summary and original check record in one folder,
use the [local solver reference (ZIP)](sample-run.zip).

## 4. Try a higher temperature

Keep the original analysis so you can compare the results. From its parent
directory, prepare a new analysis with the same example.

```sh
enumeray analysis init demo-lammps --directory lammps-warmer \
  --example liquid-diffusion
```

In `lammps-warmer/inputs.json`, change `temperature_reduced` from `1.2` to
`1.5`. Review the new analysis with `enumeray analysis status lammps-warmer`,
then [submit and collect](#2-submit-and-collect-the-run) using `lammps-warmer`
in place of `lammps-run`.

Compare the displacement curves and `results/outputs.json` in the two
folders. A steeper slope over the fitted interval corresponds to faster
diffusion. In the same reference environment, raising the temperature from
1.2 to 1.5 increased the mean diffusion estimate from 0.0869 to 0.0977.
The warmer run had a sample standard deviation of 0.0179.

<a id="change-the-inputs"></a>

## Model inputs

Supply one finite JSON number. You can start with the [example inputs](inputs.json)
and [run with your own inputs](https://doc.enumeray.com/users/run-model/).

| Field | Meaning | Allowed range | Named example value |
| --- | --- | --- | --- |
| `temperature_reduced` | Selected temperature in reduced Lennard-Jones units. | 1.1–1.6 | 1.2 |

## Numerical setup and checks

The wrapper uses LAMMPS 22 Jul 2025 Update 4. The [solver input](liquid.in)
sets up 256 atoms in a three-dimensional periodic box at a reduced number
density of 0.8. The pair potential is cut off at 2.5 times its length scale
and shifted so the potential is zero at that distance.

Each replica equilibrates at its target temperature for 4,000 steps, then
runs for 12,000 steps at constant volume and energy. The time step is 0.003
in reduced units. LAMMPS records displacement from the start of production
with center-of-mass drift removed and samples every 50 steps. The diffusion
fit uses times from 7.2 to 28.8, the middle 60% of the production interval.
The standard deviation describes the spread across the three replicas.

The offline verifier recalculates the diffusion estimates and energy drift
from the collected samples and compares fitted means for the
[reference cases](reference.json) to within 0.025 in reduced units. The total
energy per atom in each production run can drift by up to 0.005. Individual
trajectories can vary with the solver build and CPU, so compare fitted
diffusion estimates. The downloadable result and comparison above use the
locked runtime on Linux ARM64.

Reduced temperature is the ratio of thermal energy to the Lennard-Jones
energy scale. Mean squared displacement is in units of the length scale
squared. The time unit is set by the same length and energy scales and the
particle mass. The diffusion unit is the length scale squared divided by the
time unit. The estimates apply to the fixed periodic box used here because
[box size can affect the measured coefficient](https://doi.org/10.1021/jp0477147).
To interpret the result for a specific material, choose material parameters
and validate the model against measurements for that material.

### Repeat the scientific checks

Set `RESULT_DIR="$PWD/lammps-run"` from the directory containing your analysis
folder, or substitute the path to the unzipped sample. In the same shell,
[set up the locked Python environment](../README.md#set-up-the-solvers) in
`examples/scientific` and run the offline verifier.

```sh
.venv/bin/python verify_collected.py --model lammps --analysis "$RESULT_DIR"
```

Look for `"verified": true` and the diffusion checks.

## Cite and distribute the example

The Enumeray example wrapper uses Apache-2.0 and runs the GPLv2 LAMMPS
executable. Refer to the LAMMPS source for its notices.

- [Thompson et al., LAMMPS reference paper (2022)](https://doi.org/10.1016/j.cpc.2021.108171)
- [LAMMPS diffusion coefficient methods](https://docs.lammps.org/stable/Howto_diffusion.html)
- [LAMMPS reduced units](https://docs.lammps.org/stable/units.html)
- [LAMMPS source and license](https://github.com/lammps/lammps/tree/stable_22Jul2025_update4)

For questions about the example, contact [Enumeray](mailto:enumeray@raysofspace.com).
