#!/usr/bin/env bash
# Builds verifier/cota_verify.wasm, the browser's copy of the v3 ledger rules (crates/cota-verify-wasm), so that
# anyone with the same toolchain gets the same bytes: GitHub rebuilds it on every change and fails if they differ
# (.github/workflows/verifier.yml). The SHA-256 goes to verifier/cota_verify.wasm.sha256.
#
#   bash verifier/build-wasm.sh          # build and write the file
#   bash verifier/build-wasm.sh --check  # build and fail if the committed file differs
set -euo pipefail
cd "$(dirname "$0")/.."
TOOLCHAIN=1.99.1
if ! rustc "+$TOOLCHAIN" --version >/dev/null 2>&1; then
  echo "precisa do Rust $TOOLCHAIN: rustup toolchain install $TOOLCHAIN --target wasm32-unknown-unknown" >&2
  exit 2
fi
# Paths of this machine must not end up in the file (panic messages carry source paths).
export RUSTFLAGS="--remap-path-prefix=$PWD=/cota --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo -C strip=symbols"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}/wasm-verifier"
cargo "+$TOOLCHAIN" build --locked --release --target wasm32-unknown-unknown -p cota-verify-wasm
built="$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/cota_verify_wasm.wasm"
sha=$(sha256sum "$built" | cut -d' ' -f1)
if [[ ${1:-} == --check ]]; then
  committed=$(cut -d' ' -f1 verifier/cota_verify.wasm.sha256)
  if [[ $sha != "$committed" ]] || ! cmp -s "$built" verifier/cota_verify.wasm; then
    echo "verifier/cota_verify.wasm não corresponde ao código: montado $sha, no repositório $committed" >&2
    exit 1
  fi
  echo "cota_verify.wasm confere com o código ($sha)"
else
  cp "$built" verifier/cota_verify.wasm
  echo "$sha  cota_verify.wasm (Rust $TOOLCHAIN)" > verifier/cota_verify.wasm.sha256
  echo "verifier/cota_verify.wasm $(stat -c %s verifier/cota_verify.wasm) bytes, SHA-256 $sha"
fi
