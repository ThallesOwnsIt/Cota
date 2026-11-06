# Security review of program v2

Date: 2026-11-06. Scope: `programs/cota-v2` (program `FnjR7wG5zicqWJ6KQuWQ5UcqqhcSghrYozVgMmmDFNwR`), the
off-chain tools that build its instructions (`crates/cota-oracle/src/chain.rs`, `records.rs`) and the multisig
operations script (`scripts/squads/squads.mjs`).

This is an internal review by the project, not an independent audit. It follows the usual Solana/Anchor
checklist and records every finding with an executable test, so that a professional auditor can start from
here. Real money should only go into vaults after an independent audit.

## Method

1. Read every instruction and its account constraints, asking for each account: who must sign, who owns it,
   what type it is, whether its address is derived and checked, and whether it can be passed twice.
2. Wrote down the trust model (below) and looked for any path where one key can do what should need several.
3. For each finding: a LiteSVM test that runs the attack against the compiled program, confirmed to **pass on
   the old code** (the attack works) and to fail on the fixed code.
4. A cloud code review of the latest changes (the multisig script and dashboard) ran in parallel; its finding
   is F5.

## Trust model

| Role | Key | Can do |
|---|---|---|
| Committee | 3 oracle keys, 2 of 3 needed | Batches, official warnings, rain outlooks (Ed25519 signatures checked by the program) |
| Operator | `OPERATOR_KEY` (GitHub Actions) | Flow forecasts (and their alert), station records, degradation reports |
| Admin | Program upgrade authority: Squads vault `ApMy…`, 2 of 3 + 24 h wait | Replace committee members or the operator, create alert/outlook rules, upgrade the code |
| Anyone | — | Submit committee-signed records (paying fees), create vaults, crank payouts with a proof |

The operator key is the most exposed one: it lives in CI and signs automatically every day. The design goal
is that a leaked operator key cannot forge what the committee attests or move vault money on its own.

## Findings

### F1 · Critical · The operator key could take over the committee — fixed

`rotate_member`, `set_authority`, `set_alert_rule` and `set_outlook_rule` only required the operator key.

*Attack:* with a leaked `OPERATOR_KEY`, call `rotate_member` twice to replace 2 of the 3 committee members with
attacker keys. From then on the attacker signs batches and rain outlooks "as the committee": a fake outlook
above 90 mm fires the early warning and every early-warning vault pays the attacker's chosen event. The same
key could also create the rule of a region or station before the real one, with any threshold.

*Fix:* those four instructions now require the program's **upgrade authority** as signer, checked through the
program's `ProgramData` account (`AdminOnly` accounts). On devnet that is the Squads vault: 2 of 3 signatures and
a public 24 h wait. The operator keeps only routine records. No account layout changed.

*Tests:* `a_leaked_operator_key_cannot_take_over_the_committee_or_the_rules` (operator refused, fake
ProgramData refused, admin accepted), `authority_handover_takes_the_power_away_from_the_old_key`.

### F2 · High · The same event could pay twice — fixed

A committee-signed outlook can be resubmitted by anyone until the next batch (the program only requires
`issued_at` to be the end of the latest batch). Each copy becomes a different record, because it carries the
slot it landed in. A vault with `cooldown_secs = 0` paid once per copy. The +24 h and +48 h forecasts of the
same issue day also counted as two flow-alert events.

*Fix:* a payout now needs an event strictly later than the last paid one, on top of the cooldown
(`event_at > last_event_at`).

*Test:* `the_same_event_never_pays_twice_even_without_cooldown`. On the old code the second copy paid
(`Ok(())`); now it is refused with `CooldownActive`.

### F3 · Medium · Flow-alert vaults trust the operator alone — accepted, documented

Flow forecasts are computed by the model in CI and recorded by the operator, not attested by the committee. A
vault of kind `TRIGGER_FLOW_ALERT` therefore pays on the operator's word (within the immutable rule threshold).
Early-warning vaults (`TRIGGER_EARLY_WARNING`) need 2 of 3 committee signatures.

*Recommendation:* real-money vaults should use the early warning, or the forecast should be signed by the
committee (each node runs the model; the program checks k of n, as for outlooks). Until then the dashboard and
README describe the flow-alert vault as a demonstration.

### F4 · Low · Anyone can resubmit committee-signed records — accepted

Warnings and same-day outlooks can be resubmitted, appending duplicate records. It costs the submitter the fee,
moves no money (F2) and changes no verdict: the earliest record of a version is the one that counts, and the
dashboard shows only that one. It does grow the ledger and makes vault proofs stale more often (the crank
rebuilds them).

### F5 · Medium (tooling) · A typo could remove the 24 h wait — fixed

From the cloud review: `squads.mjs propose-time-lock` turned a missing or non-numeric `--seconds` into NaN, which
the Squads SDK serializes as 0, and auto-approved the proposal. A second approval would have removed the wait.
Now the value must be an explicit whole number, and lowering the wait needs `--allow-lower`. Tested on the CLI
without sending transactions.

### F6 · Info · Self-paying vault

A vault whose beneficiary is its own address would "pay" itself (no net movement, counters advance). Only the
creator can set this up, against their own vault. Not exploitable by others.

## Checklist

| Check | Result |
|---|---|
| Signers on every privileged instruction | ✓ after F1 (`has_one`, `AdminOnly` via ProgramData) |
| Account owner and type | ✓ `Account<T>` everywhere; the three `UncheckedAccount`s are address-checked (Instructions sysvar, vault beneficiary) or PDA-checked with an owner check when present (forecast's `AlertRule`) |
| PDA derivation and canonical bumps | ✓ seeds checked by Anchor, bumps stored at `init` |
| Re-initialization | ✓ `init` only; rules immutable once created |
| Account closing | ✓ `close_vault` only by the creator after expiry; payout receipts survive and keep blocking double payment |
| Arithmetic | ✓ `overflow-checks = true` in the release profile (an overflow aborts the transaction); vault balance keeps the rent floor |
| Ed25519 introspection | ✓ previous instruction only, every offset must point into that same instruction, every signature must cover exactly the expected message, unknown keys rejected, duplicates counted once (bitmap) |
| Cross-program / cross-domain replay | ✓ v2 signing domains differ from v1 and between batch, warning and outlook (tested) |
| Accumulator proofs | ✓ leaf/node prefixes (second-preimage), sibling count must equal the peak's height, index range checked, record must decode completely (Borsh `try_from_slice`) |
| Time | ✓ `slot` and time in every record come from the `Clock`, never from the submitter |
| CPIs | ✓ only a system transfer at vault creation |
| Upgrade authority | ✓ multisig 2 of 3 with a 24 h wait (see residual risks) |

## Residual risks

- **The three multisig keys and the three oracle keys are held by the same person today**, on separate files of
  the same machine. The 2-of-3 rules protect against one leaked key, not against that person or that machine.
  Next step: separate devices and people. Keep offline copies of `keys/upgrader-2.json` and `upgrader-3.json`:
  losing two of the three keys makes the program impossible to upgrade.
- **The data sources are trusted.** The committee proves that k of n nodes saw the same thing, not that INMET,
  ANA or the weather models were right. All nodes use the same ANA login.
- **F3** (flow-alert vaults) and **F4** (duplicates), as above.
- Devnet only; no independent audit yet.

## Deployment of the fixes

Built from this commit and proposed as a multisig upgrade of program v2. It waits the full 24 h before it can be
executed, which is the protection working as intended: anyone can compare the proposed buffer with this code
during the wait. Proposal 3 (buffer `GPAj1y9xSStkVrrtBTmNVLPj7ikQ798Q4M9e2g4kF2Gv`, SHA-256 of the code starting `d4dad4f2`) was approved by 2 of 3 on
2026-11-06 00:40 UTC and can be executed from 2026-11-07 00:40 UTC: `node scripts/squads/squads.mjs execute --index 3`.
Until then the program on devnet still has F1 and F2; no vault there holds real money. Status in `config/upgrade-multisig.json`.
