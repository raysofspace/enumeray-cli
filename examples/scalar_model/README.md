# External scalar reference model

This `scalar-double` fixture belongs to `enumeray-model-examples-v1`.
This directory is a separately installable Python 3.9+ standard-library model and
wrapper. Copy the directory to an approved local installation; no Python package
installation, Rust build, API access, credentials, or connector journal is needed.
The model is a protocol demonstration, not a scientific reference result.

`model.py` owns the calculation and accepts a finite `decimal.Decimal`. It doubles
the decimal coefficient without rounding to binary floating point or the active
decimal context. Run it independently from this directory:

```sh
python3 -B model.py 2
python3 -B model.py 9007199254740993.123456789
```

`wrapper.py` and the local `protocol.py` own the protocol-1 request/result envelope, identity validation,
model-specific input mapping, failure mapping, and atomic result publication.
It invokes the model in the wrapper process. The connector launches this wrapper
as an external process and owns scheduling, local logs, and recovery.

For Slurm execution, install `model.sbatch` in the same immutable release directory
and declare `"slurm_script": "model.sbatch"` in the publication descriptor. Edit
its `#SBATCH` account, partition, resources, and runtime setup for your model and
site. The connector supplies the worker executable and job
directory as arguments; the script starts that worker in the foreground, and the
worker invokes the registered wrapper with the usual request/result protocol.
Exported batch setup variables reach the wrapper. The connector validates and
pins the batch file's content alongside the wrapper. See the
[Slurm publication example](../../crates/cli/examples/slurm-scalar-publication.json).

To exercise only the wrapper with the supplied request fixture, choose an unused
result path. On macOS or Linux, from this directory:

```sh
result_dir=$(mktemp -d)
python3 -B wrapper.py execute --request request.json --result "$result_dir/result.json"
cat "$result_dir/result.json"
```

The request includes `input_files: {}` and the result includes `files: {}` because
this scalar model declares no files. The result contains `outputs: {"doubled": 4}`
with the four request UUIDs echoed.
Requests/results are bounded to 64 KiB. Duplicate JSON keys, non-finite numbers,
unknown envelope fields, invalid publication IDs, and malformed identities are
rejected. Invalid scientific input produces an explicit failed result with a safe
message; invalid envelopes produce no result. Success exits 0, an explicit failed
result exits 1, and envelope/publication failures exit 2. Logs omit input values,
result values, and local paths. A prior result is never overwritten. The result is
published through a synced temporary file and an atomic same-filesystem link;
preserve it even if a later sync error causes a nonzero exit.

The installed reference wrapper accepts model `scalar-double` and valid publication
IDs. Declare its scientific identities once in the publication definition;
registration derives the internal installation identity claims from that definition.
The publication ID identifies the complete release.
The demo's scientific contract is in `../local-scalar-model.json`; installed
entrypoints and context details are supplied through publication and context
management, then validated by the enrolled connector. Changes to model or wrapper
behavior require a new immutable release and publication. The context and
publication select execution paths; submitted inputs never select commands.

Run the standalone standard-library tests from this directory:

```sh
python3 -B -m unittest discover -s . -p 'test_*.py'
```
