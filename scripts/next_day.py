#!/usr/bin/env python3
"""Próximo dia UTC a ancorar no modo ao vivo (ou nada, se o ledger está em dia).

    python3 scripts/next_day.py            → um dia (o próximo que falta), ou linha vazia
    python3 scripts/next_day.py --all      → todos os que faltam, um por linha

O dia corrente nunca entra: a janela dele ainda está aberta. Na primeira execução depois
do replay de 2024, começa BACKFILL_DAYS (padrão 7) antes de ontem.
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
