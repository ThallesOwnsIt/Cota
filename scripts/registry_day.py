#!/usr/bin/env python3
"""The UTC day the chain is waiting a batch for, if it has already closed (or nothing).

    python3 scripts/registry_day.py --rpc https://api.devnet.solana.com

Reads program v2's registry account: the next batch starts where the latest one ended (`last_t_end`), so the
day to sign is the date of `last_t_end`, as long as that day is over. Used by scripts/node_run.sh, so an
independent node needs nothing but the chain to know what to sign.
"""

from __future__ import annotations

import argparse
import base64
import json
import struct
import urllib.request
from datetime import datetime, timezone

# PDA ["registry"] of program FnjR7wG5zicqWJ6KQuWQ5UcqqhcSghrYozVgMmmDFNwR.
REGISTRY = "6CNtD18LQ4bBZTGK1C2Z6RuJrSwyxCD3ToRpiWE3y9bR"


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--rpc", default="https://api.devnet.solana.com")
    ap.add_argument("--registry", default=REGISTRY)
    args = ap.parse_args()
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "getAccountInfo", "params": [args.registry, {"encoding": "base64", "commitment": "confirmed"}]})
    req = urllib.request.Request(args.rpc, data=body.encode(), headers={"Content-Type": "application/json"})
    data = base64.b64decode(json.load(urllib.request.urlopen(req, timeout=60))["result"]["value"]["data"][0])
    # Registry: discriminator (8) | authority (32) | members: u32 + 32·n | threshold u8 | next_batch_id u64 |
    # last_batch_root (32) | last_t_end i64 | ...
    o = 8 + 32
    n = struct.unpack_from("<I", data, o)[0]
    o += 4 + 32 * n + 1 + 8 + 32
    last_t_end = struct.unpack_from("<q", data, o)[0]
    day = datetime.fromtimestamp(last_t_end, timezone.utc).date()
    if day < datetime.now(timezone.utc).date():
        print(day.isoformat())


if __name__ == "__main__":
    main()
