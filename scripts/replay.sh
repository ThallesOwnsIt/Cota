#!/usr/bin/env bash
# Replay dia a dia: 3 nós independentes assinam, o agregador ancora o lote (e emite
# SourceDegraded), depois o previsor lê o ledger e grava P10/P50/P90 (+ alerta) on-chain.
#
# Uso: scripts/replay.sh <primeiro-dia> <último-dia>
# Variáveis: RPC (padrão local), CLUSTER, PAYER, FAULTS (config/faults.csv), NODES (1 2 3),
#            FORECAST (1 = prever após cada lote; 0 = só lotes), CONFIG (catálogo de estações)
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
CONFIG=${CONFIG:-config/stations.toml}
BIN=target/release

cargo build -q --release -p cota-oracle --bins

fault_for() { # nó, dia -> none | silent | tamper
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
  $BIN/cota-aggregator --rpc "$RPC" --cluster "$CLUSTER" --payer "$PAYER" --config "$CONFIG"
  if [[ $FORECAST == 1 ]]; then
    rm -f "data/forecasts/$day.json"
    (cd ml && uv run -q python -m cota_ml.forecast --day "$day")
    if [[ -f data/forecasts/$day.json ]]; then
      $BIN/cota-admin --rpc "$RPC" --payer "$PAYER" commit-forecast --file "data/forecasts/$day.json" --cluster "$CLUSTER"
    fi
  fi
  day=$(date -u -d "$day + 1 day" +%F)
done
