# Independent HTTP workflows

Run these Python examples to upload and verify a file, seal a collection, or
recover a run after a lost submission reply. The scripts use HTTP directly
and run with Python 3.9+ and its standard library. The examples are tested
against a disposable service. To use your service, get its API origin, a token
and resource access from the service owner. For the run example, also get
the IDs of an installed, ready model and its current publication.

Download the [complete examples archive](https://doc.enumeray.com/examples/model-examples.tar.gz)
and [SHA-256](https://doc.enumeray.com/examples/model-examples.tar.gz.sha256).
Follow the [fresh-directory extraction and verification instructions](https://doc.enumeray.com/authors/overview/#run-the-downloaded-examples-offline).
Work in `examples/http-contract/` inside the extracted archive. Keep
[raw-http.py](raw-http.py), [collection-identity.py](collection-identity.py),
`digest-vectors/` and `files/` together. These examples use files up to 1 MiB.
Check your service's policy, quotas and the model's file declarations when
adapting them. Larger transfers can require multipart object uploads.

Set `API` to the HTTPS origin without a path. Supply the token in
`ENUMERAY_TOKEN` using your approved credential method. Use plain HTTP only
for a disposable loopback fixture. Choose the scopes for the example you run.

| Task | Required scopes |
| --- | --- |
| Check the account (all examples) | `me:read` |
| Upload and download artifacts or collections | `uploads:create`, `uploads:read`, `uploads:complete`, `artifacts:read`, `artifacts:download` |
| Submit and inspect a run | `runs:create`, `runs:read` |

Your account also needs permission on each resource. The run example uses
standard access to the current publication. If you adapt it to trial a
candidate, you also need `models:manage` and the compute grant saved with the
candidate or a named delegation for it.
See the downloadable [HTTP conformance records](https://doc.enumeray.com/examples/http-conformance.json)
for the operation-specific rules.

## A standalone artifact

From the downloaded directory, run the artifact example with `API` and
`ENUMERAY_TOKEN` set.

```sh
python3 raw-http.py --api "$API" --directory artifact-operation \
  artifact --file files/a.csv --media-type text/csv \
  --metadata digest-vectors/a-metadata.json > artifact-receipt.json
```

The script uploads one CSV, verifies its download and writes a receipt. It
performs the transaction in this order.

1. Save the request, operation key, account and origin. Reserve an upload with
   `POST /api/v1/uploads`.
2. Send the file to storage with the signed transfer headers from the
   reservation. Save storage's exact `x-amz-version-id`.
3. Complete with `PUT /api/v1/uploads/{upload_id}/completion` and inspect the
   resulting artifact.
4. Request `/api/v1/artifacts/{artifact_id}/content`. The API checks access
   and returns a `303` with a temporary storage URL for the exact version.
5. Request that URL without API authorization, then verify the full size and
   SHA-256. Keep the version query intact and check redirect targets before
   following them.

Keep the completion receipt as evidence of that exact version. When you ask
to download again, the service checks current read access and availability.

After an interruption, run the same command with the original
`artifact-operation/` directory and unchanged file. The script checks the API,
account and source checksum. If a storage PUT reply was lost before the script
saved its version, repeating that same PUT can create another unpublished
version. Completion publishes only the version saved with the operation.

The private `operation.json` contains the exact request bytes, key, API and
account. The script saves that file atomically and syncs the files and directory
before the first request. It can restore the separate readable request and key
files from `operation.json`. It stops if an incomplete copied operation lacks
that file. Choose a new directory only to start a separate upload.

## A collection with two nested members

First compute the digest of the two-member request, then upload and seal
the collection.

```sh
python3 collection-identity.py digest-vectors/mixed-requested.input.json
python3 raw-http.py --api "$API" --directory collection-operation \
  collection --manifest digest-vectors/mixed-requested.input.json \
  --source-root files > collection-receipt.json
```

The script saves the complete requested manifest and the request and key for
creating the collection. It then performs these steps.

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

To use a completed artifact, pass `--reuse-artifacts reuse.json` with a JSON
object that maps each selected member path to an artifact UUID. The artifact's
bytes, checksum, version, media type and scientific metadata must match that
member, and your ownership or assignment must permit the binding. The first
example produces an artifact with the required media type and lossless
metadata for `a.csv`, so you can use its UUID for that member.

The script saves the reuse map before the first create request. When you
resume, you can omit the flag and use the saved map. A changed map stops the
command before it uploads or binds anything. Final inspection follows all
member-page cursors.

To try an empty collection, use `digest-vectors/empty-requested.input.json`
with a new operation directory. The script freezes and seals the manifest
without adding or uploading members. The returned member page is empty with a
null cursor. To recover a lost reply, repeat the full command with its original
state. Keep requested and sealed identities separately.

## Submission after a lost reply

Create `run-request.json` using the actual accessible model/publication IDs and
scientific inputs satisfying its published schema. Use this request shape
with your own model and publication IDs.

```json
{"model_id":"YOUR_MODEL_ID","expected_publication_id":"YOUR_PUBLICATION_ID","inputs":{"value":2}}
```

Make `changed-request.json` with the same model and publication and a different
valid scientific value. Review the inputs, execution location and data access
for the intended run before sending it.

```sh
python3 raw-http.py --api "$API" --directory submission-operation \
  run --request run-request.json --lose-reply \
  --changed-request changed-request.json > recovered-submission.json
```

The script saves the exact request and key in private `operation.json` before
the first POST. It discards the acceptance reply, then repeats
`POST /api/v1/runs` with the saved pair and recovers the original run and
receipt. The changed request with that same key must return
**409 `idempotency_conflict`**. Service acceptance tests verify that this
sequence creates one run, one assignment and one execution. Collection page
replay follows its own rules.

Keep `submission-operation/run/operation.json`, `request.json`, `idempotency-key.txt`, receipt and
identity files after a timeout or loss of authentication. Restore access to
the same account and API and repeat the saved operation with the original key.
The replayed `queued` receipt records initial acceptance. Read
`GET /api/v1/runs/{run_id}` for current progress. If an error or missing field
leaves the outcome uncertain, inspect the existing operation before taking any
action that could create or activate new work.
