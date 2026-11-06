#!/usr/bin/env python3
"""Downloads the signatures that independent nodes publish, before the aggregator runs.

    python3 scripts/fetch_nodes.py [--config config/nodes.toml] [--dest data/nodes]

Each node in config/nodes.toml publishes a folder (scripts/node_run.sh) with an index.json. Every listed file is
downloaded into data/nodes/<name>/, the same layout the in-repository nodes use, and checked against the index.
Nothing here is trusted: the aggregator only counts signatures from committee members, over the exact message.
A node that is down or serves garbage just counts as absent.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import tomllib
import urllib.request
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parents[1]
MAX_BYTES = 2_000_000


def get(url: str) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": "cota-aggregator"})
    with urllib.request.urlopen(req, timeout=60) as r:
        data = r.read(MAX_BYTES + 1)
    if len(data) > MAX_BYTES:
        raise ValueError(f"{url}: maior que {MAX_BYTES} bytes")
    return data


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--config", type=Path, default=ROOT / "config/nodes.toml")
    ap.add_argument("--dest", type=Path, default=ROOT / "data/nodes")
    args = ap.parse_args()
    nodes = tomllib.loads(args.config.read_text()).get("node", []) if args.config.exists() else []
    if not nodes:
        print("nenhum nó externo em config/nodes.toml")
        return
    for node in nodes:
        name, base = node["name"], node["url"].rstrip("/")
        dest = args.dest / name
        try:
            index = json.loads(get(f"{base}/index.json"))
        except Exception as e:  # a node that is down counts as absent
            print(f"{name}: índice indisponível ({e})")
            continue
        got = 0
        for rel, meta in index.get("files", {}).items():
            path = PurePosixPath(rel)
            # Only files inside the node's folder, never ../ or absolute paths.
            if path.is_absolute() or ".." in path.parts or path.suffix != ".json":
                print(f"{name}: caminho recusado {rel!r}")
                continue
            target = dest.joinpath(*path.parts)
            if target.exists() and hashlib.sha256(target.read_bytes()).hexdigest() == meta["sha256"]:
                continue
            try:
                data = get(f"{base}/{rel}")
            except Exception as e:
                print(f"{name}: {rel} indisponível ({e})")
                continue
            if hashlib.sha256(data).hexdigest() != meta["sha256"]:
                print(f"{name}: {rel} não confere com o índice; ignorado")
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            got += 1
        print(f"{name}: {got} arquivo(s) novo(s) de {base}")


if __name__ == "__main__":
    main()
