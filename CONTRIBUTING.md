# Contributing to Enumeray CLI

This public workspace is the canonical source for the unified executable,
client, connector and contracts. Update source here; service integrations consume
a pinned revision. User and connector workflows start in the [README](README.md).

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
scripts/verify.sh connector
scripts/verify.sh python
scripts/verify.sh validate
```

`static`, `native-clients`, `connector` and `python` are ordinary profiles;
`full` runs all ordinary public checks. `backends` explicitly runs the bounded
real SSH/Slurm fixture; `mounts` explicitly runs the native Linux bind-mount
regression. Register new suites and ignored tests in `scripts/test-strategy.json`
and run `scripts/verify.sh validate` after changing their structure.

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
manifest, Homebrew formula and conda-forge source recipe. Generate updates only
from a manifest accepted by the release delivery verifier. The generator is pure;
the publication helper defaults to a local plan and requires `--submit` plus a
separately scoped channel token before it creates a GitHub branch or pull request.

Use the project's configured tap. Conda submission requires an explicitly
configured maintainer-owned fork and consenting recipe maintainers; never infer
those identities from a filesystem username or automatically list someone.
Conda review remains independent of Homebrew and direct archive publication.

Build-tool Python is not an end-user installation dependency. Keep direct
installation transparent: signature/checksum verification, archive extraction,
staging beside the destination, atomic rename and explicit daemon maintenance.
Changing package delivery must preserve profiles, credentials and connector
journals and must not introduce startup hooks or a self-updater.
