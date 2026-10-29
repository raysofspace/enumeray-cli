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

The driver matrix covers process/direct, process/SSH, Slurm/direct and Slurm/SSH.
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

Service authentication, API/database persistence, object-storage conformance and
coordinated service restoration are qualified separately by the service operator.
These require an authorized service installation and are not prerequisites for
building or testing the independent public workspace.

See the [contributor guide](../../../../CONTRIBUTING.md#model-and-connector-fixture-coverage)
and [author guide](../../../cli/AUTHOR_GUIDE.md).
