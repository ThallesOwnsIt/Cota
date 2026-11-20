#!/usr/bin/env python3
"""Checks config/locations.toml, the places Cota watches and their official levels.

    python3 scripts/check_locations.py [--recent FILE]

For every place:
  - the reference gauge exists in config/stations-live.toml as an ANA river gauge;
  - the levels are in order (attention < alert < flood < record);
  - a confirmed level has a source and a link;
  - the recent levels are not above the flood level almost all the time. A level copied from another gauge's scale (each gauge has
    its own zero) usually fails here: in Nov 2026 a 2025 bulletin's 6 / 9 / 15 m for Estrela would have put an
    ordinary 13.4 m day above flood.

Recent levels come from the ledger's last 7 batches; `--recent` adds a JSON {"<ANA code>": [cm, ...]} for gauges
not in the ledger yet. Exit code 1 on any problem, 0 otherwise (warnings do not fail).
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ORDER = ["attention_m", "alert_m", "flood_m", "record_m"]


def recent_levels(extra: Path | None) -> dict[str, list[float]]:
    """Recent readings per gauge, in metres: the ledger's last 7 batches, plus an optional file."""
    out: dict[str, list[float]] = {}
    v3 = ROOT / "data/ledger-v3/batches"
    paths = sorted((ROOT / "data/ledger/batches").glob("*.json"))[-7:]
    for stream in sorted({p.name.split("-")[0] for p in v3.glob("*.json")}) if v3.exists() else []:
        paths += sorted(v3.glob(f"{stream}-*.json"))[-7:]
    for path in paths:
        for r in json.loads(path.read_text())["readings"]:
            if r["variable"] == "river_stage":
                out.setdefault(r["station_id"], []).append(r["value_milli"] / 1000)
    if extra and extra.exists():
        for code, cms in json.loads(extra.read_text()).items():
            out.setdefault(f"ANA-{code}", [c / 100 for c in cms])
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--recent", type=Path, help='JSON {"<ANA code>": [cm, ...]} for gauges not in the ledger yet')
    args = ap.parse_args()

    stations = {}
    for cat in ("config/stations-live.toml", "config/stations-rs.toml"):
        if (ROOT / cat).exists():
            stations.update({s["id"]: s for s in tomllib.loads((ROOT / cat).read_text())["station"]})
    places = []
    for f in ("config/locations.toml", "config/locations-rs.toml"):
        if (ROOT / f).exists():
            places += tomllib.loads((ROOT / f).read_text())["location"]
    levels = recent_levels(args.recent)
    problems, warnings, seen = [], [], set()

    for p in places:
        pid = p["id"]
        if pid in seen:
            problems.append(f"{pid}: id repetido")
        seen.add(pid)
        st = stations.get(p["gauge"])
        if not st or st["source"] != "ana" or st["variable"] != "river_stage":
            problems.append(f"{pid}: a régua {p['gauge']} não é uma régua da ANA nos catálogos")
        lv = [(k, p[k]) for k in ORDER if k in p]
        if any(a[1] >= b[1] for a, b in zip(lv, lv[1:])):
            problems.append(f"{pid}: cotas fora de ordem {dict(lv)}")
        if p.get("confirmed") and not (p.get("source") and p.get("source_url") and lv):
            problems.append(f"{pid}: cota confirmada precisa de cota, fonte e link")
        recent = levels.get(p["gauge"], [])
        lowest = next((v for k, v in lv if k != "record_m"), None)
        top = p.get("flood_m", lowest)
        if lowest is not None and recent:
            med = statistics.median(recent)
            p10 = sorted(recent)[len(recent) // 10]
            # Above the flood level almost all week: a level from another gauge's scale. Above the lowest level most of
            # the week can be a real flood (the Uruguay in Nov/2026): a warning to look at, not a failure.
            if p10 >= top:
                problems.append(f"{pid}: 90% das leituras recentes acima da cota de inundação {top:.2f} m (cota de outra régua?)")
            elif med >= lowest:
                warnings.append(f"{pid}: rio alto agora (mediana recente {med:.2f} m, máxima {max(recent):.2f} m, menor cota {lowest:.2f} m): cheia ou cota de outra régua, conferir")
        elif lowest is not None:
            warnings.append(f"{pid}: sem leituras recentes para conferir as cotas com o nível real")
        status = "cotas oficiais" if p.get("confirmed") else ("cotas a confirmar" if lv else "sem cota (só variação)")
        med = f"{statistics.median(recent):6.2f} m" if recent else "     —  "
        print(f"  {pid:<24} {p['gauge']:<14} mediana {med}   {status}")

    for w in warnings:
        print(f"aviso: {w}")
    for e in problems:
        print(f"PROBLEMA: {e}")
    print(f"\n{len(places)} locais, {sum(1 for p in places if p.get('confirmed'))} com cotas oficiais; "
          f"{len(problems)} problema(s), {len(warnings)} aviso(s)")
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
