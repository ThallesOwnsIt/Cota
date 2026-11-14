"""Early-warning rules for the rest of Rio Grande do Sul (Etapa 5.3): the Uruguai and Litoral regions.

    uv run python -m cota_ml.rs_rules [--only A B …] [--end YYYY-MM-DD] [--write-rules A B …]

The method of the Guaíba's sub-basins (cota_ml.subbasins), unchanged, one basin at a time:

1. Flood of a basin: GloFAS discharge at its point above that point's own 99th percentile (2015–2023) within 7 days.
2. Calibration (2015–2023): the rain over the next 5 days, averaged over the basin's points, that maximizes CSI, with
   the ERA5 reanalysis (Open-Meteo archive).
3. Out-of-sample test (Feb 2024 → `--end`): the same threshold on the rain the models actually forecast then.

The points are ANA gauges inside each basin (data/inventory/ana-rs.json: being on its rivers, they are inside it),
spread out at least ~25 km apart (the reanalysis grid), plus towns of Santa Catarina and Misiones for the upper
Uruguay, which RS's inventory does not cover. Where a basin had too few gauges, a point in its headwaters completes it.
The GloFAS point of each basin is the river cell next to its main gauge (the largest mean discharge within 0.1°).

Writes `ml/reports/rs-rules.json`. With `--write-rules`, after review, the rule files go to `ml/models/outlook-rules-rs/`,
in the format and hash of the chain's: the round does not read that folder, so they enter the ledger only with the code
of Etapa 5.4, signed by the committee.
"""

from __future__ import annotations

import argparse
import json
from datetime import date, timedelta
from pathlib import Path

import numpy as np

from .subbasins import ROOT, backtest, calibrate, era5_daily, glofas_daily, write_rule

RULES_DIR = ROOT / "ml" / "models" / "outlook-rules-rs"
REPORT = ROOT / "ml" / "reports" / "rs-rules.json"

BASINS = {
    "uruguai": {
        "name": "Rio Uruguai (calha, em Uruguaiana)",
        "region": "uruguai",
        "gauge": "77150000 Uruguaiana",
        "target": ("GLOFAS-URUGUAIANA", -29.775, -57.175),
        "points": [
            ("ana-75020200", -28.301, -53.503),  # Panambi
            ("ana-70120000", -28.446, -49.898),  # São José dos Ausentes
            ("ana-75900000", -29.117, -56.554),  # Itaqui
            ("ana-76220900", -31.142, -54.381),  # Dom Pedrito
            ("sc-joacaba", -27.178, -51.505),  # Joaçaba (SC)
            ("ar-obera", -27.487, -55.12),  # Oberá (Misiones, AR)
            ("ana-76440010", -29.499, -54.689),  # Jaguari
            ("sc-sao-miguel-do-oeste", -26.725, -53.518),  # São Miguel do Oeste (SC)
            ("ana-76700000", -29.973, -55.748),  # Alegrete
            ("ana-73371000", -28.248, -52.371),  # Passo Fundo
            ("ana-75650900", -28.683, -55.579),  # São Borja
            ("sc-chapeco", -27.1, -52.615),  # Chapecó (SC)
            ("ana-70719700", -28.321, -50.865),  # Vacaria
            ("ana-74505400", -27.405, -54.168),  # Crissiumal
        ],
    },
    "alto-uruguai": {
        "name": "Alto Uruguai (até Iraí)",
        "region": "uruguai",
        "gauge": "74100000 Iraí",
        "target": ("GLOFAS-IRAI", -27.125, -53.325),
        "points": [
            ("ana-72450500", -27.712, -51.752),  # Paim Filho
            ("ana-70120000", -28.446, -49.898),  # São José dos Ausentes
            ("ana-74100000", -27.176, -53.23),  # Iraí
            ("sc-cacador", -26.775, -51.015),  # Caçador (SC)
            ("ana-70719700", -28.321, -50.865),  # Vacaria
            ("ana-73390000", -27.945, -52.581),  # Pontão
            ("sc-lages", -27.816, -50.326),  # Lages (SC)
            ("ana-73200500", -27.314, -52.396),  # Barra Do Rio Azul
            ("sc-campos-novos", -27.401, -51.225),  # Campos Novos (SC)
            ("ana-70842000", -27.963, -51.337),  # Esmeralda
        ],
    },
    "ijui": {
        "name": "Ijuí",
        "region": "uruguai",
        "gauge": "75230000 Entre-Ijuís",
        "target": ("GLOFAS-IJUI", -28.325, -54.375),
        "points": [
            ("ana-75185200", -28.371, -53.879),  # Ijuí
            ("ana-75060000", -28.397, -53.458),  # Panambi
            ("ana-75230000", -28.355, -54.267),  # Entre-Ijuis
        ],
    },
    "ibicui": {
        "name": "Ibicuí (até Manoel Viana)",
        "region": "uruguai",
        "gauge": "76560000 Manoel Viana",
        "target": ("GLOFAS-MANOEL-VIANA", -29.625, -55.575),
        "points": [
            ("ana-76118000", -29.564, -54.453),  # Mata
            ("ana-76248000", -31.148, -54.759),  # Dom Pedrito
            ("ana-76560003", -29.596, -55.482),  # Manoel Viana
            ("ana-76300000", -30.281, -54.903),  # Rosário Do Sul
            ("ana-76069500", -29.266, -53.948),  # Quevedos
            ("ana-76111010", -29.837, -54.798),  # Cacequi
            ("ana-76220900", -31.142, -54.381),  # Dom Pedrito
            ("ana-76395010", -29.665, -54.106),  # Santa Maria
        ],
    },
    "santa-maria": {
        "name": "Santa Maria (até Rosário do Sul)",
        "region": "uruguai",
        "gauge": "76310000 Rosário do Sul",
        "target": ("GLOFAS-ROSARIO-DO-SUL", -30.125, -54.975),
        "points": [
            ("ana-76251000", -30.97, -54.693),  # Dom Pedrito
            ("ana-76310000", -30.243, -54.917),  # Rosário Do Sul
            ("ana-76220900", -31.142, -54.381),  # Dom Pedrito
            ("ana-76290000", -30.518, -55.043),  # Rosário Do Sul
        ],
    },
    "ibirapuita": {
        "name": "Ibirapuitã (até Alegrete)",
        "region": "uruguai",
        "gauge": "76750000 Alegrete",
        "target": ("GLOFAS-ALEGRETE", -29.675, -55.875),
        "points": [
            ("ana-76700000", -29.973, -55.748),  # Alegrete
            ("ibirapuita-nascentes", -30.45, -55.55),  # Alto Ibirapuitã
            ("ana-76750000", -29.768, -55.788),  # Alegrete
        ],
    },
    "quarai": {
        "name": "Quaraí",
        "region": "uruguai",
        "gauge": "77500000 Quaraí",
        "target": ("GLOFAS-QUARAI", -30.325, -56.575),
        "points": [
            ("ana-77499010", -30.392, -56.456),  # Quaraí
            ("quarai-alto", -30.65, -56.05),  # Alto Quaraí
        ],
    },
    "camaqua": {
        "name": "Camaquã",
        "region": "litoranea",
        "gauge": "87905000 Cristal",
        "target": ("GLOFAS-CAMAQUA", -31.075, -51.925),
        "points": [
            ("ana-87795000", -30.796, -52.624),  # Encruzilhada Do Sul
            ("ana-87580001", -30.816, -53.9),  # Lavras Do Sul
            ("ana-87905000", -31.012, -52.053),  # Cristal
            ("ana-87660010", -30.974, -53.046),  # Santana Da Boa Vista
            ("ana-87864950", -30.696, -52.06),  # Dom Feliciano
        ],
    },
    "piratini": {
        "name": "Piratini",
        "region": "litoranea",
        "gauge": "88640951 Pedro Osório",
        "target": ("GLOFAS-PIRATINI", -31.925, -52.725),
        "points": [
            ("ana-88579700", -31.875, -53.028),  # Piratini
            ("piratini-alto", -31.55, -53.25),  # Alto Piratini
            ("ana-88640950", -31.86, -52.817),  # Cerrito
        ],
    },
    "jaguarao": {
        "name": "Jaguarão",
        "region": "litoranea",
        "gauge": "88300040 Jaguarão",
        "target": ("GLOFAS-JAGUARAO", -32.625, -53.275),
        "points": [
            ("ana-88260000", -32.519, -53.456),  # Jaguarão
            ("jaguarao-alto", -31.85, -53.95),  # Alto Jaguarão
        ],
    },
    "mampituba": {
        "name": "Mampituba",
        "region": "litoranea",
        "gauge": "84991510 Torres",
        "target": ("GLOFAS-MAMPITUBA", -29.325, -49.775),
        "points": [
            ("ana-84991100", -29.367, -49.938),  # Morrinhos Do Sul
            ("ana-84991510", -29.299, -49.753),  # Torres
        ],
    },
    "maquine": {
        "name": "Maquiné (litoral norte)",
        "region": "litoranea",
        "gauge": "87317030 Maquiné",
        # The river's own cell (≈20 m³/s). The largest-discharge cell nearby (-29.725, -50.125, ≈62 m³/s) is where the
        # Maquiné meets the lagoon chain from Três Forquilhas: the first run used it, and Três Forquilhas, outside this basin.
        "target": ("GLOFAS-MAQUINE", -29.675, -50.225),
        "points": [
            ("ana-87317021", -29.581, -50.278),  # Maquiné (alto)
            ("ana-87317030", -29.65, -50.21),  # Maquiné
        ],
    },
}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--end", type=date.fromisoformat, default=date.today() - timedelta(days=2))
    ap.add_argument("--only", nargs="*", help="basins to run (default: all)")
    ap.add_argument("--write-rules", nargs="*", default=[], help="basins whose rule files are written (after review)")
    args = ap.parse_args()
    report = json.loads(REPORT.read_text()) if REPORT.exists() else {}
    report["method"] = __doc__.split("\n\n", 2)[2].split("\n\nWrites")[0]
    report.setdefault("basins", {})
    for sid, sb in BASINS.items():
        if args.only and sid not in args.only:
            continue
        tid, tlat, tlon = sb["target"]
        q = glofas_daily(tid, tlat, tlon, args.end)
        thr_q = float(np.quantile([v for d, v in q.items() if d <= "2023-12-31"], 0.99))
        per_point = [era5_daily(*p) for p in sb["points"]]
        rain = {d: float(np.mean([pp[d] for pp in per_point])) for d in per_point[0] if all(d in pp for pp in per_point)}
        cal = calibrate(rain, q, thr_q)
        bt = backtest(sb["points"], cal["threshold_mm"], q, thr_q, args.end)
        report["basins"][sid] = {"name": sb["name"], "region": sb["region"], "gauge": sb["gauge"], "target": tid,
                                 "target_cell": [tlat, tlon], "flood_m3s_p99": round(thr_q, 1), "points": [p[0] for p in sb["points"]],
                                 "calibration": cal, "backtest": bt}
        if sid in args.write_rules:
            write_rule(sid, sb, cal, thr_q, bt, RULES_DIR)
        ct = bt["contingency"]
        print(f"{sb['name']:<36} P99 {thr_q:>7.0f} m³/s | regra {cal['threshold_mm']:>3} mm em 5 d | calibração CSI {cal['csi']:.2f} "
              f"| teste: {ct['hits']} acertos, {ct['false_alarms']} alarmes falsos, {ct['misses']} perdidos (CSI {ct['csi']:.2f}) "
              f"| cheias {len(bt['floods'])}, avisadas {sum(1 for f in bt['floods'] if f['first_alert'])}", flush=True)
        REPORT.write_text(json.dumps(report, ensure_ascii=False, indent=1) + "\n")
    print(f"→ {REPORT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
