# Collection copy example

This complete offline fixture belongs to the `enumeray-model-examples-v1` family.
It needs Python 3.9+ and its standard library on macOS or Linux. Copy this entire
directory, including `protocol.py`, `test_*.py` and `fixtures/`; no checkout, API,
credentials or Python packages are required. This demonstrates a protocol and
byte-preserving mapping, not a scientific validation or installed service model.

From this directory, run all standalone tests:

```sh
python3 -B -m unittest discover -s . -p 'test_*.py'
```

The model is `collection-example`, the fixture publication is
`collection-example-publication`, and the declared input/output slots are
`dataset` and `snapshots`. `publication.json` describes the scientific contract;
its `/opt/models/collection-example` installation and `lab-cpu` context are
site-specific placeholders. Arrange installation and access before using this
example through a service. Valid publication IDs can change for a retained model
installation, including rollback; the request model must match this wrapper.

For one manual offline execution, run from this directory:

```sh
example_root=$(pwd -P)
workspace=$(mktemp -d)
cp -R "$example_root/fixtures/workspace/." "$workspace/"
python3 -B "$example_root/wrapper.py" execute \
  --request "$example_root/fixtures/request.json" --result "$workspace/result.json"
cat "$workspace/result.json"
cmp "$workspace/inputs/mesh/a.bin" "$workspace/snapshots/mesh/a.bin"
cmp "$workspace/inputs/series/snapshot-002.bin" "$workspace/snapshots/series/snapshot-002.bin"
```

The result echoes four canonical request UUIDs and reports
`"outputs":{"member_count":2}`. Both copied files are byte-identical.
`snapshots-members.json` declares paths, media types and scientific metadata for
the output set. The connector independently measures and validates those files
before service completion; wrapper success alone is not service acceptance.
The input sidecar contains the connector's actual flattened pinned-member shape:
`path`, `media_type`, `scientific_metadata`, `size_bytes`, `checksum_sha256`,
`artifact_id`, `version_id`. Fixture UUIDs and object versions are local examples.

The staged workspace begins with:

```text
manifest.json
inputs/mesh/a.bin
inputs/series/snapshot-002.bin
```

It gains `result.json`, `snapshots-members.json` and `snapshots/` on success.
Paths in the request are relative to the result's parent workspace; the request
file itself may live in the immutable example installation. An empty manifest
(`[]`) with an existing empty `inputs/` directory succeeds with `member_count: 0`.

All request/mapping/member validation and input integrity checks precede output
creation. Requests/results are bounded to 64 KiB; sidecars to 16 MiB and 10,000
members, with the publication's 100 GiB/member and 1 TiB total ceiling. No tests
transfer those large limits. Unknown/duplicate keys, wrong identities, invalid
paths, aliases, symlinks, malformed mappings and retained outputs fail safely.
Only the exclusive creator of `snapshots/` may publish the sidecar/result;
competing writers exit without replacing that invocation's evidence. Files are
synced and published using same-filesystem atomic links without replacement.

Success exits 0. A scientific mapping failure publishes a bounded, safe failed
result and exits 1. Request, mapping, retained-evidence and publication failures
exit 2. Tests explicitly exercise all three outcomes, partial writes, competing
writers and interruption before/after result publication. Diagnostics exclude
private input values and paths. A complete result can remain after a later sync
failure: preserve it. Partial output after interruption also stays retained;
use a fresh workspace for deliberately new offline work. Connector journals,
rather than this local example, own execution recovery.
