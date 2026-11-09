# Analysis directories

Use one private analysis directory for each run you intend to submit. It holds
the inputs you review, the run receipt and the results you collect. Use the
same directory to resume an interrupted run.

Before starting, install the CLI and [sign in to Enumeray](https://doc.enumeray.com/start/login/).
You need a model ID and permission to run it.
[Find a model and get access](https://doc.enumeray.com/users/discovery-access/)
or [run an example](https://doc.enumeray.com/start/first-run/).

## Initialize and review

Replace the value of `MODEL_ID` with the published ID from the model owner.
Read the required parameters, units and file types in `model get`. Run the
block from a folder where you can keep your work.

```sh
MODEL_ID='REPLACE_WITH_PUBLISHED_MODEL_ID'
enumeray model get "$MODEL_ID" &&
  RUN_WORK_DIR=$(mktemp -d "$PWD/enumeray-run.XXXXXX") &&
  cd "$RUN_WORK_DIR" &&
  pwd
```

Choose one input method below. Each one creates an `analysis` directory inside
this working directory. Use the same shell and stay in the printed directory.
If you're using your own model, supply the inputs it declares.

To follow the starter inputs below, ask a teammate to
[publish and activate the matching starter](https://doc.enumeray.com/authors/publication/#review-and-activate)
and grant your account access. Use the ID your teammate published for the
JSON or file starter. You can run the downloaded `starter-python-json`
project locally using the [model author guide](https://doc.enumeray.com/authors/overview/#run-the-downloaded-examples-offline).

### Use a named example

If the model publishes the input example you want, use its exact name. The
JSON starter provides `starter-example`.

```sh
enumeray analysis init "$MODEL_ID" --directory analysis \
  --example starter-example
```

### Supply JSON parameters

If you have your own parameters, put them in a JSON object and use `--inputs`.
The scalar starter accepts `{"value":2}`. Use the fields and units from
your model's input schema.

```sh
(
  set -eC
  umask 077
  cat > example-inputs.json <<'JSON'
{"value":2}
JSON
  enumeray analysis init "$MODEL_ID" --directory analysis \
    --inputs example-inputs.json
)
```

### Bind local files

Use `--inputs` for the JSON parameters and `--file SLOT=PATH` for each local
file. The CSV starter needs only the file in its `dataset` slot. An empty
JSON object is enough because the binding supplies
that input. For a model that also needs parameters, put them in the JSON object.

```sh
(
  set -eC
  umask 077
  cat > file-inputs.json <<'JSON'
{}
JSON
  cat > dataset.csv <<'CSV'
index,value
0,1
1,2
CSV
  enumeray analysis init "$MODEL_ID" --directory analysis \
    --inputs file-inputs.json --file dataset=dataset.csv
)
```

If the slot accepts more than one media type, add `--media-type SLOT=TYPE`.
For example, pass `--media-type dataset=text/csv` to select CSV. Omit it only
when the slot accepts exactly one type. Use `--metadata SLOT=metadata.json`
for optional scientific metadata. If you have a completed artifact ID or
a directory of related files, see the
[file and collection paths](https://doc.enumeray.com/users/files/).

### Review the saved inputs

Open `analysis/inputs.json` and check the values and units. Run status to see
the exact model version under **Publication**, the **Execution location**,
the **Disclosure** of who can access the data, and any **Local input
bindings**. Check the saved source paths if you attached files.

```sh
enumeray analysis status analysis
```

Initialization saves a copy of the inputs and the execution details. You can
edit `analysis/inputs.json` before the first submit. The CLI records the path,
size and checksum of a bound file without uploading it at this step.

## Submit and resume

When you're satisfied with the inputs and where the run will happen, submit it.

```sh
enumeray analysis submit analysis
```

The first submit freezes the inputs, uploads any bound files and sends the
run request. After submission returns a receipt, wait for the outcome.

```sh
enumeray analysis wait analysis --check-outcome
```

Use `enumeray analysis status analysis` for a single
check. A run is `queued` or `running` while the service prepares, executes or
delivers its work. A final state is `succeeded`, `failed` or `cancelled`. A
local wait deadline or Ctrl-C ends observation. Use the same directory to
resume waiting.
[Read logs](https://doc.enumeray.com/users/logs-provenance/) or
[cancel a run](https://doc.enumeray.com/users/cancellation-retries/#request-cancellation).

If a submit command is interrupted or its reply is lost, repeat
`analysis submit analysis` with the same directory. Keep the original files at
their saved paths while their uploads are unfinished. If the run request may
already have reached the service, the CLI replays the saved request without
needing the source files. An accepted request returns the same run. If the CLI
reports that the model version changed before submission could start,
initialize a new directory and review the new details.
[Recover an interrupted submission](https://doc.enumeray.com/users/cancellation-retries/#retry-the-same-operation)

## Collect verified evidence

When the run reaches a final state, collect its record and available results.

```sh
enumeray analysis collect analysis
```

Look for **Collection complete: yes** in the terminal. Check the scientific
state separately. A failed or cancelled run can still have a complete record.
If a download stops, repeat the command with the same directory. The CLI
checks downloaded sizes and checksums and reuses matching files. If a file
conflicts, inspect the paths it reports before moving anything aside.

| What to open | What to look for |
| --- | --- |
| `analysis/results/outputs.json` | The scalar starter returns `{"doubled":4}` for input `{"value":2}`. The CSV starter reports `sample_count: 2`. |
| `analysis/bundle.json` | The paths to verified file outputs and any unavailable optional outputs. For the CSV starter, find `processed_dataset` and check for rows `0,2` and `1,4`. |
| `analysis/results/inspection.json` | The final run state and any reported failure. |
| `analysis/results/effective-inputs.json` and `analysis/results/provenance.json` | The input values and file references used and the record of the selected model and execution. |

## Records to retain

Keep the entire analysis directory private, including `bundle.json`,
`results/` and the saved state. Keep any original input files you need for
reproducibility separately. The directory records their paths and checksums.
For a new run, initialize a new directory and review its inputs.

## Other ways to use an analysis

<a id="bind-local-collections"></a>
<a id="collect-a-collection-in-stages"></a>

- [Run a model with a directory of files](https://doc.enumeray.com/users/collections/#collection-example)
  and collect its outputs in stages.

<a id="trial-an-exact-candidate"></a>

- [Trial a model before making it available](AUTHOR_GUIDE.md#save-a-release-resume-readiness-review-and-activate)
  if you own the model or are a named reviewer.

<a id="script-with-structured-output"></a>

- [Script a run and read JSON results](https://doc.enumeray.com/reference/cli-json/#script-with-structured-output)
  when another program needs to act on the result.

## Recover an existing execution

If you lost the directory but know the service accepted the run, attach it to
a new directory. Use one command with the original run ID or receipt.

```sh
enumeray analysis attach RUN_UUID --directory recovered-analysis
# Or, if you have the acceptance receipt:
enumeray analysis attach --receipt run-receipt.json \
  --directory recovered-analysis
```

The receipt is the object in `state/receipt.json` or the nested `receipt` from
JSON submit output. Sign in with an account that has permission to read
the run. Attachment reads the accepted model publication and inputs even if
the model has changed. It creates a folder for status, logs and collection.
It does not submit a run or upload files. Only the original submitter with
cancellation authority can cancel.

Repeat an interrupted attach with the same ID or receipt and destination. The
CLI preserves conflicting evidence and unrelated directories. If you have no
receipt and do not know whether a run was accepted, search your authorized
`enumeray run list` history. A missing entry leaves
acceptance uncertain.
Preserve the original request before deciding whether to submit another run.

### Recover a saved request file

If you have a standalone prepared request file, import it into a private
analysis directory.

```sh
enumeray analysis import --prepared prepared-run.json \
  --directory imported-analysis
```

The import is local and keeps the request's original API origin, account and
retry key. Keep the source file. To submit or recover that exact request,
sign in with the original account, review the imported request and
explicitly run `enumeray analysis submit imported-analysis`.
The service can return the original acceptance or evaluate a request that was
never accepted.
If it rejects the request, inspect the response before preparing another run.

## Copied-folder use

Copy the whole accepted analysis directory with private permissions and sign
in with the same account on the other host. Status, logs, wait,
cancellation and collection use the run recorded in the folder. Completed
files are checked before reuse. Keep download checkpoints with the directory.
If the absolute destination changes, a partial file may need to download
again. For unfinished uploads, keep the source bytes and the saved transfer
state. Copying the folder leaves the run identity the same.
