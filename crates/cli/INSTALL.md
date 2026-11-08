# Install and use Enumeray

Install the `enumeray` command for the service you plan to use. The same
executable provides the CLI, connector and compute runner. For a hosted trial
or to run a model your team shares, install it on the computer where you
will submit the run.

## Before choosing a package

For the demo, [check hosted trial availability](https://doc.enumeray.com/start/hosted-release/).
Use the approved version, target and API address listed there. If the page asks
you to contact support, get those details before downloading. For a team
service, ask its administrator for the approved release or public commit, API
address and matching service verification information.

Choose the route for your computer and situation.

| What you need | Where to start |
| --- | --- |
| Install an approved Homebrew formula | [Homebrew on macOS](#homebrew-on-macos) |
| Install a specific published binary | [Verify and install an archive](#verify-and-install-an-archive) |
| Install on a machine without internet access | [Offline and SSH/HPC hosts](#offline-and-sshhpc-hosts) |
| Compile an approved published release | [Build from a release source archive](#build-from-a-release-source-archive) |
| Evaluate a service that has no qualified package | [Build from the exact public commit supplied by the service owner](#build-from-a-pinned-checkout) |
| Replace an installation with saved work | [Plan the change with the service and compute owners](#upgrade-deliberately) |

The binary targets are macOS 15 or later and Linux with glibc 2.28 or later,
each on Intel x86-64 and ARM64. Check the architecture with `uname -m` and the
system with `sw_vers -productVersion` on macOS or `getconf GNU_LIBC_VERSION`
on Linux. Choose a supported host if your system is older, uses musl or runs
Windows. The site owner supplies the separate checks for Slurm, SSH and any
managed service configuration.

Once `enumeray --version` works, [sign in](#login-and-profiles). Use a browser
for interactive sign-in. Use the [connector setup guide](SETUP_GUIDE.md)
if you also need to connect a computer that runs models.

## Homebrew on macOS

Use Homebrew when the tap publishes the formula for the version approved for
your service. The formula selects the archive for your Mac.

```sh
brew tap raysofspace/tap
brew install raysofspace/tap/enumeray
enumeray --version
```

Homebrew installs the tap's published version. To install a specific version
that differs from the formula, use the archive below. Find the packaged guides
and compatibility information under `$(brew --prefix enumeray)/share/enumeray`.

For a replacement, follow [the maintenance procedure](#upgrade-deliberately)
before running `brew upgrade raysofspace/tap/enumeray`. Use
`brew pin enumeray` to keep an installed version and `brew unpin enumeray` to
allow upgrades. `brew uninstall enumeray` removes the package. Keep separately
managed credentials and journals with the work they belong to.

## Verify and install an archive

### Download the selected archive

Use the approved published version from the
[release assets](https://github.com/raysofspace/enumeray-cli/releases).
Each binary archive contains the executable, license, compatibility information,
guides and examples. Choose the target for the computer where it will run.

| Computer | Target |
| --- | --- |
| Apple Silicon Mac | `aarch64-apple-darwin` |
| Intel Mac | `x86_64-apple-darwin` |
| ARM64 Linux with glibc | `aarch64-unknown-linux-gnu` |
| Intel or AMD x86-64 Linux with glibc | `x86_64-unknown-linux-gnu` |

Replace both placeholders below with the approved version without its leading
`v` and the target from the table. Use a terminal with a POSIX shell, `curl`,
`tar`, `install` and a SHA-256 tool (`shasum` on macOS or `sha256sum` on
Linux). For the next step, install Cosign v3 from a trusted source separately
from the archive. Use a directory you control and keep the three downloaded
files together.

```sh
mkdir -p "$HOME/enumeray-downloads"
cd "$HOME/enumeray-downloads"
VERSION=REPLACE_WITH_APPROVED_VERSION
TARGET=REPLACE_WITH_YOUR_TARGET
REPOSITORY=raysofspace/enumeray-cli
TAG="v$VERSION"
ARCHIVE="enumeray-$VERSION-$TARGET.tar.gz"
CHECKSUM="$ARCHIVE.sha256"
BASE_URL="https://github.com/$REPOSITORY/releases/download/$TAG"
curl --fail --location --proto '=https' --tlsv1.2 --output "$ARCHIVE" "$BASE_URL/$ARCHIVE"
curl --fail --location --proto '=https' --tlsv1.2 --output "$CHECKSUM" "$BASE_URL/$CHECKSUM"
curl --fail --location --proto '=https' --tlsv1.2 --output "$CHECKSUM.sigstore.json" "$BASE_URL/$CHECKSUM.sigstore.json"
```

### Verify the download

Verify on a trusted connected host. Cosign checks that the checksum file was
signed by the release workflow. The SHA-256 check compares the downloaded
archive with that signed file. Extraction runs after both checks pass.

<!-- archive-verification -->
```sh
(
  set -eu
  cosign verify-blob \
    --bundle "$CHECKSUM.sigstore.json" \
    --certificate-identity "https://github.com/$REPOSITORY/.github/workflows/release.yml@refs/heads/main" \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    "$CHECKSUM"
  case "$(uname -s)" in
    Darwin) shasum -a 256 --check "$CHECKSUM" ;;
    Linux) sha256sum --check "$CHECKSUM" ;;
    *) echo 'Unsupported verification host' >&2; exit 1 ;;
  esac
  # Extraction is unreachable when either verification command fails.
  tar -xzf "$ARCHIVE"
)
```

If either check fails, stop and obtain the correct signed files. After both
pass, set `EXTRACTED="$PWD/${ARCHIVE%.tar.gz}"`. Keep the archive and signed
checksum for your records.

### Compare the package with the service

Read the archive’s `compatibility.json` and run
`"$EXTRACTED/enumeray" --json compatibility inspect`. Follow
[the installation check](#inspect-the-selected-installation) with this
extracted executable and the evidence for the approved release. Continue when
the required checks report a match. If you’re replacing a working installation,
[check the maintenance steps](#upgrade-deliberately) before you copy the file.

### Copy the verified executable

The example installs to `$HOME/.local/bin`. Set `PREFIX` to use another
directory you control. The copy stages and checks the executable in the
destination directory before replacing the file. Use Homebrew for executables
managed by Homebrew.

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

When `enumeray --version` prints the selected version, continue to
[sign-in](#login-and-profiles). Keep the extracted guides, metadata and
verification receipt for the package you installed. To remove a direct
installation, stop any daemon that uses it and remove only the executable you
installed. Its profile, connector journal and scientific work are stored
separately.

## Offline and SSH/HPC hosts

First verify the archive signature and checksum on a trusted computer with
internet access. Transfer the archive, signed checksum, signature bundle,
release and service receipts, compatibility and source details, and installation
receipt over your approved authenticated channel. Record which computer verified
the files, the SHA-256 of the archive and how the recipient authenticated the
handoff.

On the destination, compare the archive with the verified SHA-256 before
extracting and installing it. The connected verifier can provide the matching
signed-release installation receipt and its SHA-256 through an independently
authenticated channel. Use
`compatibility check --transferred-receipt FILE --transferred-sha256 SHA256 --local-only --receipt NEWFILE`
to compare the installed file. The check reports `authenticated_transfer` for
this local check. Retain the verified files and receipts together.

Install the matching executable on each remote host that runs it, at the
path approved by the site operator. SSH requires OpenSSH on the connector host.
For Slurm, make the executable accessible to the compute nodes.

## Build from a release source archive

Each qualified release also provides `enumeray-VERSION-source.tar.gz`, its
`.sha256` checksum and `.sha256.sigstore.json` signature bundle. Select the same
explicit release version as the service. Replace the version placeholder with
the approved version without its leading `v`, then download these three files.

```sh
VERSION=REPLACE_WITH_APPROVED_VERSION
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

## Build from a pinned checkout

Use this development evaluation route when no qualified release package has
been supplied. The service owner must provide the **full 40-character public
commit** matching the selected service, not `main`, a guessed tag or an abbreviated
commit. Replace the deliberate placeholder below before running the block. Use
a new destination directory. Install Git, Rustup and the platform compiler and
linker on the build host. The public repository contains the client sources.

<!-- pinned-source-acquisition -->
```sh
(
  set -eu
  PUBLIC_REVISION=REPLACE_WITH_APPROVED_40_HEX_PUBLIC_REVISION
  case "$PUBLIC_REVISION" in
    *[!0-9a-f]*) echo 'Set the approved full public commit before acquisition' >&2; exit 1 ;;
  esac
  test "${#PUBLIC_REVISION}" -eq 40
  git clone --no-checkout https://github.com/raysofspace/enumeray-cli.git enumeray-cli-evaluation
  cd enumeray-cli-evaluation
  git checkout --detach "$PUBLIC_REVISION"
  test "$(git rev-parse HEAD)" = "$PUBLIC_REVISION"
  rustup toolchain install 1.98.1 --profile minimal
  cargo build --locked --release -p enumeray-cli --bin enumeray
  ./target/release/enumeray --version
  ./target/release/enumeray --json compatibility inspect
)
```

Run this from the **parent directory** of the new checkout. The subshell returns
there when complete; then set `EXTRACTED="$PWD/enumeray-cli-evaluation/target/release"`
and use the atomic installation block above. The checkout's `rust-toolchain.toml`
selects Rust 1.98.1. Cargo downloads the locked dependencies and needs network
access on the build host; an offline build needs that toolchain and the complete
locked dependency cache provisioned first. A source build records its actual
source identity and is **unsigned development evidence**, not a qualified release.
Keep the checkout, lockfile and build identity with the service selection.
Use a separate directory for another revision. Keep the matching executable
for any active work you still need to finish.

## Inspect the selected installation

Use this check to confirm that the executable and service match the package
or source pair selected for your installation. To see the executable’s
version, target, source and supported protocols, run:

```sh
enumeray --json compatibility inspect
```

For a binary archive, compare the output with its `compatibility.json`. A
matching version label alone is insufficient. The signed release check needs
the release manifest, the service publication receipt, and the four reports
for the supported platforms. The block below downloads and checks them. It
requires `jq`, `curl` and a separately trusted Cosign v3.

Use the same `VERSION`, `REPOSITORY`, `TAG` and `BASE_URL` as your archive.
Set `API_ORIGIN` if your administrator supplies a different address. Check
the extracted executable before installing it. To check an already installed
package, set `ENUMERAY_BIN=enumeray` instead. Use a new `EVIDENCE` directory
for each attempt so you can keep the results of an interrupted check.

<!-- release-evidence-acquisition -->
```sh
EVIDENCE="$PWD/release-evidence-$TAG"
ENUMERAY_BIN="$EXTRACTED/enumeray"
(
  set -eu
  API_ORIGIN="${API_ORIGIN:-https://api.enumeray.com}"
  mkdir "$EVIDENCE"
  for file in release-manifest.json release-manifest.json.sigstore.json \
              service-publish.json service-publish.sigstore.json; do
    curl --fail --location --proto '=https' --tlsv1.2 \
      --output "$EVIDENCE/$file" "$BASE_URL/$file"
  done
  # Authenticate the manifest before jq reads any field or selects more downloads.
  cosign verify-blob \
    --bundle "$EVIDENCE/release-manifest.json.sigstore.json" \
    --certificate-identity "https://github.com/$REPOSITORY/.github/workflows/release.yml@refs/heads/main" \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    --certificate-github-workflow-repository "$REPOSITORY" \
    --certificate-github-workflow-ref refs/heads/main \
    --certificate-github-workflow-trigger repository_dispatch \
    "$EVIDENCE/release-manifest.json"
  jq -e --arg version "$VERSION" --arg tag "$TAG" --arg repository "$REPOSITORY" \
    '.schema_version == 1 and .version == $version and .tag == $tag and .public_repository == $repository' \
    "$EVIDENCE/release-manifest.json" >/dev/null
  for target in aarch64-apple-darwin x86_64-apple-darwin \
                aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
    # Exact fixed-target filenames cannot escape EVIDENCE or change download origins.
    report=$(jq -er --arg target "$target" \
      '.targets[$target].qualification | select(. == ("qualification-" + $target + ".json"))' \
      "$EVIDENCE/release-manifest.json")
    curl --fail --location --proto '=https' --tlsv1.2 \
      --output "$EVIDENCE/$report" "$BASE_URL/$report"
  done
  "$ENUMERAY_BIN" --api "$API_ORIGIN" --json compatibility check \
    --manifest "$EVIDENCE/release-manifest.json" \
    --service-receipt "$EVIDENCE/service-publish.json" \
    --receipt "$EVIDENCE/installation-receipt.json"
)
```

The block stops if it cannot download or verify a required file. The CLI
checks both signatures, the selected service authorization and each report's
digest. Keep the evidence directory, archive and verified checksum with the
installation receipt. Supply any peer or context observations required by
your site.

For a development evaluation, use the explicit service/public selection supplied
by its owner with `--development-selection FILE` instead of the manifest and
service-receipt pair. The selection records `schema_version: 1`,
`service_repository` (`raysofspace/enumeray`), `public_repository`
(`raysofspace/enumeray-cli`), full `service_revision` and `public_revision`,
`version` and `baseline` (`id`, `sha256`, `status`). Modified builds additionally
require the independently selected `public_content_sha256` and/or
`service_content_sha256`; record their actual fingerprints, not just `clean:
false`. That result remains unsigned development evidence. Do not
create a selection by copying the current executable's fields just to make a
mismatch disappear. Supply retained peer build observations with repeated
`--peer FILE`, documentation identity with `--docs docs-build.json`, and the
served examples manifest with `--examples model-examples-manifest.json`. An
optional `--connector ID` checks its authenticated build observation; `--context
FILE` checks a saved read-only runner observation tied to the selected context.
A peer file is the saved inspect identity plus a top-level RFC3339
`observed_at` timestamp. A context observation has `schema_version: 1`, the exact
`context_id`, `observed_at` and `build_identity` from the runner identity command.
The site owner must authenticate that host/context association. Live observations
must be recent (at most 60 seconds); older files remain historical snapshots and
cannot satisfy a live check. Inspecting compatibility never launches a scientific
job or proves journal ownership. Obtain observations from the responsible host owner; API/client
identity cannot attest an author-declared runtime, dataset or institutional site.

Read the receipt's selected inputs, expected/observed check values and limits.
`match` means the supplied evidence agrees within those checks; `mismatch` means
stop before reuse; `missing_evidence` means the installation is not fully
established. An unreachable API, absent runner observation or stale heartbeat is
not a successful whole-installation verdict. Preserve the receipt and evidence,
resolve missing observations with the owner and repeat to a new receipt file.
The check exits 0 only when required comparisons in its declared scope match,
1 for mismatch, missing evidence or verification failure, and 2 for invalid
command arguments. With `--local-only`, the receipt explicitly excludes live API
and daemon observation and must not be presented as whole-installation proof.
Do not overwrite historical receipts or credentials to bypass a failed check.
The website exposes `/docs-build.json` and a footer stamp for its root/public
source pair and build time; download it from the same selected documentation
site as its examples, not from an unrelated current site.

## Login and profiles

Sign in using the API address selected for your service. A fresh configuration
uses `https://api.enumeray.com`.

```sh
enumeray auth login
enumeray auth whoami
```

In the browser, choose **Register** if you need an account, verify your email,
then finish signing in. If the CLI stops waiting, complete registration and run
`auth login` again. `auth whoami` confirms the account.

For a team service, replace the example address with the one your administrator
provided. Use the named profile with every command for that service.

```sh
enumeray --profile research auth configure https://research.example.org
enumeray --profile research auth login
enumeray --profile research auth whoami
```

Run `enumeray auth profiles` to see the names and API addresses. The CLI uses
`default` when you omit `--profile`. Changing a profile’s address clears its
saved credentials. The CLI uses a profile’s credentials only at their original
address. If you supply `ENUMERAY_TOKEN`, pair it with the issuing service.

For a CLI running on a remote host without a browser, create a personal token
from a signed-in account. Copy the private token file to the intended host using
your approved method and import its `token` field. This example uses `jq`.
Configure the `analysis` profile with the issuing service’s API address
before import if it differs from the hosted API.

```sh
enumeray auth token-create analysis-script \
  --scopes models:read,runs:create,runs:read,me:read --output token.json
jq -er '.token' token.json | enumeray --profile analysis auth login --token-stdin
enumeray --profile analysis auth whoami
```

Use the issuing profile for token creation. Add only the scopes the script
needs, including `me:read` for import and checkpoint recovery. The default
token lifetime is 30 days. Issue a replacement when it expires. Use
`auth token-list` to find token IDs and `auth token-revoke TOKEN_UUID` to
revoke one. After a lost creation reply, check the inventory before issuing a
replacement.

Browser sessions renew while the CLI is in use. If renewal fails, sign in to
the same account and repeat the command with the original saved folder.
`auth logout` removes local credentials. End browser sessions and revoke
provider tokens through the identity provider. See
[sign-in help](https://doc.enumeray.com/start/login/) for browser and profile
troubleshooting.

## Choose a workflow

| Goal | Included help |
| --- | --- |
| Run scalar, file or collection analyses | [Analysis guide](ANALYSIS_GUIDE.md) |
| Script with structured output | [Complete JSON recipe](ANALYSIS_GUIDE.md#script-with-structured-output) |
| Create, install, trial and publish a model | [Author guide](AUTHOR_GUIDE.md) |
| Work through supplied publications and inputs | [Example walkthrough](examples/README.md) |
| Connect compute and trial a model | [Guided connector setup](SETUP_GUIDE.md) |
| Operate an existing connector | [Complete connector guide](../connector/README.md) |

The archive includes complete examples under `starter-kit/`. Use the analysis
guide to review inputs, submit, check status and collect outputs in one folder.
If a command is interrupted, resume in that folder. Use a separate folder when
you intend to start another run.

## Credential and command boundaries

Use the CLI profile for user commands. For connector enrollment and startup,
use the connector’s credential file. Keep connector journals private.
`enumeray connector inspect` reads the journal, and the compute runner uses
its own protocol without API credentials. Installing the executable places
the file on disk. Follow [connector setup](SETUP_GUIDE.md) to configure and
start a service.

## Upgrade deliberately

Use this section when replacing an installation that holds valuable work.
Confirm with the service owner that the proposed binaries support the saved
state. Same-build recovery uses the matching software and records. For a
development revision without a supported change path, use a fresh evaluation
with separate state and identities. A model publication rollback follows the
[author guide](AUTHOR_GUIDE.md).

Before any binary, credential or service change, prepare this inventory with the
service, connector and execution-host owners:

| Record | Preserve before changing anything |
| --- | --- |
| Existing and proposed software | Exact service/public pair, executable identities and hashes, target/minimum host, package/source metadata, docs/examples and verification receipts; retain the old executables. |
| Configuration and authority | Private profile/configuration and credential locations, owning OS account, API origin, connector ID, context/grant identities and approved executable paths. Keep secret bytes out of shared receipts. |
| Durable work | Original connector journal/owner identity, compute launch/session records, analysis and saved-release directories, immutable installations, pending delivery/cleanup evidence and backend execution identities. |
| Service and storage | Current schema/candidate identity, coordinated database/object/connector/compute backup boundary and responsible recovery owner; existing immutable receipts/history remain unchanged. |

Follow these go/no-go steps in order:

1. Compare existing and proposed identities before stopping anything. If the
   proposed state/schema relationship is unsupported, choose a separate fresh
   evaluation with new state/identities, or stop and retain the matching old set.
   Do not copy a journal into a fresh installation and rename it a migration.
2. Stop admitting new work and use the **old matching binary and original owner
   account** to observe and drain accepted allocations, delivery and cleanup.
   Keep original execution identities. A missing journal, uncertain launch,
   unfinished cancellation or unprovable drain is a **stop/escalate condition**:
   preserve all evidence and ask the service/site owner to resolve it. Starting a
   replacement connector or deleting the evidence is not a drain procedure.
3. Establish the coordinated backup boundary before a supported change. Preserve
   private configuration, credentials, original journals, compute records and
   valuable scientific data with their ownership/permissions. A binary-only copy
   cannot restore a changed database or recover a live backend execution.
4. Stop the old daemon and verify that no second process owns its journal. Install
   to the managed path using the atomic direct-copy or package-manager operation.
   Check identities before reopening; mismatched or markerless durable state must
   remain unchanged and refused for mutation, with read-only inspection available.
5. Reopen only with the selected matching state and responsible owner. Recheck the
   API, connector and runner observations, original assignments/results and
   absence of duplicate launches. Retain the new installation receipt and the
   old evidence even after reopening succeeds.

If a replacement cannot open the retained state, keep it stopped. Use the
retained **matching binary and coordinated state** for recovery only when the
recorded compatibility/reopening checks allow it; do not point an older binary
at newer evidence or restore an older backup over newer surviving compute
records. Unsupported rollback requires escalation, not deleting marker fields,
forcing a format version, recreating the database or re-enrolling over the old
connector. A fresh disposable evaluation has separate state and explicit new
identities, while valuable old state stays intact and readable with its matching
software. A real cross-release upgrade/rollback pair needs its own selected,
supported release-policy rehearsal before it can be advertised.

For a connector interruption, see
[enrollment and service operation](../connector/README.md#enrollment-and-service-operation).
The service owner coordinates database and storage backup or restore. The
installation receipt records the software and evidence checked for the
selected scope. Use the model’s scientific checks and the site’s own
qualification process for those separate decisions.

The [documentation project](https://doc.enumeray.com/)
provides role-based pages and current command/API references. Use the documentation revision and identity download in the footer to select
guidance that matches the installation you are checking.
