#!/usr/bin/env python3
"""The station catalog for the rest of Rio Grande do Sul (Etapa 5.4): `config/stations-rs.toml`.

    python3 scripts/build_catalog_rs.py

From the ANA inventory (`data/inventory/ana-rs.json`, scripts/ana_inventory.py):
- every telemetric river gauge that was transmitting, except those already in `config/stations-live.toml`;
- each with its stream (the region its water goes to: guaiba, uruguai, litoranea) and its kind: `rio`, `jusante` (right
  below a hydropower dam: the river, regulated by the water the plant releases) or `reservatorio` (at the dam: the
  level of the plant's lake, which follows its operation, not floods). ANA's own station names say which: BARRAMENTO
  or RESERVATÓRIO for the lake, JUSANTE for below; a MONTANTE gauge is on the river above the lake.
- plus the GloFAS points that the rain rules of Etapa 5.3 (`ml/models/outlook-rules-rs`) take as their flood target.

The ledger signs each station's name, so the name says the kind in words. Read-only on its inputs; standard library.
"""

from __future__ import annotations

import json
import re
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
INVENTORY = ROOT / "data/inventory/ana-rs.json"
LIVE = ROOT / "config/stations-live.toml"
RULES = ROOT / "ml/models/outlook-rules-rs"
REPORT = ROOT / "ml/reports/rs-rules.json"
OUT = ROOT / "config/stations-rs.toml"
MAX_NAME = 64  # bytes, cota_ledger::MAX_STATION_NAME_LEN

SMALL = {"de", "da", "do", "das", "dos", "e", "ou", "d'"}
# ANA cuts long names ("PCH PASSO DO INFERNO BARRAMENT"): match the stems.
LAKE, BELOW = r"BARRAMENT|RESERVAT", r"JUSANT"
PLANT = re.compile(r"\b(UHE|PCH|CGH)\s+(.+?)\s+(BARRAMENT|RESERVAT|JUSANT|MONTANT)")


def kind(ana_name: str) -> str:
    if re.search(LAKE, ana_name):
        return "reservatorio"
    if re.search(BELOW, ana_name):
        return "jusante"
    return "rio"


def title(text: str) -> str:
    words = text.lower().split()
    return " ".join(w if (i and w in SMALL) else w[:1].upper() + w[1:] for i, w in enumerate(words))


def river(name: str) -> str:
    """The catalog's style drops 'Rio ' ('Jacuí em São Jerônimo'), but not before a preposition ('Rio das Antas')."""
    t = title(name.strip())
    rest = t[4:]
    return rest if t.startswith("Rio ") and rest.split(" ")[0] not in SMALL else t


def fit(text: str, tail: str) -> str:
    """`text` + `tail` within the ledger's limit, shortening `text` (never the kind, in `tail`)."""
    while len((text + tail).encode()) > MAX_NAME:
        text = text[:-2].rstrip() + "…"
    return text + tail


def station_name(g: dict, k: str) -> str:
    """'Jacuí em Marau (ANA)'; at a dam, its plant: 'Caí: reservatório da UHE Bugres (ANA)', 'Caí abaixo da UHE Canastra
    (ANA)', 'Jacuí acima da PCH Ernestina (ANA)'."""
    rv = river(g["river"] or g["name"])
    m = PLANT.search(g["name"] or "")
    if m:
        plant = f"{m.group(1)} {title(m.group(2))}"
        if k == "reservatorio":
            return fit(f"{rv}: reservatório da {plant}", " (ANA)")
        if k == "jusante":
            return fit(f"{rv} abaixo da {plant}", " (ANA)")
        if m.group(3) == "MONTANT":  # on the river above the plant's lake
            return fit(f"{rv} acima da {plant}", " (ANA)")
    tail = {"rio": " (ANA)", "jusante": ", abaixo de usina (ANA)", "reservatorio": ", reservatório (ANA)"}[k]
    return fit(f"{rv} em {title(g['municipality'] or '')}", tail)


def glofas_name(basin: dict) -> str:
    """'Ibicuí em Manoel Viana (GloFAS)': the basin's river at its main gauge's town."""
    river_, town = basin["name"].split(" (")[0], basin["gauge"].split(" ", 1)[1]
    return f"{river_} (GloFAS)" if river_ == town else f"{river_} em {town} (GloFAS)"


def toml_str(s: str) -> str:
    return json.dumps(s, ensure_ascii=False)


def main() -> None:
    inv = json.loads(INVENTORY.read_text())
    live = {s["id"] for s in tomllib.loads(LIVE.read_text())["station"]}
    gauges = [g for g in inv["gauges"] if g["transmitting"] and f"ANA-{g['code']}" not in live]
    gauges.sort(key=lambda g: ({"guaiba": 0, "uruguai": 1, "litoranea": 2}[g["region"]], g["code"]))

    out = [
        "# Stations of the rest of Rio Grande do Sul (Etapa 5): written by scripts/build_catalog_rs.py, do not edit by hand.",
        f"# Source: ANA inventory checked until {inv['checked_until']} (data/inventory/ana-rs.json), the gauges that were",
        "# transmitting and are not in stations-live.toml; and the GloFAS points of the rain rules in ml/models/outlook-rules-rs.",
        "# `stream`: the region whose daily batch carries the station. `kind`: rio, jusante (below a dam) or reservatorio",
        "# (the dam's lake, which follows the plant's operation, not floods).",
        "",
    ]
    report = json.loads(REPORT.read_text())["basins"]
    for rule in sorted(RULES.glob("*.json")):
        spec = json.loads(rule.read_text())["spec"]
        sid = spec["region"]
        lat, lon = report[sid]["target_cell"]
        region = report[sid]["region"]
        out += [
            "[[station]]",
            f"id = {toml_str(spec['target']['station_id'])}",
            f"name = {toml_str(glofas_name(report[sid]))}",
            'source = "open_meteo_glofas"',
            'variable = "river_discharge"',
            f"lat = {lat}",
            f"lon = {lon}",
            f'stream = "{region}"',
            "",
        ]
    names: dict[str, int] = {}
    for g in gauges:
        k = kind(g["name"] or "")
        name = station_name(g, k)
        names[name] = names.get(name, 0) + 1
        out += [
            "[[station]]",
            f'id = "ANA-{g["code"]}"',
            f"name = {toml_str(name)}",
            'source = "ana"',
            'variable = "river_stage"',
            f"lat = {round(g['lat'], 4)}",
            f"lon = {round(g['lon'], 4)}",
            f'datum_id = "ANA-{g["code"]}-regua"',
            f'stream = "{g["region"]}"',
            f'kind = "{k}"',
            f"ana_name = {toml_str(g['name'] or '')}",
            "",
        ]
    OUT.write_text("\n".join(out))
    by = {}
    for g in gauges:
        key = (g["region"], kind(g["name"] or ""))
        by[key] = by.get(key, 0) + 1
    print(f"{len(gauges)} réguas + {len(list(RULES.glob('*.json')))} pontos GloFAS → {OUT.relative_to(ROOT)}")
    for reg in ("guaiba", "uruguai", "litoranea"):
        print(f"  {reg:<10} " + ", ".join(f"{k} {by.get((reg, k), 0)}" for k in ("rio", "jusante", "reservatorio")))
    print(f"  nomes repetidos (mesmo rio e município; o código distingue): {sum(n for n in names.values() if n > 1)}")


if __name__ == "__main__":
    main()
