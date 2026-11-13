#!/usr/bin/env bash
# One round of Cota's live mode. The same script runs everywhere: on the server (each step in its own Docker
# container, see deploy/), in GitHub Actions (the backup, .github/workflows/live.yml), on an institution's node
# (scripts/node_run.sh) and on a laptop for tests (scripts/live.sh).
#
#   round.sh plan        the UTC day the chain (v3: the ledger) is waiting a batch for, if it has closed (else empty)
#   round.sh node [i]    one oracle node: signs INMET's warnings, the day's batch and the rain forecast
#   round.sh anchor      takes to the chain whatever has quorum and forecasts discharge (v3: collects
#                        into the ledger what a majority of the nodes signed)
#   round.sh publish     Bitcoin stamp, audit, dashboard data, Arweave copy, commit and push
#   round.sh aggregate   anchor + publish, for one day (GitHub Actions)
#   round.sh doctor      read-only health check (scripts/doctor.py)
#   round.sh all         the whole round under a lock: nodes 1-3 in parallel and anchor, repeated while closed
#                        days are waiting (at most MAX_DAYS), then publish once and the health check
#
# Variables (all optional):
#   RPC, CLUSTER         Solana endpoint and cluster name (default: public devnet)
#   DAY                  the day to sign and anchor; set but empty means "no day" (default: asked to the chain)
#   SETTLE_MIN           minutes after 00:00 UTC before yesterday is signed (default 60)
#   BIN                  folder with the cota-oracle binaries (default target/release)
#   SECRETS              folder with node-1.json … node-3.json, operator.json, archive.json (default keys)
#   KEY, OUT             node: its keypair (default $SECRETS/node-<i>.json) and output folder
#                        (default data/nodes/node-<i>)
#   ANA_IDENTIFICADOR, ANA_SENHA   node: the HidroWebService login
#   PAYER                anchor: the operator keypair, which pays the fees (default $SECRETS/operator.json)
#   PUSH                 publish: 1 = Bitcoin stamp, Arweave copy, commit and push. Default 0, so that a test on a
#                        laptop never uploads anything permanent nor touches the shared ledger history
#   ARCHIVE_KEY          publish: JSON of the Arweave signer (default: $SECRETS/archive.json, if present)
#   VERCEL_DEPLOY_HOOK   publish: URL that rebuilds the site after a push
#   HEALTHCHECK_URL      all: ping URL (healthchecks.io) told when the round starts, succeeds or fails, with the
#                        health check's report as the message
#   COMPOSE              all: run each step in its own container, e.g. "docker compose -f deploy/compose.yml"
#   MAX_DAYS             all: closed days anchored in one round (default 7)
#   COTA_V3_ENSAIO       1 = also run the rehearsal of the v3 ledger (docs/ledger-v3.md) next to v2, in V3_LEDGER
#                        (default data/ledger-v3-ensaio); v2 is not touched. Default 0
#   COTA_LEDGER          v2 (default) or v3. v3 = after the cutover: the round signs and collects only the v3 ledger
#                        (V3_LEDGER, default data/ledger-v3), takes the day from it and writes nothing to Solana
#   NODES_DIR            anchor: where the nodes' folders are (default data/nodes)
set -euo pipefail
SELF="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
cd "$(dirname "$SELF")/../.."

export RPC=${RPC:-https://api.devnet.solana.com}
export CLUSTER=${CLUSTER:-devnet}
SECRETS=${SECRETS:-keys}
BIN=${BIN:-target/release}
v3only() { [[ ${COTA_LEDGER:-v2} == v3 ]]; }
if v3only; then V3_LEDGER=${V3_LEDGER:-data/ledger-v3}; else V3_LEDGER=${V3_LEDGER:-data/ledger-v3-ensaio}; fi
# The v3 steps run in the rehearsal and, after the cutover, alone.
ensaio() { [[ ${COTA_V3_ENSAIO:-0} == 1 ]] || v3only; }

log() { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; }
warn() { log "aviso: $*"; }
die() { log "erro: $*"; exit 2; }

plan() {
  # DAY set (even empty) wins: in GitHub Actions the plan job decides once for every job of the run.
  if [[ -n ${DAY+x} ]]; then
    echo "$DAY"
    return
  fi
  # After the cutover the v3 ledger says which day it waits for, with the same settling time.
  if v3only; then
    "$BIN/cota-ledger3" --ledger "$V3_LEDGER" next-day --settle-min "${SETTLE_MIN:-60}" | cut -d' ' -f1
    return
  fi
  local day now
  day=$(python3 -I scripts/registry_day.py --rpc "$RPC")
  # The day closes at 00:00 UTC, but the sources publish its last hours with some delay: yesterday is signed only
  # SETTLE_MIN minutes after midnight (default 60), so every node sees the complete day.
  now=$(date -u +%H:%M)
  if [[ -n $day && $day == "$(date -u -d yesterday +%F)" ]] &&
    (( 10#${now%:*} * 60 + 10#${now#*:} < ${SETTLE_MIN:-60} )); then
    log "$day fechou há menos de ${SETTLE_MIN:-60} min; as fontes ainda publicam as últimas horas"
    day=""
  fi
  echo "$day"
}

node() {
  local i=${1:-}
  local key=${KEY:-${i:+$SECRETS/node-$i.json}}
  [[ -n $key ]] || die "node: informe o número do nó ou KEY"
  local out=${OUT:-data/nodes/node-${i:-local}} rc=0 day
  mkdir -p "$out"
  # Official warnings are independent of the batch: a failure here does not stop the rest.
  "$BIN/cota-alerts" observe --key "$key" --out "$out/alerts" || warn "avisos do INMET indisponíveis nesta rodada"
  day=$(plan) || { warn "não consegui consultar a cadeia"; return 1; }
  if [[ -n $day ]]; then
    # The node's batch file (its readings); its number and chain come from the ledger that waits for it.
    local chain=(--rpc "$RPC") next _ bid broot
    if v3only; then
      next=$("$BIN/cota-ledger3" --ledger "$V3_LEDGER" next-day --settle-min 0)
      read -r _ bid broot <<<"$next"
      chain=(--batch-id "${bid:-0}" --prev-root "${broot:-$(printf '0%.0s' {1..64})}")
    fi
    "$BIN/cota-node" --key "$key" --day "$day" "${chain[@]}" --config config/stations-live.toml --out "$out" ||
      { warn "lote de $day: o nó não conseguiu assinar"; rc=1; }
    # Without the rain forecast, the node's batch still counts.
    "$BIN/cota-outlook" observe --key "$key" --day "$day" --out "$out/outlook" || warn "chuva prevista indisponível"
    # Third-party forecast (GloFAS discharge, 30 days), signed for the ledger until program v3 records it.
    "$BIN/cota-extforecast" observe --key "$key" --day "$day" --out "$out/extforecast" || warn "previsão GloFAS indisponível"
  else
    log "nenhum dia fechado à espera de lote"
  fi
  # v3 rehearsal: every round (checkpoints and warnings do not wait for a closed day), on top of what this node has
  # just observed.
  if ensaio; then
    # The flow forecast is the node's own run of the public model, on the latest batch (once per batch).
    local fday fdir l3
    fday=$("$BIN/cota-ledger3" --ledger "$V3_LEDGER" forecast-day) || fday=""
    fdir="$(cd "$out" && pwd)/forecast"
    l3=$(cd "$V3_LEDGER" 2>/dev/null && pwd) || l3=""
    if [[ -n $fday && -n $l3 && ! -f $fdir/$fday.json ]]; then
      (cd ml && uv run --frozen python -m cota_ml.forecast --day "$fday" --ledger-v3 "$l3" --out "$fdir") ||
        warn "livro v3 (ensaio): o modelo de vazão falhou"
    fi
    "$BIN/cota-ledger3" --ledger "$V3_LEDGER" sign --key "$key" --node-dir "$out" --rpc "$RPC" || warn "livro v3 (ensaio): o nó não assinou"
  fi
  python3 -I scripts/node_index.py "$out"
  return $rc
}

anchor() {
  local payer=${PAYER:-$SECRETS/operator.json} rc=0 day
  # After the cutover: only the v3 ledger, nothing goes to Solana.
  if v3only; then
    python3 scripts/fetch_nodes.py || warn "nós externos indisponíveis"
    "$BIN/cota-ledger3" --ledger "$V3_LEDGER" collect --nodes-dir "${NODES_DIR:-data/nodes}" || { warn "livro v3: falhou"; return 1; }
    return 0
  fi
  # Independent nodes run by other institutions publish their signatures elsewhere (config/nodes.toml); they
  # count only if their keys are committee members on-chain.
  python3 scripts/fetch_nodes.py || warn "nós externos indisponíveis"
  "$BIN/cota-alerts" anchor --rpc "$RPC" --cluster "$CLUSTER" --payer "$payer" || { warn "avisos oficiais: falhou"; rc=1; }
  day=$(plan) || { warn "não consegui consultar a cadeia"; return 1; }
  if [[ -n $day ]]; then
    # A gauge added to the catalog gets its on-chain station record before its first batch (only the missing
    # ones are sent; routine records, signed by the operator).
    "$BIN/cota-admin" --rpc "$RPC" --payer "$payer" register-stations --config config/stations-live.toml ||
      warn "registro de estações novas: falhou (o lote segue)"
    if "$BIN/cota-aggregator" --rpc "$RPC" --cluster "$CLUSTER" --payer "$payer" --config config/stations-live.toml; then
      # The program applies the early-warning rule to the forecast rain of the day just anchored.
      "$BIN/cota-outlook" anchor --rpc "$RPC" --cluster "$CLUSTER" --payer "$payer" || warn "perspectiva de chuva: falhou"
      "$BIN/cota-extforecast" collect --rpc "$RPC" || warn "previsões de terceiros: falhou"
      # --frozen: uses ml/uv.lock as committed and never rewrites it, so the checkout stays clean for the pull.
      if (cd ml && uv run --frozen python -m cota_ml.forecast --day "$day"); then
        if [[ -f data/forecasts/$day.json ]]; then
          "$BIN/cota-admin" --rpc "$RPC" --payer "$payer" commit-forecast --file "data/forecasts/$day.json" --cluster "$CLUSTER" ||
            { warn "previsão de vazão de $day: não foi gravada"; rc=1; }
        fi
      else
        warn "previsão de vazão de $day: o modelo falhou"
        rc=1
      fi
    else
      warn "lote de $day: não foi ancorado"
      rc=1
    fi
  fi
  # v3 rehearsal: the checkpoint and the entries a majority of the nodes signed.
  if ensaio; then
    "$BIN/cota-ledger3" --ledger "$V3_LEDGER" collect --nodes-dir "${NODES_DIR:-data/nodes}" || warn "livro v3 (ensaio): falhou"
  fi
  # The parametric vaults were retired on 13/11/2026 (decision of the project owner): the round no longer cranks them.
  # They stay on-chain, idle, until they expire on 05/11/2027, when cota-vault can close them.
  return $rc
}

publish() {
  local rc=0 push=${PUSH:-0}
  # Daily copy of the accumulator's root in Bitcoin (free OpenTimestamps calendars); also completes the proofs
  # still waiting for their Bitcoin block.
  if [[ $push == 1 ]]; then
    python3 scripts/timestamp.py || warn "carimbo no Bitcoin: falhou"
  fi
  if v3only && [[ -n $(git status --porcelain "$V3_LEDGER") ]]; then
    # The v3 ledger replayed in full (rules, signatures, checkpoints, readings, contents, the opening committee).
    { "$BIN/cota-ledger3" --ledger "$V3_LEDGER" verify --rpc "$RPC" && python3 scripts/export_dashboard.py; } ||
      { warn "conferência do livro v3 ou dados do painel: falhou"; rc=1; }
  elif ! v3only && [[ -n $(git status --porcelain data/ledger) ]]; then
    { "$BIN/cota-audit" --rpc "$RPC" && python3 scripts/export_dashboard.py; } || { warn "auditoria ou dados do painel: falhou"; rc=1; }
  else
    log "nada novo no ledger"
  fi
  if [[ $push != 1 ]]; then
    log "PUSH≠1: sem carimbo, sem cópia permanente, sem commit"
    return $rc
  fi
  archive || warn "cópia no Arweave: falhou"
  git add data/ledger dashboard/data
  if [[ -d $V3_LEDGER ]]; then git add "$V3_LEDGER"; fi
  if ! git diff --cached --quiet; then
    bot_git commit -q -m "ao vivo: $(date -u +%FT%H:%MZ)"
  fi
  # Sends every commit GitHub does not have yet: this round's and any an earlier round failed to send, even when
  # this round had nothing new. Without an upstream (unusual), it tries anyway.
  local pending
  pending=$(git rev-list --count '@{u}..HEAD' 2>/dev/null || echo 1)
  if [[ $pending == 0 ]]; then
    return $rc
  fi
  local n
  for n in 1 2 3; do
    # The rebase rewrites the round's commits on top of GitHub's, so it needs the bot's identity too (a container
    # has none). --autostash: a tracked file changed by a tool must not block the pull. -X theirs: where GitHub
    # wrote the same ledger file (a backup run copying the same records from the chain, a stamp of the same
    # root), the round's own version, made first and with the batch's data, wins; the round only commits
    # data/ledger, dashboard/data and the v3 ledger, so code is never touched.
    if bot_git pull -q --rebase --autostash -X theirs && git push -q; then
      log "ledger enviado ($pending commit(s))"
      after_push
      return $rc
    fi
    git rebase --abort 2>/dev/null || true
    sleep 5
  done
  warn "push falhou três vezes; $pending commit(s) esperam a próxima rodada"
  git status --short | head -5 >&2
  return 1
}

# git as the bot: commits and rebases need an identity, and the container has none.
bot_git() { git -c user.name=cota-bot -c user.email=cota-bot@users.noreply.github.com "$@"; }

# Permanent copies outside GitHub: the new or changed ledger files on Arweave (free items; the map path -> id goes
# to data/ledger/archive.json, committed with the ledger).
archive() {
  local key=${ARCHIVE_KEY:-}
  if [[ -z $key && -f $SECRETS/archive.json ]]; then
    key=$(<"$SECRETS/archive.json")
  fi
  if [[ -z $key ]]; then
    log "sem ARCHIVE_KEY: cópia no Arweave pulada"
    return 0
  fi
  (
    cd scripts/archive
    # Reinstall only when the lock file changed (on the server the checkout persists between rounds).
    if [[ ! -d node_modules || package-lock.json -nt node_modules ]]; then
      npm ci --silent
      touch node_modules
    fi
    # `command`: Node.js, not this script's node() function.
    ARCHIVE_KEY=$key command node archive.mjs
  )
}

after_push() {
  if [[ -n ${VERCEL_DEPLOY_HOOK:-} ]]; then
    curl -fsS -m 30 -X POST "$VERCEL_DEPLOY_HOOK" >/dev/null || warn "deploy hook da Vercel: falhou"
  fi
  # Save request of the public repository to Software Heritage, at most once a UTC day (it keeps every version).
  local stamp
  stamp="data/runs/swh-$(date -u +%F)"
  if [[ ! -e $stamp ]]; then
    mkdir -p data/runs
    if curl -fsS -m 60 -X POST "https://archive.softwareheritage.org/api/1/origin/save/git/url/https://github.com/ThallesOwnsIt/Cota/" >/dev/null; then
      touch "$stamp"
    else
      warn "pedido ao Software Heritage: falhou"
    fi
  fi
}

aggregate() {
  local rc=0
  anchor || rc=1
  publish || rc=1
  return $rc
}

# all: one step, either here or in its own container (COMPOSE), with DAY passed along when it is set.
step() {
  local service=$1
  shift
  if [[ -n ${COMPOSE:-} ]]; then
    $COMPOSE run --rm -T ${DAY+-e DAY} "$service" "$@"
  else
    "$SELF" "$@"
  fi
}

doctor() {
  python3 scripts/doctor.py "$@"
}

# ping_health <suffix> [message]: "" = success, /start, /fail.
ping_health() {
  if [[ -n ${HEALTHCHECK_URL:-} ]]; then
    curl -fsS -m 10 --retry 3 --data-raw "${2:-}" "$HEALTHCHECK_URL$1" >/dev/null || warn "healthcheck: sem resposta"
  fi
}

all() {
  mkdir -p data/runs
  exec 9>data/runs/round.lock
  if ! flock -n 9; then
    log "outra rodada em andamento; esta termina aqui"
    return 0
  fi
  ping_health /start
  # Signatures older than 3 days are of no use any more (warnings, batches and outlooks already decided).
  find data/nodes -type f -mtime +3 -delete 2>/dev/null || true

  local rc=0 n=0 day next pids p i
  day=$(unset DAY; step aggregate plan) || { ping_health /fail; return 1; }
  while :; do
    export DAY=$day
    log "rodada: ${day:-sem dia fechado pendente}"
    pids=()
    for i in 1 2 3; do
      step "node-$i" node "$i" &
      pids+=($!)
    done
    # One node down does not stop the round: the 2-of-3 threshold decides.
    for p in "${pids[@]}"; do wait "$p" || warn "um nó falhou"; done
    step aggregate anchor || rc=1
    n=$((n + 1))
    [[ -n $day && $n -lt ${MAX_DAYS:-7} ]] || break
    # Catch up after an outage: the next closed day, if the chain moved on.
    next=$(unset DAY; step aggregate plan) || break
    [[ -n $next && $next != "$day" ]] || break
    day=$next
  done
  unset DAY
  step aggregate publish || rc=1
  # The health check's failures (a late batch, a ledger that does not match the chain, no SOL) fail the round;
  # its warnings do not. Its report goes with the ping, so the alert e-mail already says what is wrong.
  local report health=0
  report=$(step aggregate doctor 2>&1) || health=$?
  printf '%s\n' "$report" >&2
  [[ $health -ge 2 ]] && rc=1
  if [[ $rc == 0 ]]; then ping_health "" "$report"; else ping_health /fail "$report"; fi
  return $rc
}

cmd=${1:-}
shift || true
case $cmd in
  plan | node | anchor | publish | aggregate | doctor | all) "$cmd" "$@" ;;
  *) echo "uso: $0 plan|node [i]|anchor|publish|aggregate|doctor|all" >&2; exit 2 ;;
esac
