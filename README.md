<p align="center"><strong>Tamper-proof, publicly verifiable flood data, forecasts and warnings, signed by a committee of independent nodes and stamped in Bitcoin.</strong><br>
<a href="https://cota-eight.vercel.app/?lang=en">cota-eight.vercel.app</a></p>

> Research prototype. Cota does **not** issue official warnings.
> Official alerts in Brazil: **SMS 40199**, Defesa Civil (civil defense).

## About

Every day Cota records river levels, rain forecasts, discharge forecasts and official warnings for the Guaíba basin (Porto Alegre,
Brazil) in a public ledger that nobody can change afterwards, so anyone can check what was known, and when. It started from the
May 2024 flood, when the Guaíba reached 5.37 m and it was hard to piece together what had been forecast and announced beforehand.

- **Three nodes, two signatures.** Each of Cota's nodes fetches the data on its own (ANA river gauges, GloFAS river discharge,
  Open-Meteo rain forecasts, INMET warnings) and signs what it saw. An entry goes into the ledger only with the signatures of a
  majority of the committee over exactly its bytes. If one node fails or is tampered with, the others hold.
- **Forecasts on record before the facts.** Each node runs the same public model to forecast the Guaíba's discharge 1 and 2 days
  ahead, with an uncertainty band, and signs the result. The ledger refuses a forecast whose target day is already recorded.
  In the 2024 replay, the first alert was on record **two days before** the river crossed the flood limit.
- **Early warning, 4 to 7 days out.** The Guaíba fills a few days after heavy rain upstream. If three weather models forecast a
  basin-average of 90 mm or more over the next five days, an early warning is recorded (five sub-basins have their own rules).
  The rules were fixed before they ran, so they can't be tuned afterwards.
- **Official warnings kept.** INMET warnings for Rio Grande do Sul are attested by the committee with a hash of their content, so a
  copy survives after INMET takes them down.
- **Nobody can rewrite the past.** After each round the committee signs a checkpoint ("the ledger has N records and this is
  the root"), chained to the previous one and stamped in Bitcoin through OpenTimestamps, for free.
- **Verify in the browser, or without the site.** The site checks the ledger with the same rules that build it, compiled to
  WebAssembly. `node verifier/check.mjs` does the same with no site involved.

Cota does not replace civil defense, and it doesn't prove a sensor measured correctly. It proves nobody changed the data, and
when the data existed.

### Status: moving from Solana to Cota's own ledger

Until November 2026 the records went to a program on Solana's test network (devnet). Solana limited the committee to 7 members
(a transaction holds only a few signatures) and its devnet is not an archive, so Cota is moving to a ledger of its own with the
same records, signatures and rules. **Until the cutover (planned for around 18 November 2026) the live records still go to
Solana (program v2), and the new ledger runs as a rehearsal next to it** (`data/ledger-v3-ensaio`). After the cutover, v2 is
frozen as an archive and the site reads the new ledger.

---

## The ledger (v3)

- **Entries.** Stations, daily batches of readings (their Merkle root, RFC 9162), discharge forecasts with the alert their rule
  gives, official warnings, rain outlooks with the early warning their rule gives, third-party forecasts (GloFAS), corrections
  (the original entry is never removed) and committee changes. Each entry carries Ed25519 signatures from a majority of the
  committee; each kind has its own signing domain, so a signature for one kind never counts as another.
- **One accumulator since v2.** Every entry is a leaf of an append-only Merkle Mountain Range that continues program v2's: the
  first v3 entry (the opening) comes right after v2's last record and pins its root.
- **The rules are a library.** `crates/cota-ledger` checks every entry (sequence and hash chain of batches, forecast before the
  observation, one outlook per day, no copies, majority signatures) before anything is written. The aggregator uses it to build
  the ledger; every verifier uses it to check, including the browser, through `verifier/cota_verify.wasm`. GitHub rebuilds that
  file from the code on every change and fails if it differs, so nobody has to trust the published file.
- **Checkpoints and anchors.** Each round's checkpoint is signed by the committee, chained to the previous one and stamped in
  Bitcoin. A Rede Blockchain Brasil anchor can be added when a partner institution opens access.
- **The committee governs itself.** A change of members is signed by a majority of the current committee and takes effect
  seven days later, in public. There is no operator key and no admin key.
- **Contents next to their hashes.** The readings of each batch, the text of each warning, the rain per model and point, the
  GloFAS forecast and the model's inputs are published next to the entries that commit to them.

Check it:

```sh
node verifier/check.mjs                                   # v2 against the chain, and every v3 ledger with the site's verifier
cargo run --release -p cota-oracle --bin cota-ledger3 -- verify --rpc https://api.devnet.solana.com
```

`verify` replays the ledger with every signature, rebuilds each batch from its readings, checks every content against its hash
and compares the opening committee with v2's committee on-chain (the trust anchor).

## Layout

| Path | What |
|---|---|
| `crates/cota-ledger` | The v3 ledger's rules: entries, committee, checkpoints, replay (also built for the browser) |
| `crates/cota-verify-wasm` | Those rules for the browser, with a plain memory interface |
| `crates/cota-core` | Canonical formats (Borsh), Merkle tree, accumulator |
| `crates/cota-ingest` | ANA telemetric gauges (HidroWebService), GloFAS (Open-Meteo), INMET |
| `crates/cota-quality` | Sensor rules, comparison between nodes, confidence index |
| `crates/cota-oracle` | Nodes and aggregator: `cota-ledger3` (sign, collect, verify, export), `cota-node`, `cota-alerts`, `cota-outlook`, `cota-extforecast`; the v2 tools (`cota-aggregator`, `cota-admin`, `cota-audit`, `cota-vault`) |
| `programs/cota-v2`, `programs/cota` | The Solana programs v2 and v1, kept for the archive |
| `ml/` | Training, evaluation and forecasting (Python/uv); the rain rules and their backtests |
| `verifier/` | `ledger.mjs` and `cota_verify.wasm` (the v3 check), `verify.js` (the v2 check), `check.mjs` (both, with no site) |
| `config/` | Gauges (`stations-live.toml`), places and official levels (`locations.toml`), the ledger's opening (`ledger-v3.toml`) |
| `data/` | `ledger-v3*` (the v3 ledger and its rehearsal), `ledger` (v2), `ledger-v1` (v1) |
| `dashboard/data/` | Data for the site, written by the live rounds (the site's code is in a private repository) |
| `scripts/` | `live/round.sh` (one live round, the same everywhere), `export_dashboard.py`, `timestamp.py` (OpenTimestamps), `doctor.py` (health check) |

## Running

```sh
cargo test --workspace                     # ledger rules, oracles, quality, programs
(cd ml && uv run pytest)                   # formats and no leakage from the future
scripts/live.sh                            # a local round (nothing permanent: PUSH=0)
node verifier/check.mjs                    # every check of the site, with no site
bash verifier/build-wasm.sh --check        # the browser verifier rebuilds to the published bytes
```

## Live rounds

A server runs `scripts/live/round.sh` every 30 minutes in Docker containers, each node's container with only its own key;
GitHub Actions (`.github/workflows/live.yml`) is the backup. A node waits for a source that is down (fewer than half of its
stations answering) until 04:00 UTC before signing a day without it. After the cutover, `COTA_LEDGER=v3` makes the round take
the day from the ledger and write nothing to Solana.

## Copies outside GitHub

Every run also uploads the ledger's new or changed files to **Arweave** (permanent storage, `data/ledger/archive.json` maps
each path to its id; uploads signed by `7kw7om2uMrnSaJBZXriH6TAVpQrsN7Eoq7dtsqgb6Lww`) and asks
[Software Heritage](https://archive.softwareheritage.org/browse/origin/?origin_url=https://github.com/ThallesOwnsIt/Cota) to
save the repository. Any copy can be checked: the readings against each batch's root, the entries against the checkpoints,
the checkpoints against Bitcoin.

## The Solana archive (v1, v2)

- Program v2 (`FnjR7wG5zicqWJ6KQuWQ5UcqqhcSghrYozVgMmmDFNwR`): one accumulator for every record, committee 2 of 3, upgrade
  authority a [Squads](https://squads.so) multisig (2 of 3, 24 h wait; `config/upgrade-multisig.json`). Its records are in
  `data/ledger/records.jsonl` and are checked against the on-chain peaks by `verifier/check.mjs`.
- Program v1 (`4FChu6PkKzsFQRAtQt2oiGt7TuC4uZhBNSSHqDoVNFsa`, one account per record): `data/ledger-v1`.
- The parametric vaults were retired on 13 November 2026. They stay on-chain, idle, until they expire on 5 November 2027.
- After the cutover the site keeps this history on a separate archive page.

## What is open

- **Open, MIT:** everything needed to check Cota without trusting it: the ledger's rules and formats, the nodes, the server
  tooling, the verifier and the data.
- **Private for now:** the site's code (layout and texts) and the technical documentation (design notes, runbooks, security
  reviews, server and node guides).

The site cannot hide what it checks: its verifier files are this repository's (the site's footer and `ledger-v3/index.json`
give their SHA-256), and `node verifier/check.mjs` repeats every check with no site at all.

## Known limits

- The three nodes are held by the same person today (separate keys, separate containers). The next step is handing them to
  different institutions (civil defense, universities, NGOs); a node runs on any machine and publishes its signatures as files.
- A majority of the committee acting together could sign false data. Everything stays public and attributable, and a change
  of members waits seven days in public, but the defense is having independent members.
- Time comes from the batches and from Bitcoin: a checkpoint proves its entries existed by its Bitcoin block (hours of
  precision). Forecasts are made at least 24 hours ahead and warnings last days.
- The forecast is of the **daily discharge modelled by GloFAS**, 1 and 2 days ahead, not of the river gauge level; the model
  has not been trained on the ANA gauges yet.
- ANA gauges are read once per hour. If ANA stays down past 04:00 UTC, the day is signed without it and the missing hours stay
  on record.
- Early warning: the rules were calibrated on observed rain and are applied to forecast rain; a few floods in a few years is a
  small sample, and archived forecasts come from Open-Meteo, not from each model's official service.

Data: [Weather data by Open-Meteo.com](https://open-meteo.com/) (CC BY 4.0) · GloFAS / Copernicus EMS · INMET ·
ANA (Agência Nacional de Águas e Saneamento Básico, HidroWebService) ·
map: [HydroSHEDS](https://www.hydrosheds.org/) (Lehner & Grill, 2013) and [Natural Earth](https://www.naturalearthdata.com/).
