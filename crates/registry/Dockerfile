# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
#
# The name registry's image. Two stages: the Rust build, then a runtime with
# the one binary and CA certificates — nothing else, because this service
# answers HTTP and writes SQLite and needs nothing else.
#
# `docs/registry-ops.md` is the deployment guide; the release workflow builds
# and pushes this image as `ghcr.io/iridesiumresearch/tiamat-registry:<version>`.

FROM rust:1.97-bookworm AS build
WORKDIR /src

# Only the manifests and the code the registry can reach. `cargo` still
# resolves the whole workspace, so every member's manifest has to be here, but
# the client's dependencies are never built for this binary.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY spikes ./spikes

# `--locked` refuses to update the lockfile inside the image: the image must be
# the graph the repository committed, not whatever crates.io says today.
RUN cargo build --release -p registry --locked

FROM debian:bookworm-slim

# CA certificates, because outgoing SMTP over TLS verifies the provider's
# certificate; and nothing else, because the service links no C library it
# needs the image to provide.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /src/target/release/registry /usr/local/bin/registry

# Not root. The service parses strangers' bytes and holds the registry key; a
# bug in either should not come with the container's root. A fixed UID so an
# operator can `chown` a volume to it before the first start (see
# docs/registry-ops.md), and `/data` made and owned here so a fresh volume
# inherits that owner.
RUN useradd --system --uid 10001 --user-group --no-create-home registry \
    && mkdir -p /data \
    && chown registry:registry /data
USER registry

# The database, the daily backups and the logs live here. Dokploy mounts a
# volume over it; without one the whole registry forgets itself on redeploy.
ENV REGISTRY_DATA=/data
ENV REGISTRY_BIND=0.0.0.0:8080
VOLUME ["/data"]
EXPOSE 8080

# The binary is its own probe (`registry healthcheck`: GET /healthz over
# loopback, healthy only on a 200, which the service gives only when its
# database answers), because the image has no curl and should not grow one.
# A container that fails three checks in a row is unhealthy, and a Swarm
# service (what Dokploy runs) replaces it; the probe's own timeouts are two
# seconds a step, inside the five here.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD ["/usr/local/bin/registry", "healthcheck"]

ENTRYPOINT ["/usr/local/bin/registry"]
CMD ["serve"]
