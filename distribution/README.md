# Distribution maintenance

All channels derive from one qualified published release of
[`raysofspace/enumeray-cli`](https://github.com/raysofspace/enumeray-cli/releases).
The signed release manifest binds the public revision, source tree, lockfile,
compatibility baseline and all archive hashes. Direct downloads retain the
complete verified archive; no end-user installer or Python runtime is involved.

The release delivery gate must verify the manifest's Sigstore signature, matching
service authorization, all four native platform reports and public release state
before generating or submitting channel updates. The generator is not an
alternative release authorization mechanism.

## Generate and inspect

From the public repository root, with an already verified manifest:

```sh
python3 scripts/generate-channels.py --manifest /path/to/release-manifest.json \
  --output /tmp/enumeray-channel-delivery
python3 scripts/publish-channels.py --delivery /tmp/enumeray-channel-delivery \
  --channel homebrew
```

`channel-plan.json` records separate channel readiness and exact generated file
digests. Identical generation is repeatable; conflicting existing package bytes
are rejected. Use a fresh delivery directory when changing maintainer or channel
configuration. This does not replace a published version or its immutable assets.

The Homebrew output contains one macOS formula with Apple Silicon/Intel archive
URLs and SHA-256 hashes. Its local test exercises help, version reporting and
credential-free journal initialization/inspection. It installs no service.

The Conda output builds the immutable source archive with the pinned Rust
toolchain and `Cargo.lock`, using ecosystem compilers/sysroots and
`cargo auditable install`. It checks the archive's source identity and complete
dependency notices, runs `cargo-bundle-licenses`, retains both notice bundles
and the normal dependency tree,
and includes source/compatibility metadata. It downloads no upstream executable.
The recipe supports `linux-64`, `linux-aarch64`, `osx-64` and `osx-arm64`; runtimes
must satisfy glibc 2.28+ or macOS 14+. A generated recipe still needs actual native
package build/install/upgrade/removal qualification and conda-forge review.

To build a generated recipe with the ecosystem builder:

```sh
rattler-build build --recipe /tmp/enumeray-channel-delivery/conda/recipe/recipe.yaml \
  --target-platform osx-arm64 --channel conda-forge
```

Run the four targets on their corresponding native hosts. Keep package-manager
installation qualification distinct from upstream archive qualification. Confirm
that credentials, configuration and initialized journal identities survive an
upgrade/removal and that no daemon starts. Retained active-work upgrades also
require the matching service's recovery/restart rehearsal.

The native macOS release builder also runs the Homebrew channel harness against
the exact candidate archive before staging its release qualification report:

```sh
python3 scripts/qualify-homebrew.py --archive /path/to/enumeray-0.1.0-aarch64-apple-darwin.tar.gz \
  --target aarch64-apple-darwin --version 0.1.0 --output /tmp/homebrew-qualification.json
```

This requires Homebrew and actual macOS 14 on the candidate's native CPU. It uses
a unique temporary tap and keg-only formula with a local archive URL. It disables
automatic updates, unrelated dependency upgrades, analytics and global cleanup;
it never links the test executable over an existing `enumeray`. The harness runs
clean installation, the maintained formula test, an upgrade from formula revision
0 to 1 using the same candidate, another formula test and removal. It checks
private state/journal identities and cleans its own tap/kegs after failure.
The revision upgrade qualifies installation mechanics; actual released-version
compatibility still needs the matching service's upgrade rehearsal. Reports
record the host and whether the minimum environment was qualified. A rehearsal
on macOS 26 or another newer host cannot establish macOS 14 support.

## Configure channels

`channels.json` defaults the Homebrew destination to `raysofspace/homebrew-tap`.
The maintainer must establish that public tap and protect its main branch. A
scoped GitHub App installation token needs contents and pull-request write
access only to the tap. The release repository token alone cannot update another
repository. Formula updates are reviewable pull requests; delivery is complete
after the matching update is merged.

For first conda-forge submission, explicitly configure:

- `mode: "staged-recipes"`, upstream `conda-forge/staged-recipes` and destination
  `recipes/enumeray`;
- `fork_repository` naming the maintainer-owned fork of that upstream;
- a nonempty `maintainers` list containing only consenting GitHub accounts.

The checked-in maintainer list and fork are empty deliberately. Until configured,
the Conda channel reports `blocked-configuration`; Homebrew generation proceeds.
After acceptance, change the Conda configuration to `mode: "feedstock"`, upstream
`conda-forge/enumeray-feedstock`, its maintainer-owned fork and directory `recipe`.
Rerender that feedstock according to conda-forge policy before merging changes.
Keep feedstock maintenance credentials separate from the tap token.

## Release host and credential configuration

The public release workflow needs the following repository variables before
native delivery can run:

- `ENUMERAY_MANYLINUX_2_28_X86_64_IMAGE` and
  `ENUMERAY_MANYLINUX_2_28_AARCH64_IMAGE`: reviewed
  `quay.io/pypa/manylinux_2_28_x86_64@sha256:DIGEST` and
  `quay.io/pypa/manylinux_2_28_aarch64@sha256:DIGEST` image references. Tags alone
  are rejected. Use a real reviewed full digest for each native CPU.
- `ENUMERAY_MACOS14_X86_64_RUNNER` and `ENUMERAY_MACOS14_ARM64_RUNNER`: JSON
  arrays of runner labels. Defaults are
  `["self-hosted","macOS","X64","enumeray-macos14"]` and
  `["self-hosted","macOS","ARM64","enumeray-macos14"]`. Provision actual
  macOS 14, Homebrew and the corresponding CPU; labels alone do not qualify a host.

Configure public repository secrets `ENUMERAY_HOMEBREW_TOKEN` and
`ENUMERAY_CONDA_TOKEN` separately, with only their channel's required repository
contents/pull-request permissions. `channels.json` selects destinations and
consenting maintainers. Missing tokens or Conda setup produce a visible blocked
channel status; they do not establish delivery.

The private service workflow's `CLI_RELEASE_TOKEN` needs public CLI repository
contents-write permission for dispatch/release coordination and administration-read
permission for immutable-release preflight. Keep that token in the private service
repository. Public build and channel jobs never receive it or private service
source. This repository change does not provision runners, install Homebrew,
activate persistent compatibility, qualify missing platforms or publish packages.

## Submit and retry

The workflow invokes one channel per job with only that channel's scoped
`GH_TOKEN`. To submit a reviewed local plan deliberately:

```sh
python3 scripts/publish-channels.py --delivery /tmp/enumeray-channel-delivery \
  --channel homebrew --submit --status /tmp/homebrew-delivery-status.json
```

The helper creates the deterministic `codex/enumeray-vVERSION` branch and a
matching update PR. Existing identical branch/PR content is reused, including
after a lost create response. Different content under that version fails rather
than replacing it. A merged PR reports `delivered`; an open PR reports
`pending-review`; a closed unmerged PR requires maintainer action. It never merges
a PR, creates a repository/fork, force-updates a branch or overwrites release
assets. Status files describe each attempt and are kept outside immutable release
assets. A failing/pending Conda update never rolls back another delivered channel.

For a broken release, retain its immutable evidence, stop further channel
delivery and publish a qualified corrected version. Use the conda-forge broken
package process where necessary; do not silently replace package bytes. Recover
an installed daemon using its qualified release-pair/state recovery procedure.
An older executable alone is not a safe rollback after incompatible state writes.
