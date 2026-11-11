#!/usr/bin/env bash
# Cota's live round on this machine, for tests: the same scripts/live/round.sh that the server and GitHub Actions
# run, with the keys in keys/ (node-1.json … node-3.json, operator.json) and the ANA login from .env.
#
#   scripts/live.sh            the whole round: 3 nodes, anchor every closed day waiting, audit and dashboard data
#   scripts/live.sh plan       only the day the chain is waiting for
#   scripts/live.sh node 2     only node 2
#
# Nothing permanent or shared happens by default: no Bitcoin stamp, no Arweave copy, no commit (PUSH=0). Anchoring
# does write to the chain, like any round. Variables: see scripts/live/round.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
# Local credentials (ANA_IDENTIFICADOR, ANA_SENHA). Read as literal KEY=value, without executing the file: a
# password with $ or quotes arrives intact.
if [[ -f .env ]]; then
  while IFS='=' read -r k v; do
    [[ $k =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] && export "$k=$v"
  done < .env
fi
cargo build -q --release -p cota-oracle --bins
SECRETS=${SECRETS:-keys} PUSH=${PUSH:-0} exec scripts/live/round.sh "${@:-all}"
