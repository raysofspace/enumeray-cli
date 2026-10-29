# Connector drivers and connections

This reference also appears in the [web documentation](https://doc.enumeray.com/).
See that entry point for local preview and role-based infrastructure guides.

An execution context combines a **driver**, which owns the job lifecycle, with a
**connection**, which reaches the compute runner. Model wrappers own scientific
inputs, outputs and runtime setup. Connector deployment is an operational choice:
a service on a workstation, a cluster submission host, or an external gateway can
use the same context contracts.

## Choosing a context

| Driver | Connection | Typical deployment |
| --- | --- | --- |
| `process` | `direct` | Connector and execution account on the same machine |
| `process` | `ssh` | Gateway connector reaching an execution host |
| `slurm` | `direct` | Connector on a Slurm submission host |
| `slurm` | `ssh` | Gateway connector reaching a Slurm login or submission host |

Use `process` when the wrapper's foreground process represents the whole job.
A script may load modules, activate Python, or run Apptainer in the foreground.
Use `slurm` when the scheduler owns allocation, queuing and cancellation. Do not
hide `sbatch` in a process wrapper and return immediately: that would track the
submission script instead of the scientific job. SSH describes access, never job
ownership. Containers are a wrapper/runtime choice in this initial implementation.

Start with a complete CLI manifest in `../cli/examples/`:
`process-direct-context.json`, `process-ssh-context.json`,
`slurm-direct-context.json`, or `slurm-ssh-context.json`. Edit paths,
site descriptions, then register using the existing context
CLI. Shared validation rejects unsupported combinations and fields. Publication
validation checks the installed entrypoint and required Slurm batch file, their digests, execution identity and driver
readiness before the model can be activated.

For direct access, `connection.launcher` is an absolute argv prefix for a restricted
launcher. It receives `compute-runner ACTION --directory PATH` and the versioned
JSON request on stdin. It must execute as a separate model account and preserve
stdout for the JSON response. This permits site-controlled scripts that invoke
the runner; it is not a general shell-command transport.

For SSH, `connection.runner_path` identifies the same Enumeray executable on the
submission host. Credential references resolve only on the connector host; keys
and connection configuration never enter the compute job payload. Configure
strictly trusted host keys and an execution account unable to read connector state.
SSH artifact transfer uses SFTP. Direct transfer uses bounded runner chunks.
For Slurm, `driver.worker_path` must exist on compute nodes and `work_root` must be
shared with the submission host. The execution account needs `sbatch`, `squeue`,
`sacct` and `scancel`; accounting must be available.
Context configuration supplies `driver.worker_path`; the modeller's batch file
owns cluster routing, resource requests and runtime setup. Enumeray does not parse
or approve scheduler directives, and validation probes only monitoring access.
Each Slurm publication supplies `execution.slurm_script` relative to its immutable
release directory. The driver supplies recovery identity, log destinations and the
job working directory. It persists launch evidence before submission and uses
Slurm's actual `(cluster, job ID)` handle for recovery, observation and cancellation.
A missing receipt or cluster is recovered by exact launch/user evidence across
Slurm's queue and accounting records; ambiguous evidence never permits resubmission.
Supervision preserves batch-file resource limits and scratch behavior. Model
runtimes and batch files own resource requests and scientific execution deadlines.
The file receives the worker executable and job directory as positional arguments
and invokes `"$1" local-worker --directory "$2"` in the foreground. See the
[batch-file contract](README.md#compute-runner-and-slurm) and `model.sbatch` example.

## Register a context without reading the code

As an authenticated infrastructure owner, save this complete process/direct
manifest as `context.json`. The example assumes an infrastructure `lab` and its
enrolled `lab-connector`:

```json
{
  "manifest_version": 1,
  "kind": "execution-context",
  "spec": {
    "id": "lab-cpu",
    "connector_id": "lab-connector",
    "definition": {
      "driver": {
        "kind": "process",
        "scratch": {
          "max_bytes": 1073741824,
          "max_entries": 10000,
          "max_depth": 32,
          "scan_interval_ms": 1000,
          "cache": "per_run",
          "symlinks": "forbid"
        }
      },
      "installation_roots": ["/opt/enumeray/models"],
      "work_root": "/var/lib/enumeray-model-work",
      "max_outstanding_assignments": 4,
      "connection": {
        "kind":"direct",
        "launcher":["/usr/bin/sudo","-n","-u","model-runner","--",
          "/usr/local/bin/enumeray"]
      }
    },
    "metadata": {
      "location":"Research lab",
      "disclosure":"Research-owned machines process submitted model data."
    }
  }
}
```

Replace paths and accounts with reviewed site values. The installation root
contains immutable model releases. The work root belongs to the model execution
account and contains private job state; it is distinct from the connector journal.
The `max_outstanding_assignments` value bounds API admission, including completed
execution awaiting publication. Location/disclosure text is consumer-facing;
publications snapshot it at registration.

```sh
enumeray infra context-create lab --file context.json
enumeray infra contexts lab
enumeray connector diagnostics lab lab-connector
enumeray infra grant lab --context lab-cpu --user MODEL_OWNER_UUID
```

Use the model author's stable `auth whoami` user UUID. Registration saves the
context; later publication validation proves installed entrypoint/script hashes,
execution identity and backend readiness. The grant permits author publication
but provides no host login or file deployment. Agree the deployment route and
runtime with the author before publication. Execution settings are immutable;
create a new context to change roots, identity, driver or connection. Metadata
edits use `infra context-update` with its observed generation.

For SSH, replace `definition.connection` with the object below, retaining the
complete manifest envelope and other definition fields:

```json
{
  "kind":"ssh",
  "host":"login.cluster.example",
  "user":"model-runner",
  "port":22,
  "credential_reference":"cluster-key",
  "runner_path":"/usr/local/bin/enumeray"
}
```

The connector account's private `credentials.json` is separate from that public
manifest:

```json
{"ssh":{"cluster-key":"/var/lib/enumeray-connect/.ssh/id_ed25519"}}
```

Supply it using `run --credentials-file credentials.json`. The remote account
needs the matching public key, an SSH-compatible shell, SFTP and access to the
runner, installation roots and work root. Provision verified host keys in the
connector account's `known_hosts`; strict verification rejects unknown hosts.
The key path is local to the connector host while work/release paths are remote.

For either Slurm connection, use `{"kind":"slurm","worker_path":
"/usr/local/bin/enumeray"}` for `definition.driver`, and choose a
`work_root` shared by the submission host and compute nodes. Keep the worker binary
available at that path on every allocated node. The execution account needs
working `sbatch`, `squeue`, `sacct` and `scancel`, including accounting. The author
adds the foreground `model.sbatch` described above to each release; scheduler
resources/routing belong in that script, not in the context JSON.

Complete alternatives are available in
[process/direct](../cli/examples/process-direct-context.json),
[process/SSH](../cli/examples/process-ssh-context.json),
[Slurm/direct](../cli/examples/slurm-direct-context.json) and
[Slurm/SSH](../cli/examples/slurm-ssh-context.json) manifests. Successful registration
is followed by an installed publication reaching ready and a disposable run
completing on the actual site backend.

## A site-controlled launcher wrapper

A direct context can use a root-owned helper when account switching needs site
logic. For example, save `/usr/local/libexec/enumeray-runner` as:

```sh
#!/bin/sh
set -eu
case "${1-}" in
  compute-runner) ;;
  *) printf '%s\n' 'Only compute-runner is permitted' >&2; exit 64 ;;
esac
exec /usr/bin/sudo -n -u model-runner -- /usr/local/bin/enumeray "$@"
```

Install it owned by the host administrator with mode 0755 and set the direct
connection to `{"kind":"direct","launcher":["/usr/local/libexec/enumeray-runner"]}`.
Configure passwordless permission for the runner command under `model-runner`,
as in the infrastructure quickstart. The helper forwards stdin, stdout, argv and
exit status without a shell-built command string. Print any additional setup
diagnostics on stderr. It must not accept model-controlled host/credential settings,
and the service's privilege restrictions must allow the approved account switch.
Scientific runtime setup belongs to the model wrapper or Slurm batch file.

## Exercise the runner protocol

Ordinary deployments let the connector issue these operations. For backend
integration or diagnosis of an existing test job, the execution account can make
a status request that does not launch work. Save `status-request.json`:

```json
{"version":1,"action":"status","payload":null}
```

```sh
/usr/local/bin/enumeray compute-runner status \
  --directory /var/lib/enumeray-model-work/EXISTING_JOB_DIRECTORY \
  < status-request.json
```

Use the retained directory of that test job. The directory holds the pinned job
and launch identity; status does not select or launch an executable. The CLI action
must match the JSON action. A running response can be
`{"version":1,"payload":{"status":"running"}}`; terminal observation includes
retained completion evidence. Missing ownership/backend evidence can remain
unresolved and never authorizes another start.

| Runner action | Purpose and boundary |
| --- | --- |
| `validate` | Check installed descriptor/files, execution identity and driver readiness before accepting configuration |
| `identity` | Read the runner UID without reading credentials or touching job state; rechecked before direct submission |
| `protect-installations` | Persist known canonical installation overlap pins on the original compute host before readiness |
| `prepare` | Create the pinned private job directory before launch |
| `write-collection` | Install a bounded page of pinned collection metadata before launch; freeze and verify its full digest before staging completes |
| `read-collection` | Read a bounded page of runner-measured output members; completion/journal retain only the compact descriptor |
| `write-input` | Stage a bounded verified input chunk before the permanent start boundary |
| `transfer-probe` | Inspect pinned input progress or compare a bounded input/output/archive prefix digest; input inspection closes at launch |
| `start` | Attempt the original launch once; persist evidence before execution/submission |
| `status` | Observe/reconcile original work and return retained scientific completion when established |
| `allocation-status` | Observe whether backend allocation ownership has been released |
| `request-cancel` | Record/request pending or running cancellation against the persisted original handle |
| `read-output` | Read bounded output bytes from a confined retained path |
| `read-logs` | Read raw source bytes with epoch/offset evidence independently of status |
| `build-log-archive` | Finalize bounded retained raw sources after allocation release |
| `read-log-archive` | Read bounded archive chunks for publication |
| `cleanup-log-archive` | Reclaim local archive state after publication/expiry resolution |
| `cleanup-run` | Reclaim eligible delivered run payloads while preserving replay and completion evidence |

Inputs are strict version-1 envelopes with action-specific typed payloads. Unknown
versions/actions/fields fail before compute operations. `start` is never a retry
primitive: after possible submission, use original state for reconciliation and
observation. Cancellation has its own outcome; inspect status afterward to establish
termination. File reads are limited to 256 KiB chunks and protocol envelopes to
4 MiB. Transfer probes hash at most 64 MiB, return typed source evidence and never
accept arbitrary filesystem paths. Input chunk replays must match retained bytes.
Backend extensions must preserve those ownership, replay and verification
boundaries described below.

## Code boundaries

- `contracts/src/execution.rs`: tagged driver and connection configuration,
  access and path validation. Publication requirements derive `process` or `slurm` from the context.
- `src/connections.rs`: `ComputeConnection` validation, runner invocation and
  artifact upload/download. This layer must not select scheduler behavior.
- `src/drivers/`: `ExecutionDriver` capabilities, readiness, durable launch preparation, submit, reconcile,
  read-only observe and explicit `request_cancel(handle, scope)`.
  Implementations run on the compute host and must not resolve API/SSH credentials.
- `src/protocol.rs`: versioned requests, typed action payloads and normalized
  response states. Unknown versions and actions fail before compute operations.
- `src/compute.rs`: common preparation, evidence, staging, collection and recovery.
- `src/launch.rs`: immutable identity-bound startup proof and typed launch outcomes.
- `src/cleanup.rs`: delivery/release gates, protected installation inventory,
  bounded payload retirement and durable cleanup checkpoints.
- `src/runtime/local_process.rs`: durable model supervisor and wrapper protocol.

The runner supports `identity`, `validate`, `protect-installations`, `prepare`, `write-collection`, `read-collection`, `write-input`, `transfer-probe`, `start`, `status`,
`allocation-status`, `request-cancel`, `read-output`, `read-logs`, `build-log-archive`,
`read-log-archive`, `cleanup-log-archive` and `cleanup-run`. Protocol version 1 uses
`{"version":1,"action":"status","payload":null}` and replies with
`{"version":1,"payload":{"status":"running"}}`. Log payloads use a separate
typed `read-logs` request with `source`, `epoch`, raw `offset` and `complete`.
Responses contain base64 raw bytes, `earliest`, `end`, `offset`, `epoch`, `closed`
and `unavailable`, bounded to 16 KiB. The connector splits this bounded read-ahead
into 2 KiB durable deliveries. Final batch reads and archive operations fence
allocation release; archive chunks reuse the 256 KiB file transfer bound.
Requests/responses are bounded to 4 MiB and transfer chunks to 256 KiB. Wrapper
protocol version 1 is independent of this compute protocol: scalar/JSON
inputs and outputs, and declared versioned artifact references, retain their
existing model schemas. No connection-specific model schema is needed.

`cleanup-run` carries the exact acknowledged terminal result, final live-log
delivery evidence, terminal archive status and saved installation-protection
snapshot. It requires
matching assignment/run/attempt identities, provider cleanup acknowledgement,
confirmed allocation release and protected installation evidence. The runner
fences payload readers, submission and worker ownership before committing cleanup
intent and release proof, then deletes only bounded run-owned payloads. A lost
reply resumes the same intent; previously saved release proof survives later
scheduler accounting loss. Missing evidence or an unavailable host retains data.
Root launch/completion records, lock inodes and compact log manifests remain;
prepare, staging and start cannot reopen a retired launch. Published object
versions and known protected releases remain outside cleanup. Older services
without `run_payload_namespace_reserved`, or unavailable protection inventories,
permit ordinary execution/delivery while withholding deletion authority.
An overlapping protected release or declared executable/batch file blocks the
whole run's cleanup; an ancestor release directory alone does not block unrelated
scratch. Canonical aliases observed during validation acquire durable protection
pins before readiness acknowledgement. `protect-installations` receives
`references` containing `release_path` and declared `files`, bounded to 128
references and 1 MiB per request; its directory is the approved work root rather
than a job directory. A `completed` reply confirms protection persistence, not
scientific completion or installation readiness.

Cancellation is a separate typed operation, for example:

```json
{"version":1,"action":"request-cancel","payload":{"scope":"pending"}}
```

Its response contains `{"version":1,"payload":{"cancellation":{"outcome":"accepted"}}}`.
Outcomes are `accepted`, `unsupported`, `already_terminal`, or `unresolved` (with
a `reason`). No cancellation reply contains a completion report. Always follow
the request with `status`; acceptance only acknowledges a request, and even an
already-terminal backend needs observation and verified wrapper evidence before
scientific success can be reported. A lost reply or unresolved outcome permits
repeating cancellation against the original persisted handle, never submission.
Callers select only `scope` (`pending` or `running`); the runner resolves the job
identity from its durable directory. They cannot supply a job ID or cluster.

Drivers declare internal `cancel_pending` and `cancel_running` capabilities.
Validation reports readiness, installed file digests and the execution identity.
Both drivers support user cancellation through `running`, which includes pending,
starting and running work. The process supervisor consumes a durable intent and
kills its owned process group, confirms every member stopped, drains logs and
retains a `cancelled` completion. Slurm cancels the complete persisted allocation
without a state filter; queue absence and unambiguous cancelled accounting, plus
stopped worker ownership when applicable, establish termination. A stopped worker,
transport error or missing accounting alone never confirms cancellation.

`pending` is the separate access-revocation operation, supported by Slurm only.
Pending Slurm requests use the scheduler's atomic `--state=PENDING` filter so
starting/running work continues to drain if it wins the race. Unsupported
cancellation also leaves observation and collection active.

## Adding a driver

1. Add a tagged configuration variant and execution driver kind to shared contracts;
   implement structural validation and update OpenAPI.
2. Implement `ExecutionDriver` in `src/drivers/<name>.rs` and register it in
   `drivers::resolve`. Declare pending and running cancellation capabilities.
3. Return a typed `LaunchOutcome` from submission and persist the native backend
   handle before reporting it. Classify `ProvenFailure` only from evidence that the
   backend did not accept scientific execution; transport failure alone is `Uncertain`.
   Retain bounded diagnostics without interpolating raw command output into public
   failure messages. Workers may retain failure proof only before spawning the wrapper;
   accepted scheduler work also needs stopped-execution evidence before completion.
   Normalize backend
   observations; missing evidence is unresolved, never permission to resubmit.
   `reconcile` may recover and retain the original handle after a lost receipt.
   `observe` only reads evidence and never cancels or writes records; the common
   runner retains confirmed completion. `request_cancel(handle, scope)` must be
   safe to repeat after a lost reply and return an explicit cancellation outcome.
4. Use the common wrapper supervisor or explicitly define equivalent durable
   completion and artifact evidence. Leave connections and credential handling alone.
5. Add context examples and run the lifecycle tests through each supported
   connection, including lost submit replies, repeated starts, restart recovery,
   pending cancellation racing with execution, and artifact/log collection.

## Adding a connection or deployment

A new access mechanism adds a `ConnectionConfig` variant, implements
`ComputeConnection`, and registers it in `connections::resolve`. Preserve bounded
requests, private state, exact paths, host verification and verified transfers.
The existing runner protocol and driver code should remain unchanged. Test at
least process and Slurm through the new mechanism, including interrupted staging
and lost replies.

A new packaging/deployment option needs service setup, paths, trust and credential
provisioning, and a durable journal volume. It does not require a driver when job
ownership remains process or Slurm. Keep the connector identity separate from
model execution and protect its journal and credentials. Do not run two connector
instances against the same journal.

## Recovery and compatibility

Only a fresh durable launch intent permits a start. A connector-side marker is
synced before sending `start`, and the runner writes its own exclusive marker
before spawning or submitting. Repeated starts fail. Safe preparation/staging can
resume before that boundary; after it, recovery reconciles, observes, requests
pending cancellation when revoked, and collects.
Slurm can recover a lost receipt by the unique launch name. Missing or ambiguous
evidence remains blocked. Pending cancellation never stops a running model.

All project-owned contracts are at version 1: journal, archive, installation cache,
compute job, compute protocol, and wrapper protocol. During unreleased development,
change these first versions in place; do not add compatibility branches or bumps.
Unknown versions are rejected without modification. Before resetting development
state after a breaking change, stop the connector and verify that old jobs have
stopped; retain unresolved journals for reconciliation. Re-register contexts and
publications with matching manifests and binaries.

## Verification

From the public repository root, follow the [local checks](../../CONTRIBUTING.md#build-and-local-checks).
The [real backend suite](tests/real-backends/README.md) runs all four combinations
against actual OpenSSH and Slurm, including account isolation and file integrity.
The service operator separately verifies publication, API authorization,
versioned object storage and connector restart recovery against the matching
client artifacts. A driver is not qualified by mock scheduler output alone.

## User cancellation and completion races

`PUT /api/v1/runs/{run_id}/cancellation` records the creator's immutable
`pending_and_running` intent after active-credential and `runs:cancel` checks.
Assignment control delivers `cancel_running` after reconnection to enrolled
and operator-configured connectors, and disables new launches. Before a backend
start, the connector can safely report cancellation;
a runner submission lock also serializes cancellation with its start operation.
After a possible start, the original launch must be reconciled, never repeated.

Accepted requests remain `requested` until a terminated `cancelled` report
commits. Existing verified wrapper completion wins an observation race; its
result stays succeeded/failed and cancellation status becomes `completed`.
Cancellation receipts preserve `requested_at`; only confirmed cancellation sets
`cancelled_at`. Result delivery and receipt replay use the original assignment,
attempt and journal. Logs and diagnostics remain available after cancellation.
Unsupported scopes explicitly return `unsupported` and retain observation.
External adapters must declare their cancellation capabilities and satisfy these
same termination requirements when the executable adapter protocol is added.
