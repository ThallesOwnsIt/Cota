#!/usr/bin/env bash
# One round of an independent Cota node, for an institution running it on its own machine.
#
#   KEY=keys/my-node.json OUT=public/cota scripts/node_run.sh
#
# Signs what the committee attests and publishes it in $OUT, with an index.json of every file and its SHA-256:
#   - INMET's current warnings for Rio Grande do Sul           $OUT/alerts/<sha>.json
#   - the next closed day's batch, if the chain is waiting for it  $OUT/<batch_id>.json
#   - the rain forecast for the 5 days after that day            $OUT/outlook/<day>.json
# Serve $OUT on any static host (GitHub Pages, an institutional site): the aggregator downloads it and checks
# every signature, so the host does not need to be trusted. See docs/running-a-node.md.
#
# Variables: KEY (the node's keypair, never shared), OUT (default data/node-out), RPC (default public devnet),
#            ANA_IDENTIFICADOR / ANA_SENHA (the institution's own HidroWebService login), BIN (default
#            target/release), DAY (force a day, for tests).
# Run it every 30 minutes or so (cron, systemd timer): warnings change during the day; the batch only once.
set -euo pipefail
cd "$(dirname "$0")/.."

KEY=${KEY:?defina KEY com o keypair do nó}
OUT=${OUT:-data/node-out}
RPC=${RPC:-https://api.devnet.solana.com}
BIN=${BIN:-target/release}
mkdir -p "$OUT"

# Official warnings: independent of the batch, so a failure here does not stop the rest.
"$BIN/cota-alerts" observe --key "$KEY" --out "$OUT/alerts" || echo "aviso: avisos do INMET indisponíveis nesta rodada" >&2

# The day the chain is waiting for (the end of the latest anchored batch), if it has already closed.
day=${DAY:-$(python3 -I scripts/registry_day.py --rpc "$RPC")}
if [[ -n $day ]]; then
  "$BIN/cota-node" --key "$KEY" --day "$day" --rpc "$RPC" --config config/stations-live.toml --out "$OUT"
  "$BIN/cota-outlook" observe --key "$KEY" --day "$day" --out "$OUT/outlook" || echo "aviso: chuva prevista indisponível" >&2
else
  echo "nenhum dia fechado à espera de lote"
fi

python3 -I scripts/node_index.py "$OUT"
