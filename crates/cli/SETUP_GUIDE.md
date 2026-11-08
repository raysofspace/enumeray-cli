# Guided connector setup

Use `enumeray setup` to connect a machine to Enumeray so it can run models.
The wizard saves what you choose, shows what the host administrator needs to
prepare and checks readiness after the connector starts. You can stop when
the connector is ready or test it by running one model.

Decide who will run the code. On a personal machine, development mode runs
trusted code under your account. On shared or managed compute, keep the
connector and model execution in separate accounts. Install the executable
for your service and host before starting. [Installation guide](INSTALL.md)

## Choose a mode and save the journey

Run the wizard as your regular OS user. Sign in with an Enumeray account
allowed to register a connector. The default service is `https://api.enumeray.com`.
To use a saved profile, pass `--profile NAME`. If you plan to trial a model,
you also need model author access.

```sh
enumeray setup --directory enumeray-setup
```

| Mode | When to choose it | After approving the setup plan |
| --- | --- | --- |
| **Development** | Trusted local experiments where the connector and model can run as your current OS user. | Acknowledge same-user access. The wizard prepares the directories, enrolls the connector and checks local prerequisites. Continue to [startup](#start-and-establish-readiness). |
| **Managed** | A site connector with a dedicated connector account and a separate model execution account. | Follow the [administrator step](#apply-managed-host-changes-explicitly) before startup, including foreground startup. |

You can select a mode in the wizard or pass `--mode development` or
`--mode managed` on the first run. Review the proposed accounts, directories,
executable, execution location, data access disclosure, startup and first-model
choices in the saved plan before approval.

Keep the setup directory. It holds your selected account and API, the host
plan and progress. Login credentials stay in your profile store. Connector
credentials and the journal use separate host paths. Restore a backup to the
original directory and use that path to continue after an interruption.

```sh
enumeray setup --directory enumeray-setup
enumeray setup status --directory enumeray-setup
```

Status reads saved progress and authenticated readiness without initiating checks
or repairing a host. Offline or unauthorized observations remain unknown. Restore
an expired login for the same API/account and resume the original directory.
Changing mode, origin or frozen settings requires a new reviewed plan; preserve
existing journals, credentials and release evidence.

## Development setup under your current user

1. Choose **development**. The wizard selects the local process driver and direct
   connection. Choose a user-owned directory for connector state, model
   installations and execution work.
2. Review the plan and explicitly acknowledge that model code can read your files
   and connector credentials. Use models you trust with that access.
3. Sign in if prompted. The wizard prepares the host under your account, enrolls
   the connector and runs local preflight. **Skip the managed host changes section.
   Development needs neither `sudo` nor `setup apply-host`.** Use the displayed
   [start command](#start-and-establish-readiness) as your regular user.

## Managed setup with separate accounts

Choose a dedicated connector account to hold credentials and a separate
account to run models. The host administrator can create the connector
account as part of the approved plan. For same-host execution the wizard
defaults to your current OS account. It can also use another existing
local account or an existing SSH account. If the administrator needs to
create the execution account, use a reviewed
[scripted host specification](#script-the-same-saved-workflow).

Choose an execution driver and connection independently. These choices support
four combinations across Linux and macOS hosts:

| Choice | What it requires |
| --- | --- |
| **Process** driver | Launches the model directly on the execution host. Install the model's runtime and dependencies on that host. |
| **Slurm** driver | Submits model jobs to the site's scheduler. The site supplies an allocation, a prepared batch script, shared paths, the scientific runtime and an Enumeray executable visible to compute nodes. |
| **Direct** connection | Uses a restricted sudo rule to launch as the separate execution account on the connector host. Its administrator prepares both identities in one host step. |
| **SSH** connection | Uses a separate execution host. Its administrator prepares the runner and execution paths. The site provisions access for the connector, a private SSH credential reference and a verified host key. |

The connector account needs its own private state. Use paths valid on the
execution host for model installations and work. For Slurm, choose paths the
allocated compute nodes can reach.

## Apply managed host changes explicitly

After the owner approves the managed plan, the wizard registers the connector
and displays the host plan, enrollment document path and
approval digest for the administrator. Review the host specification before
running the helper. For direct execution, the helper prepares the connector and
execution sides locally. For SSH, prepare the execution host first. If the
connector account already exists, provision its SSH access before applying the
connector plan. If the helper creates the account, provision its SSH access
after the first connector-side apply and repeat that apply to check local
prerequisites again.

On the connector host, use the actual paths and digest printed by setup:

```sh
sudo /path/to/installed/enumeray setup apply-host \
  --plan /absolute/setup/host-plan.json --side connector \
  --approve HOST_PLAN_SHA256 --enrollment-file /absolute/setup/enrollment-0000.json
```

Apply validates the approved digest, checks existing identities, and creates
approved missing accounts and directories. It installs the reviewed executable in
a protected site path and, for direct execution, installs the restricted runner
rule. Managed background plans also install system service configuration.
Enrollment and local preflight run as the connector account. Check the local
preflight report before starting the connector.

A repeat verifies and reuses completed work. Conflicting identities, permissions
or files are preserved and reported for administrator correction. No account,
journal or credential is removed to make a retry succeed. macOS account creation
requires explicitly reviewed unused numeric IDs.

For an SSH execution host, run the same helper **locally on that host** with
`--side execution`. Transfer the non-secret plan through the approved site route.
`--executable-source /local/path/to/enumeray` can select that host's copy of the
executable only when its bytes match the plan's approved SHA-256. Never transfer
connector bearer credentials or private SSH keys to the model execution account.
Use the `credentials_file` path saved in the host plan for the connector
account's private SSH key map. Provision the matching public key for the remote
account and the verified host key in the connector account's `known_hosts`.
See the [SSH credential layout](../connector/README.md#enrollment-and-service-operation)
for which files belong on each host.

SSH hosts with another OS or CPU can use a different native artifact. Review its
`execution_executable_sha256` in the host specification and pass that artifact's
local path with `--executable-source` on the execution host. The wizard asks for
this separate digest; a different host platform requires it. Connector installation
continues to use `executable_sha256`.

## Start and establish readiness

Choose **foreground** for a connector attached to a terminal, or **background**
for the native service manager. Run the start command printed by setup:

| Startup | Development | Managed |
| --- | --- | --- |
| **Foreground** (default) | Run as your current user. | Run as the dedicated connector account after applying the host plan. |
| **Background** | Run as your logged-in user. Starts a user-level systemd service on Linux or a LaunchAgent on macOS. | An administrator starts the systemd service or LaunchDaemon installed by the reviewed background host plan. On Linux, site policy controls boot enablement. |

For development foreground startup, run the printed command in its own
terminal. Use the actual runtime file path from your setup plan.

```sh
enumeray connector start --configuration /absolute/connector/runtime.json --foreground
```

Leave that terminal running. In a second terminal on the connector host, run
the host status check as the account running the connector.

```sh
enumeray connector status --configuration /absolute/connector/runtime.json
```

In the second terminal, return to the directory where you started setup and
resume the same operation as the setup user. With background startup, you can
resume in the original terminal after the start command returns. Use the same
profile and configuration file as before, if applicable.

```sh
enumeray setup --directory enumeray-setup
```

The runtime file contains credential **paths**, not credential values. `--configuration`
is distinct from the global `--config` profile-file option. Host start/status do
not load owner profiles. Foreground start retains normal signal handling and the
same journal. An interrupted manual-recovery session still requires external
proof that its previous process stopped and an explicit
`--confirm-prior-process-stopped`; stale contact is insufficient.

Read all three checks in setup. **Configured** means Enumeray has a context
and enrollment. **Connected** means the connector has contacted the service
with its current credential. **Ready** means the service also has the
configuration acknowledgement and passing host checks it needs. If a check
is blocked, setup shows the next action and who can take it. A first-model
trial checks the actual installation and results separately.

## Trial a starter or existing project

Choose **demo** to generate a small Python starter and trial it on your connector,
**project** to use an author project containing `publication.json`, or **later**
to finish at connector readiness and prepare a model separately.

If you chose **later**, setup is complete when it reports **ready**.
Continue to the [author handoff](#give-an-author-access-to-the-ready-context)
when you want an author to publish on this connector.

For a project, the wizard reads its model ID and named input examples. Choose a
declared example or provide your own scientific inputs JSON file. The wizard
also accepts local file and collection bindings and can select an already
installed release. Saved input and installation paths must be absolute.

Install Python for the demo and prepare the runtime, dependencies and data needed
by your own project. For Slurm, prepare the site-specific batch script before
freezing the release.
For a generated Slurm starter, setup pauses for this work; resume with
`--confirm-model-preparation` after reviewing the site's allocation and runtime setup.

Setup discovers installation roots from the selected context and compute grant.
It stages a prepared local project into a new release directory where permitted,
or retains the approved destination while you complete an existing site/SSH
installation route. An existing installation can be selected in the wizard or
scripted input.
Original projects and earlier installed releases are preserved. Automatic staging
is bounded and refuses symbolic links; use the existing site installation route
for projects requiring special layout or provisioning.

The wizard saves the candidate in `first-release` and its run in `first-trial`
inside the setup directory. After validation, review the inputs, execution
location and who can access the data. Submit only when you are satisfied.
Resume the same setup directory after an interruption or lost reply to
continue that candidate and trial. Look in `first-trial` for the collected
results.

If you want to make the candidate current, first check its scientific result
and save the proposed release comparison.

```sh
enumeray model release review /absolute/setup/first-release
```

Read the review before deciding to activate.

```sh
enumeray model release activate /absolute/setup/first-release
```

Use the original `--profile` and `--config` when applicable. Existing project
scientific tests and interpretation remain the author's responsibility. Installation
or access requiring another person remains a site handoff; setup reports the
required action without granting that person's authority.

If you chose **demo** or **project**, check for **trial_complete** and confirm
that `first-trial` contains the collected result and provenance. The model author
can check the scientific result and
[review and activate the saved release](AUTHOR_GUIDE.md#save-a-release-resume-readiness-review-and-activate).

## Give an author access to the ready context

After setup reports **ready**, use the connector owner or co-admin account
on the same Enumeray service. Ask the author for their account UUID from
`enumeray auth whoami`. Use the connector and context IDs from your setup
plan, and choose limits appropriate to the compute you are sharing.

```sh
enumeray connector grant create CONNECTOR_ID --context CONTEXT_ID --user AUTHOR_UUID \
  --max-queued-runs 100 --max-outstanding-assignments 4
enumeray connector grant list CONNECTOR_ID
```

Check that the list includes the author's UUID, intended context and limits.
Give the author the context ID, approved installation root, execution account,
runtime requirements and the site's instructions for installing files. The
compute grant and host installation access are separate.

The author signs in to the same service and runs
`enumeray connector grant available`. The handoff is complete when that
command shows the intended context and the author knows where and how to install
the release. They can then [install and test it](AUTHOR_GUIDE.md#install-an-immutable-release-and-select-compute).
For day-to-day service tasks, see
[service operation](../connector/README.md#enrollment-and-service-operation).

## Script the same saved workflow

Provide a version-1 JSON input with `host` (the existing `HostSetupSpec`), `connector`
(`id`, `label`, `max_outstanding_assignments`), `startup` (`foreground` or `background`)
and optional `first_model`. Start from the existing
[development host specification](examples/host-setup-development.json),
[Linux host specification](examples/host-setup-linux.json) or
[macOS host specification](examples/host-setup-macos.json). Replace example accounts
and absolute paths. The examples already use the hosted API origin.

For background operation set `host.install_service` to `true`; otherwise leave it
`false`. Managed executable copying needs `executable_source` and
`executable_sha256`, obtained from the matching reviewed installation. These are
saved automatically by the interactive wizard. A managed account can set
`create: true` to authorize creation when absent. For a macOS account creation
plan, supply explicitly reviewed unused `uid` and `gid` values.

`first_model` chooses exactly one of `demo: true` or `project: "/absolute/project"`,
and supplies `model_id` plus `example` or `inputs`. Optional `installed_release`
selects an existing release under an approved root. File/collection models can
supply `files`, `collections`, `collection_manifests`, `media_types` and `metadata`
using the same `SLOT=VALUE` bindings as `analysis init`.

```sh
enumeray setup --file setup-input.json --directory setup-operation --non-interactive
# Review the saved plan and use its displayed digest.
enumeray setup --directory setup-operation --non-interactive --approve SETUP_PLAN_SHA256
# Development additionally needs --allow-development-execution.
# After reviewing the saved analysis, --submit-trial explicitly submits/resumes it.
```

Noninteractive setup requires an authenticated selected profile. Existing records
need explicit `--adopt-existing`; conflicting settings are never overwritten.
A server-assigned enrollment secret cannot be recovered by repeating connector
creation. Preserve any saved document and identity directory. If none is usable,
explicitly request `--replace-enrollment`. An uncertain replacement response is
never repeated automatically: inspect it, deliberately issue another document
with `connector enrollment`, then import it using `setup --enrollment-file NEW`.
New grants use a saved creation UUID so retrying after revocation cannot grant
access again.

Exit 0 means the requested setup/trial stage completed, 1 means review, a required
host action or an error needs attention. Bounded observers retain exit 5 for a
local deadline and 130 for interruption; trial outcome checks retain the existing
analysis exit codes. Preserve the setup directory in every recovery case.
