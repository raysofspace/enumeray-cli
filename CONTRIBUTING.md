# Contributing to Enumeray CLI

This public workspace is the canonical source for the unified executable,
client, connector and contracts. Update source here; service integrations consume
a pinned revision. User and connector workflows start in the [README](README.md).

## Feature development

Feature branches and PRs are the default. An explicit user request to work on
`main` selects the [main workflow](#explicit-work-on-main) instead. In the PR flow,
commits and pushes may proceed within the task's scope without separate
confirmation. In the explicit `main` flow, do not commit or push until the user
explicitly authorizes those actions. Editing and verification can proceed, with
changes left uncommitted for review. Choosing `main` alone does not authorize
commits or pushes; respect any separate limits on committing and pushing.

1. Inspect the workspace, preserve existing work and choose the actual target
   branch/ref, normally `origin/main`. For the default flow, fetch it and create a
   `feature/<name>` branch. Use a separate workspace for each concurrent feature;
   never silently replace a missing comparison ref.
2. Change the canonical public sources, add focused regressions and update the
   owning guides and fixtures. Regenerate affected derived documentation in the
   consuming repository. Service implementation and integration coverage belong
   in the service repository.
3. Preview and run affected profiles under [build and local checks](#build-and-local-checks),
   including required backend fixtures. Register new suites and ignored tests in
   the catalogue and validate it after changing suite structure. Keep large-scale
   qualification explicitly selected.
4. In the PR flow, commit public changes before the service records their SHA
   and push public changes before the service push. For a coordinated feature in
   the default flow, link the related PRs and merge the public PR first.
   The service then pins the final merged public SHA, accounting for squash or
   rebase merges, and verifies the final pair before its PR merges. A public-only
   feature can be adopted by the service later through a verified pin update.
5. After the relevant PRs merge or authorized direct pushes finish, preserve
   remaining work, switch to local `main` and fast-forward it with
   `git pull --ff-only origin main`. After each PR merge, remove its feature branch
   both locally and remotely, after confirming it has no later unmerged work.
   Squash merges can make Git's local merged-branch check inconclusive; confirm
   the merged PR and its final head before deleting the branch. Skip branch/PR
   cleanup in the explicit `main` flow unless separately requested. Archive
   temporary managed worktrees after preserving their work and inspect the
   checkout for leftovers.

When developing this repository as a service submodule, follow the service's
workflow as well. After synchronizing both local `main` branches, run
`git submodule update --init --recursive` from the parent repository. Leave the
public working checkout at its recorded pin, even when public `main` has newer
commits; a detached HEAD is normal. Adopting a newer public revision requires
another verified service pin update through the selected workflow. Ordinary
development and pin updates do not qualify new release artifacts; publication
requires separate qualification of the selected service/public revisions and exact
final artifacts.

### Explicit work on main

When the user explicitly requests this flow, use the existing checkout, preserve
local work and synchronize before editing:

```sh
git switch main
git pull --ff-only origin main
```

Do not create feature branches, PRs or new worktrees for that task. Record the
starting SHA before the first task commit; retain the corresponding service base
for service verification even after a push advances its `origin/main`. Run the
same affected profiles, regressions and integration checks as in the PR flow.
Leave changes uncommitted until the user authorizes committing. Once authorized,
commit directly on `main`; if pushing is authorized, push public `main` before the
service pushes a verified pin to that exact public commit. Use normal pushes. If
direct pushes are blocked, report the constraint without creating a PR or
force-pushing `main`.

## Build and local checks

Use the pinned [Rust toolchain](rust-toolchain.toml). Python 3.9+ is needed for
wrapper and generated starter tests; socket/process tests require macOS or Linux
with loopback networking and process inspection. From the repository root:

```sh
cargo build --locked -p enumeray-cli --bin enumeray
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-targets --all-features
cargo test --locked -p enumeray-contracts --lib --no-default-features
cargo test --locked --workspace --doc --all-features
python3 -m unittest discover -s examples/scalar_model -p 'test_*.py'
python3 -m unittest discover -s examples/collections -p 'test_*.py'
python3 -m unittest discover -s scripts -p 'test_*.py'
```

The public selector also provides focused check groups:

```sh
scripts/verify.sh plan --area native-clients
scripts/verify.sh native-clients
scripts/verify.sh native-platform
scripts/verify.sh connector
scripts/verify.sh python
scripts/verify.sh validate
```

`static`, `native-clients`, `native-platform`, `connector` and `python` are ordinary
profiles; `full` runs all ordinary public checks. `native-platform` adds connector
library and compute-runner tests to the client checks, covering native process,
filesystem, cleanup and recovery behavior. `connector` also runs the connector
host, service and host-enrollment integration suites.

Public CI runs `full` on Linux x86-64 and `native-platform` on Linux ARM64 and
both macOS CPUs. Formatting, static analysis, Python tooling and the connector
integration suites run on Linux x86-64. The macOS 15 checks exercise native
behavior; release qualification separately requires actual macOS 15 on each CPU,
the packaged archive and Homebrew lifecycle checks.
CI caches Rust dependencies separately for each runner image, CPU and profile,
including after test failures. It disables debug symbols and incremental builds
and limits Rust test concurrency to two threads. Superseded pull-request runs
are cancelled.

`backends` explicitly runs the bounded real SSH/Slurm fixture; `mounts` explicitly
runs the native Linux bind-mount regression. Register new suites and ignored
tests in `scripts/test-strategy.json` and run `scripts/verify.sh validate` after
changing their structure.

Add focused regressions for behavior changes. Preserve deterministic race,
authorization, cancellation, rollback and recovery coverage. Ordinary tests use
mock HTTP peers, temporary directories and local processes; no API implementation
or database is required. Tests using a real scheduler remain explicitly selected.

## Model and connector fixture coverage

Generated starter tests cover Python and executable adapters with JSON and file
I/O. The standalone scalar and collection wrappers test declared protocol
boundaries. Connector/runner tests preserve launch identities, durable journal
ownership, exact transfers, confinement, cancellation and recovery after lost
replies. A lost acknowledgement must not authorize another scientific launch.

The [real backend fixture](crates/connector/tests/real-backends/README.md) exercises
all four process/Slurm and direct/SSH combinations with separate execution
accounts. Run it when driver, connection, runner or shared execution contracts
change. It requires Docker Compose and actual Slurm/OpenSSH in its disposable
container; mock scheduler replies alone do not qualify a driver. Large scientific
workload, capacity and site-specific qualification remain separate activities.

The service operator retains authenticated service/database/storage acceptance,
coordinated restoration and service upgrades. Supply the exact client revision
and binary checksums for that integration gate; a public unit-test pass does not
establish service compatibility.

## Documentation ownership

Edit the canonical guides in `crates/cli` and `crates/connector`, examples under
`examples` and CLI starter templates alongside their consumers. The web
documentation imports the pinned guides and examples; generated pages/downloads
are derived outputs. Keep one authored source for each guide and fixture.

Customer guides contain user, author and connector workflows. Keep private
service administration and deployment procedures in the service repository.
Public guide links must resolve inside this repository or point to published
customer documentation. Use explicit working directories, prerequisites and
reviewed placeholders in copyable recipes.

## Binary archives

Build the native release binary for the actual target triple first. Python 3.12+
is required for safe archive extraction during qualification. From this root:

```sh
cargo build --locked --release --target "$TARGET_TRIPLE" -p enumeray-cli --bin enumeray
python3.12 scripts/package-binaries.py --target "$TARGET_TRIPLE" --output /tmp/enumeray-archives
python3.12 scripts/qualify-binary-archives.py --archives /tmp/enumeray-archives --target "$TARGET_TRIPLE"
```

The package contains one executable, compatibility metadata, guides, examples and
generated starter projects. The qualifier checks SHA-256 integrity, clean-host
help/version behavior, standalone guide links, journal identity and executable
starter projects outside the checkout. Packaging rejects server libraries in
normal runtime dependencies. Archive qualification does not establish a complete
service release; publication also requires the matching service integration gate.

## Channel delivery

See [distribution maintenance](distribution/README.md) for the shared signed
manifest and Homebrew formula. Generate updates only
from a manifest accepted by the release delivery verifier. The generator is pure;
the publication helper defaults to a local plan and requires `--submit` plus a
separately scoped channel token before it creates a GitHub branch or pull request.

Use the project's configured tap. Formula updates require maintainer review
and merge after native tap checks pass. Direct downloads and Homebrew are the
supported distribution channels.

Build-tool Python is not an end-user installation dependency. Keep direct
installation transparent: signature/checksum verification, archive extraction,
staging beside the destination, atomic rename and explicit daemon maintenance.
Changing package delivery must preserve profiles, credentials and connector
journals and must not introduce startup hooks or a self-updater.
