// Compiled unchanged against the client generated from the published OpenAPI.
// The independent digest implementation intentionally uses lossless Python;
// this SDK smoke uses exactly representable scientific values.
import { ArtifactsApi, Configuration, ConnectorsApi, ResponseError, RunsApi, UploadsApi,
  type UploadCompletionReceipt, type UploadRequest, type UploadView } from './sdk/index.js';

export interface SmokeContext {
  api: string;
  user_token: string;
  connector_token: string;
  assignment_id: string;
  session_id: string;
  run_id: string;
  user_request: UploadRequest;
  connector_request: UploadRequest;
  archive_request: UploadRequest;
  archive_bytes_base64: string;
}

function require(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

async function put(view: UploadView, bytes: Uint8Array): Promise<string> {
  require(view.transfer.status === 'available', 'small SDK fixture requires available single PUT');
  const transfer = view.transfer.request;
  require(transfer.method === 'PUT', 'unexpected storage method');
  const headers = new Headers(transfer.headers as [string, string][]);
  require(!headers.has('authorization'), 'API credentials must never be forwarded to storage');
  const response = await fetch(transfer.url, { method: 'PUT', headers, body: bytes as BodyInit, redirect: 'manual' });
  require(response.ok, 'signed storage PUT failed');
  const version = response.headers.get('x-amz-version-id');
  require(version && version !== 'null', 'storage must return an exact version');
  return version;
}

function verified(receipt: UploadCompletionReceipt, request: UploadRequest, version: string): void {
  require(receipt.version_id === version, 'completion lost the exact version');
  require(receipt.size_bytes === request.expected_size_bytes, 'completion size differs');
  require(receipt.checksum.value === request.checksum.value, 'completion checksum differs');
}

export async function smoke(context: SmokeContext): Promise<{ operations: number }> {
  const configure = (token: string) => new Configuration({ basePath: context.api, accessToken: token,
    fetchApi: (input, init) => fetch(input, { ...init, redirect: 'manual' }) });
  const users = new UploadsApi(configure(context.user_token));
  const connectors = new ConnectorsApi(configure(context.connector_token));
  const artifacts = new ArtifactsApi(configure(context.user_token));
  const runs = new RunsApi(configure(context.user_token));
  const bytes = new TextEncoder().encode('abc');

  const user = await users.userCreateUpload({ idempotencyKey: 'sdk-user', uploadRequest: context.user_request });
  const userId = user.reservation.upload_id;
  require((await users.userReadUpload({ uploadId: userId })).reservation.upload_id === userId, 'user method identity differs');
  const userVersion = await put(user, bytes);
  const completed = await users.userCompleteUpload({ uploadId: userId, completionBody: { version_id: userVersion } });
  verified(completed, context.user_request, userVersion);
  const repeated = await users.userCreateUpload({ idempotencyKey: 'sdk-user', uploadRequest: context.user_request });
  require(repeated.reservation.upload_id === userId && repeated.state === 'completed', 'SDK replay created another upload');
  try {
    await users.userCreateUpload({ idempotencyKey: 'sdk-user', uploadRequest: { ...context.user_request, logical_name: 'changed' } });
    throw new Error('changed upload intent was accepted');
  } catch (error) {
    require(error instanceof ResponseError && error.response.status === 409, 'expected changed-intent conflict');
  }

  const scope = { assignmentId: context.assignment_id, xConnectorSession: context.session_id };
  const output = await connectors.connectorCreateUpload({ ...scope, idempotencyKey: 'sdk-output', uploadRequest: context.connector_request });
  const outputId = output.reservation.upload_id;
  require((await connectors.connectorReadUpload({ ...scope, uploadId: outputId })).reservation.upload_id === outputId, 'connector method identity differs');
  const outputVersion = await put(output, bytes);
  verified(await connectors.connectorCompleteUpload({ ...scope, uploadId: outputId, completionBody: { version_id: outputVersion } }), context.connector_request, outputVersion);

  const archive = await connectors.archiveCreateUpload({ ...scope, idempotencyKey: 'sdk-archive', uploadRequest: context.archive_request });
  const archiveId = archive.reservation.upload_id;
  require((await connectors.archiveReadUpload({ ...scope, uploadId: archiveId })).reservation.upload_id === archiveId, 'archive method identity differs');
  const archiveBytes = Uint8Array.from(atob(context.archive_bytes_base64), (character) => character.charCodeAt(0));
  const archiveVersion = await put(archive, archiveBytes);
  verified(await connectors.archiveCompleteUpload({ ...scope, uploadId: archiveId, completionBody: { version_id: archiveVersion } }), context.archive_request, archiveVersion);
  require(new Set([userId, outputId, archiveId]).size === 3, 'upload domains collapsed into one operation');

  // Generated fetch clients treat non-2xx as ResponseError. For this documented
  // bodyless 303, retain the raw response and make a separate storage request.
  let location: string | null = null;
  try {
    await artifacts.getArtifactContentRaw({ artifactId: completed.artifact_id });
    throw new Error('artifact content unexpectedly returned a 2xx body');
  } catch (error) {
    require(error instanceof ResponseError && error.response.status === 303, 'expected checked artifact redirect');
    location = error.response.headers.get('location');
  }
  require(location, 'checked content redirect has no Location');
  const downloaded = await fetch(location, { redirect: 'manual' });
  require(downloaded.ok && await downloaded.text() === 'abc', 'exact artifact bytes differ');
  const provenance = await runs.getProvenanceContent({ runId: context.run_id });
  require(provenance.run.run_id === context.run_id, 'provenance content method returned another run');
  return { operations: 11 };
}
