# Enumeray connector

The [web documentation](https://doc.enumeray.com/) adds role-based guides,
generated command/API reference and troubleshooting, with local preview instructions.

The connector is a library used by the unified `enumeray` executable. Its normal
dependency graph contains the
shared contracts and HTTP/runtime libraries; it needs neither PostgreSQL access nor
server-side storage credentials. The API is reached through outbound HTTP(S).

Choose driver and access independently: `process` or `slurm`, combined with
`direct` or `ssh`. See [configuration and extension guide](EXTENDING.md) for the
four context templates, deployment choices and module contracts.

For an installation-to-first-trial journey, start with
[guided connector setup](../cli/SETUP_GUIDE.md). It saves selections and progress,
applies reviewed host preparation explicitly and reuses the operations below.

## Prepare configuration and check host prerequisites

Start from [Linux managed](../cli/examples/host-setup-linux.json),
[macOS managed](../cli/examples/host-setup-macos.json), or
[single-user development](../cli/examples/host-setup-development.json). Edit the
identifiers, accounts, absolute site paths and limits. macOS numeric IDs are
examples; the administrator must check name and UID/GID collisions first.

```sh
enumeray connector configuration generate --file host-setup.json --directory host-plan
```

No login is required. The new private directory contains `plan.json`, `context.json`,
restricted launcher/native service templates and `REVIEW.md`. Existing destinations
are refused. Generation never creates accounts, installs software, enrolls a
connector or starts a service. Each typed action names its host, destination,
responsible role and prerequisites. Review these before applying host changes.

The version-1 specification contains `context` (the existing context spec),
`connector_account` and `execution_account` (platform, name, group, home, creation
choice and optional numeric IDs), `connector_executable`, `api_base_url`, `journal`,
`token_file`, optional `credentials_file`, `max_workers`, `max_transfer_workers`,
`max_pending_assignments` and `shutdown_timeout_seconds`. Platforms are `linux` or
`macos`. Managed direct contexts use the reviewed `sudo -n -u EXECUTION_ACCOUNT --
ABSOLUTE_ENUMERAY` launcher. SSH needs its named execution account and a private
credential-reference map. Existing accounts can be reused.

Managed executables and ancestors must be site-controlled. A model user's writable
Homebrew prefix can undermine connector credential isolation; install the selected
matching executable at the reviewed protected path. Linux plans supply systemd
units, macOS plans supply LaunchDaemons, and development plans supply optional user
services. Private permissions, manual recovery and shutdown grace remain explicit.

After approved preparation, make the plan privately readable by the connector OS
account and run as that account:

```sh
enumeray connector preflight --plan host-plan/plan.json
# Explicit consent is required for trusted same-user development:
enumeray connector preflight --plan host-plan/plan.json --allow-development-execution
```

Local checks need no profile, API access, running connector or publication. They
check identities, private paths, runner compatibility, effective root access,
SSH/SFTP and scheduler prerequisites. They are bounded, launch no scientific code
and submit no Slurm jobs. Temporary probes stay under the approved work root and
clean only their own files. The attempt has a 120-second deadline, at most 128
checks and a 256 KiB report. `probe_id` identifies any private probe whose cleanup
could not be confirmed. Missing roots/accounts are reported, never repaired.

Once the context is registered and the connector runs, its owner/co-admin can
request checks remotely:

```sh
enumeray connector context preflight lab-connector lab-cpu --directory check-operation
enumeray connector context preflight-status lab-connector lab-cpu
enumeray connector diagnostics lab-connector
enumeray connector diagnostics lab-connector --context lab-cpu
```

Requests need `me:read`, `connectors:read` and `connectors:manage`; read-only status
needs `connectors:read`. Compute grants alone do not grant these operations. The
private operation directory binds one request to its API, account and context.
Repeat it after interruption or an uncertain reply; use a new directory for a fresh
check. One outstanding attempt per context expires after fifteen minutes.

Reports identify failed/unverified checks, stable causes, corrections and responsible
roles. Current readiness requires recent contact, the current credential/session
and a report no older than five minutes. Historical passes stay visible when stale.
Context diagnostics separates **configured** (active registration and enrollment),
**connected** (recent contact using the current credential) and **ready for setup**
(acknowledged configuration, active context, an open session and current passing
required host checks, without maintenance or withdrawn authority). It needs no
publication or model installation. A preflight host pass alone does not override
these operational controls or establish grants or scientific validity.

Diagnostics reads stored evidence and exits 0 when retrieval succeeds. It never
requests a check or repairs a host. Missing evidence appears as unknown, and old
reports remain visibly historical. Follow the listed correction and responsible
role; use `connector context preflight` with a new private operation directory for
a fresh check, or resume the existing directory after an interruption. Contact
freshness does not expire session ownership or prove a process stopped.

Local reports cannot be imported as authenticated connector evidence. Remote checks
work during maintenance without changing publication validation or admission.

Reports stay on stdout: exit 0 means required host prerequisites passed; 1 means
blocked or unverified. Remote deadline exit 5 and Ctrl+C exit 130 retain the same
operation. Read-only status exits 0 when retrieval succeeds. Service installation
is a separate observation; foreground operation is supported.

Slurm checks tools, monitoring/accounting and submission-host/shared paths, while
leaving compute-node visibility and allocation execution explicitly unverified.
Scientific installation, candidate trials and site qualification remain separate.
A host report cannot activate a model.

## Single-user development

Use development execution only for trusted models on a personal macOS or Linux
machine. The CLI, connector and model run as your existing non-root OS user.
Model code can access that user's connector credentials and files. Private file
permissions still protect against other users; they cannot isolate processes
running as the same user. Managed execution with separate identities remains the
default, including when a connector permits development contexts.

This recipe needs outbound HTTPS to a compatible deployed test service, an
ordinary CLI login, and Python 3 for the example model. A DSM-hosted test service
works the same way: use its actual HTTPS origin and normal trusted certificates.
The connector initiates outbound requests; no inbound laptop connection, local
API/database, new host account or sudo configuration is required. Development
execution does not enable insecure HTTP or bypass authentication, ownership,
compute grants or model access policy.

Install a matching `enumeray` executable under `$HOME/.local/bin` using the
[installation guide](../cli/INSTALL.md), or follow the public repository's
source-build instructions while a qualified package is unavailable. Use that
executable throughout the recipe:

```sh
export PATH="$HOME/.local/bin:$PATH"
umask 077
mkdir -p "$HOME/enumeray-development/models" "$HOME/enumeray-development/work" \
  "$HOME/enumeray-development/connector"
chmod 700 "$HOME/enumeray-development" "$HOME/enumeray-development/work" \
  "$HOME/enumeray-development/connector"
cd "$HOME/enumeray-development"
enumeray auth configure https://enumeray.example.org
enumeray auth login
enumeray auth whoami
enumeray connector create development-connector \
  --label 'Personal development connector' --max-outstanding-assignments 1 \
  --output enrollment.json
enumeray connector enroll --directory connector/credentials \
  --api-base-url https://enumeray.example.org --enrollment-file enrollment.json
```

Choose unique resource IDs if those names already exist. Retry an interrupted
enrollment with the same directory and document, as described below. User login
and connector credentials retain their separate files and purposes.

Copy [development-process-context.json](../cli/examples/development-process-context.json)
to `context.json`. Replace every `/home/researcher` prefix with your **actual
absolute home directory** (`/Users/your-name` on a typical Mac). JSON paths do not
expand `$HOME` or `~`. The reviewed definition must contain
`"execution_mode": "development"`, process scratch limits, a direct launcher
containing the absolute path to your `enumeray` executable, and the model/work
roots created above. Keep the example's reduced-isolation disclosure visible.

```sh
enumeray connector context create development-connector --file context.json
enumeray connector context list development-connector
enumeray connector grant create development-connector --context development-process --user AUTHOR_UUID \
  --max-queued-runs 10 --max-outstanding-assignments 1
```

Replace `AUTHOR_UUID` with your own `user_id` from `auth whoami`. Even when the
same person owns everything, the explicit compute grant is required. In another
terminal under that same OS user, run the connector in the foreground:

```sh
enumeray connector run --journal "$HOME/enumeray-development/connector/journal" \
  --api-base-url https://enumeray.example.org --connector-id development-connector \
  --token-file "$HOME/enumeray-development/connector/credentials/token" \
  --max-workers 1 --recovery-mode exclusive-local --allow-development-execution
```

Use `exclusive-local` only on one unshared, uncloned local state volume; otherwise
use manual recovery and explicitly confirm the old process stopped. Startup emits
the reduced-isolation warning. A registered context stores configuration. Check host prerequisites before
publication; validation of an installed model still follows publication registration.

Back in the first terminal, create and locally test the sample, then install one
immutable release under the approved root:

```sh
cd "$HOME/enumeray-development"
enumeray model init development-double --directory source
(cd source && python3 -m unittest)
cp -R source models/development-double-r1
```

Edit `models/development-double-r1/publication.json`: set
`execution.context_id` to `development-process`, and `execution.release_path` to
the absolute path of `models/development-double-r1`. The generated process
publication's entrypoint and scientific settings stay with the installed release.
Do not change that release after registration; create a new release directory for
an update. Register and wait for the connector's validation:

```sh
enumeray model validate --file models/development-double-r1/publication.json
enumeray model create development-double --file models/development-double-r1/publication.json
enumeray model release init development-double \
  --file models/development-double-r1/publication.json --directory release-r1
enumeray model release resume release-r1
enumeray model release status release-r1
```

Use the full Candidate ID displayed by release status as `PUBLICATION_ID` below.
If readiness is pending, repeat `release resume release-r1`. Inspect any validation
failure before continuing. Both the context mode and host option are required;
a managed context still rejects a same-user direct runner.

```sh
PUBLICATION_ID='REPLACE_WITH_FULL_CANDIDATE_ID_FROM_RELEASE_STATUS'
cat > inputs.json <<'JSON'
{"value":21}
JSON
enumeray analysis init development-double --candidate "$PUBLICATION_ID" \
  --inputs inputs.json --directory candidate-trial
# Review the saved destination and reduced-isolation disclosure.
enumeray analysis submit candidate-trial
enumeray analysis wait candidate-trial --check-outcome
enumeray analysis collect candidate-trial
enumeray model release review release-r1
enumeray model release activate release-r1
enumeray analysis init development-double --inputs inputs.json --directory ordinary-analysis
# Review this new execution before submitting it.
enumeray analysis submit ordinary-analysis
enumeray analysis wait ordinary-analysis --check-outcome
enumeray analysis collect ordinary-analysis
```

The example returns `doubled: 42`. Candidate success does not activate the model;
review and activation above are explicit. A new intended execution uses a new
analysis directory. Repeating submit for the same directory recovers its receipt.
To exercise cancellation, create and submit another analysis, then use
`enumeray analysis cancel DIRECTORY` followed by `analysis wait DIRECTORY`.
A fast model may already have finished; use your own trusted longer-running model
when verifying running cancellation.

Stop the foreground connector with SIGINT, then restart the same command with the
same journal, API, connector ID and token file. Keep the journal and installed
releases. Only one process may own the journal. `connector inspect --journal PATH`
shows context modes, the warning and `last_allow_development_execution`; that last
value describes a previous invocation and never authorizes a later launch.

If restarted without `--allow-development-execution`, existing submissions can
still be observed, cancelled and delivered. Fresh and prepared development work
remains blocked with its original launch identity. Restart with the option to
resume eligible work; do not delete state or submit a replacement execution.
The controlled `claim-once` and `execute-once` commands require the same option
when used with development contexts. The option has no effect on managed
contexts' separate-identity requirement. Development does not support SSH or Slurm.

## Connector ownership and compute access

Register each connector directly under your user account. Each connector has one
owner and its own optional co-administrators. Connector registration requires no
infrastructure or administrative group. Co-administrators can manage execution
contexts, enrollment, grants and operational controls; only the owner changes
membership, proposes an ownership transfer or archives the connector.

```sh
enumeray connector list
enumeray connector get lab-connector
enumeray connector member list lab-connector
enumeray connector member add lab-connector --user COADMIN_UUID --generation 1
enumeray connector ownership transfer lab-connector --to NEW_OWNER_UUID --generation 2
# The named recipient accepts using their own authenticated profile:
enumeray connector ownership accept lab-connector TRANSFER_UUID
```

Use the current ownership generation returned by `connector get` or `member list`
in place of the example numbers. Membership, transfer and archive guards use
`ownership_generation`; configuration, maintenance and authority controls use
`generation`. Ownership changes do not require re-enrollment or a new journal.
Tokens for discovery need `connectors:read`; management needs `connectors:manage`.

Compute usage remains an explicit per-user grant to an execution context, including
when the owner also runs models. `connector grant available` discovers your grants;
`connector context list CONNECTOR_ID` lists contexts under your connector.
Owning a connector or becoming a co-administrator does not grant model access or
create compute grants. Removing an administrator or transferring ownership does
not revoke independent compute grants. Revoke those grants explicitly.

Archive is permanent and uses the observed ownership generation. It blocks new
management and execution authority while retaining identities, audit records and
recovery evidence. Drain and inspect accepted work with the ordinary maintenance
and context lifecycle commands before archiving; accepted work keeps its original
connector, context, publication and grant identities.

## Enrollment and service operation

The connector owner uses the authenticated `enumeray` CLI. The connector
service account uses `enumeray connector run`; the execution account runs the
credential-free runner. Install the same matching binary on each execution host
and, for Slurm, on compute nodes. The connector needs outbound API access, private
durable local storage and a separate execution identity in managed mode.
The explicit [development exception](#single-user-development) permits trusted
same-user process/direct execution. File jobs also require
the service's verified versioned storage.

Choose IDs once, then create the connector and one-time enrollment on the
owner's CLI machine:

```sh
umask 077
enumeray auth configure https://enumeray.example.org
enumeray auth login
enumeray connector create lab-connector --label 'Lab connector' \
  --max-outstanding-assignments 4 --output enrollment.json
```

The owner CLI reports success only after validating and saving a private enrollment
document; existing output paths are preserved and refused before any request.
If creation or saving is interrupted, preserve the connector ID, settings and any
output file. The CLI makes one read-only inspection. Creation replay confirms
registration without returning the original secret and exits unsuccessfully rather
than saving that receipt as an enrollment document. Failed inspection leaves the
outcome unknown; a failed save may still have published a valid file. Inspect it
privately before requesting another secret. If no usable file was retained:

```sh
enumeray connector get lab-connector
enumeray connector enrollment lab-connector --output replacement-enrollment.json
```

Choose a new output path. Replacement invalidates earlier unused enrollments and
is never automatic. An unused enrollment expires after 15 minutes. Transfer the
saved document privately to the connector host (substitute the replacement filename
below when applicable). As the dedicated connector account, run:

```sh
mkdir -m 700 connector-state
enumeray connector enroll --directory connector-state \
  --api-base-url https://enumeray.example.org \
  --enrollment-file enrollment.json

enumeray connector run --journal connector-state/journal \
  --api-base-url https://enumeray.example.org --connector-id lab-connector \
  --token-file connector-state/token --max-workers 4 \
  --recovery-mode exclusive-local
```

`--enrollment-token-file` accepts a raw enrollment token instead. Enrollment writes
its machine identity and generated bearer token to private durable storage before
sending the enrollment request. Retry an uncertain enrollment with the same
state directory and enrollment file; do not create a new identity. Even after its
original expiry, a committed enrollment can replay its receipt while the generated
credential remains active. If an unused enrollment expired, request a replacement
into a new file; an identity that never enrolled can use it with the same directory.
After successful enrollment, use the credential rotation procedure below with a
new credential directory and the original journal. `--allow-insecure-http` is an
explicit development/private-network exception; HTTPS is required otherwise.

The `run` command remains in the foreground. Its service account must own the
state directory and no execution account may read it. Stop a foreground test
before starting its service. `exclusive-local` is appropriate only for one
installation on an unshared, uncloned local filesystem. Use the default manual
mode otherwise. The [infrastructure quickstart](https://doc.enumeray.com/infrastructure/quickstart/)
contains complete Linux account, restricted launcher and systemd setup.

After preparing the host, register a reviewed context and give an author compute:

```sh
enumeray connector context create lab-connector --file context.json
enumeray connector context list lab-connector
enumeray connector grant create lab-connector --context lab-cpu --user MODEL_OWNER_UUID \
  --max-queued-runs 100 --max-outstanding-assignments 4
enumeray connector grant list lab-connector
enumeray connector diagnostics lab-connector
```

Use the context manifest in [the backend guide](EXTENDING.md#register-a-context-without-reading-the-code)
and replace `MODEL_OWNER_UUID` with the author's `auth whoami` user ID. The grant
does not create a host account or install a model. Give the author the context ID,
installation root and deployment route; they install an immutable release and
register its publication. The daemon fetches/validates that configuration. The
publication must reach ready before it can be activated and used by consumers.

To rotate the same connector's credential, obtain a new enrollment document and
stop its daemon. Enroll into a **new private credential directory**; retain the
original journal so accepted work and backend evidence remain recoverable:

```sh
enumeray connector enrollment CONNECTOR_ID --output rotated-enrollment.json
# Stop the existing connector daemon before replacing its credential.
enumeray connector enroll --directory rotated-connector-credentials \
  --api-base-url https://enumeray.example.org --enrollment-file rotated-enrollment.json
enumeray connector run --journal connector-state/journal \
  --api-base-url https://enumeray.example.org --connector-id CONNECTOR_ID \
  --token-file rotated-connector-credentials/token --max-workers 4 \
  --recovery-mode exclusive-local --credentials-file credentials.json
```

An interrupted enrollment is retried using that same new credential directory.
Successful enrollment revokes the previous credential. Reusing an old identity
for a fresh enrollment is rejected; deleting or recreating the journal is not
part of credential rotation.

The optional private credential-reference file is:

```json
{"ssh":{"cluster-key":"/absolute/path/to/private/id_ed25519"}}
```

Credentials remain on the connector host. OpenSSH runs as the connector OS
account. The verified remote host key must already be trusted in that account's
`~/.ssh/known_hosts`. The connector requires
batch authentication and strict host-key verification; it never automatically
accepts an unknown host key.

| Host and account | Files and responsibility |
| --- | --- |
| Connector host, connector OS account | Private credential-reference JSON and SSH private key; the verified remote host key in this account's `~/.ssh/known_hosts` |
| Remote execution host, execution account | Matching public key in `~/.ssh/authorized_keys`, SSH-compatible shell, SFTP and the installed runner |
| Remote execution host, approved execution roots | Immutable model release, per-run work roots and runner paths named by the registered context |

Preparing the execution account's `known_hosts` does not prepare the connector's
outbound trust store. Obtain the host fingerprint through your site's trusted
route before testing strict SSH as the connector account.

Add `--credentials-file credentials.json` to `run` for SSH contexts. Store this
map and its private keys with mode 0600 under the connector account. Paths in the
map refer to the connector host; the context's runner/work/release paths refer to
the remote host. The remote account needs an SSH-compatible shell and SFTP access.

## Which connector command to use

| Command | When to use it | What it does |
| --- | --- | --- |
| `enroll` | First setup or deliberate credential replacement | Saves identity/token before its request and commits one-time enrollment |
| `run` | Ordinary supervised service operation | Reconciles configuration, observes existing work, claims eligible jobs and supervises execution/transfers |
| `inspect` | Local diagnosis | Shows journal/connector IDs and prepared assignment IDs without inputs/secrets |
| `cleanup` | Inspect completed run data live, or reclaim it while the service is stopped | Reports eligibility with `--dry-run`, or performs a bounded restartable pass |
| `reconcile` | Recover ownership/inventory after a stopped process | Reads existing API work and reconciles the session without claiming/launching |
| `init` | Controlled loopback development steps | Creates an empty journal bound to an API origin and connector |
| `claim-once` | Controlled one-assignment debugging | Registers a session, heartbeats and prepares at most one assignment |
| `execute-once` | Resume a prepared assignment in a one-off workflow | Uses retained launch/result evidence; it may start only previously unlaunched work |
| `publish-log-archives` | Finish retained terminal archive delivery | Publishes diagnostics without scientific execution |
| `compute-runner` | Connector transport/backend integration | Runs a typed credential-free action under the approved execution identity |

`run` is the production entry point. The step commands are useful for a
disposable loopback test when the connector service is stopped:

```sh
enumeray connector init --journal one-off-journal \
  --api-base-url http://127.0.0.1:8080 --connector-id lab-connector
enumeray connector claim-once --journal one-off-journal --token-file connector-state/token
enumeray connector inspect --journal one-off-journal
enumeray connector execute-once --journal one-off-journal --token-file connector-state/token
```

The next `claim-once` archives assignments whose scientific result and delivery
are complete before reserving new capacity. Their receipts and cleanup queue
remain durable; completed history does not fill the 64-assignment active limit.

Do not create a replacement journal to troubleshoot existing accepted work. When
resuming an unclosed session, first prove all prior connector owners stopped and
pass `--confirm-prior-process-stopped` to the recovery command. Repeat execution
against the same prepared assignment after interruption; a start marker prevents
duplicate launch. `--simulate-lost-submission-ack` is a recovery-test hook, not a
normal deployment setting.

For manual recovery of existing work:

```sh
enumeray connector reconcile --journal connector-state/journal \
  --token-file connector-state/token --confirm-prior-process-stopped
enumeray connector run --journal connector-state/journal \
  --api-base-url https://enumeray.example.org --connector-id lab-connector \
  --token-file connector-state/token --recovery-mode manual \
  --confirm-prior-process-stopped
```

The assertion belongs only to an invocation backed by stopped-owner evidence.
Journal locks cannot prove exclusivity on a shared volume, a copied journal or a
different host. Recovery does not prove backend completion; missing/ambiguous
process or scheduler evidence remains unresolved. Keep the original state and
coordinate exceptional resolution with Enumeray support.

## Tune and drain service capacity

`--max-workers` (1–64, default 1) bounds assignments reserving compute, including
pending Slurm allocations. `--max-transfer-workers` (1–64, default 2) bounds bulk
I/O separately. `--max-pending-assignments` (1–64, default 64) includes finalized
outputs not yet published and must be at least the compute-worker count. API
connector/context/grant capacity is a separate admission bound. Storage outages
can fill retained capacity even after compute has completed.

The owner can enter maintenance with typed guarded commands. Read the connector
generation and inspect the affected-work preview before applying:

```sh
enumeray connector get lab-connector
enumeray connector maintenance lab-connector --generation 3 --dry-run
enumeray connector maintenance lab-connector --generation 3
enumeray connector diagnostics lab-connector
enumeray connector wait-maintenance lab-connector --generation 4
```

Use `3` only if the read returned it and replace `4` with the post-maintenance
generation. Maintenance stops new acceptance/claims and retains queued work for
resume. Already assigned preparation, pending allocations, execution and delivery
continue subject to independent authority and user cancellation. The wait excludes
retained queued work; unfinished assignments, missing durable delivery receipts,
active uploads and pending log archives block completion. Stale contact never proves
completion. Generation changes or resume stop the wait, and its deadline leaves
maintenance enabled. JSON mode emits only the final status.

After the wait succeeds, send SIGINT/SIGTERM to the original connector. Its default
drain is 30 seconds; allow a longer service-manager stop timeout. Interrupted work
retains evidence for restart. After maintenance, start the same service, reread the
generation and use `connector resume ... --generation N [--dry-run]`. The legacy
`connector update --file` shape `{"draining":true,"expected_generation":3}` remains
a maintenance request. Label/capacity changes use `connector configure`.

`connector withdraw-authority ... --generation N [--dry-run]` is a separate
reversible control: it blocks new acceptance/claims and prelaunch work, requests
pending-allocation cancellation, and permits already running work and delivery to
finish. `restore-authority` lifts only that control. Both preserve maintenance and
independent permanent revocations, credentials, grants and user authority. None of
these commands transfer backend ownership or permit another launch. Restoring
authority cannot undo cancellation already requested or confirmed.

Delivery is separate from scientific completion. The connector acknowledges it
durably only after retained result evidence and log/archive obligations finish.
Normal service operation and `execute-once` perform this pass;
`publish-log-archives` also acknowledges completed archive delivery. `reconcile`
only retrieves existing delivery evidence. Lost replies replay the same receipt;
settlement fences new live-log batches, uploads and archive publication while
preserving historical request replay. Missing local execution or log-policy
evidence leaves delivery unresolved. For an
unrecoverable journal, coordinate delivery recovery with Enumeray support using retained
evidence; abandonment preserves the scientific outcome and does not grant local
cleanup authority. See [infrastructure operations](https://doc.enumeray.com/infrastructure/operations/).

Finalized unpublished output retention is indefinite by default. An explicit
`--unpublished-retention-seconds 604800` sets a seven-day publication deadline and
can produce `output_retention_expired`; it does not delete the scientific evidence
or files by itself. Completed-run cleanup has an independent retention policy.

The daemon runs bounded cleanup every 30 seconds with
`--completed-retention-seconds 0` by default. Eligible run-owned staging,
workspaces, transfer copies, outputs and logs are reclaimed only after confirmed
execution/allocation release and final delivery. Configure a longer local
retention with `--completed-retention-seconds`, and the sweep cadence with
`--cleanup-interval-seconds`. Active or uncertain execution, pending delivery,
live transfer ownership and unreachable compute hosts remain retained.

The worker resumes a durable queue of unfinished cleanup. Newly archived runs
enter that queue before leaving the active journal, and interrupted or blocked
runs remain queued for later passes. On upgrade, bounded discovery passes add
unfinished legacy archives once. Completed runs leave the queue while their
archived authority, compact cleanup checkpoints and replay guards remain.

Inspect or dry-run against the live journal without taking its process lock.
Relative journal paths are supported. Stop the service before applying a manual
cleanup pass to the same journal:

```sh
enumeray connector cleanup --journal connector-state/journal --dry-run --limit 8
# Stop the connector service before the applying command.
enumeray connector cleanup --journal connector-state/journal --limit 8
```

Cleanup resumes after interruption and preserves archived assignment/result
evidence and durable guards against replaying the original launch. It never
deletes published object versions, installed releases, shared work roots or
site-owned files. Age and lost contact cannot establish eligibility. Keep the
journal and compute replay records for recovery and historical provenance;
`inspect` and cleanup reports show retention and blocked work without inputs or
credentials.

Installation protection is refreshed for each deletion attempt. Permanent
per-run checkpoints retain delivery authority and progress; the full connector
installation inventory is stored separately instead of copied into every run.
Older checkpoints containing that inventory are compacted when revisited.

Automatic and manual deletion require the service's run-directory reservation
capability and a persisted inventory of protected installation releases. If an
older service omits that capability, the protection inventory is unavailable, or
manual/older cache entries lack known installation metadata, cleanup retains
payloads while ordinary scientific execution and delivery continue. Known legacy
installation releases overlapping a run directory, including canonical aliases
observed during validation, remain permanently protected even after configuration
retirement; cleanup cannot treat their installed files as disposable run data.
Unknown or unseeded protection on any compute host blocks payload deletion across
the connector. Configuration reconciliation retains affected cache references;
successfully protected bindings can still be pruned independently. Ordinary
configuration validation and accepted-work recovery continue.
A protected release or declared executable/batch file inside a run's payload
blocks cleanup of the whole run. A broad release directory that merely contains
the work root does not by itself block unrelated scratch reclamation.

The connector always pulls `/connector/v1/configuration`, validates each immutable
installation against its connector context, persists usable installations,
and acknowledges their exact generations. The installation's entrypoint and Slurm
batch file digests are checked again before execution. Required installation configurations remain in the
private cache for accepted work and retained candidates. This verifies those installed files, not every
imported library or data file; model/runtime/dataset identities remain provider
claims. Keep complete release directories immutable.

New process contexts require an explicit connector-owned `driver.scratch`
policy for total workspace bytes, entries, depth, scan interval, cache routing and
links. See [process scratch settings](https://doc.enumeray.com/infrastructure/contexts/#process-scratch-policy).
The connector retains that policy in each worker snapshot. Existing contexts and
jobs without it keep legacy limits; adopting a new policy requires a replacement
context. Configured process jobs use private `.enumeray/tmp` and optionally
`.enumeray/cache` under the workspace, reclaimed by ordinary completed-run cleanup.
Scanning is sampled and yields between bounded chunks; hard quotas remain a site
responsibility. Artifact/result/log limits remain independent. Slurm continues to
use its site-owned resource policy. Install matching connector/runner versions
before registering new process policies.

Each cached installation retains its context identity and validated connection,
work root and installation descriptor. The connector matches the accepted
assignment to that context and exact descriptor before preparing and launching it.
Production installations are compiled from API-configured publications. Context
location and data-access disclosure are descriptive metadata; publications capture
them at registration and metadata edits never redirect accepted work.
Revocation blocks new launch or pending work; running work retains its original
installation for recovery, logs and result collection.

Configuration reconciliation retains at most 1024 required publication entries
and 2 MiB of serialized desired configuration per connector. Model administrators
can explicitly retire unused publication configuration with
`enumeray model retire-publication MODEL_ID PUBLICATION_ID --generation N`.
An authorized current publication must be replaced or revoked first. Accepted
work and pending delivery block final retirement until their references are released.
Historical publication, validation and permanent revocation records remain intact.
Validation acknowledgments may arrive in bounded batches; the observed generation
advances only after every publication reports against that exact generation.
The local retained installation cache is limited to 4 MiB. An addition that would
exceed it fails validation while previous installations remain usable. Reconciliation
removes retired cache entries only when no desired publication or recoverable local
work references their installation; shared installation keys remain until their
last required reference disappears.
The API rejects additional candidates or contexts with
`configuration_capacity_exceeded` before changing configuration. Inspect capacity
and retire unused configuration before choosing another connector/context. Retain
the original connector while accepted work references its installations.

Local contexts and local Slurm submission hosts must provide an site-controlled
restricted launcher. Its complete command includes the unified executable, for example:

```json
["/usr/bin/sudo", "-n", "-u", "model", "--", "/usr/local/bin/enumeray"]
```

The connector appends `compute-runner ACTION --directory PATH`. Configure the OS
launcher to permit only this credential-free runner under the execution account.
Validation rejects a launcher that reports the connector's own UID. Job files
are created as the execution account under `context.work_root`, which must be
private to that account; the connector journal and credentials remain separate.
New release directories and their declared executable or batch-file paths cannot
occupy `work_root/UUID` or any of its descendants for any registered context.
These directories are reserved for run-owned data;
registration returns `reserved_run_installation_path` for that conflict. Broad
installation roots can still overlap a work root: a separate path such as
`work_root/releases/r1` remains available under the context's ordinary release
rules. Exact retries retain original historical publication records.
Input and output files cross this boundary through bounded, verified runner
transfers. The execution account must not have permissions to read connector
state or credentials. SSH contexts use the configured remote account. For
isolation between models, grant separate execution contexts/accounts. Process
supervision retains file/scratch
bounds; CPU and memory containment for local/SSH models is infrastructure policy
(for example per-account cgroups). Slurm batch files and cluster administrators
own scheduler requests and resource limits. Slurm workers preserve inherited
file-size limits and do not impose the process driver's scratch quota.
Contexts and publications carry no CPU, memory or wall-time requests. Enumeray
imposes no scientific wall-time deadline for either
driver. Slurm controls its job time limit; process wrappers own any runtime timeout.

One process owns the journal and serializes assignment evidence. `--max-workers`
bounds concurrent unfinished assignments, including jobs pending in Slurm.
Heartbeat and configuration reconciliation use independent workers. Unknown
submission evidence is retained and never becomes permission to resubmit.

## Compute runner and Slurm

Install the same Enumeray executable at the connection's absolute `runner_path` on the
SSH submission host. For Slurm it must also be visible at `driver.worker_path` on
compute nodes. The configured work root must be a shared filesystem visible to
the submission host and all allocated nodes.

The connector invokes the credential-free `compute-runner` subcommand over SSH.
Preparation, launch, observation, and pending cancellation are separate operations.
A launch marker is persisted before spawning a process or calling `sbatch`;
repeating `start` is rejected. Local and remote models run under the same durable
supervisor and wrapper protocol.

Every Slurm model release must include a batch file beside its model and wrapper.
Set `execution.slurm_script` to its path relative to `release_path`, for example
`"slurm_script": "model.sbatch"`. Process installations omit this field. Connector
validation requires a readable regular file within the immutable release, with a
shebang and Unix line endings, and pins its digest alongside the wrapper's digest.
The file must be UTF-8 text and need not be executable. A changed file requires a
new immutable release. Registration allocates or reuses the internal provider
binding key from the retained model/context, physical settings and scientific
identities. Configuration supplies `publications`, each containing its immutable
expanded descriptor and derived execution requirements. The API derives descriptor
identity claims from the publication's scientific definition. Readiness checks actual
protocol/driver support, installed files and backend access. Requirements do not
prove backend capability or scientific correctness. Compiled installations retain
the descriptor and observed file hashes; assignment replay and restart use the
pinned publication even after a new publication becomes current.


The batch file owns account, partition, QoS, CPU, memory, time, GPUs, and other
model-specific scheduler options. It also owns module loading and runtime setup.
For example:

```sh
#!/bin/bash
#SBATCH --account=research
#SBATCH --partition=compute
#SBATCH --cpus-per-task=1
#SBATCH --mem=256M
#SBATCH --time=00:01:00

set -eu
# Load model modules or activate the runtime here.
exec "$1" local-worker --directory "$2"
```

The batch file also owns cluster selection, including `--clusters`/`-M`,
cluster lists and the scheduler's default routing. Context configuration supplies
only the compute-node `driver.worker_path`; there is no Enumeray cluster allowlist
or forced submission destination. Slurm interprets and authorizes all scheduler
directives. Installation validation checks the file and monitoring access without
interpreting directives or executing the script.

The connector passes the compute-node worker executable as `$1` and the absolute
job directory as `$2`. Run the worker in the foreground so it can supervise the
registered wrapper and produce durable completion evidence; `srun` may launch the
worker when the model needs a job step. Keep the scientific work inside the
tracked allocation and retain foreground worker execution. Detached submissions
need their own tracked lifecycle; Enumeray does not inspect arbitrary script bodies.
The wrapper receives exported setup variables and Slurm's job environment.
Installation environment entries cannot set `HOME`, `PATH`, or
`PYTHONDONTWRITEBYTECODE`, or names beginning with `SLURM_`, `PMI_`, `PMIX_`,
`LD_`, or `DYLD_`, for either driver. Names are case-sensitive; even empty or
identical overrides are rejected with a key-only diagnostic. Approved author
entries take precedence over other inherited values; the runner then sets
`HOME` to the workspace and `PYTHONDONTWRITEBYTECODE` to `1`. Process wrappers
receive a cleared environment with `PATH=/usr/bin:/bin`. Slurm workers preserve
batch/module/container exports, including their `PATH` and loader settings.
Submission exports only the execution account's `PATH`, `HOME`, and optional
`SLURM_CONF`; configure scheduler/runtime tuning in the batch file.

The connector and compute runner recheck installation environment entries at
validation and launch boundaries, including retained prepared jobs. Rejected
unstarted work retains normal startup-failure evidence and cannot relaunch.
Historical execution records remain readable for observation, cancellation,
logs and result recovery; this policy does not rewrite running or completed work.

Submission uses `sbatch --parsable` with the registered batch file. The driver
sets only the unique recovery name, scheduler log paths and working directory
(the job directory). These managed lifecycle locations override corresponding
batch directives. Scheduler routing, resources and runtime setup come from the
file. Before submission, a durable record pins the launch UUID, execution UID,
script digest and preparation time. The receipt and normalized `(cluster, job ID)`
handle are retained separately. Slurm's returned cluster is authoritative; a
cluster-less receipt requires scheduler evidence before a handle is saved.

Lost submission acknowledgments are reconciled by exact launch name and execution
UID across the clusters exposed by Slurm (`squeue`/`sacct --clusters=all`). Both
complete queries must succeed, and their rows carry actual cluster names. Queue
and accounting evidence are combined by `(cluster, job ID)`; only a unique,
consistent identity is saved. No cluster is inferred from script syntax or an
Enumeray default. Accounting searches from the original preparation time with
five minutes of clock-skew allowance, so long outages do not move old jobs out of
the search window. Recovery has a shared 20-second query deadline. Missing,
ambiguous, contradictory or unavailable evidence stays unresolved and never
permits resubmission. Observation queries the execution UID's queue and matches
the retained job ID, so publication can recover after the controller has purged
the terminal job. Complete bounded queue responses and matching terminal
accounting are still required. Observation and cancellation target the same
persisted cluster-specific handle. Scheduler commands clear inherited
routing/format overrides so the registered file and saved job evidence remain
authoritative. Preserve the journal and resolve missing evidence operationally.

Revocation uses `scancel --ctld --quiet --state=PENDING JOB_ID` and then observes
scheduler evidence. A job that started before the cancellation wins the race and
is allowed to finish. A successful cancellation command alone does not prove
execution stopped. Scheduler accounting must be available; validation probes it.
Job arrays, federation and cross-host failover are outside this adapter.

Accepted Slurm allocations can finish with a failure report even when their worker
never starts or stops without writing completion. The runner verifies the pinned
launch name, execution UID and original accounting window against the persisted
cluster/job handle. It retains allocation and step states, exit status/signals and
start/end times. A terminal allocation, confirmed end times for every returned
step, an empty final queue observation and released worker ownership are required.
Missing accounting, active steps, requeue/cleanup states, conflicting duplicate
records, invalid local evidence and unavailable scheduler queries remain unresolved.
The foreground-worker/allocation contract above remains required.

Allocation failures use `slurm_failed`, `slurm_timeout`, `slurm_out_of_memory`,
`slurm_node_fail`, `slurm_boot_fail`, `slurm_deadline` or `slurm_preempted`.
External cancellation uses `slurm_cancelled`; only retained user intent can produce
`cancelled` with `user_cancelled`. Pending revocation intent is retained before
`scancel`; `access_revoked` requires evidence that neither the allocation nor its
worker started. A running job that wins the pending cancellation race still drains.
A user cancellation request does not relabel a different scheduler failure.

Verified retained wrapper completion takes precedence, including completion
published during scheduler observation. Scheduler `COMPLETED` without that record
or independently verified startup-failure proof remains unresolved, even when a step failed: it cannot establish scientific success
or prove that the batch did not handle the step failure. Scheduler-derived reports
contain no scientific outputs and retain available wrapper captures (empty captures
are valid before startup). Their history preserves any observed execution event
and appends a diagnostic, without inventing execution or result-collection phases.
The immutable completion and connector journal retain structured scheduler evidence,
so retry/restart preserves the original result, observation time and launch identity.
The automatic `slurm_batch` source reads the exact managed `slurm.log` path,
including failures before worker startup. Final collection follows allocation
release, so diagnostics written after the worker exits are included.

Preparation and submission retain typed `accepted`, `proven_failure` or `uncertain`
outcomes before acknowledging a known failure. Proven failures preserve the launch
UUID, assigned attempt, driver, pinned installation, original observation time and
bounded command diagnostics in immutable private records. A permanent start-attempt
record closes staging before validation; a failed first start cannot reopen inputs
or submit again. Missing or malformed replies, timeouts, signals, truncated capture
and unknown nonzero exits remain uncertain. The bundled Slurm parser recognizes
only complete, uncontradicted `sbatch` rejection diagnostics for invalid partition,
account/partition combination or QoS. Other rejection messages require reconciliation.

Workers retain validation, workspace preparation and guard-spawn failures before
scientific execution. Worker ownership and a permanent worker-attempt record fence
replay; a separate wrapper-start record distinguishes preparation from execution.
An accepted Slurm startup failure still needs the stopped allocation/step and worker
ownership evidence described above, even if accounting says `COMPLETED`. A vanished
worker without proof remains unresolved. These reports use `launch_preparation_failed`,
`launch_spawn_failed`, `launch_rejected`, `worker_validation_failed`,
`worker_preparation_failed` or `worker_spawn_failed`, with empty scientific outputs.
They use the ordinary result receipt and provenance path and append one diagnostic
without fabricating execution or collection events. Available wrapper captures are
retained; absent captures are allowed only with validated startup proof. Command
stdout/stderr diagnostics retain at most 64 KiB each, with completeness and dropped
byte counts, in private evidence rather than new public log sources.

The compute protocol sends `request-cancel` with `{"scope":"pending"}` separately
from `status`. Cancellation returns `accepted`, `unsupported`, `already_terminal`,
or `unresolved` under `cancellation.outcome`; only subsequent status observation
can return a completion report. Requests use the persisted job ID and cluster and
can safely repeat after a lost reply. Drivers declare internal `cancel_pending`
and `cancel_running` capabilities. Validation reports readiness, installed file
digests and the execution identity.
The optional private `scheduler_evidence` and `startup_evidence` fields are carried by completion and journal
execution records. Legacy records remain readable without rewriting them. Deploy
matching connector and compute-runner binaries together; private decoders are strict.
See the [extension guide](EXTENDING.md) for complete version-1 protocol examples.

This changes the development configuration contract. Slurm context manifests
use only `kind` and `worker_path` in `driver`. The former `allowed_clusters` and
`default_cluster` controls are removed; cluster selection belongs to the model
batch file. Move resource and account settings into each model's batch file and
register a publication with `slurm_script`. Drain accepted work with the previous
binary before upgrading, and preserve its journal as evidence.
Resource profiles and presets are also removed from publication requirements, contexts,
publication descriptors, admission, accepted runs and assignment receipts. Remove
these fields from external manifests; wrappers and batch files own resource requests.
As described in the repository's development setup, recreate disposable databases
and connector configuration after incompatible changes: stored contexts are
immutable, and old contexts and installation caches use the previous contract.

## Wrapper protocol 1 and files

Protocol 1 supports JSON and declared files through the
`--request request.json --result result.json` process interface. Its request has
identity/input fields, `wrapper_protocol_version: 1`, and an `input_files` map
(empty for JSON-only models):

```json
{"dataset":{"artifact_id":"ARTIFACT_UUID","path":"inputs/ARTIFACT_UUID"}}
```

Paths are relative to the isolated working directory. Original `inputs` retain
artifact references for provenance. The wrapper receives no storage URLs or
credentials. The connector verifies the pinned storage version, size and SHA-256
before staging, and verifies remote staging before launching.

Input staging and finalized output/archive collection retain verified prefixes
across interruption. Direct transfers reconcile runner offsets; SSH/SFTP compares
bounded prefix digests before appending. Conflicting sources or partials remain
blocked for inspection. Storage downloads require the pinned object version and
valid byte ranges, and publication rechecks the full file checksum. New object
uploads above 64 MiB retain immutable part receipts, including log archives.

`inspect` exposes `transfers.payload` with resumed, transferred, verified and total
payload bytes separately from processed-I/O counters, which include integrity
reads. Connection/stall/overall budgets default to 15/60/172800 seconds and can be
shortened through `ENUMERAY_TRANSFER_CONNECT_SECONDS`,
`ENUMERAY_TRANSFER_STALL_SECONDS` and `ENUMERAY_TRANSFER_OVERALL_SECONDS`.
Deploy matching API, client, connector and runner binaries together; version-1
records and existing reservations retain their original identities and evidence.

A protocol 1 result includes the identity, outcome, outputs and failure fields,
and a `files` map (empty for JSON-only models). `outputs` contains ordinary JSON outputs. File outputs are
named only in `files`; their names must be absent from `outputs`:

```json
{"result":{"path":"answer.csv","media_type":"text/csv","scientific_metadata":{}}}
```

Only schema-declared files and media types are accepted. Paths must be relative,
without parent traversal or symbolic links. The supervisor hashes completed
files and checks their declared size bounds. Wrapper file declarations accept only
`path`, `media_type`, and optional `scientific_metadata`; caller-supplied
`size_bytes` and `sha256` are rejected. Measured sizes and checksums are retained
in the connector journal for verified transfer and publication. The connector collects them, creates
assignment-scoped upload reservations, retains returned storage version IDs, and
verifies publication before replacing file slots with `{artifact_id: ...}` in the
terminal report. Interrupted delivery resumes from the same completed execution.
Failed wrapper results use empty outputs and files. Optional absent files are
omitted; protocol 1 does not currently expose wrapper-supplied optional-output
failure annotations.

Local and remote logs stream through the typed `read-logs` runner operation.
Status responses carry execution evidence; they do not include log payloads.
Retrieval runs asynchronously during service observation and cancellation.

## Rotating logs and optional archives

`run` and `execute-once` accept `--log-policy-file PATH`, a bounded JSON object
such as [the default policy](../../examples/log-policy.json). The connector reads
this policy at startup and persists the effective policy before each launch.
Restarting with another policy affects new launches; recovery uses each launch's
retained policy.

Without named declarations, each source retains at most sixteen 64 KiB segments
by default, with one additional
segment allowed while rotating. Data and the source/epoch/range manifest reach
stable storage before older segments are removed. Capture lives outside scientific
scratch accounting and continues draining wrapper pipes after retention fills.
Enumeray bounds its reads and retained copies of managed `slurm.log` and never
truncates an active file. Completed-run cleanup reclaims run-owned logs after
confirmed allocation release and final delivery. Cluster operators manage
upstream/site-owned Slurm storage separately.

Delivery uses at most 2 KiB of raw source bytes per batch. `offset` and
`byte_length` describe raw positions, including binary bytes replaced for text
viewing. The API rejects unexplained discontinuities. `retention_discarded` gaps
carry exact omitted counts; replacement/truncation starts the next epoch with a
`source_reset` gap, and unknown loss uses a null byte count. A journal retains one
exact pending request per source until acknowledgement, even if rotation removes
its segment. Each service pass processes at most eight batches per assignment,
rotating between stdout, stderr, automatic Slurm batch diagnostics and declared
model sources.
Asynchronous retrieval keeps at most 16 KiB of raw read-ahead per source, with one
runner operation per assignment; buffers are discarded when final delivery closes.
API payload history remains bounded to 1,024 entries and seven days; the existing
64-receipt verification window and cursor recovery rules remain in force.

Archives are disabled by default. Set `archive_enabled` to `true` to preserve raw
prefixes independently of live delivery. Without named declarations, each source
gets 16 MiB by default:
15 MiB for its prefix and up to 1 MiB for its recent tail. Finalization removes
overlap and creates one `.tar.gz` with source fragments and `manifest.json`.
The manifest lists byte ranges, measured omissions, unknown loss and completeness.
Exhausting content retention produces a valid partial archive. Policy validation
reserves room for all three sources, tar/manifest metadata and compression framing,
including incompressible bytes, under the default 50 MiB compressed ceiling.
Each retained raw spool and each compressed transfer copy has its own bound;
interrupted finalization does not keep two compressed copies on the same host.

Publication uses the supervised transfer pool after required input/output work.
A scientific result releases compute capacity independently of archive publication;
failed and cancelled runs can also have archives. Terminal assignments with pending
publication remain in the bounded journal. Restart resumes their attachment work
without executing or replaying scientific results. One-off workflows publish later:

```sh
enumeray connector publish-log-archives --journal JOURNAL --token-file TOKEN
enumeray run log-archive RUN_UUID
enumeray artifact download ARCHIVE_UUID --output logs.tar.gz
```

The ordinary recovery assertion applies when a prior session remains active.

Publication status (`capturing`, `pending`, `ready`, `unavailable`, `expired`) is
separate from manifest completeness. Missing storage or archive errors cannot
change the scientific outcome. Publication defaults to a 24-hour deadline after
capture finalization. The API independently caps uploads at 512 MiB and published
retention at 30 days. Reads enforce expiry before maintenance runs. Existing input
pins preserve stored versions needed by accepted consumers; expired attachments
cannot acquire new pins.

Operators run bounded, repeatable retirement separately from scientific artifacts:

Contact Enumeray support to arrange service-side archive retention and cleanup.

Cleanup addresses only the archive's reserved object key and explicit stored
version IDs, respects dependency pins, and retries unavailable storage. Verified
publication or resolved publication expiry permits the connector to reclaim its
archive prefixes and temporary copies. Completed-run cleanup subsequently reclaims
eligible workspace/log payloads while preserving historical and replay evidence.

### Model-declared log files

An immutable scientific definition can declare optional diagnostics independently
of its scientific output schema:

```json
{
  "log_files": {
    "solver": {"path": "logs/solver.log"},
    "convergence": {"path": "logs/convergence.log"}
  }
}
```

Declare at most eight unique paths. Source names match
`[a-z][a-z0-9_-]{0,63}` and appear as `model.solver`, for example, in log history,
SSE, CLI follow output and archive manifests. Paths contain at most 1024 UTF-8
bytes, use exact workspace-relative components, and cannot contain traversal,
globs, backslashes or control characters. Runner-reserved roots (`.enumeray`,
other dot-prefixed roots, `inputs`, `outputs`, `request.json`, `result.json`,
`stdout.log` and `stderr.log`) are unavailable. Symlinks and special files are
never read. No per-rank patterns or paths outside the run workspace are supported.

Files must be accessible to the runner on its execution host for both direct and
SSH access. Slurm wrappers must copy node-local diagnostics into the run workspace
before the allocation finishes. Missing files are checked again during execution
and reported unavailable if still absent at finalization; they never make a
scientific result fail. Capture never truncates or modifies model files. Existing
infrastructure workspace/scratch limits still apply to files the model creates.

Capture polls independently of API delivery. Observed truncation or replacement
starts a new source epoch; gaps identify discarded bytes or unknown loss. Polling
cannot detect every rewrite between observations. Ordering is per source, with
no promise of exact chronology across sources. Existing API cursors and CLI
checkpoints resume named sources without a separate command.

Declared sources share the existing total retention allowance equally with
stdout/stderr and, on Slurm, `slurm_batch`. Each source receives the configured
segment and archive byte allowances multiplied by the original built-in source
count (two for process, three for Slurm), divided by the new source count and
rounded down. Segment count and compressed archive limits stay unchanged; adding
sources reduces each source's retained bytes rather than increasing total limits.
When archives are enabled, they contain retained raw prefix/tail bytes for named
sources too. Rotation or capture loss produces an incomplete archive manifest.



## Verification

Collections use explicit `collection_inputs` and `collection_outputs`
capabilities. Members use the existing version-pinned artifact transfer workers,
including multipart uploads and verified range recovery. A transfer task handles
one collection sequentially and retains manifests in private sidecars. Journal
entries and completion reports contain only compact references and digests.

Each collection copy has one durable spool reservation covering retained partials
and future writes. Restart recovers that reservation before accepting new bytes.
Delivery repair retains the original launch identity; it never launches the model
again. Keep the journal, spool, runner evidence and credentials available until
the existing delivery and cleanup fences settle.

The service operator separately qualifies authenticated collection publication,
object-storage recovery and representative collection sizes. Public connector
unit tests preserve compact manifests, reservation recovery and exact member
transfer behavior; a fixture limit is not a scientific workload guarantee.

Unit tests cover exact JSON numbers, journal recovery, immutable starts, wrapper
validation, file confinement/digests and scheduler receipt parsing. The
`connector_runner` integration test launches the real packaged runner and an external
model without API credentials, then verifies replay refusal and recovery.
Process supervision tests need process-list access; restricted sandboxes can
block `/bin/ps`.

The [real backend acceptance environment](tests/real-backends/README.md) executes
local models under a separate OS user, SSH/SFTP and real Slurm with accounting.
It checks scalar/file execution, interrupted transfers, replay refusal, recovery
and pending cancellation. Production cluster qualification should additionally
exercise the site's actual shared filesystem and resource isolation settings.

Complete authenticated service, storage and restoration checks are maintained
by the service operator against the matching client revision and artifacts.
