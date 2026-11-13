# syntax=docker/dockerfile:1.7
# Cota's live-mode image: the cota-oracle binaries, Python (uv) for the forecast and the scripts, Node.js for the
# Arweave copy. The repository is not inside: deploy/compose.yml mounts the checkout at /work, so the ledger, the
# scripts and the configuration stay on the host and the image changes only when the Rust code changes.
# Built by .github/workflows/image.yml (amd64 and arm64, published at ghcr.io/<owner>/cota with the tag from
# deploy/image-tag.sh), or locally with COTA_BUILD=1 (deploy/install.sh).

FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY programs programs
# The release profile's fat LTO is there for the size of the on-chain program; the off-chain tools do not need it,
# and without it the build fits in a 2 GB server (with swap).
ENV CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 CARGO_BUILD_JOBS=2
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p cota-oracle --bins \
 && mkdir /out \
 && find target/release -maxdepth 1 -type f -executable -name 'cota-*' -exec cp {} /out/ \;

FROM node:22-bookworm-slim
# libgomp1: OpenMP runtime that LightGBM (the flow model) loads; the slim image lacks it, and without it the model
# fails on import (the VPS lost the forecast of 2026-11-12 this way).
RUN apt-get update \
 && apt-get install -y --no-install-recommends git openssh-client curl ca-certificates libgomp1 \
 && rm -rf /var/lib/apt/lists/* \
 && ldconfig -p | grep -q 'libgomp.so.1'
COPY --from=ghcr.io/astral-sh/uv:0.13 /uv /uvx /usr/local/bin/
# The forecast's Python (ml/.python-version), installed once in the image; it is also the python3 of the scripts.
ENV UV_PYTHON_INSTALL_DIR=/opt/python UV_LINK_MODE=copy
RUN uv python install 3.13 \
 && chmod -R a+rX /opt/python \
 && ln -s "$(uv python find 3.13)" /usr/local/bin/python3
COPY --from=build /out/ /usr/local/bin/
# A fixed uid/gid, the same as the host user `cota` that deploy/install.sh creates: every file the round writes in
# the checkout stays the host's, and ssh (git push with the deploy key) finds a user for that uid.
ARG COTA_UID=10001
ARG COTA_GID=10001
RUN groupadd -o -g "$COTA_GID" cota && useradd -o -m -u "$COTA_UID" -g "$COTA_GID" -s /bin/bash cota
USER cota
ENV BIN=/usr/local/bin
WORKDIR /work
ENTRYPOINT ["scripts/live/round.sh"]
