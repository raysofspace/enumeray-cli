# Guided connector setup

Use `enumeray setup` to prepare a connector, save its configuration, check host
readiness and trial a first model. Install the matching executable using the
[installation guide](INSTALL.md). Package availability and qualification remain
separate from source development; use the matching source build while a qualified
package is unavailable.

## Choose a mode and save the journey

Run setup as your regular OS user. You need your service's API origin and an
account that can register a connector and model. The wizard guides profile
selection and browser login using the existing authentication mechanisms.

```sh
enumeray setup --directory enumeray-setup
```

Choose **development** for trusted process/direct experiments under your existing
OS account. The wizard proposes user-owned model, work and connector directories.
Explicitly acknowledge that model code can access this user's files and connector
credentials. Development does not grant isolation between that user's processes.

Choose **managed** for a dedicated connector account and a separate execution
identity. Same-host setup suggests your existing login account for model execution.
You may select another existing account or supply an explicitly reviewed account
creation plan. Managed setup supports process/Slurm over direct/SSH on Linux and
macOS. Slurm allocation, compute-node visibility, modules and scientific runtimes
remain site responsibilities. For SSH, supply the execution account, a private
credential reference and a trusted host key through your site's approved route.

Review the proposed accounts, directories, executable, destination and startup
choice before approval. Foreground is the suggested startup choice. A foreground
plan does not install a background service. A background plan explicitly includes
systemd or launchd configuration.

The private setup directory retains the original profile/API binding, operation
identities, host plan and progress. User login credentials stay in the private
profile store; connector credentials and journals have their own host paths.
Restore setup backups at their original path; copied setup checkpoints are refused
so one parent operation cannot fork fresh candidate/trial identities.
Repeat the same directory after interruption:

```sh
enumeray setup --directory enumeray-setup
enumeray setup status --directory enumeray-setup
```

Status reads saved progress and authenticated readiness without initiating checks
or repairing a host. Offline or unauthorized observations remain unknown. Restore
an expired login for the same API/account and resume the original directory.
Changing mode, origin or frozen settings requires a new reviewed plan; preserve
existing journals, credentials and release evidence.

## Apply managed host changes explicitly

The wizard prints complete commands with the installed executable, saved paths
and host approval digest. Review the host specification before running the
administrator helper. It copies the approved executable into the protected site
path used by the connector and runner. A model user's writable Homebrew prefix
is unsuitable for that runner boundary; follow the installation and connector
guides for protected installation details.

The administrator step has this form, using the actual paths and digest printed
by setup:

```sh
sudo /path/to/installed/enumeray setup apply-host \
  --plan /absolute/setup/host-plan.json --side connector \
  --approve HOST_PLAN_SHA256 --enrollment-file /absolute/setup/enrollment-0000.json
```

Apply validates typed operations and the exact approved digest. It checks existing
identities, creates only approved missing accounts/directories, installs the
reviewed executable and restricted runner rule, and installs service configuration
only when background operation was selected. Enrollment runs as the connector
account without loading the owner's profile. Local prerequisite results are
returned separately; preparation alone does not establish readiness.

A repeat verifies and reuses completed work. Conflicting identities, permissions
or files are preserved and reported for administrator correction. No account,
journal or credential is removed to make a retry succeed. macOS account creation
requires explicitly reviewed unused numeric IDs.

For an SSH execution host, run the same helper **locally on that host** with
`--side execution`. Transfer the non-secret plan through the approved site route.
`--executable-source /local/path/to/enumeray` can select that host's copy of the
executable only when its bytes match the plan's approved SHA-256. Never transfer
connector bearer credentials or private SSH keys to the model execution account.
The site's SSH access and verified `known_hosts` entry remain explicit.

SSH hosts with another OS or CPU can use a different native artifact. Review its
`execution_executable_sha256` in the host specification and pass that artifact's
local path with `--executable-source` on the execution host. The wizard asks for
this separate digest; a different host platform requires it. Connector installation
continues to use `executable_sha256`.

## Start and establish readiness

As the configured connector OS account, use the saved runtime file printed by setup:

```sh
enumeray connector start --configuration /absolute/connector/runtime.json --foreground
enumeray connector status --configuration /absolute/connector/runtime.json
```

Foreground start occupies that terminal. Resume setup in another terminal. To
request the reviewed native background service instead, use `--background`;
managed background startup requires an administrator invocation. Systemd startup
starts the installed unit; administrators control boot enablement through site
policy. LaunchDaemons use the explicitly selected background configuration.
Native managed-service lifecycle qualification remains a separate activity.

The runtime file contains credential **paths**, not credential values. `--configuration`
is distinct from the global `--config` profile-file option. Host start/status do
not load owner profiles. Foreground start retains normal signal handling and the
same journal. An interrupted manual-recovery session still requires external
proof that its previous process stopped and an explicit
`--confirm-prior-process-stopped`; stale contact is insufficient.

Setup runs publication-independent prerequisites and displays **configured**,
**connected** and **ready** separately, with corrections and responsible roles.
A local host report cannot replace authenticated connector evidence, scientific
validation or compute authorization.

## Trial a starter or existing project

Choose the existing Python starter or an author project containing
`publication.json`. Python is needed only for models that use it; setup does not
install scientific runtimes, dependencies or datasets. For Slurm, prepare the
site-specific batch script before freezing the release.
The wizard reads an existing project's model ID and examples, offers an already
installed release, and accepts local file and collection bindings. Saved input
paths must be absolute so resuming from another working directory selects the
same inputs.
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

The wizard saves one candidate release and one analysis directory, waits for
validation, and requests explicit trial submission after review of the saved
inputs, execution destination and disclosure. Repeating setup reconciles the same
candidate and trial, including uncertain submission replies. Collected evidence
remains in `first-trial`; the release operation is `first-release` inside the setup
directory. A successful trial does not activate a publication.

Review the retained evidence and perform activation separately when appropriate:

```sh
enumeray model release review /absolute/setup/first-release
enumeray model release activate /absolute/setup/first-release
```

Use the original `--profile` and `--config` when applicable. Existing project
scientific tests and interpretation remain the author's responsibility. Installation
or access requiring another person remains a site handoff; setup reports the
required action without granting that person's authority.

## Script the same saved workflow

Provide a version-1 JSON input with `host` (the existing `HostSetupSpec`), `connector`
(`id`, `label`, `max_outstanding_assignments`), `startup` (`foreground` or `background`)
and optional `first_model`. Start from the existing
[development](examples/host-setup-development.json),
[Linux](examples/host-setup-linux.json) or [macOS](examples/host-setup-macos.json)
host specification and replace all example accounts and absolute paths.

For background operation set `host.install_service` to `true`; otherwise leave it
`false`. Managed executable copying needs `executable_source` and
`executable_sha256`, obtained from the matching reviewed installation. These are
saved automatically by the interactive wizard.

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
