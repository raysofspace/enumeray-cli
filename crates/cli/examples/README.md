# Model examples: offline tests and installed analyses

## Start from the complete bundle

The versioned model-examples archive contains this `examples/` directory, complete
standalone scalar and collection projects, four generated `starter-kit/` projects,
the workflow guides, `verify-examples.py` and `examples-manifest.json`. Its manifest
records the source revision, working-tree status and exact file hashes; the
archive has a separate SHA-256 checksum. Verify that checksum before extracting.
Run these commands from the extracted root with Python 3.9+:

```sh
python3 verify-examples.py --root .
```

The verifier checks the acquired bytes and runs all six projects' tests in fresh
disposable copies. It requires no network, credentials, API, source checkout or
Rust tools. Tests and fixtures are inside each project; for example:

```sh
cd examples/collections
python3 -B -m unittest discover -s . -p 'test_*.py'
```

A matching installed CLI is needed only for CLI commands below and for creating
new starter projects. Downloading an individual file does not acquire its sibling
modules, fixtures or tests; choose the complete bundle for the offline path.

## Fixture identity and file map

| Project / purpose | Model identity and entrypoint | Inputs and expected interpretation |
| --- | --- | --- |
| `examples/scalar_model/` standalone reference | `scalar-double`; `wrapper.py execute` | `request.json`, value 2; `doubled: 4` |
| Package scalar publication below | `scalar-double`; installed `run-model` | `examples/scalar-inputs.json`, value 21; `doubled: 42` |
| `starter-kit/python-json/` | `starter-python-json`; `wrapper.py execute` | `fixtures/request.json`, value 2; `doubled: 4` |
| `starter-kit/executable-json/` | `starter-executable-json`; fixed demo solver through `wrapper.py` | Same scalar contract and answer |
| `starter-kit/python-files/` | `starter-python-files`; `wrapper.py execute` | Two-row `fixtures/dataset.csv`; `sample_count: 2`, doubled CSV |
| `starter-kit/executable-files/` | `starter-executable-files`; fixed demo solver through `wrapper.py` | Same file contract and answer |
| `examples/collections/` | `collection-example`; `wrapper.py execute` | Empty or two nested members copied byte-for-byte; `member_count: 0` or `2` |
| Author tutorial generated locally | `example` / `csv-example` / `solver-example` | Separate `model init` projects; use their own publication and fixtures |
| Single-user connector tutorial | `development-double` | Separate installed development fixture; value 21 gives 42 |

The extracted directory is a local source root, not an installed model release.
The collection declaration's `/opt/models/collection-example` is an installation
placeholder. A generated starter leaves its deployment context/path unset. Model
IDs, publication IDs, entrypoints, roots and request identities must match their
own project's declaration; do not mix files across these rows. Scientific failure
is an explicit result independent of a transport or protocol rejection. Toy
answers demonstrate mapping and evidence handling, not scientific validation.

## Continue with an installed offering

These package examples consume already-installed model offerings. Select the
actual context, immutable release path and compute grant before registration;
placeholder paths do not install the scalar fixture. For a standalone editable
project, use `model init` and the included author guide.

These credential-free templates accompany the independent CLI. Follow the included
[author guide](../AUTHOR_GUIDE.md) to install, register, trial, review and activate a
model, or ask its owner for an already accessible model. Context IDs, release paths
and scientific identities are examples to replace before publication. Creating
local files does not provision compute or install a model.

Run the following user commands from the package root, whose `examples/` directory
contains this file. Use a logged-in profile and choose a new private analysis
directory for each intended execution. Inspect the model and the saved destination/disclosure
before submitting. The [analysis guide](../ANALYSIS_GUIDE.md) explains editable and
frozen inputs, retries, logs/cancellation, copied-folder recovery and the complete
[noninteractive JSON recipe](../ANALYSIS_GUIDE.md#script-with-structured-output).

For trusted local experiments, [development-process-context.json](development-process-context.json)
provides the explicit process/direct development policy. Replace its absolute
example paths and follow the [single-user connector recipe](../../connector/README.md#single-user-development).
The connector must also receive `--allow-development-execution` on every
launch-capable invocation.

## Scalar publication

The supplied [process declaration](scalar-publication.json) and
[Slurm declaration](slurm-scalar-publication.json) both describe `scalar-double`.
Their installed entrypoint must implement that contract; choose the matching
context template and follow the author installation procedure first.

```sh
umask 077
enumeray model get scalar-double
enumeray analysis init scalar-double --inputs examples/scalar-inputs.json \
  --directory scalar-analysis
# Review the saved publication, destination/disclosure and editable inputs.json.
enumeray analysis submit scalar-analysis
enumeray analysis wait scalar-analysis --check-outcome
enumeray analysis collect scalar-analysis
```

The supplied input is `{"value":21}`; the scalar-double fixture returns
`{"doubled":42}` in `scalar-analysis/results/outputs.json`. Edit the analysis's
`inputs.json` before its first submit to choose another value. Submit freezes it;
repeat submit with the same directory after interruption to recover the same
execution. A different intended execution needs a fresh directory and review.

## File starter

Create the source project with `enumeray model init csv-example --directory
csv-example --io files`, run its local tests and complete the author publication
procedure. Once that file model is accessible, create two tiny input rows outside
the new analysis directory:

```sh
umask 077
cat > dataset.csv <<'CSV'
index,value
0,1
1,2
CSV
cat > file-inputs.json <<'JSON'
{}
JSON
enumeray model get csv-example
enumeray analysis init csv-example --inputs file-inputs.json \
  --file dataset=dataset.csv --directory file-analysis
# Review the destination/disclosure and the local dataset binding.
enumeray analysis submit file-analysis
enumeray analysis wait file-analysis --check-outcome
enumeray analysis collect file-analysis
```

The binding supplies the required `dataset` reference and uploads the source
automatically. The expected ordinary output is `sample_count: 2`; `bundle.json`
maps `processed_dataset` to a verified CSV containing rows `0,2` and `1,4`.
Keep original source bytes unchanged while upload is unfinished and retain them
separately when needed: the analysis records their path/checksum without copying
them. Status, logs, cancellation and collection use the folder after acceptance.

## Collection example

The [collection publication](../../../examples/collections/publication.json)
and [Python wrapper](../../../examples/collections/wrapper.py)
describe `collection-example`, with input `dataset` and output `snapshots`.
Install and publish that model through the author workflow, then follow the
[complete two-member setup](../ANALYSIS_GUIDE.md#bind-local-collections): it creates
`mesh/grid.bin`, `snapshots/0001.bin` and `{}` scientific inputs, binds the directory
and records every member's original size/checksum before upload. Review and submit
that folder, then use the guide's [staged collection commands](../ANALYSIS_GUIDE.md#collect-verified-evidence)
to collect manifests, one selected member and finally all outputs.

Evidence-only and partial collection can exit 0 with `complete: false`. Verify
`complete: true` and `bundle.json` for every output; scientific success, failure or
cancellation is recorded independently. Repeated collection never launches the
model again. For a deliberately selected input set, use a collection manifest
instead of a directory binding. Independent artifact/collection utilities remain
available for reusable data; accepted-run IDs remain useful for authorized
investigation and attachment.

Host preparation specifications: [Linux managed](host-setup-linux.json), [macOS managed](host-setup-macos.json) and [development](host-setup-development.json). Edit accounts, paths, IDs and limits before generating a plan. Generation applies no host changes.
