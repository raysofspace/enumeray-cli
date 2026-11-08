# Analysis directories

Use one private directory for each run you intend to submit. It holds the
inputs you review, the run receipt and the results you collect. If a command is
interrupted, use the same directory to continue that run. If you want another
execution, create a new directory and review its inputs.

This guide assumes you have the CLI, an account for the intended service and
access to a model that accepts submissions. You need the model ID and either a
named example or a JSON input file. For HUXt or REBOUND on the public trial,
start with the hosted walkthrough to check availability and install the right
CLI. [Hosted first run](https://doc.enumeray.com/start/first-run/)

## Initialize and review

The following commands use the scalar starter, published as `example`, and put
the run in a new directory called `analysis`. Substitute your model's ID and
its named example. If you sign in with a named profile, pass the same
`--profile NAME` after `enumeray` for each command.

```sh
enumeray model get example
enumeray analysis init example --directory analysis --example starter-example
```

If you already have a model input file, initialize with `--inputs inputs.json`
instead of `--example starter-example`. Choose a new directory inside an
existing parent directory.

Open `analysis/inputs.json`. Before submitting, check the values, units and any
file bindings. Read the execution location and data-access disclosure that the
CLI prints. The directory saves the selected model publication and its input
contract. You can edit the JSON in the directory before the first submission.
Initialization alone performs no uploads or execution.

## Submit and resume

Once you have reviewed the analysis, submit and check its progress.

```sh
enumeray analysis submit analysis
enumeray analysis status analysis
enumeray analysis wait analysis --check-outcome
```

The first submit validates and freezes the inputs, uploads any bound files and
saves the request and acceptance receipt. If the connection drops or the reply
is lost, repeat `analysis submit analysis` with the same directory and original
input files. The CLI recovers the same request. If the service already accepted
it, the retry returns the original run. If a model publication changes before
the first request is sent and submission stops, prepare a new directory and
review the new inputs and disclosure. Once the request may have reached the
service, resume the saved request.

Read the state from `status` or `wait`. A run is `queued` or `running` while the
service prepares, executes or delivers its work. Its final state is
`succeeded`, `failed` or `cancelled`. A local wait deadline or Ctrl-C ends your
observation while the run continues. If submit was interrupted before a receipt
was saved, repeat submit first so the directory has a run to inspect.

To read messages while the model runs, use
`enumeray analysis logs analysis --follow`. To ask the service to stop the
work, use `enumeray analysis cancel analysis`, then check status for
confirmation. Repeat the same cancellation command if you lose its reply.
[Read logs](https://doc.enumeray.com/users/logs-provenance/) ·
[Cancel or retry](https://doc.enumeray.com/users/cancellation-retries/)

## Collect verified evidence

When the run reaches a final state, collect its results and record.

```sh
enumeray analysis collect analysis
```

| File or directory | What it contains |
| --- | --- |
| `analysis/bundle.json` | The output names and paths, file sizes and checksums, exact versions and any unavailable optional outputs. |
| `analysis/results/outputs.json` | The model's JSON result. The scalar starter returns `{"doubled":4}` for input `{"value":2}`. |
| `analysis/results/effective-inputs.json` | The input values and file references used by the run. |
| `analysis/results/provenance.json` | The model publication, execution and available file evidence. |
| `analysis/results/inspection.json` | The final state and result or failure. |
| `analysis/results/files/` and `analysis/results/collections/` | Any downloaded output files, collection members and manifests. |

Look for **Collection complete: yes** in human output or `complete: true`
with `--json`. A failed or cancelled run can still have a complete bundle
of the available evidence. Use the recorded run state to check the execution
outcome, and the model's scientific guide to interpret the results. The CLI
checks downloaded bytes and uses the recorded original file name when
available. Use the paths in `bundle.json` to find the files.

If a download stops, repeat `analysis collect analysis`. The CLI verifies and
reuses matching files and resumes partial downloads. It preserves conflicting
files. If a file conflicts, inspect the paths it reports before moving anything
aside. Keep the private directory for your research record. It holds local file checksums
and references. Save any original input bytes you need separately.

### Collect a collection in stages

If you use the two-file example in [Bind local collections](#bind-local-collections),
review its inputs and disclosure, then submit it and wait for a final state.

```sh
enumeray analysis submit collection-analysis
enumeray analysis wait collection-analysis --check-outcome
```

You can save the manifests first, retrieve one file, or download every output.

```sh
enumeray --json analysis collect collection-analysis --evidence-only > collection-evidence.json
enumeray --json analysis collect collection-analysis \
  --member snapshots=mesh/grid.bin > collection-partial.json
enumeray --json analysis collect collection-analysis > collection-complete.json
```

On a fresh collection, the first two commands report `complete: false` and
leave `bundle.json` unpublished. The final command downloads the rest,
reports `complete: true` and saves the bundle. All three commands use the same
run. Repeat an interrupted step with the same folder. Repeating `--member
SLOT=PATH` selects several members.

## Bind local files

If a model needs a local file, name the declared input slot and point the CLI
at the source file. For the file starter published as `csv-example`, the slot
is `dataset` and accepts a CSV. From a private working directory, create the
example file and initialize.

```sh
umask 077
cat > dataset.csv <<'CSV'
index,value
0,1
1,2
CSV
enumeray analysis init csv-example --directory csv-analysis \
  --example starter-example --file dataset=dataset.csv
```

Review `csv-analysis/inputs.json`, the binding and the execution location and
data-access disclosure. Then follow the submit, wait and collect steps above
using `csv-analysis`. The CLI records the source path, size and checksum
during initialization, uploads the file during submission and supplies the
model with a reference to the verified bytes. Keep the file unchanged and in
place until upload finishes. If you need to submit changed file bytes, prepare
a new directory.

Repeat `--file SLOT=PATH` for each file slot. Use `--media-type SLOT=TYPE` to
choose among supported types and `--metadata SLOT=metadata.json` to attach
optional JSON metadata. You can also put a completed artifact ID in the input
JSON instead of binding a local file. [Use files in an analysis](https://doc.enumeray.com/users/files/)

## Bind local collections

Use a collection binding when the model expects a directory of related files.
The collection-copy example uses `dataset` for the input and `snapshots` for
the output. After the model is available to you as `collection-example`,
create two small source files and initialize its analysis.

```sh
umask 077
mkdir -p collection-inputs/mesh collection-inputs/snapshots
printf 'grid fixture
' > collection-inputs/mesh/grid.bin
printf 'snapshot fixture
' > collection-inputs/snapshots/0001.bin
cat > collection-inputs.json <<'JSON'
{}
JSON
enumeray analysis init collection-example --directory collection-analysis \
  --inputs collection-inputs.json --collection dataset=collection-inputs \
  --media-type dataset=application/octet-stream
```

The binding supplies the only required input, so `{}` is enough for this
example. The CLI records both relative paths, sizes and checksums. Keep the
source tree unchanged until submission has uploaded it. After reviewing the inputs, location and disclosure, follow the example through
submission and collection. [Collect in stages](#collect-a-collection-in-stages)

To select members explicitly or assign different media types, put a manifest
at the root of the source tree and initialize with
`--collection-manifest dataset=MANIFEST.json`. Keep the analysis directory
outside the source tree. If the collection is already uploaded, use its
completed `collection_id` in the input JSON. [Collection declarations and examples](https://github.com/raysofspace/enumeray-cli/blob/main/crates/cli/examples/README.md)

## Recover an existing execution

If you lost the directory but know the service accepted the run, attach it to
a new directory. Use one command with the original run ID or receipt.

```sh
enumeray analysis attach RUN_UUID --directory recovered-analysis
# Or, if you have the acceptance receipt:
enumeray analysis attach --receipt run-receipt.json --directory recovered-analysis
```

The receipt is the object in `state/receipt.json` or the nested `receipt` from
JSON submit output. Sign in to the service and account with permission to read
the run. Attachment reads the accepted model publication and inputs even if
the model has changed. It creates a folder for status, logs and collection.
It does not submit a run or upload files. Only the original submitter with
cancellation authority can cancel.

Repeat an interrupted attach with the same ID or receipt and destination. The
CLI preserves conflicting evidence and unrelated directories. If you have no
receipt and do not know whether a run was accepted, search your authorized
`enumeray run list` history. A missing entry leaves acceptance uncertain.
Preserve the original request before deciding whether to submit another run.

### Recover a saved request file

If you have a standalone prepared request file, import it into a private
analysis directory.

```sh
enumeray analysis import --prepared prepared-run.json --directory imported-analysis
```

The import is local and keeps the request's original service, account and
retry key. Keep the source file. To submit or recover that exact request,
sign in to the original service and account, review the imported request and
explicitly run `enumeray analysis submit imported-analysis`. The service can
return the original acceptance or evaluate a request that was never accepted.
If it rejects the request, inspect the response before preparing another run.

## Copied-folder use

Copy the whole accepted analysis directory with private permissions and sign
in to the same service and account on the other host. Status, logs, wait,
cancellation and collection use the run recorded in the folder. Completed
files are checked before reuse. Keep download checkpoints with the directory.
If the absolute destination changes, a partial file may need to download
again. For unfinished uploads, keep the source bytes and the saved transfer
state. Copying the folder leaves the run identity the same.

## Trial an exact candidate

A model author or named reviewer can run a ready candidate before activation.
Use the exact ID from release status or from the person who delegated the
trial. For the scalar starter, prepare the trial.

```sh
cat > trial-inputs.json <<'JSON'
{"value":2}
JSON
enumeray model release status release-r1
enumeray analysis init example --candidate CANDIDATE_ID \
  --inputs trial-inputs.json --directory trial-r1
```

Review the selected candidate, installation, inputs, execution location and
disclosure. Then submit, wait and collect.

```sh
enumeray analysis submit trial-r1
enumeray analysis wait trial-r1 --check-outcome
enumeray analysis collect trial-r1
```

The directory records the candidate, trial purpose and grant or named
delegation. A delegated reviewer can use the named handoff without a separate
compute grant. It must remain active for the candidate. A scoped token needs
`me:read`, `models:read`, `models:manage` and `runs:create`, plus the scopes
for the files and run evidence you use. The service checks authorization and
readiness through launch. Repeat submit with the same directory to recover a
lost acceptance reply. A new delegation requires a separate trial.

Read `trial-r1/results/provenance.json` and check the scientific outputs.
Release status and review show trial evidence automatically. If you need a
review directory for an existing candidate, run
`model release init example --candidate CANDIDATE_ID --directory review-r1`.
Review the evidence before deciding whether to activate. Activation requires
its own explicit command. [Author workflow](AUTHOR_GUIDE.md#save-a-release-resume-readiness-review-and-activate)

## Script with structured output

Use global `--json` for machine-readable output and check the exit status.
The CLI writes runtime errors, progress and recovery hints to stderr. Argument
errors use plain text and exit 2. Check the schema version and required fields
before acting. Preserve the directory if the response lacks a required field
or uses an unsupported version. [CLI JSON contract](https://doc.enumeray.com/reference/cli-json/)

This recipe uses `jq`, an authenticated profile and access to the scalar
starter published as `example`. Initialize once in a private working directory.

```sh
set -eu
umask 077
cat > inputs.json <<'JSON'
{"value":2}
JSON
enumeray --json model get example > model.json
enumeray --json analysis init example --inputs inputs.json \
  --directory scripted-analysis > initialized.json
enumeray --json analysis status scripted-analysis > reviewed-analysis.json
```

Review `model.json`, `initialized.json`, `reviewed-analysis.json` and
`scripted-analysis/inputs.json`. Approve the intended inputs, exact publication,
execution location and data-access disclosure before authorizing submission.
Only the directory's input file remains editable before first submit.

Run this stage after review. After interruption, repeat this stage with the
same directory.

```sh
set -eu
umask 077
ANALYSIS=scripted-analysis
enumeray --json analysis submit "$ANALYSIS" > submission.json
if enumeray --json analysis wait "$ANALYSIS" --check-outcome --interval 2 --deadline 3600 > terminal-run.json; then
  outcome=0
else
  outcome=$?
fi
case "$outcome" in
  0|3|4)
    enumeray --json analysis collect "$ANALYSIS" > collection.json
    jq -e '.complete == true' collection.json >/dev/null
    ;;
  5|130)
    echo 'Observation ended; retain the analysis directory and resume waiting.' >&2
    ;;
  *)
    echo 'No terminal outcome was obtained; inspect the error and retained directory.' >&2
    ;;
esac
exit "$outcome"
```

With `--check-outcome`, wait returns 0 for success, 3 for failure, 4 for
cancellation or 5 for a local deadline. Exit 130 means interruption. It writes
terminal JSON before exits 3 or 4. Without `--check-outcome`, wait returns 0
for any final state and 1 for a deadline. The `--timeout` option limits an HTTP
operation. The wait deadline limits local observation.

Collection can succeed for failed or cancelled runs and for partial requests.
The script checks completeness separately. If collection fails, repeat it with
the same directory. If sign-in expires, renew the same service and account or
replace the scoped token, then resume. Read the acceptance receipt in
`state/receipt.json` or the nested `receipt` in `submission.json`. Check status
or wait for progress after acceptance.

## Records to retain

| Stage | Keep |
| --- | --- |
| Before uploads finish | The private directory and the original files at their saved paths. |
| A submit reply is missing | The original directory, any prepared request and its receipt. |
| The run is accepted | The whole directory, including private state files and checkpoints. |
| Collection is complete | The directory, `bundle.json`, `results/` and any original input bytes you need to retain separately. |

Keep these files private and protect them when you copy or back them up. Resume
the original directory for an interrupted run. To run the model again with
another input or publication, initialize a new directory and review it before
submitting. [Find a run by ID](https://doc.enumeray.com/users/runs/)
