# The verifier

`verify.js` is the code that checks Cota's proofs. The site runs it in every visitor's browser, and anyone can run
it with Node.

It recomputes, with no server to trust:
- the Borsh bytes of a reading and the RFC 9162 Merkle tree of a batch (the same bytes as the Rust and Python code);
- the accumulator of every record (Merkle Mountain Range) and the inclusion proof of any record;
- the decoding of the program's accounts, read straight from the chain over JSON-RPC.

## Checking the whole ledger without the site

```sh
node verifier/check.mjs                 # Node 20+; --rpc URL, --ledger data/ledger
```

What it checks:
- every record in `data/ledger/records.jsonl` matches its leaf hash and decodes completely;
- the accumulator rebuilt from those leaves has the same peaks the program keeps on-chain;
- the inclusion proof of every record reaches its on-chain peak.

Changing a single character of any record makes it fail, even if the record's leaf is recomputed to hide the change.
The peaks on the chain cannot be rewritten.

`cota-audit` (Rust) does the same check from the chain's events. Two implementations agreeing on the same bytes
is part of the point.

## How the site uses it

- The site's code is in a private repository; this file is not copied there.
- At every deploy, the site takes `verifier/verify.js` from this repository, at the commit it builds from. It shows
  the file's SHA-256 in the footer, with a link to that exact file here.
- To compare, take the site's `/verify.js` and hash it (`curl -s https://cota-eight.vercel.app/verify.js | sha256sum`),
  then compare with the file at the linked commit.
