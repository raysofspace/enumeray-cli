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

## Set up the solvers

On macOS or Linux, install [uv](https://docs.astral.sh/uv/getting-started/installation/)
to get the exact Python version and packages. Allow a few gigabytes for the
environment and download cache.

Download and extract the [model examples archive](https://doc.enumeray.com/examples/model-examples.tar.gz).
Use the [archive checksum instructions](https://doc.enumeray.com/authors/author-guide/#run-the-downloaded-model-examples-offline)
to check the download. From the extracted archive's top-level folder, run these
commands to install the pinned Python and solver dependencies.

```sh
cd examples/scientific
uv venv --python 3.12.16
uv pip install --python .venv/bin/python --require-hashes -r requirements.lock
```

If a native dependency needs a source build, install a C compiler, such as
Xcode Command Line Tools on macOS or `build-essential` on Debian and Ubuntu.
Use these two commands in place of the last install command to pin the build
tools as well.

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
