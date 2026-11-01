# Enumeray CLI

Enumeray makes installed scientific models available through a self-service CLI
and HTTP API. This repository owns the independent `enumeray` executable, typed
HTTP client, outbound connector, compute runner and shared wire contracts.

One executable supports model users, model authors and connector owners.
It connects to an authorized Enumeray service. Building and local tests require
no service checkout, database credentials or running API.

| Your goal | Start here |
| --- | --- |
| Install, sign in and select a service | [Installation guide](crates/cli/INSTALL.md) |
| Submit and collect a reviewed analysis | [Analysis guide](crates/cli/ANALYSIS_GUIDE.md) |
| Integrate with structured CLI output | [CLI JSON contracts and transcripts](crates/cli/CLI_AUTOMATION.md) |
| Prepare, test and publish a model | [Author guide](crates/cli/AUTHOR_GUIDE.md) and [examples](crates/cli/examples/README.md) |
| Connect approved compute and trial a first model | [Guided setup](crates/cli/SETUP_GUIDE.md) and [connector guide](crates/connector/README.md) |
| Extend a driver or connection | [Extension guide](crates/connector/EXTENDING.md) |
| Develop and verify the public workspace | [Contributor guide](CONTRIBUTING.md) |
| Follow role-based service guidance | [Web documentation](https://doc.enumeray.com/) |

## Install a qualified package

**Alpha: 0.1.0-alpha.1.** Commands and APIs may change. Alpha updates may require
fresh installations; in-place upgrade compatibility is not promised. Preserve
an existing installation's state and matching binaries before changing versions.

One native executable supplies the CLI, connector and compute runner. Qualified
releases support macOS 15+ and Linux with glibc 2.28+, each on Intel/x86-64 and
ARM64. Installation starts no service and needs no Python runtime.

The [installation guide](crates/cli/INSTALL.md) covers the project Homebrew tap,
and explicit-version signed binary archives for direct/offline/SSH installation. Direct
installation uses ordinary verification, extraction and file-copy commands;
there is no custom installer or self-updater. Channel availability remains
subject to release qualification and each channel's publication status.

## Build from source

Use the pinned Rust toolchain, including Cargo, rustfmt and Clippy. From this
repository root on macOS or Linux:

```sh
cargo build --locked --release -p enumeray-cli --bin enumeray
mkdir -p "$HOME/.local/bin"
install -m 755 target/release/enumeray "$HOME/.local/bin/enumeray"
export PATH="$HOME/.local/bin:$PATH"
enumeray --version
```

The current source is unreleased. Version 1 alone does not establish compatibility
between revisions. Use a qualified published release when available; coordinate
connector and compute-runner upgrades with service maintenance and retained job
evidence. Each target and distribution channel needs its own qualification;
passing local source tests does not establish package availability.

## Source ownership

| Component | Responsibility |
| --- | --- |
| `crates/cli` | User commands, author workflows, connector commands and the unified executable |
| `crates/client` | Typed HTTP requests, login, transfer and observation helpers |
| `crates/connector` | Durable job ownership, transfers, recovery and process/Slurm execution over direct access or SSH |
| `crates/contracts` | Shared request/response types, schemas and validation |
| `examples`, `release/fixtures` | Credential-free model examples and retained edge compatibility fixtures |
| `scripts`, `distribution` | Verification, binary packaging, archive checks and channel delivery |

These sources are canonical. The managed service consumes a pinned revision;
changes are made here and then integrated into the service. The service operator
separately qualifies authentication, persistence, object storage and complete
service execution against the matching client artifacts.

Connectors initiate contact with the service; no inbound service connection to
compute is required. Model owners supply installed scientific software and test
its scientific results. The included scalar model demonstrates the protocol.

## License

Enumeray CLI, client, connector and contracts are licensed under
[Apache License 2.0](LICENSE). Third-party dependencies retain their own licenses.
