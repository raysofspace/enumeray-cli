# Independent HTTP workflows

These small integrations use HTTP directly, without the Enumeray client library
or a source checkout. They are verified against a disposable service. Obtain an
API origin, account/token, model/publication IDs and resource access from your
service owner. A model must already be installed, ready and open for submission;
this recipe does not install science or grant access.

Download the [complete examples archive](https://doc.enumeray.com/examples/model-examples.tar.gz)
and [SHA-256](https://doc.enumeray.com/examples/model-examples.tar.gz.sha256).
Follow the [fresh-directory extraction and verification instructions](https://doc.enumeray.com/authors/overview/#run-the-downloaded-examples-offline).
Inside its one versioned root, work in `examples/http-contract/`, including
[raw-http.py](raw-http.py), [collection-identity.py](collection-identity.py),
`digest-vectors/` and `files/`. The archive includes every companion file;
individual links are useful for inspection, but one script alone is incomplete.
Python 3.9+ with its
standard library is sufficient. Files are at most 1 MiB in this recipe; larger
uploads have a distinct multipart transport. The service's policy, declarations
and quotas apply independently of this example's bound.

Set `API` to your actual HTTPS origin (no path), and provide the scoped token in
the `ENUMERAY_TOKEN` environment variable using your approved credential method.
Only disposable loopback fixtures may use HTTP. Required token scopes are
`me:read` for the account check, plus `uploads:create`, `uploads:read`,
`uploads:complete`, `artifacts:read` and `artifacts:download` for artifact/collection
work, or `runs:create` and `runs:read` for submission/inspection. Token scopes and
resource authority both apply; submission additionally needs the model's execution
access. The run recipe uses an ordinary current publication; candidate trials
also need `models:manage` and their captured compute-grant or named-delegation authority.
See the downloadable [HTTP conformance records](https://doc.enumeray.com/examples/http-conformance.json)
for the operation-specific rules.

## A standalone artifact

From the downloaded directory, set `API` and `ENUMERAY_TOKEN`, then run:

```sh
python3 raw-http.py --api "$API" --directory artifact-operation \
  artifact --file files/a.csv --media-type text/csv \
  --metadata digest-vectors/a-metadata.json > artifact-receipt.json
```

The complete transaction is: save the body/key and account/origin →
`POST /api/v1/uploads` → use only its signed transfer headers in a storage PUT →
save storage's exact `x-amz-version-id` →
`PUT /api/v1/uploads/{upload_id}/completion` → inspect the artifact →
request `/api/v1/artifacts/{artifact_id}/content` → receive a checked **303** →
make a separate unauthenticated storage GET → verify byte length and SHA-256.
The API does not stream the artifact body. Never forward the API Authorization
header to a storage URL, follow storage redirects blindly, or remove its exact
version query. A completion receipt is immutable evidence; a new download still
checks current read authority and availability.

`artifact-operation/` contains private original request, key, reservation and
completion intent/receipt files. Rerun the exact command after interruption.
The script checks the same API/account and source checksum; do not replace the
source bytes, state or key. A different directory is a new operation, not upload
recovery. If the PUT reply was lost before a version could be retained, repeating
the same reservation's unchanged PUT can create another unpublished storage
version; completion still publishes only its saved exact version.
The private `operation.json` atomically retains request bytes, key, API and
account before mutation. Complete files and directory entries are synced before
the request. Human-readable request/key sidecars can be restored from that
envelope; an incomplete copied operation without its envelope stops safely.

## A collection with two nested members

First reproduce the requested manifest digest independently:

```sh
python3 collection-identity.py digest-vectors/mixed-requested.input.json
python3 raw-http.py --api "$API" --directory collection-operation \
  collection --manifest digest-vectors/mixed-requested.input.json \
  --source-root files > collection-receipt.json
```

The transaction saves the complete requested manifest and create body/key, then:

1. `POST /api/v1/collections` records count, total bytes and requested digest.
2. `PUT /api/v1/collections/{id}/members/{index}` registers ordered metadata
   pages. This recipe sends one member per page and replays each identical page.
   Reusing an occupied page with changed metadata conflicts; never renumber
   remaining members or change the original digest after an uncertain response.
3. `PUT /api/v1/collections/{id}/freeze` with `{}` checks the complete manifest.
4. Each member completes the artifact subtransaction described above, then
   `PUT /api/v1/collections/{id}/members/{index}/artifact` binds its artifact ID.
5. `PUT /api/v1/collections/{id}/completion` with `{}` seals immutable artifact
   and version pins. The script independently computes the sealed digest.
6. Inspect the collection and member page, retain `{ "collection_id": "…" }`
   for a declared scientific collection input, and verify member downloads.

To reuse already completed artifacts, pass `--reuse-artifacts reuse.json`, where
the JSON object maps member paths to actual artifact UUIDs. Reuse still verifies
bytes and the service enforces binding authority, checksum and exact version.
Do not assume arbitrary public artifact access grants permission to bind it.
Media type and scientific metadata must also equal that member's specification,
and ownership/assignment must permit the binding. The standalone command above
uses the matching media type and lossless metadata for `a.csv`, so its returned
artifact UUID can be reused for that member. A generic octet-stream artifact with
empty metadata cannot replace it merely because its file bytes match.
The selected reuse map is saved before the first create request. Resume can omit
the flag and reuse that saved map; a changed map conflicts locally before another
upload or binding. Final inspection follows every member-page cursor.

For an empty collection use `digest-vectors/empty-requested.input.json` with the
same command and a new operation directory. Freeze and seal still occur; there
are no member pages, uploads or bindings. Its member page is empty with a null
cursor. Repeat a full collection command with the original state to reconcile
lost replies; retain requested and sealed identities separately.

## Submission after a lost reply

Create `run-request.json` using the actual accessible model/publication IDs and
scientific inputs satisfying its published schema. This is a complete request
shape; these placeholder IDs must be replaced before submitting:

```json
{"model_id":"YOUR_MODEL_ID","expected_publication_id":"YOUR_PUBLICATION_ID","inputs":{"value":2}}
```

Make `changed-request.json` with the same model/publication and a different valid
scientific value. Review and authorize the intended execution before running:

```sh
python3 raw-http.py --api "$API" --directory submission-operation \
  run --request run-request.json --lose-reply \
  --changed-request changed-request.json > recovered-submission.json
```

The script durably saves the exact request bytes and key together in its private
`operation.json` envelope before its first POST,
discards the accepted reply, and repeats `POST /api/v1/runs` with the saved pair.
It recovers the original run/receipt. Changed content under that same key must
return **409 `idempotency_conflict`**. Service acceptance tests also verify no
second run, assignment or execution was created. Collection page replay is a
separate operation and does not provide this run guarantee.

Keep `submission-operation/run/operation.json`, `request.json`, `idempotency-key.txt`, receipt and
identity files after timeout or authentication loss. Restore access for the same
API/account and repeat the same operation; do not generate a new key to recover
an uncertain submission. A replayed queued receipt records original acceptance,
while GET `/api/v1/runs/{run_id}` reports current progress. No automatic new
execution or activation is authorized by an error, missing field or retry.
