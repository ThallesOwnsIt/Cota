#!/usr/bin/env bash
# The image tag of this checkout: a hash of everything that goes into the image (deploy/Dockerfile and the Rust
# sources, as committed). The image workflow (.github/workflows/image.yml) and the server compute the same tag, so a
# round always runs with the binaries built from its own commit.
set -euo pipefail
cd "$(dirname "$0")/.."
sum=$(git ls-tree -r HEAD -- Cargo.toml Cargo.lock crates programs deploy/Dockerfile deploy/Dockerfile.dockerignore | sha256sum)
echo "src-${sum:0:12}"
