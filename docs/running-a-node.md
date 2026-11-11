# Running an independent Cota node

Cota's committee is 3 oracle nodes with a 2-of-3 threshold. Today all three are run by the same person, as
separate GitHub Actions jobs. The committee only means something when its members are different institutions
(a university, a civil defense office, an NGO), each with its own machine, its own key and its own data access.
This is what one of them needs to do.

## What a node does

Every round (`scripts/node_run.sh`), the node, on its own:

1. reads INMET's current warnings for Rio Grande do Sul and signs each version it sees;
2. asks the chain which day is waiting for a batch (`scripts/registry_day.py`). If that day has closed, it
   downloads the day's readings from the public sources (ANA river gauges, GloFAS discharge), builds the Merkle
   tree and signs its root;
3. downloads the rain forecast for the next 5 days (three weather models) and signs its summary;
4. writes everything to a folder, with an `index.json` listing each file and its SHA-256.

Honest nodes with the same sources reach the same bytes, so their signatures agree. The aggregator takes to the
chain whatever at least 2 members signed identically; the program checks the signatures itself.

## Requirements

- A Linux machine that is on most of the time (any small server or VM; no inbound ports needed).
- Rust 1.89 and Python 3.11+ to build and run (`cargo build --release -p cota-oracle --bins`).
- The node's own **Ed25519 keypair** (`solana-keygen new -o node.json`). It never leaves the institution.
- The institution's own **ANA HidroWebService login** (free registration with ANA), so that the nodes do not
  depend on a single account.
- A place to publish a folder of static files: GitHub Pages, an institutional website, any object storage.

## Running it

```bash
export KEY=/secure/path/node.json OUT=/srv/www/cota ANA_IDENTIFICADOR=… ANA_SENHA=…
scripts/node_run.sh
```

Every 30 minutes, from cron:

```
*/30 * * * * cd /opt/cota && KEY=/secure/node.json OUT=/srv/www/cota ANA_IDENTIFICADOR=… ANA_SENHA=… scripts/node_run.sh >> /var/log/cota-node.log 2>&1
```

Warnings change during the day, so frequent rounds catch every version. The batch is signed once per day,
right after 00:00 UTC (21:00 in Brasília).

Then serve `$OUT` publicly, e.g. at `https://example.org/cota/`. The folder holds only public data and
signatures; the host does not need to be trusted, because every signature is checked against the committee on
the chain, and a tampered file simply fails.

## Joining the committee

1. Send the project the node's **public key** and the URL of the published folder.
2. The URL goes into `config/nodes.toml`; the aggregator downloads it before every run (`scripts/fetch_nodes.py`).
3. The key becomes a member through a multisig proposal, replacing one of the current keys: 2 of 3 signatures
   and a public 24 h wait, as for any change to the committee. A member key cannot be added by the operator alone
   (see `docs/security-review.md`, F1). The proposal is built with
   `cota-admin --as-multisig <VAULT> --out ix.json rotate-member --old <current key> --new <node's public key>` and
   opened with `node scripts/squads/squads.mjs propose-ix --file ix.json`. It is simulated first: an instruction
   that would fail is never proposed. The full path is in `docs/operations.md`.

Until step 3, the node's signatures are downloaded but do not count. The committee holds at most 8 members; today
it can only grow by replacing a key (an `add_member` instruction is future work).

## Keeping the key safe

- One key per node, generated on the node's machine, readable only by the user that runs it (`chmod 600`).
- Keep an offline backup. If the key leaks or is lost, the multisig replaces it (`rotate_member`): the node's
  position, and the history it signed, stay the same.
- A hardware security module or a cloud key vault (KMS) is better than a file. The node only needs to sign a
  few messages a day with Ed25519; the signing call is isolated in `crates/cota-oracle/src/keys.rs`, the place
  to plug one in.

## What the institution gains

Its signature on what was known, and when, recorded where nobody can change it, alongside the other members.
If its node goes down, the others keep going (2 of 3); if it disagrees with them, that disagreement is recorded
too (`SourceDegraded`).
