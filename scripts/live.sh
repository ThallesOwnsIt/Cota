#!/usr/bin/env bash
# Cota live mode.
#
#   scripts/live.sh alerts    3 nodes check INMET's warnings; the aggregator anchors the new ones
#   scripts/live.sh daily     anchors each missing closed UTC day, attests the forecast rain,
#                             forecasts discharge and issues alerts
#   scripts/live.sh publish   audits against the chain, builds the dashboard data and publishes it
#   scripts/live.sh all       all three, in that order
#
# Variables: RPC, CLUSTER, PAYER, BACKFILL_DAYS (earlier days to anchor on the 1st run, default 7),
#            DEPLOY (1 = publish to Vercel at the end of publish)
set -euo pipefail
cd "$(dirname "$0")/.."

export RPC=${RPC:-https://api.devnet.solana.com}
export CLUSTER=${CLUSTER:-devnet}
export PAYER=${PAYER:-$HOME/.config/solana/id.json}
BACKFILL_DAYS=${BACKFILL_DAYS:-7}
DEPLOY=${DEPLOY:-0}
BIN=target/release
# Local credentials (ANA_IDENTIFICADOR, ANA_SENHA); in GitHub Actions they come from the secrets.
# Read as literal KEY=value, without executing the file: a password with $ or quotes arrives intact.
if [[ -f .env ]]; then
  while IFS='=' read -r k v; do
    [[ $k =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] && export "$k=$v"
  done < .env
fi

cargo build -q --release -p cota-oracle --bins

alerts() {
  local pids=()
  for i in 1 2 3; do
    $BIN/cota-alerts observe --key "keys/node-$i.json" --out "data/nodes/node-$i/alerts" &
    pids+=($!)
  done
  for p in "${pids[@]}"; do wait "$p"; done
  $BIN/cota-alerts anchor --rpc "$RPC" --cluster "$CLUSTER" --payer "$PAYER"
}

daily() {
  local days
  mapfile -t days < <(python3 -I scripts/next_day.py --all)
  if [[ ${#days[@]} -eq 0 ]]; then
    echo "nada a ancorar: o ledger está em dia"
    return
  fi
  OUTLOOK=1 CONFIG=config/stations-live.toml FAULTS=/dev/null scripts/replay.sh "${days[0]}" "${days[-1]}"
}

publish() {
  $BIN/cota-audit --rpc "$RPC"
  python3 -I scripts/export_dashboard.py
  if [[ $DEPLOY == 1 ]]; then
    (cd dashboard && npx --yes vercel@latest deploy --prod --yes)
  fi
}

case ${1:-all} in
  alerts) alerts ;;
  daily) daily ;;
  publish) publish ;;
  all) alerts; daily; publish ;;
  *) echo "uso: $0 [alerts|daily|publish|all]" >&2; exit 2 ;;
esac
