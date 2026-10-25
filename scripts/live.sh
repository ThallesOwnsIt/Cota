#!/usr/bin/env bash
# Modo ao vivo do Cota.
#
#   scripts/live.sh alerts    3 nós consultam os avisos do INMET; o agregador ancora os novos
#   scripts/live.sh daily     ancora cada dia UTC encerrado que falta, prevê e emite alertas
#   scripts/live.sh publish   audita contra a cadeia, gera os dados do dashboard e publica
#   scripts/live.sh all       os três, nessa ordem
#
# Variáveis: RPC, CLUSTER, PAYER, BACKFILL_DAYS (dias anteriores a ancorar na 1ª execução, padrão 7),
#            DEPLOY (1 = publicar na Vercel ao final do publish)
set -euo pipefail
cd "$(dirname "$0")/.."

export RPC=${RPC:-https://api.devnet.solana.com}
export CLUSTER=${CLUSTER:-devnet}
export PAYER=${PAYER:-$HOME/.config/solana/id.json}
BACKFILL_DAYS=${BACKFILL_DAYS:-7}
DEPLOY=${DEPLOY:-0}
BIN=target/release

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
  CONFIG=config/stations-live.toml FAULTS=/dev/null scripts/replay.sh "${days[0]}" "${days[-1]}"
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
