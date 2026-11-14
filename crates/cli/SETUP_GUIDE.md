# Guided connector setup

Use `enumeray connector setup` to connect your compute to Enumeray at
`https://api.enumeray.com`. The wizard saves your choices, shows what the
host administrator needs to prepare and finishes when the execution context
is ready.

## Before you start

- [Install the CLI](INSTALL.md) on the connector host. Run the wizard as
  your regular OS user.
- [Sign in to Enumeray](https://doc.enumeray.com/start/login/) as the account
  that will own a new connector. To adopt an existing connector, sign in
  as its owner or a co-admin and review its saved settings.
- For shared compute, arrange for a host administrator to prepare separate
  connector and execution accounts. On your own machine, development mode
  runs trusted models under your account.

## Choose a mode and save your settings

Run the wizard from a directory where you can keep its files. If you omit
`--directory`, setup saves its files in `./enumeray-setup`.

```sh
enumeray connector setup --directory enumeray-setup
```

| Mode | When to choose it | After approving the setup plan |
| --- | --- | --- |
| **Development** | Trusted local experiments where the connector and model can run as your current OS user. | Acknowledge same-user access. The wizard prepares the directories, enrolls the connector and checks local prerequisites. Continue to [startup](#start-and-establish-readiness). |
| **Managed** | A site connector with a dedicated connector account and a separate model execution account. | Follow the [administrator step](#apply-managed-host-changes-explicitly) before startup, including foreground startup. |

You can select a mode in the wizard or pass `--mode development` or
`--mode managed` on the first run. Review the proposed accounts, directories,
executable, execution location, data access disclosure and startup choice in
the saved plan before approval.

After approval, setup registers the connector and context. Once the context
is ready, the connector owner decides who can publish there and grants them
access separately.

Keep the setup directory. It holds your account, the host plan and
progress. Login credentials stay in your profile store. Connector
credentials and the journal use separate host paths. Restore a backup to the
original directory and use that path to continue after an interruption.

```sh
enumeray connector setup --directory enumeray-setup
enumeray connector setup status --directory enumeray-setup
```

Status reads saved progress and authenticated readiness without initiating checks
or repairing a host. Offline or unauthorized observations remain unknown.
If your login expires, sign back in to the original account and resume the
saved directory. Changing the mode or host settings requires a new
reviewed plan. Keep existing journals, credentials and release evidence.

## Development setup under your current user

1. Choose **development**. The wizard selects the local process driver and direct
   connection. Choose a user-owned directory for connector state, model
   installations and execution work.
2. Review the plan and explicitly acknowledge that model code can read your files
   and connector credentials. Use models you trust with that access.
3. Sign in if prompted. The wizard prepares the host under your account, enrolls
   the connector and runs local preflight. **Skip the managed host changes section.
   Development needs neither `sudo` nor `connector setup apply-host`.** Use the displayed
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

The wizard uses one assignment limit for the connector, the context and
the local worker count. Use a scripted setup input if your site needs
different limits for these three settings.

## Apply managed host changes explicitly

After the owner approves the managed plan, the wizard registers the connector
and displays the host plan, enrollment document path and
administrator host plan digest. Review the host specification before
running the helper. For direct execution, the helper prepares the connector and
execution sides locally. For SSH, prepare the execution host first. If the
connector account already exists, provision its SSH access before applying the
connector plan. If the helper creates the account, provision its SSH access
after the first connector-side apply and repeat that apply to check local
prerequisites again.

On the connector host, use the actual paths and digest from the printed
**Apply plan on the connector host** command.

```sh
sudo /path/to/installed/enumeray connector setup apply-host \
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

For SSH, transfer the host plan to the execution host through the approved
site route. Use the copied plan path, the same host plan digest and that
host's matching executable. Run this command **locally on the execution host**.

```sh
sudo /path/to/installed/enumeray connector setup apply-host \
  --plan /absolute/copied-host-plan.json --side execution \
  --approve HOST_PLAN_SHA256 --executable-source /local/path/to/enumeray
```

Keep the enrollment document, connector bearer credentials and private SSH
keys on the connector side.
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
resume in the original terminal after the start command returns.

```sh
enumeray connector setup --directory enumeray-setup
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
is blocked, setup shows the next action and who can take it.

## Give an author access to the ready context

After setup reports **Ready: yes**, use your connector owner or co-admin
account. Ask the author for their account UUID from `enumeray auth whoami`.
Use the connector and context IDs from your setup plan, and choose limits
appropriate to the compute you are sharing. Use your own account UUID if
you also plan to publish a model.

```sh
enumeray connector grant create CONNECTOR_ID --context CONTEXT_ID --user AUTHOR_UUID \
  --max-queued-runs 100 --max-outstanding-assignments 4
enumeray connector grant list CONNECTOR_ID
```

Check that the list includes the author's UUID, intended context and limits.
Give the author the context ID, approved installation root, execution
account, runtime requirements and instructions for installing files.
Arrange host installation access as well as the compute grant.

The author runs `enumeray connector grant available`. The handoff is
complete when the command shows the intended context and the author knows
where and how to install the release. The author can then use
[`enumeray model setup`](AUTHOR_GUIDE.md#use-guided-model-setup) to prepare
the model, run a candidate trial and review it for activation.
For day-to-day service tasks, see
[service operation](../connector/README.md#enrollment-and-service-operation).

## Script the same saved workflow

Copy the [development](examples/host-setup-development.json),
[managed Linux](examples/host-setup-linux.json) or
[managed macOS](examples/host-setup-macos.json) host specification to
`host-setup.json` and edit the accounts, IDs and absolute paths. Use
`https://api.enumeray.com` for the hosted service. The host specification
describes the host and includes the execution context. With
`connector configuration generate`, it can also produce a standalone
`context.json` and a host plan for a manual setup.

For the noninteractive wizard, embed the whole host specification under
`host` in a version-1 setup input. The outer `connector` object sets the
connector ID, display label and maximum outstanding assignments. Its ID
must match `host.context.connector_id`. Set `startup` to `foreground`
and leave `host.install_service` false to run in a terminal. Set
`startup` to `background` and `host.install_service` to true to use a
service manager.

This example uses `jq` to assemble a complete foreground input from your
edited host specification. The connector limit of four is an example. Set it
to the number of outstanding assignments the connector should accept across
its contexts.

```sh
jq '{
  manifest_version: 1,
  host: (. + {install_service: false}),
  connector: {
    id: .context.connector_id,
    label: "Research lab connector",
    max_outstanding_assignments: 4
  },
  startup: "foreground"
}' host-setup.json > setup-input.json
enumeray connector setup --file setup-input.json \
  --directory setup-operation --non-interactive
```

Managed executable copying requires `executable_source` and
`executable_sha256` for the same reviewed executable. A managed account
can set `create: true` to authorize creation if it is absent. To create
an account on macOS, supply reviewed unused numeric `uid` and `gid` values.

The first command that starts setup reports `review_required`. Review
both plans and replace `SETUP_PLAN_SHA256` with the displayed setup digest.
Use the separate host digest in the administrator's printed command.
In development mode, add `--allow-development-execution` to the approval
command after reviewing the same-user access disclosure.

```sh
enumeray connector setup --directory setup-operation \
  --non-interactive --approve SETUP_PLAN_SHA256
```

Follow the printed instructions for host preparation and startup, then
resume `enumeray connector setup --directory setup-operation --non-interactive`
to check readiness.

Noninteractive setup requires an existing Enumeray login. Existing records
need explicit `--adopt-existing`; conflicting settings are never overwritten.
A server-assigned enrollment secret cannot be recovered by repeating connector
creation. Preserve any saved document and identity directory. If none is usable,
explicitly request `--replace-enrollment`. An uncertain replacement response is
never repeated automatically: inspect it, deliberately issue another document
with `connector enrollment`, then import it using
`connector setup --enrollment-file NEW`.

Exit 0 from setup means the context is ready. Exit 1 means review, a
required host action or an error needs attention. A local wait deadline
returns 5 and interruption returns 130. Resume with the same setup directory.

## Recover files from an earlier setup

If the CLI reports an older setup directory, preserve it and begin connector
setup in a new directory. Use `--adopt-existing` if the connector and context
already match your new plan. Keep the original connector journal, enrollment
identity and credentials. A saved `first-release` or `first-trial` from the
earlier wizard can still be used directly with `model release` or `analysis`.
Inspect its status before deciding how to resume or review it.
