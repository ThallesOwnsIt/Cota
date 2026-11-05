# Cota v2: one accumulator instead of one account per record

Status: design, not implemented yet. v1 (program `4FChu6PkKzsFQRAtQt2oiGt7TuC4uZhBNSSHqDoVNFsa`) stays
on devnet, untouched, as the archive of the 2024 replay and of the live days recorded with it.

## Why

v1 creates a rent-exempt account for every record. Measured on devnet (Nov 2026):

| Record | Bytes | Rent (SOL) | Per day |
|---|---|---|---|
| Batch | 183 | 0.00158 | 1 |
| Forecast | 240 | 0.00187 | 2 |
| Rain outlook | 169 | 0.00151 | 1 |
| Official warning | 196 | 0.00165 | ~1.7 |

About 0.0096 SOL per day, 3.5 SOL per year, all of it locked rent. Batches are only ~17% of it, so the
accumulator has to hold every record type, not just batches. On mainnet, v2's running cost becomes
transaction fees only (a few thousand lamports per transaction): cents per year.

## Design

### One `Registry` account

The committee (members, threshold, authority, batch sequence, last batch root, last window end) plus a
Merkle Mountain Range (MMR) over every record the program has ever accepted:

- `leaf_count: u64`
- `peaks: [[u8; 32]; 32]`: peak of height `h` is valid when bit `h` of `leaf_count` is set
  (32 heights = up to 4 billion records, fixed size, no realloc).

Hashing is the same as the batch trees (RFC 9162): leaf `H(0x00 ‖ data)`, node `H(0x01 ‖ left ‖ right)`.

Append is a binary counter: carry the new leaf up while the bit at that height is set, merging with the
stored peak; store it at the first free height. O(log n) hashes, no new account.

### Records are leaves, written by the program

Every instruction that used to `init` an account now builds a record, appends `H(0x00 ‖ borsh(record))`
and emits it in an event. The record carries the `slot` and `unix_timestamp` the program read from the
`Clock` at that moment, so the "when" is still written by the program, not by the submitter.

```
enum Record {                        // Borsh, discriminant = kind
    Batch      { header, signers, signer_count, slot, committed_at },
    Forecast   { station_id, variable, based_on_batch_id, based_on_root, issued_at, target_ts,
                 horizon_hours, p10, p50, p90, model_sha256, features_root,
                 alert: Option<(threshold_milli, level)>, slot, committed_at },
    Outlook    { header, threshold_milli, triggered, signers, signer_count, slot, committed_at },
    PublicAlert{ header, signers, signer_count, slot, committed_at },
    Degradation{ batch_id, station_id, reason, score_milli, slot },
    Station    { args, slot },
}
```

Changes this forces, and why they are acceptable:

- **Forecast alert in the same instruction.** v1 had a separate `issue_alert` that read the forecast
  account. v2 reads the `AlertRule` while committing the forecast and stores the result in the record.
  Same rule (P90 ≥ threshold, target not anchored yet), one transaction less.
- **Stations are records, not accounts.** Nothing on-chain needs to read a station after registration.
- **Duplicate official warnings are no longer rejected by the chain.** v1 used `init` on the content hash.
  In v2 the same version could be appended twice; the earliest record is the one that counts, and the
  aggregator skips versions already in its ledger. The proof property ("this content existed by slot S")
  is unchanged.

### Accounts that stay

Only things the program must read later, or that are created rarely:

- `Registry` (one), `AlertRule` (one per station with a rule), `OutlookRule` (one per region);
- `Vault` and `Payout` (only when someone creates a vault; `Payout` still prevents paying twice).

### Vault payouts with a proof

`trigger_payout` receives the trigger record (Borsh) and its MMR inclusion proof
(leaf index, siblings up to its peak). The program recomputes the peak, checks it against
`Registry.peaks`, then applies the same checks as v1 (kind, `triggered`, subject, cooldown, recorded after
the vault was created). `Payout` seeds become `["payout", vault, leaf_hash]`.

A proof is against the current peaks. If another record is appended between building the proof and the
transaction landing, the peak may have merged and the transaction fails; the crank rebuilds the proof and
retries.

### Off-chain

- The ledger keeps every record in append order (`data/ledger-v2/records.jsonl`); anyone can rebuild the
  MMR and must get the on-chain peaks.
- The dashboard verifies a reading in two steps: reading → batch root (as today), then batch record →
  peak read live from the `Registry` account.
- **Permanence at zero cost:** after each daily run, the MMR root (peaks bagged) is timestamped with
  OpenTimestamps (free public calendars, anchored in Bitcoin). The `.ots` files go to the ledger. If
  devnet is ever reset, the record still has a Bitcoin-anchored timestamp.

## Rollout (all on devnet, free)

1. Program v2 with a new program ID; tests in LiteSVM for every rule, including the MMR against a
   reference implementation.
2. Oracles, aggregator, audit and vault crank on v2; dashboard verification for v2 records.
3. Replay of 2024 on v2 (devnet SOL is free), then live mode switches to v2 on a cutover day.
   Live days before the cutover stay verifiable against v1.
4. OpenTimestamps for each day's root.

Devnet SOL needed: about 5.2 (program rent plus a temporary upload buffer of the same size), from the free
faucet. Mainnet later: ~2.5 SOL of rent for the program (recoverable by closing it) and fees only after that.
