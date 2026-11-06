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

After installation, sign in and follow the
[hosted trial](https://doc.enumeray.com/start/first-run/) to run the supplied HUXt
or REBOUND model without operating a connector. For your own compute, use
[guided connector setup](SETUP_GUIDE.md) to select a mode, prepare the host,
establish readiness and trial a first model with saved progress.

**Alpha: 0.1.0-alpha.2.** Commands and APIs may change. Alpha updates may require
a separate fresh installation; no in-place upgrade compatibility is promised.
Read the target release's instructions before upgrading. Keep valuable existing
state with its matching binaries; installation and removal never recreate or
clear credentials, configuration or connector journals. The persistent baseline
remains provisional. Alpha labeling does not relax execution, authorization or
recovery checks.

## Before choosing a package

Obtain the approved public source revision or release,
matching service selection/receipt and responsible installation contact from the
service or site owner. A model ID, a `persistent-v1` label, a version string and
this guide's example `0.1.0-alpha.2` are not substitutes for that selection.
Confirm that the selected version is published for your chosen channel;
Homebrew additionally requires its qualified tap formula. The examples below
do not establish availability. If no qualified package is identified, use the pinned source
evaluation route agreed with the service owner; do not guess a revision or reuse
an older pilot's durable state.

Use a terminal with a POSIX shell (`sh`), `curl`, `tar`, `mkdir`, `install`,
`mktemp`, `mv`, `rm` and a SHA-256 utility (`shasum` on macOS or `sha256sum` on
Linux). Source acquisition additionally needs Git, a trusted Rustup installation
and the platform's native compiler/linker. Obtain these tools through your
institution's approved package or toolchain process. Signature verification
requires **Cosign v3 on a trusted connected host**, obtained independently of the
Enumeray download; an unverified tool shipped beside an archive cannot establish
its trust. Confirm tools with `command -v TOOL` and `cosign version` before
following a recipe. Python 3.9+ is needed only for supplied author examples, and
`jq` only for scripted JSON/HTTP workflows.

Confirm the host's CPU and minimum system before downloading: `uname -m` reports
`arm64`/`aarch64` or `x86_64`; `sw_vers -productVersion` reports macOS, and
`getconf GNU_LIBC_VERSION` reports glibc on Linux. Stop for an unknown architecture,
missing glibc or an older system. A different target's binary is not an upgrade
route. Slurm/OpenSSH versions and institutional storage/runtime behavior require
the site's selected backend evidence; the package matrix alone does not qualify
them or native systemd/launchd service management.

Work in a new download directory you control. Keep the verified archives,
metadata and receipt after installation; the executable alone cannot recover
all verification evidence.

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
mkdir -p "$HOME/enumeray-downloads/alpha-evaluation"
cd "$HOME/enumeray-downloads/alpha-evaluation"
VERSION=0.1.0-alpha.2
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

After that block succeeds, set `EXTRACTED="$PWD/${ARCHIVE%.tar.gz}"`.


Stop immediately if either verification command fails. Keep the signed checksum
and archive together. Read `compatibility.json` and the source identity before
using the package with a service. Do not skip signature verification because a
checksum from the same untrusted download happens to match.

Choose any writable prefix, with `$HOME/.local` as the default. The commands below
stage beside the destination and rename on the same filesystem, so an interrupted
copy leaves the previous executable in place. Use a prefix you manage directly;
use Homebrew to update executables owned by that package manager.
Coordinate connector/runner maintenance before replacing an existing package.
Run `"$EXTRACTED/enumeray" --json compatibility inspect` before replacement and
compare it with the package metadata and selected service as described below;
invoke that extracted binary for the pre-install compatibility check. Stop on a
wrong target, mismatch or missing required evidence before replacing a working
executable. `--local-only` establishes only its declared installed-file scope,
not permission to reuse an incompatible journal.

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
Transfer the verified archive, signed checksum, signature bundle,
release/service receipts, compatibility/source metadata and installation receipt
through your approved authenticated SSH transfer. Record which connected host
verified them, the artifact SHA-256 and how the recipient authenticated that
handoff. Recheck the archive's SHA-256 at the destination using that verified
checksum, then extract and install as above. A successful local checksum only
checks the transfer against that trusted handoff; it does not independently
repeat online signature verification. The connected verifier can give the
recipient a prior matching signed-release installation receipt and its SHA-256
through an independently authenticated channel. On the offline recipient, use
`compatibility check --transferred-receipt FILE --transferred-sha256 SHA256
--local-only --receipt NEWFILE` to compare the installed bytes within that
explicit local scope. The resulting trust is `authenticated_transfer`, not a
claim of freshly repeated online signing or observation of a live service. Cosign is needed on the verification host; the offline execution host
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
VERSION=0.1.0-alpha.2
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
a new destination directory; Git refuses to clone over an existing checkout.
The public repository is sufficient: no private API checkout, database or API
credentials are needed to build the client.

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
Keep the checkout or matching source archive, lockfile and build identity with
its selected service pair. Check out a different commit in a separate directory;
do not overwrite the matching executable needed to drain an earlier pilot.

## Inspect the selected installation

`enumeray --version` confirms an executable starts. Use the machine-readable
identity to inspect what was actually built:

```sh
enumeray --json compatibility inspect
```

The identity names the package/version/target, public repository and exact
revision/tree/lockfile/content digest, source cleanliness and API, connector,
runner and wrapper protocol versions. The archive's `compatibility.json` must
agree with the embedded executable identity. The content digest identifies
modified development sources; it does not turn that build into a signed release.
A matching protocol number or source revision alone does not establish a match.

For a qualified release, acquire its complete signed evidence from the **same
selected `VERSION`, `REPOSITORY`, `TAG` and `BASE_URL`** used for the archive above.
This step additionally requires `jq`, `curl` and the independently trusted
**Cosign v3** on the connected verification host. The four native report files
are evidence for the selected release's supported platform set; downloading them
does not install other targets. Confirm that the selected version's release
assets are published before downloading; an example version alone does not
establish availability.

Set `API_ORIGIN` to the service origin supplied by its owner. The block below
uses the verified extracted executable for a check **before replacement**. For
an already installed package, replace the `ENUMERAY_BIN` assignment with
`ENUMERAY_BIN=enumeray`. `EVIDENCE` must name a new directory: an interrupted or
failed attempt leaves its original files intact, so select a fresh directory for
a retry instead of overwriting retained evidence.

<!-- release-evidence-acquisition -->
```sh
EVIDENCE="$PWD/release-evidence-$TAG"
ENUMERAY_BIN="$EXTRACTED/enumeray"
(
  set -eu
  : "${API_ORIGIN:?Set the approved service API origin}"
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

Any failed download, manifest verification, selected-version check or report-name
validation stops the block before compatibility checking. The CLI then rechecks
both workflow signatures, the exact service publication authorization and all
manifest-bound native report digests. A downloaded publication receipt is not
trusted merely because the manifest's signature passed. Keep this complete
evidence directory, the archive and its verified checksum with the resulting
installation receipt. Supply any required peer/context observations to the check
as described below; absent live evidence must remain explicit.

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

The CLI connects to https://enumeray.com without separate service configuration:

```sh
enumeray auth login
enumeray auth whoami
```

Login discovers the service's identity provider automatically; the hosted service
uses auth.enumeray.com. Choose registration in the browser if you need an account,
then complete email verification and sign in. If onboarding exceeds the login
wait, finish registration and run `enumeray auth login` again. Continue with the
[hosted trial](https://doc.enumeray.com/start/first-run/) to run HUXt or REBOUND
on the supplied connector.

For another service, save its administrator-supplied origin in a separate profile:

```sh
enumeray --profile research auth configure https://research.example.org
enumeray --profile research auth login
```

Browser login uses PKCE and a temporary loopback callback. `--no-browser` prints
the URL for a browser on the **same host**; a laptop browser cannot complete a
login waiting on an SSH/HPC host's loopback listener. For remote CLI use with the
hosted service, create a scoped personal token from a signed-in account and import
it privately:

```sh
enumeray auth token-create analysis-script \
  --scopes models:read,runs:create,runs:read,me:read --output token.json
jq -er '.token' token.json | enumeray --profile analysis auth login --token-stdin
enumeray --profile analysis auth whoami
```

For another service, configure the `analysis` profile with that same service's
origin before importing its token. An unconfigured named profile uses
https://enumeray.com even when a different profile selects another service.

Import only the `token` value. Add artifact/upload scopes for files and
`runs:cancel` for cancellation. Tokens retain the account's resource restrictions;
`me:read` verifies the identity during stdin login and checkpoint recovery. The
CLI defaults to a 30-day token lifetime; renew personal tokens by issuance rather
than browser refresh. Inspect `auth token-list` and revoke unused tokens with
`auth token-revoke TOKEN_UUID`. After a lost creation reply, inspect inventory
and revoke uncertain credentials before deliberately creating another.

Profiles and credential state are private. `--profile` selects a named service
(default `default`); `--config` selects another profiles file. Origin selection
uses `--api` first, then the selected saved profile, then https://enumeray.com
when that profile has not been configured. Existing profiles, including
api.enumeray.com, keep their saved origins. A failed connection never selects a
different service. Configure an alternate origin before importing its token.
API origin changes clear saved credentials, and `--api` never forwards a profile token to a different
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

The current alpha supports fresh evaluations and recovery with the **same
selected build**, not in-place migration between arbitrary development revisions.
`persistent-v1` is still a candidate. Legacy record readers and login-profile
conversion handle their explicitly supported formats; they do not establish
binary, database, connector-journal or compute-session migration. A model
publication rollback is also separate from rolling back Enumeray itself.

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

Fresh WIP installations use only `enumeray`; the former `enumeray-connector`
executable is not supported. Installation and removal do not alter privately
managed state. The [connector recovery guidance](../connector/README.md#enrollment-and-service-operation)
explains same-build interruption recovery. Service operators coordinate schema,
backup and restore decisions under their service runbook. Package installation,
matched build evidence and successful fixtures are separate from scientific
validation, native service lifecycle and production-site qualification.

The [documentation project](https://doc.enumeray.com/)
provides role-based pages and current command/API references. Its current site
may describe a newer build; use its visible source stamp and identity download
before combining it with retained historical evidence.
