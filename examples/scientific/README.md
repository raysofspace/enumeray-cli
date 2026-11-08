# Run a simulation locally

Run a solar-wind or orbit simulation on your computer. You can start by looking
at a checked result, then install the solvers and repeat the experiment. You
can do both without an Enumeray account.

## Inspect a sample result

Download and unzip a result. Open the PNG first to see what the model calculated.

| Example | What to look for | Download |
| --- | --- | --- |
| [HUXt solar wind](https://doc.enumeray.com/models/huxt/) | The fast stream reaches 1 AU. The speed there peaks near 802.16 km/s at 4.284 days. | [HUXt sample ZIP](huxt/sample-run.zip) |
| [REBOUND orbit](https://doc.enumeray.com/models/rebound/) | An Earth-like orbit ranges from 0.9833 to 1.0167 AU from its star. | [REBOUND sample ZIP](rebound/sample-run.zip) |

Each ZIP contains the plot, CSV data, `inputs.json` with the parameters,
`outputs.json` with the numerical summary and `run-record.json` with the checks.

## Inspect an Enumeray collection

Download the [example REBOUND collection](rebound/sample-collection.zip) and
extract `rebound-collection`. This teaching fixture uses data from the
local orbit calculation. Its run and publication IDs, compute location and
access disclosure are examples.

1. Open `bundle.json` to check the run ID and find the verified plot and CSV.
2. Open `results/inspection.json` to see the inputs, model publication,
   execution location and who could access the data.
3. Open `results/provenance.json` to see the recorded model and runtime
   identities. Find a file under `output_artifacts` and compare its checksum
   with the corresponding entry in the bundle.

After [setting up the solvers](#set-up-the-solvers), run the checker from
`examples/scientific`. Replace the example path with the extracted folder.

```sh
.venv/bin/python verify_collected.py --model rebound --analysis /path/to/rebound-collection
```

Look for `"verified": true`. The checker reads local files and verifies the
file checksums and the orbit calculation.
[Read more about run records](https://doc.enumeray.com/users/logs-provenance/).

## Set up the solvers

You need Python 3.9 or newer to extract the archive and
[uv](https://docs.astral.sh/uv/getting-started/installation/) to install the
pinned Python and solver packages. Allow a few gigabytes for the environment
and download cache. These steps were checked on macOS ARM64. Enumeray also
uses the locked solver packages on Linux.

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

Setup downloads packages. The simulation itself can run offline and uses one
CPU thread. The HUXt package includes the ephemeris used by this example.

## Run an example

From `examples/scientific`, run HUXt with the default solar-wind parameters.

```sh
.venv/bin/python run_example.py huxt --directory huxt-default
```

For the default orbit, run REBOUND from the same folder.

```sh
.venv/bin/python run_example.py rebound --directory rebound-default
```

The runner checks the installed versions and saves the same set of files as the
sample ZIP in the directory you choose. Open the plot and `outputs.json` there.
Use a new directory for each run so you can compare results side by side.

To try a change, follow the [HUXt speed comparison](https://doc.enumeray.com/models/huxt/#change-the-inputs)
or [REBOUND eccentricity comparison](https://doc.enumeray.com/models/rebound/#change-the-inputs).
Each changes one value and shows what to look for in the new output.

## Understand the checks

`run-record.json` records the Python and solver versions, dependency lock
hashes and SHA-256 hashes of the result files. Those hashes let you check that
the files still have the bytes recorded for that run. The numerical checks
answer a different question. HUXt's default speed series is compared with its
saved reference. REBOUND's trajectory is compared with a solution of Kepler's
equation and checked for conservation of energy and angular momentum.

Plots can vary with fonts and plotting libraries. Use the CSV data and the
numerical checks to compare calculations. These are simplified models, so the
checks establish agreement for the stated setups. Testing a prediction about
the real solar wind or the real Earth would require observations and a model
suited to that question.

The record describes the local calculation. For the identity, receipts and
provenance of a hosted run, see [Logs and provenance](https://doc.enumeray.com/users/logs-provenance/).

## Test the example code

For work on the example implementation, run the protocol and supervisor tests
with system Python. The solver tests use the locked environment.

```sh
python3 -B -m unittest discover -s . -p test_runtime.py
.venv/bin/python -B -m unittest discover -s . -p test_models.py
```

[The qualification script](qualify.py) can also test every corner of each
input range with `--extrema` and record elapsed time and peak memory. For a
downloaded hosted analysis, [the collected-results checker](verify_collected.py)
checks file hashes and numerical outputs without contacting the service.
