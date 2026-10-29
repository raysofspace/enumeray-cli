# Author starter: {{MODEL_ID}}

This project needs Python 3.9 or later and its standard library. It needs no API
checkout, Rust toolchain, API credentials, database or Python package installation
for local tests. Run from this directory:

```sh
python3 -m unittest discover -s . -p 'test_starter.py'
```

Tests create fresh workspaces, stage fixture files, invoke `wrapper.py`, and compare
results and file bytes. Fixture artifact UUIDs are local examples, not API artifacts.
Edit `model.py` and `implementation.py`, then update the schemas, scientific identities,
examples and fixtures in `publication.json`. The wrapper reads its model ID and schema
from that file and accepts the publication ID supplied in each request.
Executable starters use `demo_solver.py`; replace its fixed argv and native-format
mapping in `implementation.py` with your installed solver interface.

## Install and register a candidate

Local tests work while `execution.context_id` and `execution.release_path` are null.
Arrange an immutable installation and compute access using the bundled
[author guide](AUTHOR_GUIDE.md), then fill both fields and validate:

```sh
enumeray model validate --file publication.json
enumeray model create {{MODEL_ID}} --file publication.json
enumeray model release init {{MODEL_ID}} --file publication.json --directory release-r1
enumeray model release resume release-r1
enumeray model release status release-r1
```

Create the catalogue model once. Source metadata is not automatically updated by
publication. Initialization freezes one candidate and saves its ID privately before
registration. Resume the same release directory after interruptions; edits to this
project cannot change that saved candidate. API registration checks current
administration and compute authority; the connector checks installed content.

## Trial, review and activate

The following uses the exact candidate saved in this release operation; it does
not require an already active publication. Retain the release and trial folders.

```sh
CANDIDATE_ID=$(python3 -c 'import json; print(json.load(open("release-r1/release.json"))["candidate"]["id"])')
enumeray analysis init {{MODEL_ID}} --candidate "$CANDIDATE_ID" \
  --example starter-example --directory trial-r1{{ANALYSIS_BINDINGS}}
# Review the printed installation, execution location and data-access disclosure.
enumeray analysis submit trial-r1
enumeray analysis wait trial-r1 --check-outcome
enumeray analysis collect trial-r1
enumeray model release status release-r1
```

Inspect `trial-r1/results/outputs.json`, verified files and provenance, and complete
your scientific assertions. A successful trial is execution evidence; it never
activates a publication. After your scientific review:

```sh
enumeray model release review release-r1
enumeray model release activate release-r1
```

Delegated reviewers use the grant holder's exact active candidate handoff and their
own private release review operation, as described in the author guide. They do
not substitute a different grant or candidate inside a trial folder.

## Run the published model

Once access and ordinary admission are configured, initialize a separate folder:

```sh
enumeray analysis init {{MODEL_ID}} --example starter-example \
  --directory analysis-one{{ANALYSIS_BINDINGS}}
# Edit analysis-one/inputs.json if needed and review the destination/disclosure.
enumeray analysis submit analysis-one
enumeray analysis status analysis-one
enumeray analysis logs analysis-one
enumeray analysis wait analysis-one --check-outcome
enumeray analysis collect analysis-one
```

Repeat submit or collect with the same folder after interruption. Use
`analysis logs analysis-one --follow` for resumable logs and
`analysis cancel analysis-one` for an explicit cancellation request; status/wait
report authoritative confirmation. Initialize a new named folder for deliberately
new work. Do not change frozen inputs or discard uncertain submission records.
Original source bytes remain external; retain them separately when needed.
Collection completeness (`complete: true` and `bundle.json`) is independent of
scientific success, failure or cancellation.

The bundled [analysis guide](ANALYSIS_GUIDE.md) covers the full lifecycle,
noninteractive JSON scripting, credential renewal, copied folders and recovery of
existing executions. The [author guide](AUTHOR_GUIDE.md) covers installation,
delegation, sharing, updates and rollback.
