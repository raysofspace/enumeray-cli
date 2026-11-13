# Meep photonic-crystal filter

Use Meep to see how a small change in a waveguide changes which wavelength
gets through. You’ll measure the transmission peak of a two-dimensional
photonic-crystal cavity, widen the gap between its two central holes, and
compare the two spectra.

To run the hosted example, [install the CLI and sign in to Enumeray](https://doc.enumeray.com/start/first-run/).
These commands use the default profile. If you signed in with a named profile,
add `--profile NAME` after `enumeray` in each command.

## 1. Prepare and review the cavity

From a directory where the name `meep-run` is available, look up the model
and prepare an analysis with its `cavity-transmission` example.

```sh
enumeray model get demo-meep &&
  enumeray analysis init demo-meep --directory meep-run \
    --example cavity-transmission
```

Open `meep-run/inputs.json`. The example places the centers of the two
innermost holes 1.4 lattice periods apart and uses 26 grid cells per period.
Check the model, publication, execution location and disclosure before
sending the inputs.

```sh
enumeray analysis status meep-run
```

## 2. Submit and collect the run

```sh
enumeray analysis submit meep-run &&
  enumeray analysis wait meep-run --check-outcome --deadline 600 &&
  enumeray analysis collect meep-run
```

The model can run for up to 120 seconds. Queueing and file transfers can add
time. If you lose the submit reply, repeat `analysis submit` with the same
folder to recover the original request. If the wait reaches its deadline,
run `analysis wait` again with the same folder. If collection stops before
**Collection complete: yes**, run `analysis collect meep-run` again to finish
downloading the files. If the run fails,
[read its logs](https://doc.enumeray.com/troubleshooting/execution/).

<a id="read-a-collected-meep-result"></a>

## 3. Read the transmission peak

If you’re following along without running the model, unzip the
[collected sample (ZIP)](https://doc.enumeray.com/examples/meep-run-bundle.zip)
and use `meep-run-bundle` wherever the steps below use `meep-run`. The sample
contains the calculated outputs and illustrative run IDs and execution details.

In `meep-run/bundle.json`, look under `outputs` for the paths to the plot and
CSV. These paths are relative to the analysis folder.

Compare the plot with this example for the same inputs.

<figure class="docs-result-plot">

![The transmission spectrum has a narrow peak near 4.26 lattice periods. The field snapshot shows the waveguide and its six holes.](https://doc.enumeray.com/examples/meep-transmission.png)

<figcaption>The upper panel marks the largest sampled transmission inside the resonance search band. The lower panel shows the out-of-plane magnetic field at one instant, with the dielectric boundaries outlined.</figcaption>
</figure>

The holes form two partial mirrors with a gap between them. Light close to
the cavity resonance passes through the structure. In the sample, the peak
is at **4.2626 lattice periods**, with a sampled transmitted fraction of
**0.7999**. The field snapshot shows where the field remains after the pulse
has passed through the guide.

| File | What to look for |
| --- | --- |
| `transmission.png` (`transmission_plot_png`) | The transmission spectrum and a transient magnetic field snapshot. |
| `spectrum.csv` (`spectrum_csv`) | 401 frequency samples with wavelength, straight-guide flux, cavity flux and their ratio. |
| `results/outputs.json` | The frequency, wavelength and transmitted fraction at the sampled peak. |
| `results/effective-inputs.json` and `results/provenance.json` | The inputs the model used, plus its publication, version and execution details. |

### Verify the files

Save the [result checker](https://doc.enumeray.com/examples/check-demo-result.py)
in the directory that contains `meep-run`. Run it with Python 3.9 or later.

```sh
python3 check-demo-result.py meep-run
```

The checker confirms that both files match the sizes and SHA-256 checksums in
`bundle.json`. It recomputes the transmission ratio, wavelength and peak from
the CSV and compares the peak with `results/outputs.json`.

For the solver’s plot, CSV, summary and original check record in one folder,
use the [local solver reference (ZIP)](sample-run.zip).

## 4. Try a wider cavity

Keep the original analysis so you can compare the results. From its parent
directory, prepare a new analysis with the same example.

```sh
enumeray analysis init demo-meep --directory meep-wider \
  --example cavity-transmission
```

In `meep-wider/inputs.json`, change `cavity_spacing_periods` from `1.4` to
`1.5`. Review the new analysis with `enumeray analysis status meep-wider`,
then [submit and collect](#2-submit-and-collect-the-run) using `meep-wider`
in place of `meep-run`.

Compare `peak_wavelength_over_a` in both result summaries. Widening the cavity
moves the sample peak from about **4.263** to **4.525** lattice periods.

### Check the grid resolution

Prepare an analysis with `--example finer-grid` and use a fresh directory.
This example keeps the 1.4-period spacing and increases the resolution to
30 cells per period. The reference wavelength at that resolution is about
4.259 periods. Set the spacing to `1.5` in another fresh analysis with the
same finer-grid example to compare the shift at both resolutions.

<a id="change-the-inputs"></a>

## Model inputs

Start with the [example inputs](inputs.json) or
[run with your own inputs](https://doc.enumeray.com/users/run-model/).

| Field | Meaning | Allowed values | Named example value |
| --- | --- | --- | --- |
| `cavity_spacing_periods` | Center-to-center distance between the innermost holes, divided by the lattice period. | 1.3–1.5 | 1.4 |
| `grid_cells_per_period` | Number of finite-difference grid cells in one lattice period. | 26 or 30 | 26 |

## Numerical setup and checks

The wrapper uses the serial Meep 1.25.0 executable from Debian Bookworm.
The [control file](cavity.ctl) defines a 2D waveguide of width `1.2 a` and
relative permittivity 13, where `a` is the lattice period. Each side of the
cavity has three air holes of radius `0.36 a`. The source, flux monitor and
absorbing layers stay fixed. The wrapper runs the straight guide and the
cavity at the same grid resolution, then divides the cavity flux by the
straight-guide flux at each frequency.

The spectrum covers 0.195–0.275 in units of `c/a` with spacing 0.0002, where
`c` is the speed of light in vacuum. The reported peak is the first sample
with maximum transmission inside 0.21–0.27. A narrow resonance can fall
between samples, so the sampled peak height is sensitive to the frequency
grid. The field image shows the normalized out-of-plane magnetic field at
time `200 a/c` from the simulation start. Each image uses its own field
scale.

The offline verifier checks the frequency grid, wavelength conversion and
flux ratio, then compares the peak with the [reference cases](reference.json).
The saved cases use a peak frequency tolerance of 0.0004 in units of `c/a`
and a transmitted fraction tolerance of 0.03. A two-percent numerical
passivity tolerance applies across the spectrum. The downloadable result
uses the locked runtime on Linux ARM64. The 2D model represents an idealized
guide. A fabricated device would require its actual materials and
three-dimensional geometry.

### Repeat the scientific checks

Set `RESULT_DIR="$PWD/meep-run"` from the directory containing your analysis
folder, or substitute the path to the unzipped sample. In the same shell,
[set up the locked Python environment](../README.md#set-up-the-solvers) in
`examples/scientific` and run the offline verifier.

```sh
.venv/bin/python verify_collected.py --model meep --analysis "$RESULT_DIR"
```

Look for `"verified": true` and the spectrum checks.

## Cite and distribute the example

The Enumeray wrapper uses Apache-2.0 and runs the GPLv2-or-later Meep
executable. Debian ships the solver notices in `/usr/share/doc/meep/copyright`.

- [Meep reference paper](https://doi.org/10.1016/j.cpc.2009.11.008)
- [Meep waveguide cavity tutorial](https://meep.readthedocs.io/en/stable/Scheme_Tutorials/Resonant_Modes_and_Transmission_in_a_Waveguide_Cavity/)
- [Meep license and copyright](https://meep.readthedocs.io/en/stable/License_and_Copyright/)
- [Debian Meep source package](https://packages.debian.org/source/bookworm/meep)

For questions about the example, contact [Enumeray](mailto:enumeray@raysofspace.com).
