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
[noninteractive JSON recipe](../CLI_AUTOMATION.md#script-with-structured-output).

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
enumeray analysis status scalar-analysis
```

Check where the run will happen and who can access its data. Review
`scalar-analysis/inputs.json` before submitting.

```sh
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
enumeray analysis status file-analysis
```

Review the local `dataset` binding and check where the file will go and who
can access it before submitting.

```sh
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

Use a collection when your model takes a directory of related files as one
input. This example sends two files to `collection-example`. The model
copies them into its `snapshots` output. The commands use the `research`
profile from the team sign-in example. Replace it with the profile you signed
in to. To run the commands as written, your service needs the `collection-example`
model activated and your account needs permission to run it. To publish it,
use the [collection publication](../../../examples/collections/publication.json)
and [Python wrapper](../../../examples/collections/wrapper.py), then
[review and activate the tested publication](https://doc.enumeray.com/authors/publication/#review-and-activate).

### Prepare and review a two-file run

From a folder where you can keep your work, create a private working
directory and stay there for the rest of the example.

```sh
COLLECTION_WORK_DIR=$(mktemp -d "$PWD/enumeray-collection.XXXXXX") &&
  cd "$COLLECTION_WORK_DIR" &&
  pwd
```

The `dataset` binding supplies the only required input, so the JSON input
object can be empty. The analysis directory is separate from the source tree.

```sh
(
  set -eC
  umask 077
  mkdir -p collection-inputs/mesh collection-inputs/snapshots
  printf '%s\n' 'grid fixture' > collection-inputs/mesh/grid.bin
  printf '%s\n' 'snapshot fixture' > collection-inputs/snapshots/0001.bin
  printf '%s\n' '{}' > collection-inputs.json
  enumeray --profile research analysis init collection-example \
    --directory collection-analysis --inputs collection-inputs.json \
    --collection dataset=collection-inputs --media-type dataset=application/octet-stream
  enumeray --profile research analysis status collection-analysis
)
```

Check both files in the source directory and read the execution location and
data-access disclosure shown by status. Keep the files in place while their
uploads are unfinished. Submit only after review.

```sh
enumeray --profile research analysis submit collection-analysis
```

After you have the receipt, wait for the outcome.

```sh
enumeray --profile research analysis wait collection-analysis --check-outcome
```

### Collect the output in stages

After a successful run, try all three stages below. To collect the whole
result in one step, use the last command on its own.

```sh
enumeray --profile research --json analysis collect collection-analysis --evidence-only
enumeray --profile research --json analysis collect collection-analysis \
  --member snapshots=mesh/grid.bin
enumeray --profile research --json analysis collect collection-analysis
```

The first step saves the run record and manifests. The second downloads
`mesh/grid.bin` from the `snapshots` output. On a fresh collection, both
report `complete: false`. The last step downloads the remaining files,
reports `complete: true` and writes `collection-analysis/bundle.json`.

Open `collection-analysis/results/outputs.json` and check that
`member_count` is `2`. Use the paths in the bundle to find
`mesh/grid.bin` and `snapshots/0001.bin` within the downloaded collection.
Both should match their respective source files. If a transfer stops, repeat
the same collection command with the same directory.

### Choose which files to send

Use `--collection SLOT=PATH` to include the regular files under a directory.
If the slot accepts several media types, set `--media-type SLOT=TYPE` to use
one type for the entire directory. For a selected list of files or different
types by file, put the manifest at the source root and initialize with
`--collection-manifest SLOT=MANIFEST.json`. For a previously uploaded
collection, put its completed `collection_id` in the input JSON.

A collection can have up to 10,000 members and a manifest of up to 16 MiB.
The model can impose lower limits. The 10,000-file total includes standalone
files and collection members in each direction. Keep original source bytes
separately when you need them. [File and collection limits](https://doc.enumeray.com/reference/limits/)

## Host preparation specifications

Choose the [Linux managed](host-setup-linux.json), [macOS managed](host-setup-macos.json)
or [development](host-setup-development.json) specification. Edit accounts,
paths, IDs and limits before generating a plan. Generating a plan only
prepares it for review.
