# Distribution maintenance

Direct downloads and the project Homebrew tap derive from one qualified published
release of [`raysofspace/enumeray-cli`](https://github.com/raysofspace/enumeray-cli/releases).
The signed manifest binds the public revision, tree, lockfile, baseline status,
source archive and all four native archive hashes. Users verify/extract/copy the
native executable with ordinary tools; Python is only a maintainer dependency.

## First alpha

The first release is **0.1.0-alpha.1**, using public tag `v0.1.0-alpha.1` and
private qualification tag `cli-v0.1.0-alpha.1`. Canonical SemVer prereleases and
stable versions are supported; build metadata is rejected to keep artifact
identities unambiguous. GitHub prereleases are never marked as the latest stable
release. All retries and channel downloads verify that GitHub's prerelease flag
matches the signed version.

Alpha updates may require fresh installations, with no in-place upgrade promise.
`persistent-v1` remains a candidate. Candidate snapshots must still validate and
their signed baseline identity includes `status: candidate`; stable publication
requires `status: released` and deliberate baseline activation. Keep an older
alpha's valuable state and original matching binaries together. No installation
or removal command resets user state or starts a daemon.

## Native qualification

The four targets are macOS **15+** on ARM64/Intel and Linux **glibc 2.28+** on
ARM64/x86-64. `scripts/build-release-native.sh` takes explicit source, revision,
target and output arguments and requires the pinned Rust/Cargo versions.

Release jobs use native `macos-15` and `macos-15-intel` hosts. Both run the exact
candidate through archive checks and the isolated Homebrew lifecycle harness:
clean install/formula test, same-candidate formula-revision upgrade, test again,
and removal while preserving private state and the existing global executable.
The harness qualifies packaging mechanics, not compatibility between different
alphas. Rosetta and newer-host smoke do not establish minimum-platform evidence.

Configure these public repository variables with reviewed immutable image digests:

- `ENUMERAY_MANYLINUX_2_28_X86_64_IMAGE`:
  `quay.io/pypa/manylinux_2_28_x86_64@sha256:DIGEST`;
- `ENUMERAY_MANYLINUX_2_28_AARCH64_IMAGE`:
  `quay.io/pypa/manylinux_2_28_aarch64@sha256:DIGEST`.

The Linux workflow verifies the runner, host and Docker daemon CPU before using
its native image. All four final platform reports and matching public source
must pass before staging. Private service acceptance uses the exact extracted
Linux executable, then signs authorization for that final manifest digest.
Publication never rebuilds accepted binaries. Enable immutable releases first.

## Homebrew configuration and delivery

`channels.json` selects `raysofspace/homebrew-tap`, protected branch `main`.
Configure public secret `ENUMERAY_HOMEBREW_TOKEN` with access only to that tap's
contents and pull requests. The tap checks installation, formula tests and
removal on both native macOS 15 architectures. Formula updates are reviewable
PRs; a maintainer merges after checks pass. The single `enumeray` formula carries
the alpha until a later qualified release updates it.

Configure private secret `ENUMERAY_CLI_RELEASE_TOKEN` with public CLI repository
contents-write permission for dispatch/candidate downloads and administration-read
permission for immutable-release preflight. Keep it in the private service
repository. Build jobs receive neither that token nor private source. Keep tap
credentials separate. Do not use a broad personal CLI login token as a CI secret.

The delivery verifier checks signatures, matching preparation/publication
receipts, baseline policy, all native reports and public release state before
channel generation. To inspect an already verified manifest:

```sh
python3 scripts/generate-channels.py --manifest /path/to/release-manifest.json \
  --output /tmp/enumeray-homebrew-delivery
python3 scripts/publish-channels.py --delivery /tmp/enumeray-homebrew-delivery \
  --channel homebrew
```

To submit deliberately, add `--submit --status /tmp/homebrew-delivery-status.json`.
The helper reuses identical `codex/enumeray-vVERSION` branches and PRs, including
a lost create response; conflicting content fails without overwriting it. Open
PRs report `pending-review`, merged PRs `delivered`, and closed unmerged PRs need
maintainer action. It never merges PRs. Missing credentials report a configuration
blocker. Delivery status remains outside immutable release assets.

For a broken release, retain its immutable evidence, stop further channel delivery
and publish a qualified corrected version. Coordinate connector maintenance and
matching runner replacement explicitly. Retain existing state and use the
release-specific recovery procedure; replacing only an executable does not
establish a safe state rollback.
