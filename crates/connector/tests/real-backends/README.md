# Real SSH and Slurm driver verification

This fixture exercises the independent Enumeray connector and compute runner
against actual OpenSSH, Munge, MariaDB and Slurm, including scheduler accounting.
Run from the public repository root with Docker Compose available:

```sh
docker compose -p enumeray-backend-acceptance \
  -f crates/connector/tests/real-backends/compose.yaml up --build \
  --abort-on-container-exit --exit-code-from slurm
docker compose -p enumeray-backend-acceptance \
  -f crates/connector/tests/real-backends/compose.yaml down --volumes
```

The disposable single-node fixture publishes no host ports and requires no host
filesystem mounts, Docker socket or privileged container. The connector runs as
an unprivileged account; model processes use a separate execution account through
a restricted launcher or SSH. Its process-based Slurm containment is a test
configuration; sites must qualify their actual isolation, shared filesystem and
scheduler configuration before admitting real work.

The image supplies `systemd-analyze` and `visudo` to validate generated Linux
service and runner rules with their native parsers. Native macOS host-plan tests
require `plutil` and `visudo`, registered in the public verification catalogue.
Parser checks do not rehearse service start/stop/restart; managed Linux and macOS
service lifecycle qualification requires disposable native hosts.

The driver matrix covers process/direct, process/SSH, Slurm/direct and Slurm/SSH.
Each path first probes host prerequisites without a model descriptor or scientific
entrypoint. Slurm preflight only reads tools, queue and accounting; compute-node
visibility and allocation execution remain separately unverified in its report.
It checks scalar and file wrappers, generated author projects, exact output
bytes, credential isolation, interrupted transfers, journal recovery, rejected or
ambiguous submissions, retained scheduler identity, cancellation and replay
refusal. Losing contact never authorizes another scientific launch. Deterministic
runner and connector tests complement the actual-backend matrix.

The driver-only Compose service uses a private 512 MiB tmpfs for verbose capture
and model workspaces. This checks process interruption and recovery; it does not
qualify power-loss durability or persistent-storage throughput. Startup polls
MariaDB, accounting and the Slurm node with finite readiness deadlines. Keep
Docker available until cleanup completes.

## File-wrapper workload profiles

`file-wrapper.py` accepts an optional `inputs.fixture_profile` in its request.
Omitting it keeps the standalone driver's full `coverage` workload. Profiles
change test payload size and synchronization; they do not relax execution-account,
credential-isolation, output-confinement or exact-result checks.

| Profile | Capture and scratch workload | Completion |
| --- | --- | --- |
| `coverage` (default) | More than 2 MiB each on stdout and stderr; when scratch coverage is enabled, a 4 MiB intermediate file, 600 small files, nested directories and a symlink. | Uses the descriptor's configured delay, then writes the result. |
| `coverage-controlled` | The same full workload. | Waits for an explicit continuation marker. |
| `functional` | Small stdout/stderr captures and no bulk scratch files. | Writes the result without an artificial delay. |
| `controlled` | The same small workload. | Waits for an explicit continuation marker. |

Controlled profiles write `.acceptance-ready` in the execution workspace only
after their logs and scratch setup are ready. The harness observes that marker
before exercising maintenance, disconnect/recovery or cancellation. Normal
completion requires `.acceptance-continue` in that same workspace; cancellation
must stop the waiting execution without releasing it. The gate fails after
180 seconds by default, rather than waiting indefinitely.

The service acceptance harness uses `coverage-controlled` for its normal
process/direct, process/SSH, Slurm/direct and Slurm/SSH cases. All four retain
large-log retention, complete log archive and bounded multi-pass cleanup checks.
Candidate trials use `functional`; cancellation cases use `controlled` and retain
their cancellation, recovery and single-launch assertions. Avoid adding a full
capture workload or fixed sleep to a scenario unless its assertions need them.

Fixture-only environment overrides can bound log bytes, scratch bytes, scratch
entry count and the gate deadline. Invalid or excessive values fail the wrapper;
they are not production model tuning options. The service repository may also
prepare fixture images once and share their immutable image IDs across isolated
Compose projects. That reuse verifies the selected service/public sources,
image identity and Docker daemon/platform before suppressing builds. Standalone
commands retain their normal build and source checks when no prepared manifest
is supplied.

Service authentication, API/database persistence, object-storage conformance and
coordinated service restoration are qualified separately by the service operator.
These require an authorized service installation and are not prerequisites for
building or testing the independent public workspace.

See the [contributor guide](../../../../CONTRIBUTING.md#model-and-connector-fixture-coverage)
and [author guide](../../../cli/AUTHOR_GUIDE.md).
