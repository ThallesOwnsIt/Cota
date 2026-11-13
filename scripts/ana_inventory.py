#!/usr/bin/env python3
"""ANA's telemetric river gauges in a state, and which of them are transmitting (Etapa 5: Rio Grande do Sul).

    set -a; . ./.env; set +a          # ANA_IDENTIFICADOR, ANA_SENHA (HidroWebService login)
    python3 scripts/ana_inventory.py --uf RS --out data/inventory/ana-rs.json
    python3 scripts/ana_inventory.py --out data/inventory/ana-rs.json --retag    # regions only, no ANA

1. The inventory (HidroInventarioEstacoes) of every station in the state.
2. The telemetric river gauges among them, each tagged with its region (see `region`).
3. Whether each one transmitted: its level readings over the 2 days up to yesterday (HidroinfoanaSerieTelemetricaAdotada,
   10 stations per request).

Read-only, standard library only. ANA's service often answers 502/503/504: every request is retried with a growing wait.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from collections import Counter
from datetime import date, timedelta
from pathlib import Path

API = "https://www.ana.gov.br/hidrowebservice/EstacoesTelemetricas"
UA = {"User-Agent": "cota-inventory"}


def get(url: str, headers: dict, attempts: int = 6) -> dict:
    for k in range(attempts):
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers={**UA, **headers}), timeout=180) as r:
                return json.load(r)
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as e:
            if k == attempts - 1:
                raise
            wait = 10 * (k + 1)
            print(f"aviso: {e} (tentativa {k + 1} de {attempts}); de novo em {wait} s", file=sys.stderr)
            time.sleep(wait)
    raise AssertionError


BASIN = Path(__file__).resolve().parent.parent / "data/inventory/guaiba-basin.json"
# The Guaíba itself and what drains straight into it below the Cais Mauá (outside the basin outline, which ends there).
GUAIBA_BELOW_OUTLET = {"LAGO GUAÍBA", "RIO GUAÍBA", "ARROIO DO RIBEIRO"}


def inside(ring: list[list[float]], lon: float, lat: float) -> bool:
    """Point in polygon (even-odd rule)."""
    hit = False
    for (x1, y1), (x2, y2) in zip(ring, ring[1:]):
        if (y1 > lat) != (y2 > lat) and lon < x1 + (lat - y1) * (x2 - x1) / (y2 - y1):
            hit = not hit
    return hit


def region(g: dict, ring: list[list[float]]) -> str:
    """Where a gauge's water goes, which is what its flood depends on.

    - uruguai: the Uruguay basin (ANA codes 7x).
    - guaiba: drains to the Cais Mauá (inside the HydroBASINS outline the site's map shows: Jacuí, Taquari-Antas, Caí,
      Sinos, Gravataí), or is on the Guaíba below it.
    - litoranea: the rest, draining to the coastal lagoons (Patos, Mirim, the north coast) or the sea. ANA's code does not
      separate this from the Guaíba: sub-basin 87 holds both.
    """
    if g["code"].startswith("7"):
        return "uruguai"
    if inside(ring, g["lon"], g["lat"]) or (g["river"] or "").upper() in GUAIBA_BELOW_OUTLET:
        return "guaiba"
    return "litoranea"


def summary(gauges: list[dict]) -> None:
    by = Counter((g["region"], g["transmitting"]) for g in gauges)
    for reg in ("guaiba", "uruguai", "litoranea"):
        print(f"{reg:<10} {by[(reg, True)] + by[(reg, False)]:>4} réguas, {by[(reg, True)]:>4} transmitindo")


def field(st: dict, *names: str):
    for n in names:
        if n in st and st[n] not in (None, ""):
            return st[n]
    return None


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--uf", default="RS")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--day", type=date.fromisoformat, default=date.today() - timedelta(days=1), help="último dia da conferência")
    ap.add_argument("--retag", action="store_true", help="só refaz as regiões do inventário salvo em --out, sem consultar a ANA")
    args = ap.parse_args()
    ring = json.loads(BASIN.read_text())["coordinates"][0]

    if args.retag:
        inv = json.loads(args.out.read_text())
        for g in inv["gauges"]:
            g["region"] = region(g, ring)
        args.out.write_text(json.dumps(inv, ensure_ascii=False, indent=1) + "\n")
        summary(inv["gauges"])
        print(f"→ {args.out}")
        return

    auth = get(f"{API}/OAUth/v1", {"Identificador": os.environ["ANA_IDENTIFICADOR"], "Senha": os.environ["ANA_SENHA"]})
    token = auth["items"]["tokenautenticacao"]
    bearer = {"Authorization": f"Bearer {token}"}
    inv = get(f"{API}/HidroInventarioEstacoes/v1?" + urllib.parse.urlencode({"Unidade Federativa": args.uf}), bearer)["items"] or []
    print(f"{len(inv)} estações no inventário de {args.uf}", file=sys.stderr)

    def is_river_telemetric(st: dict) -> bool:
        kind = str(field(st, "Tipo_Estacao", "Tipo Estacao", "tipo_estacao") or "").lower()
        tele = str(field(st, "Tipo_Estacao_Telemetrica", "Telemetrica", "Tipo Estacao Telemetrica") or "").strip().lower()
        operating = str(field(st, "Operando", "operando") or "1").strip().lower()
        return kind.startswith("flu") and tele in ("1", "sim", "true", "s") and operating in ("1", "sim", "true", "s")

    gauges = []
    for st in inv:
        if not is_river_telemetric(st):
            continue
        code = str(field(st, "codigoestacao", "Codigo_Estacao", "codigo_estacao"))
        gauges.append(
            {
                "code": code,
                "region": "",
                "name": field(st, "Estacao_Nome", "Nome_Estacao", "estacao_nome"),
                "municipality": field(st, "Municipio_Nome", "municipio_nome"),
                "river": field(st, "Rio_Nome", "rio_nome"),
                "basin": field(st, "Bacia_Nome", "Sub_Bacia_Nome", "bacia_nome"),
                "operator": field(st, "Operadora_Sigla", "Responsavel_Sigla", "operadora_sigla"),
                "lat": float(field(st, "Latitude", "latitude") or 0),
                "lon": float(field(st, "Longitude", "longitude") or 0),
            }
        )
        gauges[-1]["region"] = region(gauges[-1], ring)
    print(f"{len(gauges)} réguas telemétricas de rio em operação", file=sys.stderr)

    # Transmission: level readings in the 2 days up to `day`.
    readings: Counter[str] = Counter()
    codes = [g["code"] for g in gauges]
    for i in range(0, len(codes), 10):
        chunk = ",".join(codes[i : i + 10])
        url = (
            f"{API}/HidroinfoanaSerieTelemetricaAdotada/v2?Codigos_Estacoes={chunk}"
            f"&Tipo%20Filtro%20Data=DATA_LEITURA&Data%20de%20Busca%20%28yyyy-MM-dd%29={args.day}&Range%20Intervalo%20de%20busca=DIAS_2"
        )
        for row in get(url, bearer).get("items") or []:
            if row.get("Cota_Adotada") not in (None, ""):
                readings[str(row["codigoestacao"])] += 1
        print(f"  {min(i + 10, len(codes))}/{len(codes)} conferidas", file=sys.stderr)
    for g in gauges:
        g["readings_2d"] = readings.get(g["code"], 0)
        g["transmitting"] = g["readings_2d"] > 0

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(
        json.dumps({"uf": args.uf, "checked_until": str(args.day), "stations_in_inventory": len(inv), "gauges": gauges}, ensure_ascii=False, indent=1) + "\n"
    )
    summary(gauges)
    print(f"→ {args.out}")


if __name__ == "__main__":
    main()
