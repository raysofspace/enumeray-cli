# Install and use Enumeray

One `enumeray` executable provides user commands, connector operation and the
compute runner. Install the same qualified package on each required host.
Users need no API checkout, PostgreSQL or database credentials. Linux archives
require glibc. Select the archive for your operating system and CPU; the package
matrix targets x86-64 and ARM64 on Linux and macOS. This checkout is unreleased:
use a qualified published archive when available, or build from source below.

## Verify and install an archive

Check the repository's [releases](https://github.com/raysofspace/enumeray-cli/releases)
for qualified packages. Draft artifacts do not establish release qualification.
Each archive has a SHA-256 checksum and Sigstore bundle. Set `REPOSITORY` to
`raysofspace/enumeray-cli`, `TAG` to the actual release tag, and the remaining variables
to the downloaded filenames and extracted directory. The qualified-release
workflow signs from `main`; the archive version is identified by its release tag:

```sh
cosign verify-blob \
  --bundle "$CHECKSUM.sigstore.json" \
  --certificate-identity "https://github.com/$REPOSITORY/.github/workflows/release.yml@refs/heads/main" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  "$CHECKSUM"
sha256sum --check "$CHECKSUM"
tar -xzf "$ARCHIVE"
mkdir -p "$HOME/.local/bin"
install -m 755 "$EXTRACTED/enumeray" "$HOME/.local/bin/enumeray"
export PATH="$HOME/.local/bin:$PATH"
enumeray --version
```

On macOS use `shasum -a 256 -c "$CHECKSUM"`. Keep the executable directory in
your shell's `PATH`. Install the same executable on infrastructure hosts.
SSH execution needs OpenSSH on the connector host and the matching Enumeray
executable on the remote execution host. Slurm compute nodes need that same executable.

## Build from source

Use the repository's pinned Rust toolchain. From the repository root:

```sh
cargo build --locked -p enumeray-cli --bin enumeray
install -m 755 target/debug/enumeray "$HOME/.local/bin/enumeray"
```

Create the destination directory and configure `PATH` as above. The build
needs no running service. Authors need Python 3.9+ for supplied starters; scripted
recipes use `jq`, and raw HTTP recipes additionally use `curl`.

## Login and profiles

Obtain the actual API origin from your service administrator:

```sh
enumeray auth configure https://enumeray.example.org
enumeray auth login
enumeray auth whoami
```

Browser login uses PKCE and a temporary loopback callback. `--no-browser` prints
the URL for a browser on the **same host**; a laptop browser cannot complete a
login waiting on an SSH/HPC host's loopback listener. For remote CLI use, create a
scoped personal token from a signed-in account and import it privately:

```sh
enumeray auth token-create analysis-script \
  --scopes models:read,runs:create,runs:read,me:read --output token.json
enumeray --profile analysis auth configure https://enumeray.example.org
jq -er '.token' token.json | enumeray --profile analysis auth login --token-stdin
enumeray --profile analysis auth whoami
```

Import only the `token` value. Add artifact/upload scopes for files and
`runs:cancel` for cancellation. Tokens retain the account's resource restrictions;
`me:read` verifies the identity during stdin login and checkpoint recovery. The
CLI defaults to a 30-day token lifetime; renew personal tokens by issuance rather
than browser refresh. Inspect `auth token-list` and revoke unused tokens with
`auth token-revoke TOKEN_UUID`. After a lost creation reply, inspect inventory
and revoke uncertain credentials before deliberately creating another.

Profiles and credential state are private. `--profile` selects a named service
(default `default`); `--config` selects another profiles file. API origin changes
clear saved credentials, and `--api` never forwards a profile token to a different
origin. An explicit `ENUMERAY_TOKEN` takes precedence over the selected profile;
pair it with its intended origin. Saved browser sessions renew automatically
before expiry while commands are active; `auth refresh` remains available for
explicit renewal and one-time migration of an already-expired legacy profile.
Expired or revoked refresh sessions and unknown renewal outcomes need another
login. Preserve the same API/account and saved operation when recovering.
`auth logout` removes local credentials without
revoking provider sessions or server-side personal tokens.

## Choose a workflow

| Goal | Included help |
| --- | --- |
| Run scalar, file or collection analyses | [Analysis guide](ANALYSIS_GUIDE.md) |
| Script with structured output | [Complete JSON recipe](ANALYSIS_GUIDE.md#script-with-structured-output) |
| Create, install, trial and publish a model | [Author guide](AUTHOR_GUIDE.md) |
| Work through supplied publications and inputs | [Example walkthrough](examples/README.md) |
| Enroll and operate compute | [Connector guide](https://github.com/raysofspace/enumeray-cli/blob/main/crates/connector/README.md) |

The archive includes standalone connector and driver guides; their local links
work after extraction without a checkout. It includes complete
Python/executable JSON/file starter projects
under `starter-kit/`. `model init` creates an editable project without credentials.
The package examples require the stated installed release and compute grant;
publication JSON is different from a scientific input object or HTTP request.

Use analysis directories for CLI execution, including candidate trials. Review
the saved publication, destination/disclosure and inputs before `analysis submit`.
Repeat operations in their original directory after interruption. A new intended
execution uses a new directory. Collection completeness is independent of scientific
success; inspect `bundle.json` and retain original input bytes separately.

Use `--json` when saving or parsing structured output. Runtime errors are JSON on
stderr; argument errors remain plain text and exit 2. Native commands otherwise
show human output. Listings fetch one bounded page; preserve filters and opaque
cursors while continuing. ID-based run investigation and independent data utilities
remain available; consult built `--help` for exact command syntax.

## Credential and command boundaries

User commands use CLI profiles. `enumeray connector enroll/run` use explicit
connector credential files and never load a user login. `enumeray connector inspect`
and cleanup inspection use the private journal; `enumeray compute-runner` and
internal workers do not load API credentials. Host commands emit JSON; the runner
retains its versioned stdin/stdout protocol. Installing the executable starts no
service. Managed execution continues to require a separate model account.

## Upgrade deliberately

Fresh WIP installations use only `enumeray`; the old `enumeray-connector`
executable and its command paths are not supported. DSM demo recreation starts
from fresh state. Read the archive's `compatibility.json`. The proposed persistent baseline is still
a candidate; version 1 does not prove compatibility between unreleased revisions.
Use the exact matching component set approved for your service. Infrastructure
owners stop the previous daemon before replacing a connector/runner binary or
credential and preserve the complete private state and original execution evidence.
Follow the [connector recovery guidance](https://github.com/raysofspace/enumeray-cli/blob/main/crates/connector/README.md#enrollment-and-service-operation)
for an interrupted service. Installation readiness and successful fixture trials
do not establish scientific validity or qualification of every production site.

The [documentation project](https://doc.enumeray.com/)
provides role-based pages and current command/API references.
