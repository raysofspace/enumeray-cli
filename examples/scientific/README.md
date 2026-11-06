# Maintained scientific demonstrations

These are small, real solver integrations: [HUXt solar wind](huxt/README.md) and
[REBOUND orbital dynamics](rebound/README.md). These hosted examples have
preinstalled solvers and an Enumeray-operated connector. Users need only the Enumeray executable and a
verified account; they do not need Python or a connector on their own machine.

Each model directory contains its authored `publication.json`, a complete editable
`inputs.json`, a model card and scientific implementation. Both publications use
the same immutable installed package and protocol-1 `wrapper.py`. File outputs
are declared separately from small numerical JSON summaries.

For an offline author checkout, use **Python 3.12.16** on macOS or Linux:

```sh
cd examples/scientific
python3.12 -m venv /tmp/enumeray-scientific
/tmp/enumeray-scientific/bin/python -m pip install --require-hashes -r requirements.lock
/tmp/enumeray-scientific/bin/python -m unittest discover -s . -p test_models.py
/tmp/enumeray-scientific/bin/python qualify.py --extrema
```

The requirements file includes the complete upstream dependency sets, including
HUXt's notebook/solar packages. They enlarge the installation; the hosted cases
use one CPU thread and require no HPC, GPU, observational download or network
connection during scientific execution. The HUXt ephemeris comes with its locked
distribution. `runtime-manifest.json` records its checksum, the solver releases,
the exact interpreter and dependency-lock checksum.

On a platform requiring native source builds (including Linux ARM), first install
`build-requirements.lock` with `pip install --require-hashes`, then install
`requirements.lock` using `--require-hashes --no-build-isolation`. The separate
build lock pins setuptools, wheel, Cython and the same NumPy used at runtime;
the compiler toolchain belongs to the image build stage. No solver version changes
are needed. Both lock checksums are in the runtime manifest.

Without installing scientific dependencies, check the protocol/supervisor:

```sh
python3 -B -m unittest discover -s . -p test_runtime.py
```

Scientific tests and `qualify.py` execute the installed solvers. Qualification
compares numerical outputs, exercises all eight parameter-range corners per model
when `--extrema` is supplied, and emits JSON with wall time, peak solver RSS and
the tested identities. Its acceptance budgets are under 60 seconds for each
default example, under 120 seconds for a corner case, and under 1 GiB peak solver
RSS. Actual site qualification must pass on the deployment hardware.

The wrapper enforces 180 seconds for HUXt and 60 seconds for REBOUND, including
imports and just-in-time compilation. CSV and PNG files together are at most
8 MiB per run (with the same upper bound on each file). It bounds CPU
time, disables core dumps, caps native threads, rejects outbound socket connections
in the scientific child and confines runtime data/cache writes to its workspace.
Timeout and cancellation terminate and reap that child; Linux additionally binds
its lifetime to the supervisor. Preserve a timed-out workspace as evidence and
submit changed work in a new analysis directory.

The deployed connector has a separate credential-free model account. Scientific
definitions and test results are distinct from host isolation and service
qualification. `verify_collected.py --model huxt --analysis ANALYSIS` (or
`--model rebound`) checks actual CLI-collected CSV/PNG artifacts, their checksums
and the numerical assertions. It does not contact the service.
