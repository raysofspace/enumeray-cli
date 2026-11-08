# CLI JSON contracts

Use `enumeray --json` when a script needs to read a command's result. Set the
flag explicitly even when redirecting output to a file. Check the exit code,
then read the JSON fields that govern your next action. Use the
[scripting recipe](#script-with-structured-output) for a complete run.

The [CLI output schemas](https://doc.enumeray.com/contracts/cli-output-v1.json)
come from the Rust types in `enumeray_contracts::automation`. The bundle
contains five JSON Schema documents you can use independently.

| Schema key | What it describes |
| --- | --- |
| `analysis-summary` | Initialization, submission and status for an analysis folder |
| `analysis-collection-summary` | Partial and complete collection of results |
| `release-summary` | Status and review for a saved model release |
| `compatibility-receipt` | Comparison of selected installation identities |
| `runtime-error` | Errors reported by user commands |

These outputs carry `schema_version: 1`. Scientific inputs and results use
the model's JSON schemas. Model inspection returns the HTTP management
response. The transcripts below come from CLI processes running against
disposable HTTP fixtures. They normalize local paths, API origins and generated
release IDs, and omit tokens and temporary storage URLs.

## Compatibility and safe decisions

The compatibility baseline is provisional, so version-1 formats can differ
between builds. Choose a verified service and public source pair and use the
schemas and transcripts for that pair. Keep saved operation directories with
their selected build when recovering work.

With a compatible contract, scripts can ignore additional object fields. Read
the required decision fields with their declared types. Nullable values use
`null` when unavailable, and empty arrays and maps use `[]` and `{}`. The
original acceptance receipt uses its own optional-field representation.

Stop and keep the current directory if a version or decision value is unknown,
a required field is missing, or a type is wrong. In particular, treat a missing
`complete`, `available`, `stage` or guard as an error. Inspect unknown blocker
and error codes before taking action. Partial and full collection use the same
schema. Stored analysis, release, upload and bundle formats use version 1.

## Installation comparison receipts

To identify your executable, run `enumeray compatibility inspect --json`.
It reports the build's embedded identity and a separate check of the executable
file without reading a login profile. `compatibility check --json` compares the
selected inputs and writes a new private `compatibility-receipt`. Follow the
[installation procedure](INSTALL.md#inspect-the-selected-installation) to
choose signed release, unsigned development or authenticated transfer evidence.

Check `schema_version`, `status` and `trust` before using the receipt.
`match` exits 0. `mismatch` and `missing_evidence` exit 1 and still write the
receipt to stdout and the requested new file. Invalid CLI arguments exit 2.
Malformed input or an existing output path produces a runtime error and
preserves the existing file. The `trust` field describes the evidence used as
`signed_release`, `unsigned_development`, `authenticated_transfer` or
`unverified`. Read it separately from the identity comparison result.

Each check records the component, field, expected and observed values, and
reason. Read `selected` for the exact service and public identities and evidence
hashes. Read `limits` for the scope the comparison leaves unchecked.
`--local-only` compares saved evidence. A live comparison stops if required
observations are stale or unavailable. A heartbeat identifies the build the
connector reports. Documentation and example manifests identify selected
sources. Journal ownership, file verification, site qualification, scientific
approval and execution authority each require their own evidence.

## Streams, errors and exits

| Result | stdout | stderr and exit |
| --- | --- | --- |
| Successful ordinary JSON command | One compact JSON object and newline | Progress may be written to stderr; exit 0 |
| Runtime failure with `--json` | Empty unless the operation explicitly returns a status summary | One versioned runtime error object; exit 1 unless the command documents another outcome |
| Invalid arguments | Empty | Plain-text parser diagnostic; exit 2 even with `--json` |
| `analysis wait --check-outcome` | Terminal run observation before scientific outcome exit | Succeeded 0, failed 3, cancelled 4; local observation deadline 5, interruption 130 |
| `analysis collect` | Collection summary, including a successful partial result | Exit 0 means this requested collection step succeeded; inspect `complete` separately |
| `compatibility check` | Versioned identity comparison receipt | Match 0; mismatch or missing evidence 1; no repair or credential refresh |
| `model release resume` with a known blocker | Versioned release summary | Exit 1; retain the directory and inspect its blockers |

Without `--check-outcome`, `analysis wait` exits 0 for any terminal state and
1 when the observation deadline expires. The remote run can continue after
an observation deadline or interruption. A successful collection command can
save evidence of a failed or cancelled run. Log follow mode writes
newline-delimited events. See the [log command reference](https://doc.enumeray.com/reference/cli/analysis-logs/)
for that format.

The error projection applies to native user commands. Connector host commands
and compute-runner use their separate credential domains and protocol outputs.

After a command fails, check stderr for a runtime error. It contains
`schema_version`, nullable HTTP `status`, `error.code`, `error.message` and
nullable `retry_after`. API errors also preserve the request ID and structured
details when available. `retry_after` copies the HTTP header. Use documented
codes and typed fields to choose the next action. Keep the exact operation
before renewing credentials or recovering from a lost reply.

## Script with structured output

This walkthrough saves the result of one run so another program can use it.
Use `jq`, a signed-in CLI profile and access to the scalar starter published
as `example`. Start in a new private working directory. If you use a named
profile, pass the same `--profile NAME` after `enumeray` on every command.

### Prepare one run

```sh
(
  set -eu
  umask 077
  printf '%s\n' '{"value":2}' > inputs.json
  enumeray --json model get example > model.json
  enumeray --json analysis init example --inputs inputs.json \
    --directory scripted-analysis > initialized.json
  enumeray --json analysis status scripted-analysis > reviewed-analysis.json
)
```

Read [the four files](#read-the-four-files-before-submitting) before
submitting. Confirm the inputs, model version, execution location and data
access. To change the inputs before submission, edit
`scripted-analysis/inputs.json`.

### Submit, wait and collect

Run this stage after review. After interruption, repeat it with the same
analysis directory.

```sh
(
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
      jq -e '.schema_version == 1 and .complete == true' collection.json >/dev/null
      ;;
    5|130)
      echo 'Observation ended; retain the analysis directory and resume waiting.' >&2
      ;;
    *)
      echo 'No terminal outcome was obtained; inspect the error and retained directory.' >&2
      ;;
  esac
  exit "$outcome"
)
```

For a successful scalar run, check for `{"doubled":4}` in
`scripted-analysis/results/outputs.json`. The script checks collection
completeness before returning the run outcome. With `--check-outcome`, wait
returns 0 for success, 3 for failure, 4 for cancellation or 5 for a local
deadline. Exit 130 means interruption. A failed or cancelled run can still
have a complete record. Open `terminal-run.json` for the outcome and
`collection.json` for the collected record.

Read the original acceptance from the nested `receipt` in `submission.json`
or the saved `scripted-analysis/state/receipt.json`. Use status or wait for
current progress. If collection fails, rerun `analysis collect` with the
same directory. If sign-in expires, renew the same service and account or
replace the scoped token before resuming.

## Script log collection

Use `enumeray --json run logs RUN_UUID` or
`enumeray --json run events RUN_UUID` to read one retained page. Replace
`RUN_UUID` with your run ID. Each response is one JSON object. Keep the
response and pass its `next_cursor` unchanged with `--cursor` to read the
next page. Use the last page's `resume_cursor` to poll for new entries.
Cancellation events can appear on more than one event page. Log records
include source offsets, epochs and gap markers.

For a live stream, `--follow` writes newline-delimited JSON with one event
per line. The analysis command saves its own checkpoint. With only a run ID,
choose a private checkpoint file. Substitute your analysis directory for
`analysis` if needed.

```sh
enumeray --json analysis logs analysis --follow >> log-events.ndjson
enumeray --json run logs RUN_UUID --follow --state watch.json >> run-log-events.ndjson
```

Each checkpoint belongs to one API origin, account and run. A scoped token
needs `me:read` when resuming. The CLI writes the event before recording its
position, so an interruption between those actions can replay entries.
Use the run ID and log position to deduplicate them.

The stream reads the final retained pages after the run reaches a terminal
state. A stream exit of 0 means observation completed. Use
`analysis wait analysis --check-outcome` to act on the run's outcome. For
recovery from missing pages, see the
[analysis logs](https://doc.enumeray.com/reference/cli/analysis-logs/)
and [run logs](https://doc.enumeray.com/reference/cli/run-logs/) references.

## Analysis summaries and acceptance identity

Complete specimens:

- [Initialized](examples/automation/analysis-initialized.json) and
  [reviewed before submission](examples/automation/analysis-reviewed.json).
- [Uncertain acceptance](examples/automation/analysis-submission-uncertain.json)
  and [reconciled submission](examples/automation/analysis-submitted.json).
- [Queued](examples/automation/analysis-status-queued.json),
  [running](examples/automation/analysis-status-running.json),
  [succeeded](examples/automation/analysis-status-succeeded.json) and
  [failed](examples/automation/analysis-status-failed.json) observations.
- [Runtime error](examples/automation/analysis-runtime-error.json).

Each transcript lists the command arguments, exit code, parsed stdout and
exact stderr. Use `contract` to choose a schema. For `runtime-error`, parse
the JSON string in stderr. These fixtures use the sample model `model` and
publication `release-one`. Use your own model and publication IDs for a live run.

`stage` describes the local workflow and can be `editable`, `uploading`,
`submission_uncertain`, `accepted` or `attachment_incomplete`. Only `accepted`
has a known `run_id`. `receipt` records the original acceptance and remains
`queued` when the current `run` is already running or terminal. All three share
the same model/publication/run identities. Queued and terminal observations have
no running phase. Attached evidence may know the accepted run without retaining
the original acceptance receipt; local source bytes and fingerprints may remain
explicitly unavailable. None of those omissions authorizes another submission.

`collection` and `cancellation` describe retained local progress, independently of
the current run. A pending or uncertain cancellation does not establish remote
termination. After an uncertain submission, explicitly repeat submit for the
same directory; status and import never submit automatically.

## Read the four files before submitting

The [complete four-file specimen](examples/automation/four-file-review.json)
contains the actual `model.json`, `initialized.json`, `reviewed-analysis.json`
and analysis `inputs.json` produced by the [scripting recipe](#script-with-structured-output).
The fixture uses directory `analysis`; the recipe chooses `scripted-analysis`.
Review the corresponding paths and identities consistently in your own case.

| File | Decision fields to review |
| --- | --- |
| `model.json` | `model_id`, `current_publication_id`, current scientific definition, execution location/disclosure and submission availability; discovery does not itself authorize a run |
| `initialized.json` | `schema_version`, pinned `model_id`/`publication_id`, `context_id`, installation, execution location/disclosure and `stage: editable`; initialization has created no execution |
| `reviewed-analysis.json` | Same pinned identity and destination, no accepted `run_id`/`receipt`; inspect input-file/collection progress and the next action |
| `analysis/inputs.json` | Scientific values in the private analysis directory, validated against the selected publication's input schema |

Approve the exact inputs, execution location and data access before submitting.
The analysis keeps the publication it saved, even if a different one becomes
current. Stop if review fields are missing. After a lost submission reply,
recover from the same directory so you keep the original request identity.

## Collection completeness

Compare the complete specimens for [empty scalar output files](examples/automation/analysis-collected-empty.json),
[evidence-only partial collection](examples/automation/analysis-collected-partial.json),
[verified file outputs](examples/automation/analysis-collected-files.json) and
[failed scientific outcome](examples/automation/analysis-collected-failed.json).
`scientific_state` and collection `complete` are separate decisions. `bundle` is
`null` until full output verification completes, then names the immutable bundle.
`outputs` contains verified local file mappings. `collections` reports each
collection's immutable digest, manifest, local root and completeness. Empty maps
are valid for scalar work. A partial success is never a promise that all outputs
exist. The complete bundle also retains any optional-output failures.

## Saved release decisions

Compare [prepared](examples/automation/release-prepared.json),
[ready](examples/automation/release-ready.json),
[reviewed](examples/automation/release-reviewed.json) and
[blocked](examples/automation/release-blocked.json) release summaries. Required
decision fields include `stage`, `candidate_status`, `blockers`,
`guard_changed`, `registration_uncertain`, `activation_uncertain`,
`candidate_is_current` and the current/expected publication identities. Nullable
trial/authority fields are unavailable, not affirmative approval. Empty trial
counts are valid and establish no scientific validation.

Only explicit review saves a new activation comparison. Do not activate while
blocked, uncertain or after the guard changed; reconcile the same candidate and
review again as directed. Trial success remains advisory and never activates a
publication. `next_command` is a public command preserving the selected profile;
display it for an authorized next step rather than executing it blindly.

All release initialization source modes need `me:read`, `models:read` and current
model administration. `init --file` and `init --rollback-from` create a fresh
candidate; `init --candidate` adopts an exact existing candidate. Initialization
does not discover grants. The later fresh registration on `resume` needs
`connectors:read` and `models:manage`; delegated-candidate adoption/inspection
needs no grant inventory, while review/activation requires `models:manage` and
available server-reported release authority.
