# Operations: what to do when something goes wrong

Start with the health check. It is read-only, and it says which part is wrong:

```sh
python3 scripts/doctor.py          # or: scripts/live/round.sh doctor
```

It also runs at the end of every round: on the server its report goes with the healthchecks.io ping, and in GitHub
Actions it is the last step of the run. `✓` means fine, `!` is a warning, `✗` is a failure. The exit code is 0, 1 or
2 for the same three states.

| Check | What it looks at |
|---|---|
| `lote` | A closed UTC day still has no batch 3 hours after it closed |
| `árvore` | The local ledger has every record the chain has, and the last audit found no problem |
| `saldo` | SOL left for the operator, in days at the measured spend (0.0055 SOL/day in Nov 2026) |
| `fontes` | ANA, INMET, GloFAS, the rain forecast, the RPC and GitHub answer |
| `carimbo` | The accumulator's root was stamped in Bitcoin in the last 36 hours |
| `ledger` | The ledger was committed in the last 26 hours |
| `disco` | Free space |

Two rules hold in every case below:
- **Never edit or delete files in `data/ledger` by hand.** The ledger is checked against the chain, and every
  record in it is already permanent on-chain. A wrong file is fixed by rebuilding it from the chain, never by
  editing it.
- **Every change to the committee, the rules or the program goes through the multisig.** That means 2 of 3
  approvals and a public 24-hour wait, never one key alone (`docs/security-review.md`, F1).

## A data source is down (`fontes`)

**ANA** (the HidroWebService often returns 502/503) or **INMET**.
- Nothing to do in the moment. The round goes on without the source:
  - a gauge with no readings is recorded as unavailable;
  - the quality check reports it on-chain (`SourceDegraded`).
- The failure is part of the record, which is the point: "the gauge was down that day" is a fact the ledger keeps.
- If it lasts days, check that the API did not change: test it by hand, and see the source's own status page.
  - A changed API means a change in `crates/cota-ingest`.

**Open-Meteo** (GloFAS, rain forecast).
- Without the forecast rain, the node still signs its batch, but that day has no early-warning outlook.

**RPC** (Solana).
- Every tool retries on 429.
- If the public endpoint keeps failing, set a dedicated one (Helius' free plan) as `RPC` in `/etc/cota/cota.env`
  and in the GitHub secret `RPC_URL`.

## The batch is late (`lote`)

1. Is the server running the rounds?
   - `systemctl list-timers cota-round.timer` shows the next and last rounds.
   - `journalctl -u cota-round --since -6h` shows their logs.
2. Run one now: `systemctl start cota-round`, then follow it with `journalctl -u cota-round -f`.
3. The usual causes, in the log:
   - **"lote de D: o nó não conseguiu assinar"** on all three nodes: a source or the RPC failed while fetching the
     day. It is retried in the next round, 30 minutes later.
   - **"lote de D: não foi ancorado"**: fewer than 2 nodes signed the same thing. Look at each node's lines: were
     they down, or did they disagree?
   - **The round never starts**: Docker is down (`systemctl status docker`), or the image is missing
     (`docker images`).
4. **The server is down for good.**
   - With `COTA_BACKUP_MODE = true`, GitHub Actions takes over by itself. It acts when a closed day is still without
     a batch at 03:00 UTC.
   - To run GitHub at once: `gh workflow run live.yml`. A manual run ignores the backup rule.

A late batch is not lost. The next round anchors every closed day that is waiting, up to 7 per round.

## The ledger does not match the chain (`árvore`)

- **"falta git pull"**: the local copy is behind. Run `git pull`. On the server the round does it before pushing.
- **The audit found a problem**, or the local ledger has *more* records than the chain. **Stop and investigate
  before the next commit.**
  - `cota-audit --rpc "$RPC"` rebuilds the accumulator from the chain's events and lists every difference
    (`unmatched`, `*_with_problems` in `data/ledger/audit.json`).
  - The chain is the reference. A record missing locally comes back from the chain's events (`records.jsonl` is
    synced by the tools).
  - A local file with no record on-chain is the problem to explain. Do not delete it to make the check pass.

## The operator is running out of SOL (`saldo`)

- **On devnet:** `solana airdrop 1 3wPjiZdtuJpmivtLCgqgL7gyeSTrbydj7U7R7wwHCe2v --url devnet`. If the airdrop is
  limited, use https://faucet.solana.com.
- **On mainnet:** a transfer from the project's wallet.
- 1 SOL lasts about six months at the measured spend.

## No Bitcoin stamp (`carimbo`)

- `scripts/timestamp.py` stamps once per UTC day, and only with `PUSH=1` (server or GitHub).
- A stamp waiting for its Bitcoin block for more than a day usually means the free calendars are slow. The next
  run completes it.
- If no stamp in 36 hours, look at the "Carimbo no Bitcoin" lines of the round's log.

## A key leaked or was lost

| Key | Where it lives | What to do |
|---|---|---|
| Node (`node-N.json`) | Server, GitHub secret `NODE_N_KEY` | Replace the member on-chain (`rotate_member`), through a multisig proposal. Then put the new key on the server and in the secret. One node alone cannot sign anything (2 of 3). |
| Operator (`operator.json`) | Server, GitHub secret `OPERATOR_KEY` | Hand the authority to a new key (`set_authority`), through a multisig proposal. Since F1 this key cannot touch the committee, the rules or the vaults; a thief can only spend its SOL and record routine data. |
| Git deploy key | Server (`/etc/cota/secrets/deploy_key`) | Delete it in GitHub (repository → Settings → Deploy keys). Delete the file and run `deploy/install.sh` again, which creates a new one. Add the new one. |
| Arweave signer (`archive.json`) | Server, GitHub secret `ARCHIVE_KEY` | It holds no money and only signs free uploads. Generate another and update both places; the manifest keeps the old uploads. |
| Multisig member (`upgrader-N.json` or the main wallet) | Offline | Replace the member with a multisig config proposal, approved by the other two (see the gap below). With two keys lost, the program can no longer be upgraded: that is why the offline copies matter. |

**Known gap.** `scripts/squads/squads.mjs` proposes program upgrades and time-lock changes only. Two kinds of
proposal still have to be built:
- a proposal that runs an admin instruction of the program (`rotate_member`, `set_authority`, new rules);
- a proposal that replaces a member of the multisig itself.

This is planned for step 1.7 of the plan, the one about separating keys. Until then, replacing a node, the
operator or a multisig member needs that tool written first.

## Updating

- **The server, after a push:** `bash /srv/cota/deploy/update.sh`. It pulls the code and the image of that commit.
  - If the Rust code changed, the image takes about 15 minutes to appear on GitHub.
- **The program on-chain:** only through the multisig. The full path is in `scripts/squads/squads.mjs`: write a
  buffer, hand it to the vault, propose, two approvals, 24-hour wait, execute. Check that the new code fits the
  program account first (`propose-upgrade` checks and prints the extend command).
- **Moving to another machine:** see `docs/server.md`.

## The server's git is stuck

If a push failed in the middle of a rebase, the round aborts the rebase and keeps its commit for the next round.

If the log keeps showing "push falhou três vezes":
1. `runuser -u cota -- git -C /srv/cota status`.
2. Usually two writers committed the ledger at the same time, for example the server and GitHub both running as
   primary. Make sure only one is primary: `COTA_BACKUP_MODE = true` in GitHub whenever the server's timer is on.
3. Then `runuser -u cota -- git -C /srv/cota pull --rebase`, keeping the server's version of the generated files
   (`dashboard/data/*`, `data/ledger/audit.json`). The next round regenerates them from the chain anyway.
