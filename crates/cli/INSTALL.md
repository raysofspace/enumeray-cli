# Install and use Enumeray

One `enumeray` executable provides user commands, connector operation and the
compute runner. Install the same qualified package on each required host.
Users need no API checkout, PostgreSQL, database credentials, Python or installer
to run the executable. Select a qualified archive for your operating system and
CPU. The supported matrix is macOS **15 or newer** and Linux **glibc 2.28 or newer**,
each on x86-64 and ARM64. Alpine/musl, Windows and older systems are outside this
matrix. Each platform must pass its own oldest-supported-environment checks before
publication. A build on a newer system alone does not qualify that minimum.

Package availability follows the matching service's release qualification.
Until a qualified release is published, build from source below. Direct downloads
and the project Homebrew tap are the supported distribution channels.

After installation, use [guided connector setup](SETUP_GUIDE.md) to select a mode,
prepare the host, establish readiness and trial a first model with saved progress.

**Alpha: 0.1.0-alpha.1.** Commands and APIs may change. Alpha updates may require
a separate fresh installation; no in-place upgrade compatibility is promised.
Read the target release's instructions before upgrading. Keep valuable existing
state with its matching binaries; installation and removal never recreate or
clear credentials, configuration or connector journals. The persistent baseline
remains provisional. Alpha labeling does not relax execution, authorization or
recovery checks.

## Homebrew on macOS

Once the project tap's qualified formula is published:

```sh
brew tap raysofspace/tap
brew install raysofspace/tap/enumeray
enumeray --version
```

The single formula selects the Intel or Apple Silicon archive and installs its
guides and compatibility metadata under `$(brew --prefix enumeray)/share/enumeray`.
It has no service definition or startup hook. For upgrades, follow the maintenance
steps below, then run `brew upgrade raysofspace/tap/enumeray`. Use `brew pin enumeray`
to retain the installed version; `brew unpin enumeray` allows later upgrades.
Homebrew installs the tap's current formula. Use a versioned direct archive when
an exact historical version is required. Remove the package with
`brew uninstall enumeray`; separately managed credentials and journals remain.

## Verify and install an archive

Check the repository's [releases](https://github.com/raysofspace/enumeray-cli/releases)
for qualified packages. Draft artifacts do not establish release qualification.
Each archive contains one executable plus its license, compatibility/source
metadata, guides and examples. There is no custom installer or self-updater.
Use an explicit published version and target; do not use a draft or infer service
compatibility from a version number. This example selects Apple Silicon macOS;
replace `TARGET` with `x86_64-apple-darwin`, `aarch64-unknown-linux-gnu` or
`x86_64-unknown-linux-gnu` as appropriate:

```sh
VERSION=0.1.0-alpha.1
TARGET=aarch64-apple-darwin
REPOSITORY=raysofspace/enumeray-cli
TAG="v$VERSION"
ARCHIVE="enumeray-$VERSION-$TARGET.tar.gz"
CHECKSUM="$ARCHIVE.sha256"
BASE_URL="https://github.com/$REPOSITORY/releases/download/$TAG"
curl --fail --location --proto '=https' --tlsv1.2 --output "$ARCHIVE" "$BASE_URL/$ARCHIVE"
curl --fail --location --proto '=https' --tlsv1.2 --output "$CHECKSUM" "$BASE_URL/$CHECKSUM"
curl --fail --location --proto '=https' --tlsv1.2 --output "$CHECKSUM.sigstore.json" "$BASE_URL/$CHECKSUM.sigstore.json"
```

Verify on a trusted connected host with a separately trusted Cosign v3. The
qualified-release workflow signs from `main`; the requested version is bound by
the signed checksum filename and the release/source metadata:

```sh
cosign verify-blob \
  --bundle "$CHECKSUM.sigstore.json" \
  --certificate-identity "https://github.com/$REPOSITORY/.github/workflows/release.yml@refs/heads/main" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  "$CHECKSUM"
# Linux: sha256sum --check "$CHECKSUM"
# macOS (also works on Linux with Perl):
shasum -a 256 --check "$CHECKSUM"
tar -xzf "$ARCHIVE"
EXTRACTED="$PWD/${ARCHIVE%.tar.gz}"
```

Stop immediately if either verification command fails. Keep the signed checksum
and archive together. Read `compatibility.json` and the source identity before
using the package with a service. Do not skip signature verification because a
checksum from the same untrusted download happens to match.

Choose any writable prefix, with `$HOME/.local` as the default. The commands below
stage beside the destination and rename on the same filesystem, so an interrupted
copy leaves the previous executable in place. Use a prefix you manage directly;
use Homebrew to update executables owned by that package manager.
Coordinate connector/runner maintenance before replacing an existing package.

<!-- manual-install-copy -->
```sh
(
  set -eu
  PREFIX=${PREFIX:-"$HOME/.local"}
  mkdir -p "$PREFIX/bin"
  staged=$(mktemp "$PREFIX/bin/.enumeray.XXXXXX")
  trap 'rm -f "$staged"' 0 HUP INT TERM
  install -m 755 "$EXTRACTED/enumeray" "$staged"
  "$staged" --version
  mv -f "$staged" "$PREFIX/bin/enumeray"
)
```

Add your chosen `PREFIX/bin` to `PATH`, for example:

```sh
export PATH="$HOME/.local/bin:$PATH"
enumeray --version
```

Keep the extracted metadata/guides wherever convenient; the executable runs
independently. Removal is simply `rm "${PREFIX:-$HOME/.local}/bin/enumeray"` after stopping any
daemon that uses it. Remove only the path you installed. This leaves your user
configuration, credentials, connector journal and scientific work untouched.

## Offline and SSH/HPC hosts

Verify the signature and archive checksum on a trusted connected host first.
Transfer the verified archive, signed checksum and compatibility/source metadata
through your approved authenticated SSH transfer. Recheck the archive's SHA-256
at the destination using that verified checksum, then extract and install as
above. Cosign is needed on the verification host; the offline execution host
needs no Python, installer or network access for installation. Retain the
verification evidence with the downloads for later reuse.

SSH execution needs OpenSSH on the connector host and the matching Enumeray
executable on each remote execution host. Slurm compute nodes need the same
qualified executable. Install it at the path approved by the site operator.


## Build from a release source archive

Each qualified release also provides `enumeray-VERSION-source.tar.gz`, its
`.sha256` checksum and `.sha256.sigstore.json` signature bundle. Select the same
explicit release version as the service and download these three files from the
public release:

```sh
VERSION=0.1.0-alpha.1
TAG="v$VERSION"
ARCHIVE="enumeray-$VERSION-source.tar.gz"
CHECKSUM="$ARCHIVE.sha256"
BASE_URL="https://github.com/raysofspace/enumeray-cli/releases/download/$TAG"
for file in "$ARCHIVE" "$CHECKSUM" "$CHECKSUM.sigstore.json"; do
  curl --fail --location --proto '=https' --tlsv1.2 --output "$file" "$BASE_URL/$file"
done
```

Apply the signature and SHA-256 verification steps above to these filenames
before extracting the archive. The expected signing identity is the same public
release workflow. The archive includes `LICENSE`, `Cargo.lock`, the pinned
`rust-toolchain.toml`, dependency notices and all public CLI, client, connector
and contract sources. It builds independently of the private API source. Read
`release-source.json` to identify the exact source revision and tree.

With the pinned Rust toolchain installed, build from the extracted source root:

```sh
cd "enumeray-$VERSION-source"
cargo build --locked --release --target-dir target -p enumeray-cli --bin enumeray
EXTRACTED="$PWD/target/release"
```

Use the atomic copy steps above to install `EXTRACTED/enumeray` under your chosen
prefix. Cargo obtains dependencies matching `Cargo.lock`; an offline source
build needs the toolchain and those dependencies already available locally.
Locally rebuilding a release does not replace its published archive qualification
or establish compatibility with another service release.

## Build from the checkout

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
