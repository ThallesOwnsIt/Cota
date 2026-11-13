# Cota ledger v3: specification

Status: reviewed with the project owner (Etapa 4.1), 13/11/2026. Not built yet.

v3 replaces the on-chain program with a ledger of its own:

- the records, the accumulator, the committee signatures and the validity rules stay the same;
- the rules move from a Solana program to one library, used by whoever builds the ledger and by every verifier;
- the root is signed by the committee in a checkpoint and anchored in Bitcoin.

The reasons are in `~/Desktop/Cota-Etapa4-Livro-Razao.md` (Portuguese) and in the diary:

- a Solana transaction holds 4 to 5 committee signatures, so a majority committee stopped at 7 members;
- devnet is not an archive, and mainnet costs SOL;
- the project should not depend on one network.

The first v3 design, an on-chain program, is kept on the branch `solana-v3-pausado`. Its rules are the starting point
of the library.

## 1. What a verifier can prove

| Question | How |
|---|---|
| Who said it? | Every entry carries Ed25519 signatures of a majority of the committee of its epoch |
| What exactly? | The entry's bytes are a leaf of the accumulator; the data an entry refers to (a batch's readings, a warning's text, a forecast file) is bound by its SHA-256 or Merkle root |
| Nothing changed afterwards? | The accumulator is append-only; every checkpoint commits to its root and to the previous checkpoint |
| Since when? | Each checkpoint is anchored in Bitcoin (OpenTimestamps): the entries it covers existed no later than that block |
| Everyone sees the same ledger? | Checkpoints form a hash chain, are anchored publicly and are cosigned by the members' nodes, each holding a full copy |
| Without trusting anyone? | The whole ledger, the library and the verifier are public; verification runs offline, in a terminal or a browser |

What the Solana program did and the verifier now does: §10.

## 2. Continuity with v1 and v2

- v2's accumulator continues. Leaves `0 … N−1` are v2's records, byte for byte as program
  `FnjR7wG5zicqWJ6KQuWQ5UcqqhcSghrYozVgMmmDFNwR` emitted them (`data/ledger/records.jsonl`). Here N is v2's
  `leaf_count` at the cutover.
- Leaf `N` is the `Genesis` entry (§4.1). It pins v2's final state, so anything written to v2 after the cutover is
  outside this ledger by definition.
- From `N` on, every leaf is a v3 entry. v1 stays a separate, frozen archive; Genesis records its hash.
- Leaf and node hashes are as in v1/v2: leaf = `H(0x00 ‖ bytes)`, node = `H(0x01 ‖ left ‖ right)`, with H = SHA-256.
  This is RFC 9162's tree, appended as a Merkle Mountain Range (`cota_core::mmr`).
- **v3 entries are prefixed with the byte `0xC3`**. A v2 record starts with its kind tag (0–5), so the two kinds of
  leaf can never be confused: v3 leaf = `H(0x00 ‖ 0xC3 ‖ borsh(Entry))`.

## 3. Entries

```text
Entry  = { body: Body, quorum: Quorum }
Quorum = { committee: u8, epoch: u32, signatures: Vec<(member: u16, signature: [u8; 64])> }
```

- Each `Body` kind has its own signing domain: `message = domain ‖ borsh(body)` (§3.1). A signature for one kind can
  never count as another, and a v2 signature (domains `…-v2`) never counts here.
- `member` is the signer's position in the committee of `epoch`. Positions are u16, so there is no 8- or 32-member
  limit. Signatures are sorted by position and have no repeats.
- An entry is valid only if it carries signatures from at least `threshold` distinct members of that epoch's
  committee, all over exactly this message. Signatures from non-members, or extra or invalid ones, make the entry
  invalid (as in v2, so a signature set is never ambiguous).
- The signatures are inside the leaf: the evidence of who vouched is itself in the accumulator. In v2 only a
  bitmap was.
- `committee` is 0 (the only committee). The field lets regional committees come later without a new format.
- **Every entry is committee-signed.** There is no operator key. Stations and degradation reports, which v2 let the
  operator write alone, now need the majority like everything else. The nodes read the same catalog, so they sign
  the same station entries.

### 3.1 Bodies and domains

| Kind | Domain | Body (new or changed fields in bold) |
|---|---|---|
| `Genesis` | `COTA-GENESIS-v3` | v2 program id, v2 registry, **v2 leaf_count N and bagged root**, v1 archive SHA-256, initial committee (members, threshold), cutover day |
| `CommitteeChange` | `COTA-COMMITTEE-v3` | **new epoch, members, threshold, announced_at** (see §5) |
| `StreamCreate` | `COTA-STREAM-v3` | stream id (≤ 16 bytes), starts_at |
| `RuleSet` | `COTA-RULE-v3` | one of: flow alert rule (station, stream, threshold, level), outlook rule (region, stream, threshold, window, min models, spec SHA-256), external source (source, stream) |
| `Station` | `COTA-STATION-v3` | station args as in v2, **stream** |
| `Batch` | `COTA-BATCH-v3` | **stream**, batch_id, t_start, t_end, leaf_count, merkle_root, station_set_hash, prev_batch_root |
| `Degradation` | `COTA-DEGRADATION-v3` | **stream**, batch_id, station, score, reason |
| `Forecast` | `COTA-FORECAST-v3` | **stream**, station, based_on_batch_id, based_on_root, target_ts, horizon, p10/p50/p90, model SHA-256, features root |
| `PublicAlert` | `COTA-PUBALERT-v3` | as v2: issuer, alert id, content SHA-256, severity, onset, expires |
| `Outlook` | `COTA-OUTLOOK-v3` | as v2: region, issued_at, window_end, models, rain mean/min/max, content SHA-256 |
| `ExternalForecast` | `COTA-EXTFC-v3` | source, **stream**, issued_at, horizon_days, content SHA-256 |
| `Correction` | `COTA-CORRECTION-v3` | corrects (leaf index), reason, content SHA-256 of the corrected data |

Values are integers (thousandths of the unit, UTC seconds), as in v2. A forecast's quantiles are rounded to whole
m³/s before signing, so nodes on different machines reach the same bytes.

## 4. Validity rules

The library is a state machine: `apply(state, entry) → state' | refusal`. The state:

- the committee (members, threshold, epoch) and at most one pending change;
- per stream: next batch id, last root, last `t_end`;
- the rules and sources, each with its `last_issued_at`;
- the receipts: warning versions and corrections already recorded;
- `leaf_count` and the peaks.

All of it is rebuilt from the entries alone.

1. **Genesis**: exactly once, at index N, as the first v3 entry. Its v2 fields must match v2's records (leaf_count,
   bagged root).
2. **Streams and rules**: a stream id, a rule's station or region, and a source are created once and never change.
   A rule's stream must exist.
3. **Station**: its stream exists.
4. **Batch**: in its stream, `batch_id` is next, `prev_batch_root` is the last root, `t_end > t_start ≥` the last
   `t_end`. A gap between windows is allowed and stays visible; an overlap is not.
5. **Degradation**: only for its stream's latest batch.
6. **Forecast**: `based_on` is its stream's latest batch; `target_ts ≥` that batch's `t_end`; p10 ≤ p50 ≤ p90;
   horizon > 0. The library applies the station's alert rule (if any, and only if it is the same stream) and the
   result is part of the derived state, as v2's program did. A second entry with the same body is refused.
7. **PublicAlert**: the version (`content_sha256`) was never recorded; expires ≥ onset; severity ≤ 3.
8. **Outlook**: `issued_at` = its rule's stream's latest `t_end`; window = the rule's; models ≥ the rule's
   minimum; min ≤ mean ≤ max; `issued_at >` the rule's `last_issued_at`. The library applies the rule (warning
   fired or not).
9. **ExternalForecast**: the source exists and belongs to this stream; `issued_at` = the stream's latest `t_end`;
   `issued_at >` the source's `last_issued_at`; horizon > 0.
10. **Correction**: `corrects < leaf_count`; known reason; the pair (corrects, content) was never recorded. The
    corrected entry is never removed.

Deduplication costs nothing here: receipts are part of the state, not accounts with rent.

## 5. The committee governs itself

- `CommitteeChange` is signed by a majority of the **current** committee. Its new committee must also be a
  majority committee: threshold > members/2, threshold ≤ members, distinct keys.
- **It takes effect 7 days after it is announced**, counted on the ledger clock (§6). Until then the current
  committee keeps signing, and anyone reading the ledger or the site sees the pending change. This replaces the
  multisig's 24 h wait.
- A second change cannot be announced while one is pending. A pending change can be withdrawn by a
  `CommitteeChange` signed by the current majority that restates the current committee.
- A lost key does not stop the ledger while a majority remains. If a majority of keys is lost, the ledger stops. A
  new ledger can then start with a Genesis pointing at this one's last checkpoint; it is visible as a new start,
  never a silent rewrite.

## 6. Time

There is no trusted clock inside the ledger. Time comes from three places:

- **Ledger clock**: the largest `t_end` of any batch so far. It is deterministic, and the 7-day committee delay
  uses it.
- **Signed claims**: each checkpoint states `signed_at`, which is the committee's word.
- **Anchors**: Bitcoin proves each checkpoint existed no later than its block. A verifier checks that no checkpoint
  covers a batch whose window ends after the checkpoint's anchor time, plus a tolerance. That is the "no batches
  from the future" check the program got from Solana's clock.

The precision is hours, not seconds. Forecasts are made at least 24 h ahead and warnings last days. A faster anchor
(§7) can be added at any time.

## 7. Checkpoints and anchors

```text
Checkpoint = { log: "cota", size: u64, root: [u8; 32], prev: [u8; 32], epoch: u32, signed_at: i64 }
message    = "COTA-CHECKPOINT-v3" ‖ borsh(Checkpoint)
```

- `root` is the accumulator's bagged root at `size` leaves (`cota_core::mmr::bag`). `prev` is the SHA-256 of the
  previous checkpoint's message (zeros for the first). `signed_at` is the ledger clock at that size, so every member
  signs the same bytes; real time comes from the anchors.
- Each node signs the checkpoint of the ledger as it found it, after replaying and verifying it itself. The
  aggregator publishes it in the next round, before appending that round's entries. So the checkpoint of a round's
  entries appears one round (30 minutes) later.
- A checkpoint is valid with a majority of signatures from the committee of `epoch`. It has no member limit,
  because a checkpoint is a file, not a transaction. Every member's node signs every checkpoint it can verify, so
  the extra signatures act as witnesses.
- A round that appends entries publishes one checkpoint. The daily Bitcoin stamp of today's
  `data/ledger/timestamps/` becomes the checkpoint's stamp.
- **Anchors** are interchangeable pieces. Each takes a checkpoint and returns a proof stored beside it:
  1. **Bitcoin / OpenTimestamps**: always. Free, permanent, neutral. Proofs are upgraded once the block confirms.
  2. **Rede Blockchain Brasil**: when a partner institution opens access. One transaction per day with the
     checkpoint hash. It gives institutional weight in Brazil but is never the base: writing depends on an
     invitation, and the BNDES–TCU agreement runs to about April 2027.
  3. **Any other public chain**: optional, for second-level time. One tiny transaction per day.
- **Mirrors**: GitHub (this repository), Software Heritage, Arweave, and the nodes of the institutions.

## 8. Rehearsal and tools

- `cota-ledger3` (in `cota-oracle`): `sign` (each node), `collect` (aggregator), `verify` (anyone). A node rewrites
  `data/nodes/<node>/v3/` every round with exactly what it attests: the bodies it observed that the ledger does not
  have yet, and the checkpoint of the state it verified.
- **Rehearsal** (decided on 13/11/2026): with `COTA_V3_ENSAIO=1`, the round also writes `data/ledger-v3-ensaio`
  next to v2. Its Genesis cites the v2 records of the day the rehearsal starts (`config/ledger-v3.toml`). The real
  cutover happens on the new server; the rehearsal ledger is then discarded.
- The flow forecast: each node runs the public model (`cota_ml.forecast --out <node>/forecast`) on the main
  stream's latest batch, rounds the quantiles to whole m³/s and signs. The model reads that batch from the v2 ledger,
  where it lands at the end of the round that anchors it, so the nodes forecast one round after the batch
  (`cota-ledger3 forecast-day` says which day). After the cutover the model reads the v3 batches instead.

## 9. Storage

`data/ledger-v3/`:

- `entries.jsonl`: one line per v3 entry, `{index, leaf, entry (hex), kind}`, like v2's `records.jsonl`.
- `checkpoints/<size>.json`: the checkpoint, its signatures, its anchor proofs (`<size>.ots`, and others).
- Human-readable files per kind, as today. Examples: `batches/<id>.json` with the readings, whose Merkle root is in
  the entry; `public_alerts/…` with the warning text; `external_forecasts/…` with the forecast content.

`data/ledger/` (v2) stays as it is, frozen at the cutover.

## 10. What moved from the program to the verifier

| The Solana program… | Now |
|---|---|
| refused an invalid record before it entered | the library refuses it when building. If an invalid entry is published anyway, every verifier detects it, and it carries the signatures of who signed it |
| kept the peaks in an account | the checkpoint carries the root, signed and anchored |
| read time from its clock | ledger clock + anchors (§6) |
| checked admin changes against the upgrade authority (multisig) | the current committee's majority, with a 7-day delay (§5) |
| paid out vaults on an inclusion proof | frozen. A future contract on any chain would verify a signed checkpoint and an inclusion proof, the same logic |

## 11. Verification

A full verifier, in a terminal or a browser through WebAssembly:

1. Loads v2's records and the v3 entries. Checks that leaves `0 … N−1` rebuild v2's on-chain peaks (as
   `verifier/check.mjs` does today) and that Genesis matches them.
2. Applies every entry with the library: signatures, quorum, rules. It stops at the first refusal and says which
   entry and why.
3. Recomputes each checkpoint's root and its `prev` chain, and checks its signatures.
4. Checks each anchor proof: OpenTimestamps against Bitcoin headers, others if present.
5. Reports what the derived state says: forecasts recorded before their target, warnings, outlooks that fired,
   corrections.

A light check of one entry needs only an inclusion proof against one anchored, signed checkpoint.

## 12. Cutover

- **Day D−1**: last v2 batch. From then on nothing more is sent to the v2 program.
- **Day D**: the committee signs Genesis at index N with the v2 summary and the initial committee (today's three
  node keys, threshold 2), then the streams (`guaiba`, `uruguai`, `litoranea`), the rules (basin-wide and the five
  sub-basins), the flow alert rule and the GloFAS source. The first checkpoint is published and anchored.
- **D 00:00 UTC**: the streams start. `guaiba` carries the 14 gauges; the other two start empty until Etapa 5.
- The site reads v1, v2 and v3.

## 13. Security considerations

- **A majority of the committee colludes.** It can sign false entries, as in any k-of-n system. Mitigations: the
  members are independent institutions (Etapa 7); every entry stays public and attributable; a committee change
  waits 7 days in public.
- **The aggregator misbehaves.** It can withhold entries or publish invalid ones. Withholding shows as a gap, and
  any member can publish instead. Invalid entries are refused by every verifier. It cannot forge signatures.
- **Split view** (showing different ledgers to different people). The checkpoint hash chain, public anchors and
  members' cosignatures make two versions provable.
- **Rewriting the past.** Any change alters the root of every later checkpoint, and those roots are in Bitcoin.
- **Lost or leaked key.** One key alone signs nothing valid. It is replaced by a `CommitteeChange` (7 days). A
  leaked key's past signatures stay valid for the entries they signed; that is the point of an evidence ledger.
- **Time manipulation.** Batch windows are checked against anchor times (§6).

## 14. Decisions taken in review (13/11/2026)

- A committee change takes effect 7 days after it is announced, and the current majority may withdraw it while it
  is pending (§5).
- Every entry is committee-signed; there is no operator key (§3).
- A checkpoint is published and anchored on every round that appends entries, not once a day (§7).
- The verifier uses the same rule library as the builder: Rust, built for the browser with WebAssembly. It replaces
  the hand-written decoder in `verifier/verify.js`.
