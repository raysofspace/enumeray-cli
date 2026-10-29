# Analysis directories

Keep one intended execution, its reviewed inputs, exact retries and verified results in one private directory. This guide ships with the CLI and is the source for the web analysis guide.

**Role:** Model user, candidate author/reviewer or authorized run reader. **Prerequisites:** An authenticated CLI profile for remote commands. New work requires access to the selected publication and upload/artifact scopes when using files; fresh candidate trials also require current model administration, execution access and the captured grant or an exact named delegation. Accepted-run recovery uses existing run-read authority; local prepared-operation import needs no credentials. **Files:** A private directory for one intended execution; original local input bytes remain external.

## Initialize and review

Inspect `enumeray model get MODEL` for its schema, named examples, availability, execution location and scientific-data access disclosure. Initialize from one of its examples or your own inputs:

```sh
enumeray analysis init example --directory analysis --example starter-example
# Alternative, for supplied model-specific JSON:
enumeray analysis init example --directory supplied-analysis --inputs inputs.json
```

Use the actual model ID and example name. Initialization saves the exact current publication and contract, the execution location/disclosure and private operation identities. It performs no uploads or model execution. The destination's parent must exist; an existing destination is refused.

Edit `analysis/inputs.json` to change scientific values before the first submission. Review the printed execution location, disclosure and local file bindings. The first `submit` validates and freezes the edited JSON before any remote mutation. A frozen operation cannot be edited into another execution. For the initialized scalar folder, explicitly submit after review, then observe and collect:

```sh
enumeray analysis submit analysis
enumeray analysis status analysis
enumeray analysis wait analysis --check-outcome
enumeray analysis collect analysis
```

The scalar starter with input `{"value":2}` returns `{"doubled":4}` in `results/outputs.json`. Use the matching directory if you selected the supplied-input alternative. The file and collection examples below use separate explicitly named folders.

## Trial an exact candidate

Authors and delegated reviewers can select a ready or published retained candidate directly, including before the model's first activation:

```sh
cat > trial-inputs.json <<'JSON'
{"value":2}
JSON
enumeray model release status release-r1
enumeray analysis init example --candidate CANDIDATE_ID \
  --inputs trial-inputs.json --directory trial-r1
# Or use --example NAME from this candidate's scientific examples.
# Review the exact installation, execution location and full disclosure.
enumeray analysis submit trial-r1
enumeray analysis wait trial-r1 --check-outcome
enumeray analysis collect trial-r1
enumeray model release status release-r1
# Complete your scientific checks before saving the activation review.
enumeray model release review release-r1
```

Use the exact candidate ID from your release or the grant holder. Initialization reads that candidate rather than today's current publication and saves `candidate_trial` purpose, its captured grant and any named delegation. The ordinary file and collection bindings below use the candidate's declarations. Review the displayed identity before explicitly submitting; initialization never activates the candidate.

Scoped credentials need `me:read`, `models:read`, `models:manage` and `runs:create`, plus scopes for the uploads and run evidence you use. Delegated reviewers need no separate compute grant; the named handoff must remain active for the exact candidate. A revoked handoff cannot be replaced inside an existing trial directory. Revocation, readiness, maintenance and ordinary capacity rules still apply through launch. Repeat `analysis submit trial-r1` after uncertain acceptance to recover the original execution, including after activation, replacement or changed authority.

Folder status, logs, cancellation and collection retain the trial identity. Collected provenance is in `trial-r1/results/provenance.json`. Release status/review shows trial evidence automatically without copying a receipt or run ID. If you have no local release operation, adopt the existing candidate with `model release init example --candidate CANDIDATE_ID --directory review-r1`; adoption creates this account's private review operation. A successful trial remains advisory execution evidence. Authors own scientific assertions and activation requires a separate explicit `model release activate` command. See the included [author workflow](AUTHOR_GUIDE.md#save-a-release-resume-readiness-review-and-activate).

## Bind local files

The file starter declares `dataset` as a `text/csv` slot. Run this example from a private working directory after `csv-example` has been installed, activated and made accessible:

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

The CLI hashes the original file and records its absolute path and expected size/checksum. It uploads those bytes to storage during submission; the remote connector stages the verified artifact on the compute host. The local path is used by the CLI for uploading and retry checks. No extra local copy is made.

Repeat `--file SLOT=PATH` for more declared slots. A binding replaces that slot's JSON artifact reference when effective inputs are constructed. For slots allowing multiple media types, select one explicitly with `--media-type SLOT=TYPE`. Scientific upload metadata is optional: `--metadata SLOT=metadata.json` takes a JSON object and defaults to `{}`.

Supplied JSON can also reference existing completed artifacts without a local file binding. The CLI checks those references before creating uploads. Keep original source files unchanged and available until their uploads complete; changed sources require a new directory. Input JSON values still have to satisfy the selected model's contract.

## Bind local collections

The collection-copy example declares input `dataset` and output `snapshots`. After publishing that model as `collection-example`, create two tiny inputs outside the analysis directory:

```sh
umask 077
mkdir -p collection-inputs/mesh collection-inputs/snapshots
printf 'grid fixture\n' > collection-inputs/mesh/grid.bin
printf 'snapshot fixture\n' > collection-inputs/snapshots/0001.bin
cat > collection-inputs.json <<'JSON'
{}
JSON
enumeray analysis init collection-example --directory collection-analysis \
  --inputs collection-inputs.json --collection dataset=collection-inputs \
  --media-type dataset=application/octet-stream
```

The empty scientific JSON is complete for this declaration because the binding supplies its required `dataset` reference. Other models may require additional scientific values. Initialization freezes the two relative member paths, byte sizes and checksums without uploading. The uniform media type must be permitted by the slot. Keep the source tree unchanged until upload completes.

For an explicit member list and per-member media types, use `--collection-manifest dataset=MANIFEST.json` instead of `--collection`. The manifest's parent is the source root; list only intended files and keep the analysis outside that tree. Existing completed `{collection_id}` references can be supplied in input JSON without a local binding. See the [example walkthroughs](https://github.com/raysofspace/enumeray-cli/blob/main/crates/cli/examples/README.md) for the collection declaration and wrapper.

## Submit and resume

```sh
enumeray analysis submit csv-analysis
enumeray analysis status csv-analysis
enumeray analysis wait csv-analysis --check-outcome
```

`submit` explicitly authorizes execution at the reviewed destination. It durably freezes inputs, resumes each upload, substitutes verified artifact references, saves a prepared request and retains the acceptance receipt. After interruption or a lost response, repeat the same command and directory. The upload and submission identities remain the same.

For an ordinary current-publication analysis, a changed current publication before first submission requires a new directory and disclosure review. Candidate trials keep their explicitly selected publication. Once submission may have reached the API, recovery replays the exact saved request, allowing the original acceptance to be recovered even if the model has since changed. It never silently chooses the latest publication.

An accepted analysis is observed by its saved run ID. Original source files are no longer needed for observation, acceptance reconciliation or collection. Renew credentials for the same account and resume the directory after authentication failure. Another account/API is rejected.

Public run states are `queued`, `running`, `succeeded`, `failed` and `cancelled`. While running, status distinguishes preparation, backend waiting, execution, collection and uploading. Stale progress does not establish failure. `wait` uses the ordinary [run observer](https://doc.enumeray.com/users/runs/). Its default exits 0 for any terminal state. With `--check-outcome`, exits are success 0, failure 3, cancellation 4 and local deadline 5; terminal JSON remains on stdout. A deadline or Ctrl-C ends observation while execution continues. Use `--interval` and `--deadline` to adjust observation.

Inspect logs or request cancellation with the same directory:

```sh
enumeray analysis logs csv-analysis --limit 50
enumeray analysis logs csv-analysis --follow
# In another terminal, if you intend to stop remote execution:
enumeray analysis cancel csv-analysis
enumeray analysis status csv-analysis
```

Status, waiting, log observation and cancellation can run concurrently, including while submission uploads or collection downloads are active. Submission and collection still have one writer per directory. A directory permits one live log follower; retained-page reads remain independent. Follow automatically resumes its private `state/logs.json` checkpoint; repeat the same folder command after interruption or credential renewal. See [log recovery](https://doc.enumeray.com/users/logs-provenance/) for gaps, stream output and observation exits.

`cancel` saves local intent before contacting the service. A lost reply leaves the request uncertain; repeat `analysis cancel csv-analysis` to retry cancellation of the same accepted run. Local intent alone does not prove API acknowledgement or backend termination. Use `status` or `wait` to inspect authoritative cancellation and final run state. See [cancellation and retries](https://doc.enumeray.com/users/cancellation-retries/).

Logs, waiting and cancellation require an accepted run identity from a saved receipt or attachment. If an imported or initialized operation has uncertain submission and no accepted identity, explicitly repeat `analysis submit csv-analysis` to reconcile the same operation first. Observation never submits work or sends cancellation. A deliberately new execution starts with `analysis init` into a new directory.

## Recover an existing execution

If the directory was lost, attach accepted work by its run ID or a saved receipt:

```sh
enumeray analysis attach RUN_UUID --directory recovered-analysis
# Alternative, using the saved acceptance receipt:
enumeray analysis attach --receipt run-receipt.json --directory receipt-analysis
enumeray analysis status recovered-analysis
enumeray analysis logs recovered-analysis
enumeray analysis wait recovered-analysis --check-outcome
enumeray analysis collect recovered-analysis
```

Choose either a run ID or `--receipt`. A receipt file contains the receipt object itself, as saved in `state/receipt.json` or extracted from the `.receipt` field of `analysis submit --json` output. The full submit output is an analysis summary, not a receipt. Attachment authenticates against the selected API and saves the accepted inputs, exact publication/contract, purpose and available provenance in a new private directory. It uses accepted-run evidence even when today's current publication changed or is inaccessible. It does not submit work, upload inputs or need original source files. Repeat the same attach command and directory after interruption; unrelated existing destinations and conflicting evidence are preserved.

An attached directory supports the usual status, logs, wait, cancellation and collection commands. `analysis submit` cannot turn it into another execution. Attachment grants no additional access: run inspection requires existing read authority, and only the original submitter with cancellation authority can cancel the run. Current model administration alone does not permit cancellation of another account's work. Original local paths, source checksums and upload checkpoints that are absent from retained records are reported as unavailable.

Import a legacy operation from the former `run prepare` command without contacting the API:

```sh
enumeray analysis import --prepared prepared-run.json --directory imported-analysis
# Authenticate to the original API/account and review the retained request.
enumeray analysis status imported-analysis
# Explicitly authorize submission or reconciliation of that exact operation:
enumeray analysis submit imported-analysis
```

The former `run prepare` and `run submit` commands are removed and have no compatibility aliases. Import validates and preserves the original API/account, request, publication, purpose/delegation and idempotency key. It leaves the source file unchanged and does not need a configured profile or credentials. An operation without a saved receipt may already have been accepted; import keeps that uncertainty. Only explicit `analysis submit` sends the original request to reconcile or obtain acceptance. It never prepares against a newer publication or creates a replacement key. Completed artifact and collection references remain reusable without uploading their bytes again. A never-accepted operation remains subject to the API's current acceptance rules; a rejected original request does not authorize a replacement execution.

## Copied-folder use

Copy a complete accepted directory to another host, retain its private permissions and use the same API/account. A copied directory continues the same execution and retry identities; initialize a new directory for deliberately new work. Status, logs, wait, cancellation and collection do not need the old absolute source paths. Completed result files are rehashed and reused when they match pinned evidence. Preserve unfinished download checkpoints too; a partial byte prefix may need to download again when its absolute destination changes. For work whose uploads are unfinished, retain the original unchanged sources and transfer state before moving hosts; submission explains missing-source requirements. Importing a prepared run does not reconstruct earlier upload operations or missing source bytes.

Keep the prepared operation until its outcome is known, and retain receipts or the accepted directory afterward. If both the uncertain operation's identity and its receipt are lost, search authorized `run list` history for accepted work and attach its run ID. History cannot prove that a lost request was never accepted. Do not silently submit a replacement. See [files, logs and recovery](https://doc.enumeray.com/troubleshooting/data-recovery/).

## Collect verified evidence

```sh
enumeray analysis collect csv-analysis
```

Collection requires a terminal run. It saves:

- `results/inspection.json`, the retained final inspection;
- `results/effective-inputs.json` and `results/outputs.json`, scientific JSON values;
- `results/provenance.json`, exact publication, execution and artifact evidence;
- `results/files/<artifact-uuid>`, downloaded file outputs;
- `results/collections/<collection-uuid>/`, full collection manifests and verified selected output members;
- `bundle.json`, the complete bundle inventory keyed by declared output name, including local paths, sizes, checksums, immutable versions and optional output failures.

Repeat `collect` after interruption. It retains operation progress, resumes verified partial downloads by byte range and checks existing files against pinned evidence before adopting them. Conflicting files are preserved; move them aside or restore the exact bytes before retrying. It never substitutes a different remote artifact version. Download checkpoints remain beside result files until explicitly removed.

For the two-member collection example above, review the saved destination/disclosure, then submit explicitly and collect in stages:

```sh
enumeray analysis submit collection-analysis
enumeray analysis wait collection-analysis --check-outcome
enumeray --json analysis collect collection-analysis --evidence-only > collection-evidence.json
enumeray --json analysis collect collection-analysis \
  --member snapshots=mesh/grid.bin > collection-partial.json
enumeray --json analysis collect collection-analysis > collection-complete.json
```

The first collection saves provenance and full manifests without scientific payloads. The second downloads only `mesh/grid.bin` from the declared `snapshots` output. Both report `complete: false` for this fresh two-member example and leave `bundle.json` unpublished. The final command completes the remaining output and reports `complete: true`; `bundle.json` records both members through its collection manifest/root mapping. All three steps observe the same execution. Repeat an interrupted step with the same directory. Default collection downloads all output members; repeated `--member SLOT=PATH` selects members explicitly.

A complete bundle records the scientific state independently. Failed or cancelled runs can have complete evidence bundles; `collect` exits 0 when the requested step succeeds, including partial or evidence-only collection. Check `complete: true` and `bundle.json` for verification of every output. Missing optional outputs remain visible as reported failures. Attached runs support the same partial and evidence-only collection. Original input bytes remain outside the bundle; retain those sources separately when needed. Protect the directory: it contains scientific inputs/results, available source evidence and private operation records.

Use analysis directories for execution submission and the normal local lifecycle, including scripts. The `run` commands retain authorized history and investigation by ID; independent `artifact` and `collection` utilities handle reusable data operations.

## Script with structured output

Use global `--json` for compact machine-readable responses. Runtime error objects, progress and recovery hints go to stderr. Argument parsing errors remain plain text on stderr with exit 2. `--timeout` bounds an individual HTTP operation; `analysis wait --deadline` bounds observation and does not limit scientific runtime. A native command's redirected human output is still human output unless `--json` is present.

This complete scalar-starter recipe needs `jq`, an authenticated profile and access to the activated `example` model. Run initialization once in a private working directory:

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

Review `model.json`, `initialized.json`, `reviewed-analysis.json` and `scripted-analysis/inputs.json`: check the scientific values, exact publication, execution location and full disclosure. Edit scientific values in `inputs.json` inside the analysis directory only before the first submit. For an automation, approve the saved contract/disclosure and intended inputs before authorizing the submission stage. Initialization never authorizes execution.

Run the following submission/observation stage after review. After interruption or an uncertain reply, run this same stage with the same directory; do not rerun initialization:

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

Wait writes terminal JSON before exits 3/4. `--check-outcome` exits success 0, scientific failure 3, cancellation 4 or observation deadline 5; ordinary errors exit 1, invalid arguments 2 and interruption 130. Default wait exits 0 for every terminal outcome, with deadline exit 1. Deadline/Ctrl-C leave remote execution running. Collection can succeed for failed/cancelled runs and for partial/evidence-only requests; this script checks full completeness separately. A collection failure stops the script with its own nonzero status and is recoverable by repeating collection with the same directory.

Inspect `bundle.json` and `results/outputs.json` after complete collection. Keep `submission.json` as a summary; its nested `.receipt` and `state/receipt.json` contain the raw acceptance object. A replayed receipt's `queued` state describes original acceptance. Use status/wait for current progress. No UUID extraction is needed. Saved browser sessions renew automatically during active workflows. Authentication failures retain operation identity: follow the error guidance to use `auth refresh`, `auth login` or the automation's replacement scoped token for the same API/account, then resume the same stage. An unknown refresh-exchange outcome requires login; do not discard the analysis directory or reuse a potentially consumed token.

## Records to retain

| Stage | Retain | Why |
| --- | --- | --- |
| Editable initialization or unfinished uploads | Entire private analysis directory and unchanged original file/collection sources | The directory pins publication/disclosure, local fingerprints and transfer identities; it does not copy source bytes |
| Uncertain submission | Original directory or imported prepared operation and any acceptance receipt | Reconcile the exact saved request/key before deciding on another execution |
| Accepted execution | Whole directory, including state files, checkpoints and stable lock files | Status, logs, cancellation, copied-folder use and collection retain the original execution |
| Collected evidence | Directory, `bundle.json`, `results/` and original input bytes separately when needed | The bundle verifies outputs and retained provenance; original scientific input bytes remain external |
| Author release | Private release operation and immutable installed files, runtime/dataset dependencies | Candidate/review evidence and accepted work remain tied to the exact installation |

Do not edit frozen inputs, saved request keys or acceptance evidence to make a retry succeed. Protect scientific records and private state when copying or backing them up. Credentials and temporary storage capabilities are not retained in analysis state; renew access explicitly. If unfinished sources are unavailable, preserve the original operation and investigate its outcome before requesting new work.

A copied or attached folder continues an existing execution. Deliberately new work uses `analysis init` and a new directory with a fresh review. Historical reruns against old contracts and parameter sweeps are outside the current release.
