#!/usr/bin/env bash
# Day-by-day replay: 3 independent nodes sign, the aggregator anchors the batch (and emits
# SourceDegraded), then the forecaster reads the ledger and records P10/P50/P90 (+ alert) on-chain.
#
# Usage: scripts/replay.sh <first-day> <last-day>
# Variables: RPC (default local), CLUSTER, PAYER, FAULTS (config/faults.csv), NODES (1 2 3),
#            FORECAST (1 = forecast after each batch; 0 = batches only), CONFIG (station catalog),
#            OUTLOOK (1 = attest the forecast rain after each batch; live only), LEDGER (data/ledger)
set -euo pipefail
cd "$(dirname "$0")/.."

FROM=${1:?primeiro dia, ex.: 2024-04-20}
TO=${2:?último dia, ex.: 2024-05-25}
RPC=${RPC:-http://127.0.0.1:8899}
CLUSTER=${CLUSTER:-custom&customUrl=http%3A%2F%2Flocalhost%3A8899}
PAYER=${PAYER:-keys/local-payer.json}
FAULTS=${FAULTS:-config/faults.csv}
NODES=${NODES:-1 2 3}
FORECAST=${FORECAST:-1}
OUTLOOK=${OUTLOOK:-0}
CONFIG=${CONFIG:-config/stations.toml}
LEDGER=${LEDGER:-data/ledger}
BIN=target/release

cargo build -q --release -p cota-oracle --bins

fault_for() { # node, day -> none | silent | tamper
  local node=$1 day=$2
  if [[ -f $FAULTS ]]; then
    while IFS=, read -r n f a b; do
      [[ -z $n || $n == \#* ]] && continue
      if [[ $n == "$node" && ! $day < $a && ! $day > $b ]]; then echo "$f"; return; fi
    done < "$FAULTS"
  fi
  echo none
}

day=$FROM
while [[ ! $day > $TO ]]; do
  echo "=== $day"
  pids=()
  for i in $NODES; do
    $BIN/cota-node --key "keys/node-$i.json" --day "$day" --rpc "$RPC" \
      --config "$CONFIG" --out "data/nodes/node-$i" --fault "$(fault_for "node-$i" "$day")" &
    pids+=($!)
  done
  for p in "${pids[@]}"; do wait "$p"; done
  $BIN/cota-aggregator --rpc "$RPC" --cluster "$CLUSTER" --payer "$PAYER" --config "$CONFIG" --ledger "$LEDGER"
  if [[ $OUTLOOK == 1 ]]; then
    # Forecast rain: only valid right after the day's batch, before the next batch.
    for i in $NODES; do
      $BIN/cota-outlook observe --key "keys/node-$i.json" --day "$day" --out "data/nodes/node-$i/outlook" || true
    done
    $BIN/cota-outlook anchor --rpc "$RPC" --cluster "$CLUSTER" --payer "$PAYER" --ledger "$LEDGER"
  fi
  if [[ $FORECAST == 1 ]]; then
    rm -f "data/forecasts/$day.json"
    (cd ml && uv run -q python -m cota_ml.forecast --day "$day" --ledger "../$LEDGER")
    if [[ -f data/forecasts/$day.json ]]; then
      $BIN/cota-admin --rpc "$RPC" --payer "$PAYER" --ledger "$LEDGER" commit-forecast --file "data/forecasts/$day.json" --cluster "$CLUSTER"
    fi
  fi
  day=$(date -u -d "$day + 1 day" +%F)
done
