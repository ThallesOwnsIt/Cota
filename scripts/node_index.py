#!/usr/bin/env python3
"""Writes the index.json of a node's publication folder: every file with its size and SHA-256.

    python3 scripts/node_index.py data/node-out

The aggregator (scripts/fetch_nodes.py) reads the index to know what to download and checks each file against
it. The SHA-256 only protects the transfer; what makes a file count is the node's signature inside it.
"""

from __future__ import annotations

import hashlib
import json
import sys
from datetime import datetime, timezone
from pathlib import Path


def main() -> None:
    out = Path(sys.argv[1])
    files = {}
    for p in sorted(out.rglob("*.json")):
        rel = p.relative_to(out).as_posix()
        if rel == "index.json":
            continue
        b = p.read_bytes()
        files[rel] = {"sha256": hashlib.sha256(b).hexdigest(), "bytes": len(b)}
    index = {"format": "cota-node-publication/1", "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"), "files": files}
    (out / "index.json").write_text(json.dumps(index, indent=1) + "\n")
    print(f"{out}/index.json: {len(files)} arquivo(s)")


if __name__ == "__main__":
    main()
