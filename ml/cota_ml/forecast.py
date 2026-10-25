"""Previsão a partir dos lotes ancorados.

    uv run python -m cota_ml.forecast --day 2024-05-02

Lê só o ledger (`data/ledger/batches/*.json`), confere a raiz Merkle e o encadeamento
de cada lote e exige que o último lote seja o do dia de emissão. A saída
(`data/forecasts/{dia}.json`) é o que `cota-admin commit-forecast` grava on-chain.
"""

from __future__ import annotations

import argparse
import json
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

import polars as pl

from . import model
from .canonical import feature_leaf, merkle_root, reading_bytes
from .features import FEATURES, HORIZONS, QUANTILES, TARGET, daily_table, feature_table

ROOT = Path(__file__).resolve().parents[2]


def start_of_day(d: date) -> int:
    return int(datetime(d.year, d.month, d.day, tzinfo=timezone.utc).timestamp())


def load_ledger(ledger: Path, until: date) -> tuple[list[dict], dict]:
    """Leituras de todos os lotes até `until`, conferidas contra as raízes ancoradas."""
    entries = [json.loads(p.read_text()) for p in sorted((ledger / "batches").glob("*.json"))]
    entries = [e for e in entries if date.fromisoformat(e["day"]) <= until]
    if not entries or date.fromisoformat(entries[-1]["day"]) != until:
        raise SystemExit(f"o último lote ancorado não é de {until}")
    prev = bytes(32).hex()
    readings = []
    for e in entries:
        h = e["header"]
        if h["prev_batch_root"] != prev:
            raise SystemExit(f"lote {e['batch_id']}: encadeamento quebrado")
        if merkle_root([reading_bytes(r) for r in e["readings"]]).hex() != h["merkle_root"]:
            raise SystemExit(f"lote {e['batch_id']}: leituras não batem com a raiz ancorada")
        prev = h["merkle_root"]
        readings.extend(e["readings"])
    return readings, entries[-1]


def forecast(readings: list[dict], last: dict, day: date, models, widen, manifest) -> dict | None:
    table = feature_table(daily_table(pl.DataFrame(readings))).filter(pl.col("day") == day)
    if table.is_empty() or table["lq0"][0] is None:
        return None  # sem vazão do Guaíba no dia: não há previsão honesta a fazer

    # q0 (a leitura ancorada do dia) entra junto: é a base da conversão para m³/s.
    features = [{"name": "q0", "value_milli": round(table["q0"][0] * 1000)}]
    features += [{"name": f, "value_milli": table[f"{f}_milli"][0]} for f in FEATURES]
    out = []
    for h in HORIZONS:
        q = model.predict(models, table, h, widen.get(h, 0.0))
        out.append(
            {
                "horizon_hours": 24 * h,
                "target_ts": start_of_day(day + timedelta(days=h)),
                "target_day": (day + timedelta(days=h)).isoformat(),
                **{f"p{round(t * 100)}": round(float(q[t][0]) * 1000) for t in QUANTILES},
            }
        )
    return {
        "station_id": TARGET,
        "issue_day": day.isoformat(),
        "issued_at": start_of_day(day + timedelta(days=1)),
        "based_on_batch_id": last["batch_id"],
        "based_on_root": last["header"]["merkle_root"],
        "model_sha256": manifest["model_sha256"],
        "features": features,
        "features_root": merkle_root([feature_leaf(f["name"], f["value_milli"]) for f in features]).hex(),
        "forecasts": out,
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--day", type=date.fromisoformat, required=True, help="dia de emissão (último lote ancorado)")
    ap.add_argument("--ledger", type=Path, default=ROOT / "data/ledger")
    ap.add_argument("--models", type=Path, default=ROOT / "ml/models")
    ap.add_argument("--out", type=Path, default=ROOT / "data/forecasts")
    args = ap.parse_args()

    models, widen, manifest = model.load(args.models)
    readings, last = load_ledger(args.ledger, args.day)
    result = forecast(readings, last, args.day, models, widen, manifest)
    if result is None:
        print(f"{args.day}: sem dados suficientes para prever")
        return
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / f"{args.day}.json").write_text(json.dumps(result, indent=2) + "\n")
    for f in result["forecasts"]:
        print(f"{args.day} → {f['target_day']}: P10 {f['p10'] / 1000:.0f}  P50 {f['p50'] / 1000:.0f}  P90 {f['p90'] / 1000:.0f} m³/s")


if __name__ == "__main__":
    main()
