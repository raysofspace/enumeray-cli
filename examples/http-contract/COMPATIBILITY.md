# Generate an SDK for Enumeray

Use the OpenAPI document for the service you will call. The service owner
supplies the selected service and public revisions. The API and scientific
contract use version 1, and the selected revisions identify the exact contract
for your client.

## Generated SDK method names

Use the operation IDs from the downloadable OpenAPI document to generate client
methods. Your generator may change capitalization. The website uses its own
anchors for navigation. Generate clients from the download or from your service's
`/api/v1/openapi.json`.

| Method and path | Operation ID |
| --- | --- |
| GET `/api/v1/artifacts/{artifact_id}/content` | `get_artifact_content` |
| GET `/api/v1/runs/{run_id}/provenance/content` | `get_provenance_content` |
| POST `/api/v1/uploads` | `user_create_upload` |
| GET `/api/v1/uploads/{upload_id}` | `user_read_upload` |
| PUT `/api/v1/uploads/{upload_id}/completion` | `user_complete_upload` |
| POST `/connector/v1/assignments/{assignment_id}/uploads` | `connector_create_upload` |
| GET `/connector/v1/assignments/{assignment_id}/uploads/{upload_id}` | `connector_read_upload` |
| PUT `/connector/v1/assignments/{assignment_id}/uploads/{upload_id}/completion` | `connector_complete_upload` |
| POST `/connector/v1/assignments/{assignment_id}/log-archive/uploads` | `archive_create_upload` |
| GET `/connector/v1/assignments/{assignment_id}/log-archive/uploads/{upload_id}` | `archive_read_upload` |
| PUT `/connector/v1/assignments/{assignment_id}/log-archive/uploads/{upload_id}/completion` | `archive_complete_upload` |

The TypeScript-fetch smoke check exercises the operations in this table.
For a bodyless 303 response, read the raw response from the generated
`ResponseError`. Send a separate request to the storage URL without API headers.
If a digest depends on exact scientific numbers, use the
[lossless collection identity implementation](COLLECTION_IDENTITY.md).

### Reproduce the generated client

Use Node 24+, Docker and TypeScript 6.0.3 for this tooling check. In a
fresh directory, save the matching [published OpenAPI](https://doc.enumeray.com/openapi.json)
as `openapi.json`, plus [generator-settings.json](generator-settings.json) and
[generated-client-smoke.ts](generated-client-smoke.ts). The complete examples
archive also supplies those two companion files in `examples/http-contract/`.

The pinned generator requires form models to be retained and JSON null to be a
native TypeScript primitive. TypeScript 6 needs `--ignoreConfig` when compiling
explicit source files from the generated SDK, which includes its own `tsconfig.json`.
The settings map the preflight ID `request` to `requestContextPreflight` so it
has a different name from the transport method `BaseAPI.request`. Run the
following with the downloaded specification.

```sh
node --input-type=module <<'JS'
import { readFileSync, writeFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
const settings = JSON.parse(readFileSync('generator-settings.json', 'utf8'));
execFileSync('docker', ['run', '--rm', '-v', `${process.cwd()}:/work`,
  settings.generator_image, 'generate', '-i', '/work/openapi.json',
  '-g', settings.generator_name, '-o', '/work/sdk', ...settings.options],
  { stdio: 'inherit' });
writeFileSync('package.json', JSON.stringify({ private: true, type: 'module' }) + '\n');
execFileSync('npx', ['--yes', '--package', `typescript@${settings.typescript_version}`,
  'tsc', ...settings.typescript_options, 'sdk/index.ts', 'generated-client-smoke.ts'],
  { stdio: 'inherit' });
JS
```

These commands generate and compile the SDK locally. The smoke function takes
the disposable fixture described by its typed `SmokeContext`. The service
acceptance suite supplies that fixture to exercise the eleven methods.
Normal integrations can import the generated SDK and follow the independently
executable [HTTP workflows](HTTP_WORKFLOWS.md).

## CLI machine projections

Analysis and release summaries and runtime errors use `schema_version: 1`.
Partial and complete collection results use the same shape. For the fields
your script can rely on, use the matching
[CLI automation schemas and transcripts](../../crates/cli/CLI_AUTOMATION.md).
The HTTP responses and files in saved operation directories have their own
contracts.

## Preserved identities and authority

Use the documented [collection serialization](COLLECTION_IDENTITY.md)
to calculate a digest. Check whether a digest refers to the requested entries
or the sealed collection. Keep the artifact IDs and their exact versions.

If the reply to a submission is lost, retry with the same account, request
and key. A different request with the same key returns a conflict. Upload
reservations keep their assigned transport and expiry. Use a scoped user
credential with access to the resource for scientific reads. Connector
credentials authorize operations for the connector's assignments.
