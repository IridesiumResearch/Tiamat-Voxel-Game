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

# The database, the daily backups and the logs live here. Dokploy mounts a
# volume over it; without one the whole registry forgets itself on redeploy.
ENV REGISTRY_DATA=/data
ENV REGISTRY_BIND=0.0.0.0:8080
VOLUME ["/data"]
EXPOSE 8080

# No HEALTHCHECK instruction: the image has no curl, and Dokploy's own HTTP
# check asks `GET /healthz`, which is a better test than a shell one-liner.

ENTRYPOINT ["/usr/local/bin/registry"]
CMD ["serve"]
