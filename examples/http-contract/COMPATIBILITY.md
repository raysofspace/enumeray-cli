# TASK-104 candidate contract changes

Use the service/public source pair qualified together by the service's verification
record. The unreleased candidate keeps `/api/v1`, scientific `contract_version: 1`
and project-owned formats at version 1. Matching a version number alone does not
establish cross-build compatibility or activate a persistent release.

## Generated SDK method names

The downloadable OpenAPI now has unique operation IDs. Regenerate clients from
that exact document; do not apply the website's route-anchor transformation.
The former ambiguous names map as
follows (method names in a generated language may use that language's casing):

| Previous ID | Method and path | New operation ID |
| --- | --- | --- |
| `request` | PUT `/api/v1/connectors/{connector_id}/contexts/{context_id}/preflight/{request_id}` | `request_context_preflight` |
| `get_content` | GET `/api/v1/artifacts/{artifact_id}/content` | `get_artifact_content` |
| `get_content` | GET `/api/v1/runs/{run_id}/provenance/content` | `get_provenance_content` |
| `create_upload` | POST `/api/v1/uploads` | `user_create_upload` |
| `read_upload` | GET `/api/v1/uploads/{upload_id}` | `user_read_upload` |
| `complete` | PUT `/api/v1/uploads/{upload_id}/completion` | `user_complete_upload` |
| `create_upload` | POST `/connector/v1/assignments/{assignment_id}/uploads` | `connector_create_upload` |
| `read_upload` | GET `/connector/v1/assignments/{assignment_id}/uploads/{upload_id}` | `connector_read_upload` |
| `complete` | PUT `/connector/v1/assignments/{assignment_id}/uploads/{upload_id}/completion` | `connector_complete_upload` |
| `create_upload` | POST `/connector/v1/assignments/{assignment_id}/log-archive/uploads` | `archive_create_upload` |
| `read_upload` | GET `/connector/v1/assignments/{assignment_id}/log-archive/uploads/{upload_id}` | `archive_read_upload` |
| `complete` | PUT `/connector/v1/assignments/{assignment_id}/log-archive/uploads/{upload_id}/completion` | `archive_complete_upload` |

These names do not change routes, request bodies, durable reservations, keys,
receipts or access policy. Complete token inventories and publication evidence
now have explicit shared schemas. Token issuance accepts only documented explicit
scopes; inventories may also contain operator-issued wildcard credentials.
Driver/connection variants now carry their actual literal kinds and discriminator
mappings, allowing independent clients to distinguish them.

The TypeScript-fetch acceptance uses OpenAPI Generator 7.26.0 and compiles the
complete generated SDK without editing generated source or repairing its input.
Its `generated-client-smoke.ts` exercises all eleven renamed operations. Generated
fetch methods report bodyless 303 responses through their raw `ResponseError`;
inspect that response and make the separate storage request without API headers.
Native JavaScript numbers cannot represent all scientific JSON values; use the
lossless [collection identity implementation](COLLECTION_IDENTITY.md) when
calculating digests, and preserve scientific numeric precision in other clients.

## CLI machine projections

Analysis/release summaries and runtime errors now include `schema_version: 1`;
partial/full collection has one decision-bearing shape. Select the exported CLI
schemas and matching complete transcripts described in the
[automation contract](../../crates/cli/CLI_AUTOMATION.md). Update scripts written
against earlier unversioned candidate summaries. Raw HTTP responses and local
saved analysis/upload/release state are separate contracts; do not rename or
discard retained state to update an output parser.

## Preserved identities and authority

Collection serialization is the existing v1 byte algorithm, not a new canonical
JSON scheme. Requested/sealed digests and immutable artifact/version pins remain
distinct. Existing upload reservations keep their original transport/expiry.
Submission recovery continues to use the original account, request and key;
changed content under that key conflicts rather than starting another execution.
Scientific reads remain available to submitters and current model administrators
under the existing checks; connector-only operational rights do not grant those
reads. Publication and execution still require their explicit authorized steps.

The formerly unique `request` operation also changes because its generated method
would override the TypeScript client transport method `BaseAPI.request`. Every
other previously unique operation ID is retained. The reference-generation check
compiles the complete SDK, including multipart form models; no specification
repair or generated-source patch is applied.
