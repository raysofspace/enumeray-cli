# Model author guide

Use this guide to take a model from a local test to a version other people can
run. Your part is to describe the inputs and results, connect your calculation
to the wrapper and check the science. The connector owner helps you arrange
where the model will run and gives your account permission to publish there.

| I want to… | Start here |
| --- | --- |
| Try a local example | [Download the projects and run the tests](#run-the-downloaded-model-examples-offline). You need Python 3.9 or newer to run them. |
| Adapt a model | [Generate a starter and map your inputs and outputs](#scaffold-and-map-your-science). You can test it before signing in. |
| Publish a tested model | [Arrange installation](#install-an-immutable-release-and-select-compute), then [trial the candidate and activate it](#save-a-release-resume-readiness-review-and-activate). |
| Give other people access | [Choose who can find and run the model](#choose-visibility-and-access-explicitly) or [give a named reviewer access to a candidate](#hand-a-candidate-to-another-administrator). |
| Change the version users run | [Publish an update or restore a prior version](#updates-and-rollback). |

The scalar starter turns `2` into `4`. When you replace its calculation, add
tests with results you can verify for your model. If you are running a model
that is already published, follow the [analysis guide](ANALYSIS_GUIDE.md).

Choose an unused model ID in Enumeray's shared catalogue before generating
your project. A lab or project prefix helps distinguish it. Keep the same ID
from `model init` through publication and share it with your users. Treat
`example` and `csv-example` in sample declarations and reference commands
as illustrative IDs. [Sign in to Enumeray](https://doc.enumeray.com/start/login/)
before publishing. Replace sample context IDs, paths and values such as
`RUN_UUID`. Use a new folder for each release or run and keep it when
resuming that work.

## Run the downloaded model examples offline

Download the [model examples archive](https://doc.enumeray.com/examples/model-examples.tar.gz)
and its [model examples archive checksum](https://doc.enumeray.com/examples/model-examples.tar.gz.sha256)
into a new directory, keeping those filenames. These commands need Python 3.9+.
Its archive extractor preserves the file names used in the manifest, including
Unicode spelling. You can run these tests without an API, credentials or a
source checkout.

```sh
(
  python3 - <<'PY' &&
from hashlib import sha256
from pathlib import Path
archive = Path("model-examples.tar.gz")
expected = Path("model-examples.tar.gz.sha256").read_text().split()[0]
if sha256(archive.read_bytes()).hexdigest() != expected:
    raise SystemExit("Archive checksum mismatch")
PY
  python3 -m tarfile -e model-examples.tar.gz &&
    cd enumeray-examples-* &&
    python3 verify-examples.py --root .
)
```

The block checks the checksum before opening the archive. If that check fails,
download the archive and its matching checksum again before continuing. The
extracted root contains `examples-manifest.json`, which records the package
version, exact public source revision and file hashes. The verifier checks those
bytes and tests the wrappers, local runtime and HTTP digest examples. For
an example of validating a solver and wrapper together before publication, see
[scientific integration validation](https://doc.enumeray.com/authors/scientific-validation/).

After verification succeeds, run the scalar starter on the supplied input.
Start from the download directory.

```sh
(
  set -eu
  cd enumeray-examples-*/starter-kit/python-json
  project=$PWD
  work=$(mktemp -d)
  cd "$work"
  python3 -B "$project/wrapper.py" execute \
    --request "$project/fixtures/request.json" --result "$work/result.json"
  python3 -m json.tool "$work/result.json"
  printf 'Saved %s\n' "$work/result.json"
)
```

Look for `"doubled": 4` under `outputs`. The request fixture supplies input
`2`. Use this table to choose the files for another local example.

| Project from the extracted root | Model and fixture |
| --- | --- |
| `starter-kit/python-json/` | `starter-python-json`, named example `starter-example`; `fixtures/request.json` has `value: 2`, expected `doubled: 4` |
| `starter-kit/python-files/` | `starter-python-files`, named example `starter-example`; CSV fixture rows `0,1` and `1,2` become `0,2` and `1,4`, with `sample_count: 2` |
| `starter-kit/executable-json/`, `starter-kit/executable-files/` | `starter-executable-json` / `starter-executable-files`; same corresponding fixtures, with a fixed `demo_solver.py` command |
| `examples/scalar_model/` | `scalar-double`; local `request.json` uses `value: 2`, expected `doubled: 4`; run `wrapper.py execute` with `protocol.py` and `model.py` alongside it |
| `examples/collections/` | `collection-example`; `fixtures/request.json` and `fixtures/workspace/` copy two nested members into `snapshots/`, with `member_count: 2`; empty sets are valid |
| `examples/scalar-publication.json`, `examples/slurm-scalar-publication.json` | Installed `scalar-double` / named example `double-two`; the separate `scalar-inputs.json` uses `value: 21`, expected `doubled: 42`; these declarations require a matching installed `run-model` entrypoint |

Use a model ID you choose when adapting a bundled starter. The connector's
single-user journey separately uses `development-double`
under `models/development-double-r1/`. Choose one project and keep its model ID,
publication and request fixtures together; do not combine fixtures by filename.
The scalar tutorial uses `/opt/enumeray/models/my-lab-double/r1`, the installed scalar
templates use `/opt/enumeray/models/scalar-double/r1`, and the collection declaration
uses `/opt/models/collection-example`. These are example installed roots to agree
with a site, separate from the extracted project and per-run workspace. Bundled
starter declarations leave site context/root unset until that agreement.

The individual declarations describe already installed offerings; they do not
install code. A `run-model` entrypoint must invoke an implementation matching its
declaration; it is not the filename of the supplied Python wrapper. Start with a
complete starter when you need a ready local project. The
[file/download chooser](https://doc.enumeray.com/reference/manifests/#choose-the-file-for-your-command)
distinguishes author declarations, scientific inputs, wrapper envelopes and
collection manifests. The HUXt, REBOUND, SWMM and LAMMPS demo models each have
documented inputs and outputs.

## Responsibilities and execution choices

Agree with the connector owner on a context, installation directory and
execution account. The model owner supplies the wrapper, schemas, examples,
software and any installed data. The connector owner sets up compute and
issues the grant to publish there. Arrange separately who can copy the files
to the host and verify the execution account can read them.

Choose two independent dimensions:

| Driver | Connection | Execution |
| --- | --- | --- |
| process | direct | Foreground wrapper under the context's local execution account |
| process | SSH | Foreground wrapper on the SSH execution host |
| Slurm | direct | Model batch file submits to the locally accessible scheduler |
| Slurm | SSH | Model batch file submits through the SSH scheduler host |

Contexts own connection details and credential references. The publication selects a
context; hosts, SSH keys and arbitrary launchers remain infrastructure settings. Model wrappers need no API,
connector or storage credentials. They receive staged files; connectors upload
declared outputs. Slurm scripts own scheduler routing/resources and runtime setup.

## Files and one editable publication

The generated project runs without an API checkout:

| File | Purpose |
| --- | --- |
| `publication.json` | Complete editable metadata, identities, schemas/examples and execution settings |
| `model.py` | Standalone scientific Python functions |
| `implementation.py` | Scientific input/output mapping; executable projects call fixed native argv |
| `wrapper.py`, `protocol.py` | Process entrypoint, bounded envelopes and durable result publication |
| `fixtures/`, `test_starter.py`, `test_protocol.py` | Full request/result fixtures, scientific checks and shared boundary tests in fresh workspaces |
| `model.sbatch` | Slurm resources/runtime setup and foreground worker |

A complete process publication is:

```json
{
  "publication_version": 1,
  "model": {
    "description": "Editable protocol demonstration; replace with your scientific model.",
    "id": "example",
    "name": "example"
  },
  "identities": {
    "model": "example@v1",
    "wrapper": "example-wrapper@v1",
    "runtime": "author-example:python3-standard-library:1"
  },
  "execution": {
    "context_id": "lab-cpu",
    "release_path": "/opt/enumeray/models/example/r1",
    "entrypoint": [
      "wrapper.py",
      "execute"
    ]
  },
  "input_schema": {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "additionalProperties": false,
    "properties": {
      "value": {
        "type": "number"
      }
    },
    "required": [
      "value"
    ],
    "type": "object"
  },
  "output_schema": {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "additionalProperties": false,
    "properties": {
      "doubled": {
        "type": "number"
      }
    },
    "required": [
      "doubled"
    ],
    "type": "object"
  },
  "examples": [
    {
      "inputs": {
        "value": 2
      },
      "name": "starter-example",
      "outputs": {
        "doubled": 4
      }
    }
  ]
}
```

The scaffold omits `execution.context_id` and `execution.release_path` until you
select compute and agree an installed destination. Local Python tests need neither
an account nor those fields. Ordinary `model validate` requires them and explains
missing fields; it never assigns a candidate ID or modifies the source.

`publication_version` is required and is `1`. The required scientific values are
`model.id`, `identities.model`, `identities.wrapper`, `identities.runtime`,
`input_schema` and `output_schema`; `execution.entrypoint` is also required.
The context supplies its driver. Omitted `model.name` defaults to `model.id`;
`model.description` defaults to `""`. `identities.datasets`, `examples` default
to `[]`, `log_files` and `execution.environment` to `{}`, and `contract_version`
and `wrapper_protocol_version` to `1`. These document defaults do not implement
scientific input defaulting: implement that explicitly in your wrapper.

Add schema descriptions with units, meaning and scientifically justified ranges.
Named examples are checked against schemas; executable tests establish actual
behavior and tolerances. Identities identify scientific software, wrapper mapping,
runtime/dependencies and installed datasets. They are author claims. Keep runtime
lockfiles/container digests and dataset checksums with immutable releases when
needed; connector-observed executable/batch-file hashes are separate evidence.

Use `model create MODEL_ID --file publication.json` to create the catalogue
entry with the ID, name and description in your declaration.
There is one authored file; release operations save their own immutable evidence.

## Scaffold and map your science

```sh
MODEL_ID=my-lab-double
enumeray model init "$MODEL_ID" --directory "$MODEL_ID" &&
  cd "$MODEL_ID" &&
  python3 -B -m unittest discover -s . -p 'test_*.py'
```

For a different starter, run a command from the parent directory. Replace
the sample IDs and destination names before publishing.

```sh
enumeray model init csv-example --directory csv-example --io files
enumeray model init solver-example --directory solver-example --kind executable --io files --driver slurm
```

The destination must be new and its parent must exist. Defaults are Python,
JSON-only, process. All four kind/I/O combinations work locally.
Scientific identities are editable; operation and candidate IDs are generated
when an explicit release is initialized. The wrapper reads model/scientific
content from `publication.json` and accepts a valid publication ID in each
request, allowing an unchanged installation to serve a new candidate or rollback.
Keep the declaration with the installed wrapper so it can read the model's
contract when it starts.

Edit `model.py` for Python calculations and `implementation.py` for mapping.
Executable starters demonstrate a fixed argv native command using `demo_solver.py`.
Replace `SOLVER_ARGV` and native format mapping with your solver. Use argument
arrays with `shell=False`; inputs must never select commands, hosts or credentials.
Your solver need not import Enumeray or understand the wrapper envelope.

Declare actual model, wrapper, runtime and dataset identities. The starter runtime
identity is an example author claim, not attestation of an installed interpreter.
Use supported Draft 2020-12 schemas: references must be local JSON Pointers;
outputs are closed objects with named properties and unconditional required names.
File declarations use direct `x-enumeray-file` annotations and currently cannot
contain schema references. The generated examples show the supported form.

Local fixtures include complete request/result envelopes. Tests use fresh
workspaces and compare numerical results and file bytes; update fixtures when you
change scientific behavior. Wrapper protocol 1 echoes four canonical UUIDs,
rejects malformed envelopes without a result, emits explicit scientific failures,
and publishes results durably without replacing evidence. Requests/results are
limited to 64 KiB; this example's files are limited to 1 MiB. Exact decimal
doubling avoids binary floating-point rounding. Adapt numerical assertions to
your actual scientific method.

For a manual scalar run, start in the generated project and use absolute
request/result paths in a fresh workspace:

```sh
PROJECT_DIR="$(pwd)"
WORK_DIR="$(mktemp -d)"
cd "$WORK_DIR"
python3 "$PROJECT_DIR/wrapper.py" execute \
  --request "$PROJECT_DIR/fixtures/request.json" \
  --result "$WORK_DIR/result.json"
python3 -m json.tool "$WORK_DIR/result.json"
cd "$PROJECT_DIR"
```

For a file starter, stage its fixture before executing:

```sh
PROJECT_DIR="$(pwd)"
WORK_DIR="$(mktemp -d)"
mkdir "$WORK_DIR/inputs"
cp fixtures/dataset.csv "$WORK_DIR/inputs/dataset.csv"
cd "$WORK_DIR"
python3 "$PROJECT_DIR/wrapper.py" execute \
  --request "$PROJECT_DIR/fixtures/request.json" \
  --result "$WORK_DIR/result.json"
cmp "$WORK_DIR/processed.csv" "$PROJECT_DIR/fixtures/expected.csv"
cd "$PROJECT_DIR"
```

A scalar run returns `{"doubled":4}`; a file run reports `sample_count: 2` and
creates a CSV with rows `0,2` and `1,4`. Example artifact IDs are local fixture
labels, not uploaded service artifacts. Existing result/output files are
preserved; choose a fresh workspace for another local execution.

## Full wrapper envelopes and editable mapping

The scalar fixture request contains every required envelope field:

```json
{
  "assignment_id": "22222222-2222-4222-8222-222222222222",
  "attempt_id": "33333333-3333-4333-8333-333333333333",
  "input_files": {},
  "inputs": {
    "value": 2
  },
  "launch_intent_id": "44444444-4444-4444-8444-444444444444",
  "model_id": "example",
  "publication_id": "starter-publication",
  "run_id": "11111111-1111-4111-8111-111111111111",
  "wrapper_protocol_version": 1
}
```

Its successful result is:

```json
{
  "assignment_id": "22222222-2222-4222-8222-222222222222",
  "attempt_id": "33333333-3333-4333-8333-333333333333",
  "failure": null,
  "files": {},
  "launch_intent_id": "44444444-4444-4444-8444-444444444444",
  "outcome": "succeeded",
  "outputs": {
    "doubled": 4
  },
  "run_id": "11111111-1111-4111-8111-111111111111",
  "wrapper_protocol_version": 1
}
```

Echo the four UUIDs exactly. Results omit model/publication IDs and inputs.
A failed calculation publishes the same result identities with
`outcome: "failed"`, empty `outputs` and `files`, and
`failure: {"code":"model_failure","message":"Model could not process its declared inputs or publish outputs."}`.
Invalid envelopes must produce no scientific result. The starter exits 0 after
success, 1 after an explicit scientific failure, and 2 for invalid
request/declaration or failed result publication. Preserve an existing result
regardless of a nonzero process exit; uncertain production execution must not be
repeated by deleting evidence.

The starter's scalar Python mapping can be replaced by this complete
`implementation.py`, which uses the generated `model.py` and `protocol.py`:

```python
from decimal import Decimal
from model import double
from protocol import ModelError

def evaluate(request, workspace, mode):
    if mode != "json":
        raise ModelError("JSON inputs are required.")
    if (request["input_files"]
            or request["inputs"].keys() != {"value"}
            or not isinstance(request["inputs"]["value"], Decimal)):
        raise ModelError("One finite numeric value is required.")
    return {"doubled": double(request["inputs"]["value"])}, {}
```

The tuple is `(ordinary_outputs, file_outputs)`. Keep `wrapper.py` and
`protocol.py` for identity checks and durable publication; replace the
scientific function, property validation and schemas/tests together. The starter
does not provide a general schema-driven dispatcher for arbitrary science:
adapt its fixed mode/slot detection when changing the contract.

Executable mappings use fixed `SOLVER_ARGV`, temporary native files and
`subprocess.run([...], shell=False)`. The demo native command is:

```text
demo_solver.py --mode json --input INPUT_JSON --output OUTPUT_JSON
demo_solver.py --mode files --input INPUT_CSV --output OUTPUT_CSV
```

JSON mode translates `{"value":2}` to the native input and reads
`{"doubled":4}` from its output. File mode passes the staged CSV and reads a
generated CSV. Replace argv and mapping for your solver; validate its output
before returning it. The generated executable mapping suppresses native
stdout/stderr. Keep native processes in the foreground and choose safe
diagnostics if you enable their logs.

File request fixtures contain ordinary public references in `inputs` and a
matching path map in `input_files`, for example:

```json
{
  "dataset": {
    "artifact_id": "55555555-5555-4555-8555-555555555555",
    "path": "inputs/dataset.csv"
  }
}
```

That JSON is the value of `input_files`, not the complete request. File starters
include a full `fixtures/request.json`; inspect it with
`python3 -m json.tool fixtures/request.json`. Declare file slots directly on
named schema properties, using this complete property schema:

```json
{
  "type": "object",
  "properties": {"artifact_id": {"type": "string"}},
  "required": ["artifact_id"],
  "additionalProperties": false,
  "x-enumeray-file": {
    "media_types": ["text/csv"],
    "max_bytes": 1048576
  }
}
```

The connector verifies and stages completed input artifacts. Wrappers report
output file slots in `files`, with their relative paths, media types and optional
scientific metadata:

```json
{
  "processed_dataset": {
    "path": "processed.csv",
    "media_type": "text/csv",
    "scientific_metadata": {}
  }
}
```

This is the `files` field, with `{"sample_count":2}` in ordinary `outputs`.
File slot names must be absent from ordinary outputs. Do not supply sizes,
checksums or public output artifact IDs. The connector measures/verifies/uploads
the files and substitutes completed artifact references in public outputs.
Keep paths inside the workspace without parent traversal or symbolic links.
Omit optional absent files; protocol 1 has no wrapper-provided optional-output
failure annotation.


## Install an immutable release and select compute

Sign in and find a context your connector owner has granted you access to.

```sh
enumeray auth login
enumeray connector grant available
```

Check the context's driver, location and data-access information. Put its ID
and the approved absolute release path in `execution.context_id` and
`execution.release_path` in `publication.json`. The entrypoint is relative to
the release directory, followed by its fixed arguments. The connector owner
manages hosts, SSH credentials, launchers and shared roots. Registration selects
your active grant and derives the installation identity.

For Slurm, prepare `model.sbatch` with the account, partition, cluster and
resources for your site. Set up modules, the runtime, container or MPI there.
Use `exec "$1" local-worker --directory "$2"` to keep the worker in the
foreground. The connector supplies the worker executable and job directory.
Exported runtime setup and allocation variables reach the wrapper. Make the
dependencies, release and workspace paths available to the compute nodes.
Set `execution.slurm_script` to `"model.sbatch"` for Slurm and leave it out for
process execution.

For either driver, have the connector owner prepare the approved parent and
installation access. On the execution host, start in the tested project and
copy it to a new release directory. Set `RELEASE_DIR` to the path from your
declaration.

```sh
RELEASE_DIR=/opt/enumeray/models/my-lab-double/r1
mkdir "$RELEASE_DIR" &&
  cp -R . "$RELEASE_DIR/"
```

Copy the solver, runtime dependencies and datasets that live outside the
project. Include the completed Slurm script if the context uses one. Give the
execution account permission to traverse the parents, read the required files
and execute the wrapper. Use the site's method to test as that account. For the
Python starter, replace the sample path with your installed release path.

```sh
(
  cd /opt/enumeray/models/my-lab-double/r1 &&
    test -x wrapper.py &&
    python3 -B -m unittest discover -p 'test_*.py'
)
```

Once the files, runtime and access checks pass, have the installer apply the
site's read-only permissions to the release. Resolve installed resources
relative to the release and write scratch files and results to the job
workspace. Use a new directory for changed files. Keep the release and declared
entrypoint or batch script outside every context's per-run `work_root/UUID`
directories. Registration reports `reserved_run_installation_path` for paths
in that area. A sibling path such as `work_root/releases/r1` can work if the
context allows it. The connector protects known releases and their observed
aliases if they overlap retained run data.

Environment settings are bounded string runtime configuration, never credentials.
They cannot override `HOME`, `PATH`, `PYTHONDONTWRITEBYTECODE`, or names beginning
with `SLURM_`, `PMI_`, `PMIX_`, `LD_` or `DYLD_`. Names are case-sensitive and
even empty/identical overrides fail; diagnostics show the key without its value.
Use ordinary settings such as `OMP_NUM_THREADS`. Slurm batch files own scheduler,
module and container setup. The runner supplies workspace `HOME` and
`PYTHONDONTWRITEBYTECODE=1`.

The [complete process and Slurm examples](https://doc.enumeray.com/reference/manifests/)
use the same author format for direct and SSH execution. For runnable scientific
examples, [choose a demo tutorial](https://doc.enumeray.com/start/first-run/)
and inspect the models' contracts before preparing your own publication.

## Save a release, resume readiness, review, and activate

Once local tests pass and the files are installed, return to the editable
project. Read the model ID from the declaration and validate the publication.
Keep using this shell for the remaining steps.

```sh
MODEL_ID=$(python3 -c 'import json; print(json.load(open("publication.json"))["model"]["id"])') &&
  enumeray model validate --file publication.json
```

Create a catalogue entry for a new model. The signed-in account becomes its
owner. If the ID belongs to another model, choose a new ID for your starter
before continuing. For an existing model you administer, skip creation.

```sh
enumeray model create "$MODEL_ID" --file publication.json
```

The release directory keeps the candidate ID, readiness checks and
activation review together.

```sh
enumeray model release init "$MODEL_ID" --file publication.json --directory release-r1 &&
  enumeray model release resume release-r1
```

Initialization requires the complete declaration and matching model ID, assigns one candidate ID and freezes the source, normalized machine request and observed current publication. It reads API identity/model authority but does not register or activate. A fresh initialization into a new directory explicitly requests a new candidate; resume the existing directory for a retry.

The private directory binds the operation to the original API/account. Preserve `release.json`, `publication.json`, `state.json` and `release.lock`; credentials are not saved. Source edits or removal after initialization do not change the candidate. Existing destinations and altered evidence are refused. The saved `publication.json` is machine evidence with a candidate ID, not an author file. Do not re-import machine evidence as editable source.

If `release init` itself was interrupted before it wrote `state.json`, and no
`resume` or registration was ever attempted, preserve that partial directory and
explicitly initialize a **new** directory from the original editable publication.
If that source is unavailable, its frozen author value is `source.publication`
in `release.json` for a `source.kind` of `publication`; save that value as a new
author file before initialization. This creates a new candidate ID for an unused
initialization. Missing files alone do not prove registration never started. If
`resume` may have run, preserve the original candidate and evidence, recover its
original checkpoints or inspect/adopt that exact candidate with `--candidate`.
Never create a fresh ID merely to resolve uncertain registration.

`resume` registers the exact frozen candidate and waits for readiness. Repeat it after interruption or an uncertain reply; it never activates or refreshes the saved review guard. `--interval` defaults to 2 seconds and `--deadline` to 3600. Deadline exit 5 and interruption exit 130 retain the operation. Known blockers exit 1 with status on stdout; request/authentication errors go to stderr. `status` shows blockers, trial evidence and the next command. The [CLI JSON contract](https://doc.enumeray.com/reference/cli-json/) defines its complete structured response and decision fields.

All release initialization source modes require `me:read`, `models:read` and
current model administration. `init --file` freezes local settings;
`init --rollback-from` reads retained settings and allocates a fresh candidate;
`init --candidate` adopts that exact candidate without registration. None of
these initialization modes discovers compute grants. Before a fresh registration,
`resume` additionally requires `connectors:read` to discover the current account's
grant and `models:manage` to register. Delegated-candidate adoption/inspection
needs no grant inventory; review/activation requires `models:manage` and available
server-reported release authority. Trial submission has its separate
run/artifact scopes and explicit review.

Registration selects your active grant for the context and derives an
installation key. Exact retries keep that grant. Matching settings and
scientific identifiers can reuse a retained installation. Changed settings
or identifiers get a new key. A revoked installation stays unavailable even
if a grant is reissued. Keep `grant_id`, `provider_binding_id` and other
derived installation claims out of the editable declaration.

On readiness failure, inspect validation messages and coordinate context, grant, path and execution-account corrections with the connector owner. Changed immutable science or installation settings require a new operation. Local validation checks contracts and settings; API registration checks authority/context policy; connector readiness checks installed entrypoint/batch files and execution/backend access. Readiness does not attest scientific correctness or every runtime dependency.

### Run the candidate and check its results

Use the full Candidate ID displayed by `model release status release-r1` as
`PUBLICATION_ID`, or adapt its displayed trial-init command. Before activation,
exercise the ready candidate in a separate analysis directory. The scalar
starter provides `starter-example` with input `2`.

```sh
PUBLICATION_ID='REPLACE_WITH_FULL_CANDIDATE_ID_FROM_RELEASE_STATUS'
enumeray analysis init "$MODEL_ID" --candidate "$PUBLICATION_ID" \
  --example starter-example --directory trial-r1
```

Read the saved inputs, installation, location and data-access information.
Run `enumeray analysis status trial-r1` if you need to see the saved details
again. When you are ready, submit the trial folder and wait for the run.

```sh
enumeray analysis submit trial-r1 &&
  enumeray analysis wait trial-r1 --check-outcome
```

After the run succeeds, collect its outputs and compare them with your
expected result. The scalar example should report `{"doubled":4}` in
`trial-r1/results/outputs.json`. With Slurm, the trial also exercises the
scheduler and the runtime on the compute nodes.

```sh
enumeray analysis collect trial-r1
```

If the run fails, inspect `enumeray analysis status trial-r1` and
`enumeray analysis logs trial-r1` before continuing.

Initialization displays and saves the exact candidate installation, readiness,
execution location, scientific-data access disclosure, captured grant and any
named delegation. Review them before explicitly submitting. This also works for a
first ready candidate when the model has no current publication. Use
`--inputs FILE` to supply your own values. Bind
ordinary files with `--file SLOT=PATH` and collections with
`--collection SLOT=DIR` or `--collection-manifest SLOT=MANIFEST.json`, using the
candidate's declarations and any required `--media-type SLOT=TYPE`. Completed
artifact and collection references can also be supplied in the input JSON.

The caller must still administer the model, retain model execution access and
own the candidate's captured compute grant or an exact active named handoff from
its holder. Scoped tokens need `models:read`, `models:manage`, `runs:create` and
`me:read` for initialization/submission; uploads and inspection need the ordinary
run/artifact scopes. Another administrator's grant cannot be substituted.

Trials use ordinary queue/capacity, results, logs, cancellation and recovery.
Retry an uncertain upload or submission with `analysis submit trial-r1`; the same
directory retains one execution and its original request. To request cancellation,
use `analysis cancel trial-r1`, then inspect status or wait for confirmation.
Initialize a new directory for deliberately new work. Trials retain
`purpose: candidate_trial`, their exact publication, grant and delegation even if
activation or replacement occurs meanwhile. Removal of model
administration, model access or compute authority blocks pending launch; observed
execution retains normal drain behavior. Maintenance pauses new acceptance/claims.
Retirement prevents fresh trials while accepted work and delivery finish.

`model release status release-r1` and `model release review release-r1` show
publication-keyed trial evidence automatically, including failed and cancelled
trials; no receipt or run ID needs copying from the analysis. When trials are
available, release output also shows an exact-candidate initialization command
with marked input-file and new-directory placeholders. `analysis status trial-r1`
links back to exact publication inspection and release review guidance. Collected
scientific JSON and provenance are in `trial-r1/results/`; inspect those results
before promotion.
Installation readiness, execution success and scientific correctness are separate:
a successful trial is execution evidence; authors own scientific validation.
Trial success is advisory and never automatically activates or gates activation.
Trials do not change the current publication, public admission or access policy.
Validated, authorized retained publications can also be trialled after activation.

### Review and activate

Save the proposed publication comparison once you finish the scientific checks.

```sh
enumeray model release review release-r1
```

Read the comparison, then activate the candidate when you are satisfied with
the change.

```sh
enumeray model release activate release-r1 &&
  enumeray model get "$MODEL_ID"
```

Review captures the observed current publication, including no current publication on first activation. If it changes, inspect the competing update and run review again before explicit activation. Initialization and registration never activate or share.

Check that `model get` shows the candidate as the current publication.
Accepted runs retain their original publication. Set access explicitly before
handing the model ID and its scientific examples to a user.

## Choose visibility and access explicitly

Inspect `model get` and `access policy`, preview the change, then apply it with the same observed generation:

```sh
umask 077
enumeray model get example
# Copy the numeric Generation just displayed; replace this placeholder:
GENERATION=OBSERVED_GENERATION
enumeray access set-policy example --mode invite --visibility unlisted \
  --generation "$GENERATION" --dry-run
enumeray access set-policy example --mode invite --visibility unlisted \
  --generation "$GENERATION"
```

New models start invite-only and unlisted. Omitting `--visibility` preserves the
current visibility; pass it explicitly when you intend to change discovery.

Dry-run reads the policy without changing it. Normal apply shows a before/after
summary on stderr; `--json` preserves the existing apply response without that
summary. Inspection needs `models:read`; applying also needs `access:manage`.
Both commands require the supplied generation to match. A concurrent change
stops apply without retrying. After a lost successful reply, an exact retry with
the original generation conflicts; inspect the policy and audit before deciding
on another guarded change.

Choose `listed`/`unlisted` for discovery and `open`/`request`/`invite` for run access.
Open access applies to authenticated users and remains subject to explicit blocks
and execution/service limits. Unlisted models remain discoverable to authorized
users who know the ID. Publication and access changes have separate guards.

For request access, first select `--mode request --visibility listed` with a
freshly observed generation. The prospective user then runs:

```sh
enumeray access request example --message 'Please approve runs for my study.'
```

The owner/co-admin inspects and decides the actual returned request ID:

```sh
enumeray access requests example
enumeray access decide example REQUEST_ID approved
```

Use `rejected` to decline; the requester may use `withdrawn` on their own request.
For a private invitation:

```sh
enumeray access invite example --email colleague@example.org --output invitation.json
enumeray access invitations example
```

Use `--user USER_UUID` instead of `--email` if you know the recipient's service
UUID. The output path must be new; it saves private JSON containing `token`,
`id`, `model_id` and `expires_at`. Invitations expire after seven days.
Deliver the file privately. The recipient logs in as the bound identity and accepts:

```sh
python3 -c 'import json; print(json.load(open("invitation.json"))["token"])' \
  | enumeray access accept example
```

Only the token goes to stdin. Piping the complete JSON file is incorrect.
Email invitations require the matching verified email. An invitation grants run
access, not model administration.

Explicit `access block example USER_UUID --generation CURRENT_GENERATION` overrides run
access; `access unblock` removes the block and creates an explicit run-access
grant using a newly observed generation, including in request/invite mode.

## Delegate administration and transfer ownership

The owner alone adds/removes co-admins, transfers ownership and archives. Co-admins
may register/activate publications, manage run access and inspect all model runs
and scientific data. Connector-only administration does not grant this data access.

As the owner, read the membership generation and delegate:

```sh
enumeray model members example
# Copy the numeric Generation just displayed; replace this placeholder:
GENERATION=OBSERVED_GENERATION
enumeray model member-add example --user USER_UUID --generation "$GENERATION"
enumeray model members example
enumeray model audit example
```

Use `member-remove` with a newly read generation to remove membership. Neither
member command targets the acting owner. For ownership transfer:

```sh
enumeray model transfer example --to USER_UUID --generation CURRENT_GENERATION
```

The designated recipient accepts the returned proposal ID in their own profile:

```sh
enumeray model accept-transfer example TRANSFER_ID
```

A proposal alone does not transfer ownership. Intervening generation changes
can invalidate acceptance. `model archive example --generation CURRENT_GENERATION`
retires the model from new use without deleting retained run/release evidence;
there is no model unarchive command.

## Files, runs, inspection and retries

For the separate `csv-example` file project, repeat installation/publication for
that model first. Then bind the source directly to an analysis directory:

```sh
enumeray model get csv-example
enumeray analysis init csv-example --example starter-example \
  --file dataset=fixtures/dataset.csv --directory file-analysis
# Review the destination/disclosure and the declared file binding.
enumeray analysis submit file-analysis
enumeray analysis wait file-analysis --check-outcome
enumeray analysis logs file-analysis
enumeray analysis collect file-analysis
```

Use the actual model ID, named example and declared input slot. For a scalar
project, supply `--inputs inputs.json` containing `{"value":2}` with model
`example`, and initialize a new directory. First submission freezes inputs,
uploads bound sources and constructs verified artifact references automatically.
Repeat submit after interruption with the same directory and unchanged original
source bytes; after acceptance, status, logs, wait, cancellation and collection
need no manual run/artifact IDs or separate retry files. Different scientific
work requires an explicitly new directory. Candidate trials use the same
lifecycle and retain their exact candidate, purpose and delegation.

The directory pins the API/account, exact publication, disclosure, request and
key. `analysis submit --json` returns a summary with nested `receipt`, while
`state/receipt.json` contains the original acceptance object. Use status/wait
for current progress. Retain the directory and the original input bytes
separately. Collection verifies downloaded size/SHA-256 and records the outcome
independently of completeness; check `complete: true` and `bundle.json` for all
outputs, including after a successful partial or evidence-only collection step.
`analysis cancel file-analysis` requests cancellation; inspect confirmed status
rather than inferring termination from a request or disconnected connector.

Use the artifact and collection commands for reusable data. Supply a completed
artifact reference in analysis input JSON as
`{ "artifact_id": "UUID" }`. File wrappers receive staged workspace paths in
`input_files`, then report output paths and media types in `files`. Connectors
publish those bytes as verified artifact references. Never fabricate public
output artifact IDs inside the wrapper. Use `run list`, `run get` and
`run events` to investigate runs you have permission to read.

If you have a saved prepared request and need to find out whether it was
accepted, keep the file and run
`analysis import --prepared run.json --directory imported-analysis`. Then
run `analysis submit imported-analysis` to reconcile the same request and key.
For accepted work whose directory was lost, use `analysis attach RUN_UUID
--directory recovered-analysis` or supply its raw acceptance receipt with
`--receipt`. See the included [analysis recovery guide](ANALYSIS_GUIDE.md#recover-an-existing-execution)
and [noninteractive JSON recipe](ANALYSIS_GUIDE.md#script-with-structured-output).
A copied accepted folder continues the same execution for the same API/account;
retain its private state, locks and download checkpoints. Original input bytes
remain external, and unfinished uploads still need their unchanged sources.
The [tiny collection walkthrough](ANALYSIS_GUIDE.md#bind-local-collections) shows
local member bindings, evidence-only/partial collection and complete collection.
Historical reruns and parameter sweeps are outside the current release.

## Updates and rollback

Changes to scientific behavior, schemas, examples or scientific identity require
a new publication with an explicit scientific contract. Schema/example-only edits
can reuse the existing installation when its settings and scientific installation
identities remain unchanged and the installed wrapper supports the new contract.
When installed model, wrapper, runtime or dataset content changes, install a new
immutable release and update the corresponding identities. Unchanged science can
copy its definition into a new publication for another installation. Update tests
and examples first, install a new release when needed, then repeat
the saved release workflow through readiness, review and explicit activation. Each publication retains its own
immutable definition. Registration allocates an installation key for changed
installation settings or identities. Retain editable declarations, saved release operations and installed files.

Rollback creates a **new publication**, using the saved prior scientific contract
and physical installation settings. Registration automatically reuses the retained
installation key for the same model/context and identical settings and identities.
Initialize a fresh release, resume registration/readiness with current authorization, and
activate with the current publication guard. Do not reactivate historical
publication records. Missing installed files, a revoked installation or absent active compute
grant makes that rollback target unavailable. A replacement grant can authorize
the new publication; earlier publications retain their original grant. Retain releases and their original
connectors while previously accepted work references them. Earlier accepted runs
keep their pinned publication, scientific contract, destination and installation; access history remains.

Save each release directory and immutable installed files. After making and testing changes, install a new release when necessary, edit `publication.json` and initialize a new operation:

```sh
enumeray model release init example --file publication.json --directory release-r2
enumeray model release resume release-r2
# Run trials and complete scientific checks, then review and activate explicitly.
enumeray model release review release-r2
enumeray model release activate release-r2
```

`review` displays the current and candidate contracts, identities, installation, execution location, disclosure and trial evidence, then saves the expected-current guard. A competing update blocks activation until another explicit review. Status/resume do not change the guard or submit activation. Lost activation replies remain checkpointed; observation can confirm the candidate is current without inventing a missing receipt. An uncommitted request requires explicit activation again.

To create a rollback candidate from any readable retained publication:

```sh
enumeray model release init example --rollback-from PRIOR_PUBLICATION_ID --directory rollback-r1
enumeray model release resume rollback-r1
# Inspect fresh readiness, trial evidence and scientific assertions.
enumeray model release review rollback-r1
enumeray model release activate rollback-r1
```

The new candidate copies only scientific and physical installation settings, with a fresh publication ID and current authorization/validation. Source configuration retirement does not erase those settings or establish that physical files still exist. Permanent installation revocation blocks reuse. A new grant can authorize the fresh candidate without changing the historical grant.

To permanently stop new acceptance, dispatch and launch through one publication,
run `enumeray model revoke-publication example PUBLICATION_ID`. This CLI command
revokes only the selected publication; other publications sharing its retained
installation are unaffected.
Already executing work still delivers its results; users can still cancel accepted
runs and resume their original analysis directories. Publishing a replacement alone
does not revoke accepted work. The revoked publication cannot be reactivated.
A new publication may reuse a retained installation only while that installation
remains authorized, with fresh validation. Contact Enumeray support if every publication sharing an installation needs revocation.
Installation revocation is permanent and cannot be bypassed by cloning its
descriptor. The default `--scope publication` affects only the named publication.
Keep the original publication descriptors, validation evidence and connector
configuration while accepted work or retained candidate tests still need them.

To reclaim unused publication configuration after a replacement is current and
earlier accepted work has finished, inspect the old publication and use the
model's freshly observed generation:

```sh
enumeray model publication example OLD_PUBLICATION_ID
enumeray model get example
# Copy the numeric Generation just displayed; replace this placeholder:
GENERATION=OBSERVED_GENERATION
enumeray model retire-publication example OLD_PUBLICATION_ID --generation "$GENERATION"
enumeray model publication example OLD_PUBLICATION_ID
```

An authorized current publication must be replaced or revoked before requesting
retirement. Accepted queued, active or uncertain references and pending delivery
block final retirement while the request remains durable. Inspect
`configuration_retention` for requested, blocked and retired state; exact retries
use the original generation. Retained candidates stay configured until explicitly retired; another
publication sharing an installation keeps that cache entry. Retirement preserves
scientific definitions, installation descriptors, validation, permanent
revocations, provenance and original retry receipts. It never removes installed
releases or published artifacts. A retired record cannot activate; inspect its
rollback availability and create a fresh authorized, validated publication when
the release is still available. Re-read after a conflict or lost reply.

Connector services separately reclaim eligible completed staging, workspaces,
transfer copies, outputs and logs after confirmed execution release and final
delivery. The default completed retention is zero seconds with a 30-second
sweep. Uncertain work and pending delivery keep their files and installation
references. Archived evidence and launch replay guards remain on the connector
and compute hosts; this cleanup does not change downloaded artifacts or run
provenance.
Deletion requires run-directory reservations from the service, a saved
protected-release inventory and known installation metadata. If any are
missing, the connector retains the files while execution and delivery
continue.

## Move to a replacement execution context

A connector owner may replace a host, execution account, backend or scratch
policy by creating a fresh context ID. Obtain that ID, its installation
instructions and your own active compute grant. Moving contexts requires a fresh
publication and fresh installation validation even when the science and installed
release are unchanged. Update `execution.context_id`, release path and Slurm settings in
`publication.json` after verifying the release as the new execution account.
The context owns the driver. Preserve the old declaration, installed release and
saved operation.

```sh
enumeray connector grant available
# Edit publication.json to use the approved replacement context and installation.
enumeray model release init example --file publication.json --directory release-new-context
enumeray model release resume release-new-context
# Trial the displayed candidate and complete scientific checks.
enumeray model release review release-new-context
enumeray model release activate release-new-context
enumeray model release status release-new-context
```

The connector owner cannot transfer readiness, activate on your behalf or
redirect an accepted run. Inspect the new location, disclosure and trial evidence
before activation. Coordinate the handoff: cutover is blocked while a live current
publication remains on the source. If discontinuing the offering, explicitly
withdraw its current publication with `model revoke-publication` instead of
activating a replacement; permanent revocation retains its usual dispatch effects.

Once the source is `draining`, fresh registration, activation, standard submissions
and candidate trials there are closed. Under `drain`, eligible previously accepted
queued runs can still execute; `cancel-queued` cancels only runs still queued at
cutover. Assigned work and delivery retain their original destination and release.
Ordinary capacity, maintenance, access and permanent revocation checks still apply.
Historical and abandoned candidates may be explicitly released by the
connector owner through context configuration retirement. A fresh publication
using an active context still needs current authorization and validation.

Final context retirement waits for all source work and delivery, publication
configuration retirement and connector acknowledgement. Keep required old releases
available and coordinate retention with the connector owner. Reopening before
final retirement restores only future eligibility: it never undoes cancellations,
retirement requests, revocations or your publication choices. See the
[infrastructure replacement procedure](https://doc.enumeray.com/infrastructure/operations/#replace-and-retire-an-execution-context)
for dependency planning and retirement diagnostics.

## Collection contracts

Collection slots use a closed `{collection_id}` object with an
`x-enumeray-collection` declaration: `media_types`, `max_members`,
`max_file_bytes` and `max_total_bytes`. Declarations are restricted to named
top-level slots. The collection itself may be optional; every listed member is
required. Empty collections are valid. Total input/output membership, including
standalone files, is limited to 10,000 in each direction.

Protocol 1 adds `input_collections` only when used. Each entry supplies a staged
`root` directory and a local `manifest` with sorted member paths, exact artifact
versions, byte counts, full-object SHA-256 and scientific metadata. For an output,
report `collections: {"snapshots": {"root": "snapshots", "manifest": "members.json"}}`.
The sidecar is an array of objects with `path`, `media_type` and optional
`scientific_metadata`. Paths are relative to `root`; the runner measures content
integrity. Keep member arrays out of wrapper envelopes. Manifests are capped at
16 MiB and transported in pages of at most 64 KiB.

The [complete model examples archive](https://doc.enumeray.com/examples/model-examples.tar.gz)
contains the collection wrapper, protocol helper, publication, full request/result
fixtures and executable tests together. From its extracted root, run
`python3 -B -m unittest discover -s examples/collections -p 'test_*.py'`.
For a manual run from that root, stage the supplied input tree in a fresh workspace:

```sh
COLLECTION_DIR="$(pwd)/examples/collections"
WORK_DIR="$(mktemp -d)"
cp -R "$COLLECTION_DIR/fixtures/workspace/." "$WORK_DIR/"
python3 -B "$COLLECTION_DIR/wrapper.py" execute \
  --request "$COLLECTION_DIR/fixtures/request.json" \
  --result "$WORK_DIR/result.json"
cmp "$WORK_DIR/inputs/mesh/a.bin" "$WORK_DIR/snapshots/mesh/a.bin"
cmp "$WORK_DIR/inputs/series/snapshot-002.bin" "$WORK_DIR/snapshots/series/snapshot-002.bin"
python3 -m json.tool "$WORK_DIR/result.json"
```

Expect `outcome: succeeded`, `member_count: 2` and byte-identical members.
The output sidecar is `snapshots-members.json`; output paths are relative to
`snapshots/`. The tests also cover an empty set, malformed identities/mappings,
existing evidence, safe scientific failure and interrupted/competing publication.
Rejecting invalid requests or existing evidence must leave previous files and
scientific side effects unchanged. Local wrapper success does not establish
service acceptance or scientific suitability. Immutable collections retain
membership after individual bytes become unavailable; inspect current availability
before reuse.

For collection inputs in an analysis directory, see the included
[analysis guide](ANALYSIS_GUIDE.md#bind-local-collections). For protocol examples,
use the complete archive above; an individual
[wrapper download](https://doc.enumeray.com/examples/collection-wrapper.py) also
requires its protocol helper and matching fixtures.

## Maintainer verification mapping

Authors maintain scientific tests for their own model and installation.
If you maintain the public CLI or its fixtures, follow the
[documentation and contribution guide](https://doc.enumeray.com/maintaining/).

## Hand a candidate to another administrator

The captured compute-grant holder can explicitly authorize another active model
administrator to trial and activate the same candidate. The reviewer does not need
a separate compute grant. A delegation applies only to this publication; it cannot
be used to register another installation or delegate authority onward. An author
with the required access can also trial and activate their own release.

Inspect `model release status release-r1` for `model_generation`, then use that
observed value for the handoff (replace the example generation below):

```bash
enumeray model release delegate release-r1 --user REVIEWER_UUID --generation 2 --dry-run
enumeray model release delegate release-r1 --user REVIEWER_UUID --generation 2
```

The CLI saves the delegation UUID and exact request before sending. After a lost
reply, repeat the same recipient and generation in the same directory. An old
revoked receipt stays revoked; use a freshly observed generation for a fresh handoff.
Give the reviewer the model and candidate publication IDs. In their own profile:

```bash
enumeray model release init example --candidate CANDIDATE_ID --directory review-r1
enumeray analysis init example --candidate CANDIDATE_ID --inputs inputs.json --directory review-trial-r1
```

Review the installation, data access and delegation shown for the trial.
When ready, submit the trial and collect its result.

```bash
enumeray analysis submit review-trial-r1
enumeray analysis wait review-trial-r1 --check-outcome
enumeray analysis collect review-trial-r1
enumeray model release status review-r1
```

Inspect `review-trial-r1/results/` and complete the scientific checks before
saving the review.

```bash
enumeray model release review review-r1
```

Read the review, then activate explicitly.

```bash
enumeray model release activate review-r1
```

Adoption reads the existing publication and creates a private, API/account-bound
operation. It never registers a replacement or copies another account's review.
The analysis and saved activation request retain the exact delegation. Repeat
`analysis submit review-trial-r1` after a lost reply; a replacement handoff cannot
revive a trial pinned to a revoked delegation. Resume only
observes readiness and never activates. A changed delegation or current publication
requires another explicit review.

The grant holder or current model owner can withdraw a handoff with
`model release revoke-delegation DIRECTORY DELEGATION_UUID --generation N`,
optionally previewing with `--dry-run`. A model owner can first adopt the candidate
into their own directory. Removing either participant's model administration or
suspending either account permanently invalidates their handoffs, including role
loss through ownership transfer. Restoring the account or role requires a fresh
handoff. Replacements cannot revive trials pinned to an invalidated delegation.
Already running work can deliver results, and revoking a handoff does not withdraw
an already published model. Grant, publication and installation revocations remain
independent. CI follows the same account and scoped-token rules.
