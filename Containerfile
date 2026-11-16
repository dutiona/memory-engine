# The memory-engine server image: `podman build --pull=newer -t memory-engine:<tag> .` from a checkout of a tag.
# Both stages name the same Debian release: the binaries link the builder's glibc and OpenSSL.

FROM docker.io/library/rust:1-slim-trixie AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p memory-engine-mcp -p memory-engine-cli

FROM docker.io/library/debian:trixie-slim
LABEL org.opencontainers.image.source="https://github.com/dutiona/memory-engine"
# ca-certificates: TLS to the embedder; sqlite3: read-only VACUUM INTO backups; curl: health probe.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3t64 sqlite3 curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 memory-engine \
    && useradd --uid 10001 --gid 10001 --no-log-init --create-home \
        --home-dir /var/lib/memory-engine --shell /usr/sbin/nologin memory-engine
COPY --from=builder /src/target/release/memory-engine-mcp /src/target/release/memory-engine-cli /usr/local/bin/
# Fails the build if a shared library the binaries or tools need is missing.
RUN memory-engine-mcp --version && memory-engine-cli --version && sqlite3 --version && curl --version
USER 10001:10001
WORKDIR /var/lib/memory-engine
ENTRYPOINT ["memory-engine-mcp"]
