# syntax=docker/dockerfile:1
#
# termlyx-server image. Build context is the repository root (or the trimmed
# tree that scripts/deploy-server.sh sends to the build host, which has the
# same layout): `docker compose -f server/compose.yml build`.

# ---- build -----------------------------------------------------------------
# Same Rust version as rust-toolchain.toml. Pinned by digest (tag: rust:1.100.0-trixie);
# bump both together on purpose.
FROM rust:1.100.0-trixie@sha256:223cc32ddca8f3ee90c436dbc94b6b5f052ea7f9653c5201e9612f08ad7d21b8 AS build

# Small build hosts (the target VM has 2 vCPU / 2-4 GB): two jobs, no LTO,
# many codegen units, no symbols. Lower peak memory, slightly larger binary.
ENV CARGO_BUILD_JOBS=2 \
    CARGO_PROFILE_RELEASE_LTO=false \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
    CARGO_PROFILE_RELEASE_STRIP=symbols

WORKDIR /build
COPY Cargo.lock ./
COPY crates/core-proto crates/core-proto
COPY server server
# The server-only workspace manifest is the root manifest here.
COPY server/deploy/Cargo.toml ./Cargo.toml

# Both steps are no-ops on the tree the deploy script stages (it already did
# them locally); they make a plain checkout build too. The metadata call prunes
# Cargo.lock entries of crates outside this workspace without changing any
# version, so the build below can insist on `--locked`.
RUN sh server/deploy/strip-dev-deps.sh server/Cargo.toml crates/core-proto/Cargo.toml \
 && cargo metadata --format-version 1 >/dev/null

# Per-platform id: a multi-arch build must not share target/ between arches.
ARG TARGETPLATFORM
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,id=target-$TARGETPLATFORM,target=/build/target \
    cargo build --release --locked -p tether-server \
 && install -D target/release/termlyx-server /out/termlyx-server

# ---- runtime ---------------------------------------------------------------
# Pinned by digest (tag: debian:trixie-slim); bump on purpose.
FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --uid 10001 --user-group --no-create-home --shell /usr/sbin/nologin termlyx \
 && mkdir /data \
 && chown termlyx:termlyx /data

COPY --from=build /out/termlyx-server /usr/local/bin/termlyx-server
# Old name, so existing `exec tether-server ...` commands keep working.
RUN ln -s termlyx-server /usr/local/bin/tether-server

ENV TERMLYX_DATA=/data
USER termlyx
WORKDIR /data
# Declared after the chown so a fresh named volume inherits termlyx ownership (uid/gid 10001, unchanged, so old volumes stay writable).
VOLUME /data
EXPOSE 8443

# `health` talks to the local listener, trusting only the certificate in
# /data/tls (no internet, no DNS).
HEALTHCHECK --interval=30s --timeout=10s --start-period=20s --retries=3 \
    CMD ["termlyx-server", "health"]

ENTRYPOINT ["termlyx-server"]
CMD ["serve"]
