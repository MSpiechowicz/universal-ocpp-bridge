# Build from the repository root: docker build -f packaging/compose/Dockerfile --target daemon .
# Cargo.lock and the Rust toolchain pin are shared by all three executable targets.
FROM rust:1.98.0-slim-bookworm@sha256:1469a27c125cb5a3aebfa4f4e4665d935b02fb72cc093b2c974b3d740e43f157 AS build
WORKDIR /workspace
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ crates/
COPY adapters/ adapters/
COPY bins/ bins/
COPY tests/ tests/
RUN cargo build --locked --release --bin uob --bin uob-sim --bin uob-compose-client

# Only one executable is copied into each Debian slim runtime image. The daemon's
# browser assets are embedded at compile time from adapters/management/ui.
FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251 AS daemon
RUN mkdir -p /var/lib/uob /run/uob && \
    chown 10001:10001 /var/lib/uob /run/uob && \
    chmod 0700 /var/lib/uob /run/uob
COPY --from=build /workspace/target/release/uob /usr/local/bin/uob
USER 10001:10001
ENTRYPOINT ["/usr/local/bin/uob"]

FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251 AS simulator
COPY --from=build /workspace/target/release/uob-sim /usr/local/bin/uob-sim
USER 10001:10001
ENTRYPOINT ["/usr/local/bin/uob-sim"]

FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251 AS ems-client
COPY --from=build /workspace/target/release/uob-compose-client /usr/local/bin/uob-compose-client
USER 10001:10001
ENTRYPOINT ["/usr/local/bin/uob-compose-client"]
