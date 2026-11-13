# Validate a scientific model integration

Model authors can use the HUXt, REBOUND, SWMM and LAMMPS wrappers to practice validating
an integration before publishing it. Run a known case with the pinned solver,
then change an input to check that the wrapper passes the value through and
produces the expected change in the result.

## Set up the solvers

You need Python 3.9 or newer to extract the archive and
[uv](https://docs.astral.sh/uv/getting-started/installation/) to install the
pinned Python and solver packages. Allow a few gigabytes for the environment
and download cache. Use Linux for local SWMM and LAMMPS solver runs. HUXt and
REBOUND can also run locally on macOS ARM64.

Download the [model examples archive](https://doc.enumeray.com/examples/model-examples.tar.gz)
and [its checksum](https://doc.enumeray.com/examples/model-examples.tar.gz.sha256)
into a new directory. Keep both filenames and run this block there.

```sh
(
  set -eu
  python3 - <<'PY'
from hashlib import sha256
from pathlib import Path
archive = Path("model-examples.tar.gz")
expected = Path("model-examples.tar.gz.sha256").read_text().split()[0]
if sha256(archive.read_bytes()).hexdigest() != expected:
    raise SystemExit("Archive checksum mismatch")
PY
  python3 -m tarfile -e model-examples.tar.gz
)
```

If the checksum fails, download the archive and its matching checksum again.
From the extracted `enumeray-examples-…` directory, install the pinned
runtime. Downloads use Astral's Python builds on GitHub and packages from
PyPI, including `files.pythonhosted.org`.

```sh
cd examples/scientific
uv venv --python 3.12.16
uv pip install --python .venv/bin/python --require-hashes -r requirements.lock
```

If a dependency has no wheel for your platform and uv reports a build error,
install a C compiler, such as Xcode Command Line Tools on macOS or
`build-essential` on Debian and Ubuntu. Use these commands in place of the
last install command to pin the build tools as well.

```sh
uv pip install --python .venv/bin/python --require-hashes -r build-requirements.lock
uv pip install --python .venv/bin/python --require-hashes --no-build-isolation -r requirements.lock
```

Setup downloads packages. After setup, the wrapper checks can run offline and
use one CPU thread. The HUXt package includes the ephemeris used by this example.

<a id="run-an-example"></a>

## Exercise the example wrappers

From `examples/scientific`, run HUXt with the default solar-wind parameters.

```sh
.venv/bin/python run_example.py huxt --directory huxt-default
```

For the default orbit, run REBOUND from the same folder.

```sh
.venv/bin/python run_example.py rebound --directory rebound-default
```

On Linux, run SWMM for the default 25% paved catchment from the same folder.

```sh
.venv/bin/python run_example.py swmm --directory swmm-default
```

On Linux, run LAMMPS to compare diffusion at the example temperature with
diffusion at the fixed reference temperature.

```sh
.venv/bin/python run_example.py lammps --directory lammps-default
```

The runner checks the installed versions and saves the plot, CSV data,
numerical summary (`outputs.json`) and run record in the directory you choose.
Open the plot and `outputs.json` there. Use a new directory for each run so you
can compare results side by side.

### Check that the wrapper uses edited inputs

From `examples/scientific`, copy `huxt/inputs.json` to `huxt-faster.json` and
change only `fast_speed_kms` from `700` to `750`.

```sh
.venv/bin/python run_example.py huxt --inputs huxt-faster.json --directory huxt-faster
```

Compare `outputs.json` in `huxt-default` and `huxt-faster`. At 1 AU, the peak
speed should rise from about 802.16 to 859.46 km/s and move from about 4.284 to
4.123 days.

For REBOUND, copy `rebound/inputs.json` to `rebound-eccentric.json` and change
only `eccentricity` from `0.0167` to `0.1`.

```sh
.venv/bin/python run_example.py rebound --inputs rebound-eccentric.json --directory rebound-eccentric
```

Compare `outputs.json` in `rebound-default` and `rebound-eccentric`. The minimum
and maximum orbital distances should move from 0.9833 and 1.0167 AU to 0.9 and
1.1 AU. The period should stay the same because the semimajor axis and masses
stay fixed.

For SWMM, copy `swmm/inputs.json` to `swmm-more-paved.json` and change only
`paved_percent` from `25` to `60`.

```sh
.venv/bin/python run_example.py swmm --inputs swmm-more-paved.json --directory swmm-more-paved
```

Compare `outputs.json` in `swmm-default` and `swmm-more-paved`. The peak flow
should rise from about 39.50 to 57.76 L/s and total runoff volume from about
102.61 to 171.62 m³. The storm depth stays at 25 mm.

For LAMMPS, copy `lammps/inputs.json` to `lammps-warmer.json` and change only
`temperature_reduced` from `1.2` to `1.5`.

```sh
.venv/bin/python run_example.py lammps --inputs lammps-warmer.json --directory lammps-warmer
```

Compare the displacement plots and the fitted diffusion coefficients in both
runs. The fixed reference temperature stays at `1.0`.

## Understand the checks

`run-record.json` records the Python and solver versions, dependency lock
hashes and SHA-256 hashes of the result files. Those hashes let you check that
the files still have the bytes recorded for that run. The numerical checks
answer a different question. HUXt's default speed series is compared with its
saved reference. REBOUND's trajectory is compared with a solution of Kepler's
equation and checked for conservation of energy and angular momentum. SWMM's
runoff is compared with saved references and checked for water balance.
The LAMMPS checker fits the diffusion coefficients again from the saved
displacements and checks the production energy drift.

Plots can vary with fonts and plotting libraries. Use the CSV data and the
numerical checks to compare calculations. These are simplified models, so the
checks establish agreement for the stated setups. Testing a prediction against
the real system would require observations and a model suited to that question.

<a id="inspect-a-downloaded-run-bundle"></a>

The [SWMM tutorial](https://doc.enumeray.com/models/swmm/#read-a-collected-swmm-result)
explains the files saved by `analysis collect`.

## Test the example code

For work on the example implementation, run the protocol and supervisor tests
with system Python.

```sh
python3 -B -m unittest discover -s . -p test_runtime.py
```

Run the full solver suite on Linux in the locked environment.

```sh
.venv/bin/python -B -m unittest discover -s . -p test_models.py
```

[The qualification script](qualify.py) can also test every corner of each
input range with `--extrema` and record elapsed time and peak memory. For a
collected analysis, [the collected-results checker](verify_collected.py)
checks file hashes and numerical outputs without contacting the service.
