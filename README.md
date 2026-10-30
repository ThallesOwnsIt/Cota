<p align="center"><img src="docs/logo-banner.png" alt="Cota" width="640"></p>

<p align="center"><strong>Tamper-proof, publicly verifiable flood data, forecasts and warnings on Solana.</strong><br>
<a href="https://cota-eight.vercel.app/?lang=en">cota-eight.vercel.app</a></p>

> Historical replay / research prototype. Cota does **not** issue official warnings.
> Official alerts in Brazil: **SMS 40199**, Defesa Civil (civil defense).

## About

In May 2024 the Guaíba river in southern Brazil rose higher than ever recorded and Porto Alegre went underwater. More than 180
people died in Rio Grande do Sul. Afterwards, everyone asked the same questions: who knew? Who warned us, and when?

Those questions were hard to answer. Forecast websites overwrite old forecasts, official warnings go offline when they expire, and
spreadsheets can be edited later. Cota is the record that was missing: a river diary nobody can erase. Every day it writes what was
known about the river to Solana (devnet), where anyone can read it and nobody can change it.

- **Three oracles, two signatures.** A blockchain can't see the weather. Each of Cota's three oracles fetches data on its own from
  INMET rain gauges, GloFAS river discharge and Open-Meteo forecasts, and signs what it saw. The program only records a value that
  at least 2 of the 3 signed byte for byte. If one fails or is tampered with, the other two hold.
- **Forecasts on record before the facts.** A quantile model forecasts the Guaíba's discharge 1 and 2 days ahead with an uncertainty
  band. The program refuses any forecast whose target day is already anchored. In the 2024 replay, the first alert was on record
  **two days before** the river crossed the flood limit.
- **Early warning, 4 to 7 days out.** The Guaíba fills a few days after heavy rain upstream. If three weather models forecast a
  basin-average of 90 mm or more over the next five days, the program raises an alert. The rule was fixed on-chain before it ran,
  so it can't be tuned afterwards. Out of sample (Feb 2024 – Oct 2026) it warned of all three floods.
- **Official warnings kept.** INMET warnings for Rio Grande do Sul are attested by the committee with a hash of their content, so a
  copy survives after INMET takes them down.
- **Verify in the browser.** The dashboard recomputes Merkle proofs and hashes and checks them against the chain. Tick "Simulate
  tampering" and the proof fails.
- **Parametric vaults.** Funds can be locked with a written condition ("if an early warning is issued for the Guaíba, pay this
  wallet"). When the trigger appears on-chain, the program pays by itself, once per event. Test SOL only.

Cota does not replace civil defense, and it doesn't prove a sensor measured correctly. It proves nobody changed the data, and
when the data existed.

---

## What gets recorded

Cota records on Solana (devnet), so that anyone can check it, **who knew what, and when**:

- rain readings (INMET) and river discharge (GloFAS via Open-Meteo), signed by a committee of 3 oracles with a 2-of-3 threshold and
  anchored as Merkle roots (RFC 9162), one batch per day;
- P10/P50/P90 forecasts of the Guaíba's discharge (quantile LightGBM, fixed `model_sha256`), which the program only accepts **before**
  the observation is anchored;
- alerts (`AlertReceipt`) when P90 crosses a threshold fixed in advance;
- official INMET warnings for Rio Grande do Sul, attested by the committee with the SHA-256 of their content (INMET's API only shows
  warnings currently in force);
- **early warning**: the rain forecast for the next 5 days (ECMWF, GFS, ICON; the day's 00Z run) at 6 points in the basin, attested
  by the committee right after each batch; the program applies an immutable rule (≥ 90 mm, calibrated on 2015–2023 only) and issues
  an `EarlyWarning`;
- source degradation (`SourceDegraded`): station without data, stuck sensor, nodes that disagree;
- **parametric trigger**: vaults (`create_vault`) where any wallet locks SOL under fixed terms; when Cota records a trigger
  (`EarlyWarning` or `AlertReceipt`), any account can call `trigger_payout` and the program pays the beneficiary, once per trigger
  and respecting the cooldown. The creator only gets the balance back after expiry (`close_vault`).

Dashboard: **https://cota-eight.vercel.app** (Portuguese and English): live mode, early warning, vaults and the replay of the
April–May 2024 flood, with Merkle and on-chain account checks done in the browser.

## Layout

| Path | What |
|---|---|
| `programs/cota` | Anchor program: committee, stations, batches (Ed25519 via precompile), forecasts, alerts, official warnings, outlooks, vaults |
| `crates/cota-core` | Canonical formats (Borsh), Merkle tree, signed message |
| `crates/cota-ingest` | INMET ingestion (yearly zip) and GloFAS (Open-Meteo) |
| `crates/cota-quality` | Sensor rules, MAD between nodes, confidence index C_s |
| `crates/cota-oracle` | Nodes, aggregator, admin, INMET warnings (`cota-alerts`), forecast rain (`cota-outlook`), vaults (`cota-vault`), audit (`cota-audit`) |
| `ml/` | Training, evaluation and forecasting (Python/uv); `cota_ml.outlook` calibrates the rain rule and runs the backtest |
| `dashboard/` | Static dashboard (HTML/JS) |
| `data/ledger/` | State: every anchored batch, forecast and warning, with explorer links |
| `scripts/` | `replay.sh`, `live.sh`, `export_dashboard.py`, `next_day.py` |

## Running

```sh
cargo test --workspace                     # program (LiteSVM), oracles, quality
(cd ml && uv run pytest)                   # formats and no leakage from the future
scripts/live.sh all                        # warnings + pending day(s) + audit + dashboard data
python3 -m http.server -d dashboard 8000   # local dashboard
```

Flood replay: `RPC=… CLUSTER=devnet PAYER=… scripts/replay.sh 2024-04-20 2024-05-25`.
Model: `(cd ml && uv run python -m cota_ml.train)` (needs the history from `cota-ingest --lenient`).
Rain rule and backtest: `(cd ml && uv run python -m cota_ml.outlook)` → `ml/models/outlook-rule.json` (its `rule_sha256` is what
`cota-admin set-outlook-rule` fixes on-chain) and `ml/reports/outlook-backtest.json`.
Vault: `cota-vault --rpc … --payer <wallet> create --name … --id 1 --kind early-warning --subject guaiba --beneficiary <key>
--payout-sol 0.05 --deposit-sol 0.2`; the workflow runs `cota-vault crank` on every run.

## Live mode (GitHub Actions)

`.github/workflows/live.yml` runs every 30 minutes: three node jobs (one secret each) and an aggregator that only takes to the
chain what has 2 of 3 signatures, then commits `data/ledger` and `dashboard/data`.

Repository secrets (Settings → Secrets and variables → Actions): `NODE_1_KEY`, `NODE_2_KEY`, `NODE_3_KEY` and `OPERATOR_KEY`, with
the contents of `keys/node-N.json` and `keys/operator.json`. The operator is the committee authority (`cota-admin set-authority`);
the program's upgrade authority never leaves the deployer's machine.

## Known limits

- The three oracles fetch and sign data independently, but today they are run by the same person (separate GitHub Actions jobs).
  The next step is handing them to different institutions (civil defense, universities, NGOs).
- The forecast is of the **daily discharge modelled by GloFAS**, 1 and 2 days ahead, not of the river gauge level.
- In the replay, the transactions are from 2026: the proof is of **order** (forecast before observation), not of the 2024 calendar.
- Rain from INMET stations arrives about a month late; live mode uses only discharge until real-time access is available.
- Vaults run on devnet with test SOL. The `replay-2024` vault accepts the replay's alerts (recorded in 2026 over 2024 data) to
  demonstrate the mechanism; `ao-vivo` only accepts triggers recorded after it was created. The program is built with
  `opt-level = "z"` to fit in the account already allocated.
- Early warning: the rule was calibrated on **observed** rain and is applied to **forecast** rain at the station points. In the
  backtest, the forecast used is one day older than live mode's (no peeking at the future). Three floods in 2.7 years is a small
  sample: the CSI of 0.25 is very uncertain. Archived forecasts come from Open-Meteo, not from each model's official service.

Data: [Weather data by Open-Meteo.com](https://open-meteo.com/) (CC BY 4.0) · GloFAS / Copernicus EMS · INMET.
