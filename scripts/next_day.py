#!/usr/bin/env python3
"""Next UTC day to anchor in live mode (or nothing, if the ledger is up to date).

    python3 scripts/next_day.py            → one day (the next missing one), or an empty line
    python3 scripts/next_day.py --all      → every missing day, one per line

The current day never goes in: its window is still open. On the first run after
the 2024 replay, it starts BACKFILL_DAYS (default 7) before yesterday.
"""

import glob
import json
import os
import sys
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LIVE_FROM = date(2025, 1, 1)


def missing_days() -> list[date]:
    yesterday = datetime.now(timezone.utc).date() - timedelta(days=1)
    files = sorted(glob.glob(str(ROOT / "data/ledger/batches/*.json")))
    last = date.fromisoformat(json.loads(Path(files[-1]).read_text())["day"]) if files else None
    if last is None or last < LIVE_FROM:
        first = yesterday - timedelta(days=int(os.environ.get("BACKFILL_DAYS", "7")) - 1)
    else:
        first = last + timedelta(days=1)
    return [first + timedelta(days=i) for i in range((yesterday - first).days + 1)]


if __name__ == "__main__":
    days = missing_days()
    print("\n".join(d.isoformat() for d in (days if "--all" in sys.argv else days[:1])))
