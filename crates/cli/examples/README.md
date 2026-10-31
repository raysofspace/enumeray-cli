# Analysis examples

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

The [collection publication](https://github.com/raysofspace/enumeray-cli/blob/main/examples/collections/publication.json)
and [Python wrapper](https://github.com/raysofspace/enumeray-cli/blob/main/examples/collections/wrapper.py)
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
