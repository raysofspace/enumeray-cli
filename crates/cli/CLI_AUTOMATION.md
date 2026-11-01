# CLI JSON contracts

Use `enumeray --json` for automation. Redirecting human output does not select
JSON. Analysis initialization/submission/status, collection and saved-release
summaries carry `schema_version: 1` and have public typed projections in
`enumeray_contracts::automation`. Scientific inputs, schema definitions and
results retain their model-defined JSON shapes. A model inspection is the
underlying HTTP management projection, not an analysis summary.

The downloadable [CLI output schemas](https://doc.enumeray.com/contracts/cli-output-v1.json)
contain four independently usable JSON Schema documents, keyed
`analysis-summary`, `analysis-collection-summary`, `release-summary` and
`runtime-error`. They are exported from the Rust types used by the CLI rather
than separately maintained examples. The complete transcripts linked below are
captured through actual CLI processes and disposable HTTP fixtures. Only local
paths, API origins and generated release identities are normalized; tokens and
temporary object-store capabilities are never included.

## Compatibility and safe decisions

The current alpha remains an unreleased compatibility candidate. Project-owned
version-1 formats evolve in place until a persistent release is deliberately
activated. A matching `schema_version` alone does not establish compatibility
between different candidate builds: select a verified service/public source pair
and its matching schema/transcript set. Activation of a persistent baseline
establishes the documented subsequent compatibility obligations.

Within an accepted compatible contract, additional object fields can be ignored.
Required decision fields retain their types and meaning; unavailable nullable
fields are emitted as `null`, arrays/maps as `[]`/`{}` when empty. The original
acceptance receipt retains its historical optional-field representation. An
unknown version or decision enum, a missing required field or a wrong field type
must stop the automation and preserve the current directory. Do not turn a
missing `complete`, `available`, `stage` or guard into a successful default.
Unknown blockage/error codes require inspection rather than execution or retry
with a replacement identity.

This change introduces versioned CLI projections and normalizes partial/full
collection to one shape. Scripts written against earlier unversioned candidate
responses must select these schemas and check required fields. Stored analysis,
release, upload and bundle formats remain version 1; this is not a supported
cross-release upgrade or a reason to discard retained operation identity.

## Streams, errors and exits

| Result | stdout | stderr and exit |
| --- | --- | --- |
| Successful ordinary JSON command | One compact JSON object and newline | Progress may be written to stderr; exit 0 |
| Runtime failure with `--json` | Empty unless the operation explicitly returns a status summary | One versioned runtime error object; exit 1 unless the command documents another outcome |
| Invalid arguments | Empty | Plain-text parser diagnostic; exit 2 even with `--json` |
| `analysis wait --check-outcome` | Terminal run observation before scientific outcome exit | Succeeded 0, failed 3, cancelled 4; local observation deadline 5, interruption 130 |
| `analysis collect` | Collection summary, including a successful partial result | Exit 0 means this requested collection step succeeded; inspect `complete` separately |
| `model release resume` with a known blocker | Versioned release summary | Exit 1; retain the directory and inspect its blockers |

Default analysis wait exits 0 for every terminal scientific state; without
`--check-outcome`, its deadline exits 1. A deadline or interruption does not
cancel remote execution. Successful evidence collection can accompany failed or
cancelled science. Follow mode is a separate newline-delimited event protocol;
these ordinary-object schemas do not describe `run logs --follow` or
`analysis logs --follow`.

The error projection applies to native user commands. Connector host commands
and compute-runner use their separate credential domains and protocol outputs.

Runtime errors contain required `schema_version`, nullable HTTP `status`,
`error.code`, `error.message` and nullable `retry_after`. API errors preserve the
request ID and structured details when available. `retry_after` is the original
HTTP header string; it is not permission to repeat a scientific launch. Parse
an error only after the process fails, and retain the exact operation before
renewing credentials or reconciling an uncertain response. Error messages and
human output are explanatory text; branch on documented codes and typed fields.

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

Each transcript separates command arguments, exit code, parsed stdout and exact
stderr. The `contract` key selects the output schema; for `runtime-error`, parse
the JSON stderr string. Fixtures use the scalar `model` / `release-one` and its
own tiny input. Their names are illustrative service identities, not a public
installed offering or replacements for the `example` model in the author guide.

`stage` describes the local workflow: `editable`, `uploading`,
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
and analysis `inputs.json` produced by the [scripting recipe](ANALYSIS_GUIDE.md#script-with-structured-output).
The fixture uses directory `analysis`; the recipe chooses `scripted-analysis`.
Review the corresponding paths and identities consistently in your own case.

| File | Decision fields to review |
| --- | --- |
| `model.json` | `model_id`, `current_publication_id`, current scientific definition, execution location/disclosure and submission availability; discovery does not itself authorize a run |
| `initialized.json` | `schema_version`, pinned `model_id`/`publication_id`, `context_id`, installation, execution location/disclosure and `stage: editable`; initialization has created no execution |
| `reviewed-analysis.json` | Same pinned identity and destination, no accepted `run_id`/`receipt`; inspect input-file/collection progress and the next action |
| `analysis/inputs.json` | Scientific values in the private analysis directory, validated against the selected publication's input schema |

Approve those exact scientific inputs and disclosure before the explicit submit
stage. A later current-publication change does not replace the publication
already pinned by the analysis. Missing review fields stop the script; they are
not an invitation to initialize a replacement directory after uncertain submit.

## Collection completeness

Compare the complete specimens for [empty scalar output files](examples/automation/analysis-collected-empty.json),
[evidence-only partial collection](examples/automation/analysis-collected-partial.json),
[verified file outputs](examples/automation/analysis-collected-files.json) and
[failed scientific outcome](examples/automation/analysis-collected-failed.json).
`scientific_state` and collection `complete` are separate decisions. `bundle` is
`null` until full output verification completes, then names the immutable bundle.
`outputs` contains verified local file mappings; `collections` reports each
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
