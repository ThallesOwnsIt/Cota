#!/usr/bin/env python3
"""Timestamps the accumulator's root in Bitcoin with OpenTimestamps (free public calendars).

    python3 scripts/timestamp.py            # stamps the current root (if new) and upgrades pending proofs

Devnet can be reset. Once a day the root of program v2's accumulator (every record ever accepted, folded
into one hash) is written to a small text file and its SHA-256 is sent to the OpenTimestamps calendars,
which aggregate many hashes into one Bitcoin transaction. The proof (`.ots`) starts as pending and becomes
complete a few hours later, when the calendar's transaction is mined; each run upgrades the pending ones.

To check one: drop `NNNNNN.txt` and `NNNNNN.txt.ots` on https://opentimestamps.org, or run
`ots verify NNNNNN.txt.ots` with a Bitcoin node. The root itself is recomputed here from `records.jsonl`
(same algorithm as `cota_core::mmr::bag`), not taken from the audit.

Output in data/ledger/timestamps/: one `.txt` + `.txt.ots` per stamped root, and `index.json`.
"""

from __future__ import annotations

import hashlib
import json
import re
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LEDGER = ROOT / "data/ledger"
OUT = LEDGER / "timestamps"
OTS = ["uvx", "--from", "opentimestamps-client", "ots"]
MAX_PEAKS = 32


def node(left: bytes, right: bytes) -> bytes:
    return hashlib.sha256(b"\x01" + left + right).digest()


def accumulator_root(leaves: list[bytes]) -> bytes | None:
    """Peaks by binary-counter append, then folded from the newest (lowest) to the oldest (highest)."""
    peaks: list[bytes | None] = [None] * MAX_PEAKS
    for n, leaf in enumerate(leaves):
        carry, h = leaf, 0
        while (n >> h) & 1:
            carry = node(peaks[h], carry)
            peaks[h] = None
            h += 1
        peaks[h] = carry
    acc = None
    for h in range(MAX_PEAKS):
        if (len(leaves) >> h) & 1:
            acc = peaks[h] if acc is None else node(peaks[h], acc)
    return acc


def ots(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run([*OTS, *args], capture_output=True, text=True, timeout=180)


def status(ots_file: Path) -> dict:
    info = ots("info", str(ots_file))
    heights = [int(h) for h in re.findall(r"BitcoinBlockHeaderAttestation\((\d+)\)", info.stdout)]
    return {"bitcoin_block": min(heights)} if heights else {"bitcoin_block": None}


def main() -> None:
    records = [json.loads(line) for line in (LEDGER / "records.jsonl").read_text().splitlines() if line.strip()]
    audit = json.loads((LEDGER / "audit.json").read_text())
    root = accumulator_root([bytes.fromhex(r["leaf"]) for r in records])
    if root is None:
        sys.exit("acumulador vazio: nada a carimbar")
    OUT.mkdir(parents=True, exist_ok=True)

    # 1. The current root, at most once per UTC day: the tree only grows, so a later root covers every
    #    record of the earlier ones.
    name = f"{len(records):06d}.txt"
    today = datetime.now(timezone.utc).date().isoformat()
    stamped_today = any(
        line.startswith("stamped") and line.split()[1].startswith(today)
        for f in OUT.glob("*.txt")
        for line in f.read_text().splitlines()
    )
    if not stamped_today and not (OUT / f"{name}.ots").exists():
        (OUT / name).write_text(
            "Cota v2 accumulator root\n"
            f"program  {audit['program']}\n"
            f"registry {audit['registry']}\n"
            f"records  {len(records)}\n"
            f"root     {root.hex()}\n"
            f"stamped  {datetime.now(timezone.utc).isoformat(timespec='seconds')}\n"
        )
        r = ots("stamp", str(OUT / name))
        if r.returncode != 0:
            (OUT / name).unlink()
            sys.exit(f"ots stamp falhou: {r.stderr.strip()}")
        print(f"raiz de {len(records)} registros enviada aos calendários: {root.hex()}")

    # 2. Pending proofs: ask the calendars whether the Bitcoin transaction was mined.
    index = []
    for f in sorted(OUT.glob("*.txt.ots")):
        st = status(f)
        if st["bitcoin_block"] is None:
            ots("upgrade", str(f))
            st = status(f)
        text = (OUT / f.name.removesuffix(".ots")).read_text()
        fields = dict(line.split(None, 1) for line in text.splitlines()[1:] if line.strip())
        index.append(
            {
                "file": f.name.removesuffix(".ots"),
                "records": int(fields["records"]),
                "root": fields["root"].strip(),
                "stamped": fields["stamped"].strip(),
                "bitcoin_block": st["bitcoin_block"],
            }
        )
    (OUT / "index.json").write_text(json.dumps(index, indent=1) + "\n")
    done = sum(1 for i in index if i["bitcoin_block"])
    print(f"{len(index)} carimbo(s), {done} já confirmado(s) no Bitcoin")


if __name__ == "__main__":
    main()
