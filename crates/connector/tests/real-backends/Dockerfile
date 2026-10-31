# syntax=docker/dockerfile:1
FROM rust:1.98.1-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends python3 procps && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml ./Cargo.toml
COPY Cargo.lock rust-toolchain.toml ./
COPY crates/contracts crates/contracts
COPY crates/connector crates/connector
COPY crates/client crates/client
COPY crates/cli crates/cli
COPY examples examples
COPY release release
# A copied source snapshot can predate outputs from another cached build.
# Rebuild this package while retaining its dependency cache.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=enumeray-real-backend-target,target=/build/target,sharing=locked \
    cargo clean --release -p enumeray-connector -p enumeray-cli \
 && cargo build --locked --release -p enumeray-cli --bin enumeray \
 && (cargo test --locked --release -p enumeray-connector --lib --no-run --message-format=json > /tmp/test-artifacts.json || { cat /tmp/test-artifacts.json; exit 1; }) \
 && mkdir /out && cp target/release/enumeray /out/ \
 && cp "$(sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' /tmp/test-artifacts.json)" /out/driver-tests \
 && target/release/enumeray model init generated-json --directory /out/generated-json --driver slurm \
 && target/release/enumeray model init generated-files --directory /out/generated-files --io files --driver slurm \
 && mkdir /out/context-releases \
 && for context in process-direct process-ssh slurm-direct slurm-ssh; do \
      target/release/enumeray model init "context-$context" --directory "/out/context-releases/context-$context" --driver slurm || exit 1; \
    done \
 && sed -i '/#SBATCH --nodes/i #SBATCH --account=research\n#SBATCH --partition=debug\n#SBATCH --clusters=enumeray-acceptance' /out/generated-*/model.sbatch /out/context-releases/*/model.sbatch

FROM debian:bookworm-slim
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    slurm-wlm slurmdbd mariadb-server munge openssh-server openssh-client python3 procps sudo ca-certificates systemd \
 && rm -rf /var/lib/apt/lists/* \
 && useradd -m -u 10001 enumeray && useradd -m -u 10002 model \
 && usermod --shell /bin/sh model && passwd -d model \
 && printf 'enumeray ALL=(model) NOPASSWD: /usr/local/bin/enumeray compute-runner *\n' > /etc/sudoers.d/enumeray-acceptance \
 && chmod 0440 /etc/sudoers.d/enumeray-acceptance
COPY --from=build /out/ /usr/local/bin/
COPY --from=build /out/generated-json /opt/enumeray/generated-json
COPY --from=build /out/generated-files /opt/enumeray/generated-files
COPY --from=build /out/context-releases /opt/enumeray/context-releases
COPY examples/scalar_model /opt/enumeray/scalar-model-v1
COPY crates/connector/tests/real-backends/file-wrapper.py /opt/enumeray/file-model-v1/wrapper.py
COPY crates/connector/tests/real-backends/model.sbatch /opt/enumeray/scalar-model-v1/model.sbatch
COPY crates/connector/tests/real-backends/failure-before-worker.sbatch /opt/enumeray/scalar-model-v1/failure-before-worker.sbatch
COPY crates/connector/tests/real-backends/rejected.sbatch /opt/enumeray/scalar-model-v1/rejected.sbatch
COPY crates/connector/tests/real-backends/worker-preparation-failure.sbatch /opt/enumeray/scalar-model-v1/worker-preparation-failure.sbatch
COPY crates/connector/tests/real-backends/model.sbatch /opt/enumeray/file-model-v1/model.sbatch
COPY crates/connector/tests/real-backends/rejected.sbatch /opt/enumeray/file-model-v1/rejected.sbatch
COPY crates/connector/tests/real-backends/worker-preparation-failure.sbatch /opt/enumeray/file-model-v1/worker-preparation-failure.sbatch
COPY examples/artifact-input.json examples/artifact-output.json /opt/enumeray/examples/
COPY crates/cli/examples/host-setup-linux.json /opt/enumeray/examples/host-setup-linux.json
COPY crates/connector/tests/real-backends/start.sh /usr/local/bin/acceptance-start
COPY crates/connector/tests/real-backends/wait-ready.sh /usr/local/bin/acceptance-wait-ready
COPY crates/connector/tests/real-backends/slurm.conf /etc/slurm/slurm.conf
RUN chmod 0755 /usr/local/bin/acceptance-start /opt/enumeray/scalar-model-v1/wrapper.py /opt/enumeray/file-model-v1/wrapper.py \
 && install -d -m 0700 -o enumeray -g enumeray /var/lib/enumeray /var/lib/enumeray/test-identities \
 && install -d -m 0700 -o model -g model /srv/enumeray-jobs \
 && install -d -m 0755 -o slurm -g slurm /var/spool/slurmctld /var/spool/slurmd /var/log/slurm
ENV ENUMERAY_REAL_BACKENDS=1
ENTRYPOINT ["acceptance-start"]
CMD []
