#!/usr/bin/env bash
# One round of an independent Cota node, for an institution running it on its own machine.
#
#   KEY=keys/my-node.json OUT=public/cota scripts/node_run.sh
#
# Signs what the committee attests and publishes it in $OUT, with an index.json of every file and its SHA-256:
#   - INMET's current warnings for Rio Grande do Sul               $OUT/alerts/<sha>.json
#   - the next closed day's batch, if the chain is waiting for it  $OUT/<batch_id>.json
#   - the rain forecast for the 5 days after that day              $OUT/outlook/<day>.json
# Serve $OUT on any static host (GitHub Pages, an institutional site): the aggregator downloads it and checks
# every signature, so the host does not need to be trusted. See docs/running-a-node.md.
#
# Variables: KEY (the node's keypair, never shared), OUT (default data/node-out), RPC (default public devnet),
#            ANA_IDENTIFICADOR / ANA_SENHA (the institution's own HidroWebService login), BIN (default
#            target/release), DAY (force a day, for tests).
# Run it every 30 minutes or so (cron, systemd timer): warnings change during the day; the batch only once.
# The work itself is in scripts/live/round.sh, the same round the project's own nodes run.
set -euo pipefail
KEY=${KEY:?defina KEY com o keypair do nó} OUT=${OUT:-data/node-out} exec "$(dirname "$0")/live/round.sh" node
