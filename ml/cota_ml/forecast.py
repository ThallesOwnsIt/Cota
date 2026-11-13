"""Forecast from the anchored batches.

    uv run python -m cota_ml.forecast --day 2024-05-02

Reads only the ledger (`data/ledger/batches/*.json`), checks each batch's Merkle root and
chaining, and requires the latest batch to be the issue day's. The output
(`data/forecasts/{day}.json`) is what `cota-admin commit-forecast` records on-chain.
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


def checked_entries(ledger: Path, until: date) -> list[dict]:
    """Batches of one ledger up to `until`, each checked against its root and the chain before it."""
    entries = [json.loads(p.read_text()) for p in sorted((ledger / "batches").glob("*.json"))]
    prev = bytes(32).hex()
    out = []
    for e in entries:
        h = e["header"]
        if h["prev_batch_root"] != prev:
            raise SystemExit(f"{ledger.name} lote {e['batch_id']}: encadeamento quebrado")
        if merkle_root([reading_bytes(r) for r in e["readings"]]).hex() != h["merkle_root"]:
            raise SystemExit(f"{ledger.name} lote {e['batch_id']}: leituras não batem com a raiz ancorada")
        prev = h["merkle_root"]
        if date.fromisoformat(e["day"]) <= until:
            out.append(e)
    return out


def v3_entries(ledger3: Path, until: date, stream: str = "guaiba") -> list[dict]:
    """Batches of the v3 ledger's stream (`batches/<stream>-<id>.json`) up to `until`, each checked against the root
    the committee signed. The entries themselves (order, signatures) are checked by the cota-ledger rules."""
    out = []
    for p in sorted((ledger3 / "batches").glob(f"{stream}-*.json")):
        e = json.loads(p.read_text())
        if merkle_root([reading_bytes(r) for r in e["readings"]]).hex() != e["merkle_root"]:
            raise SystemExit(f"livro v3, lote {e['batch_id']} de {stream}: leituras não batem com a raiz assinada")
        if date.fromisoformat(e["day"]) <= until:
            out.append({"batch_id": e["batch_id"], "day": e["day"], "header": {"merkle_root": e["merkle_root"]}, "readings": e["readings"]})
    return out


def load_ledger(ledger: Path, until: date, archive: Path | None = None, ledger3: Path | None = None) -> tuple[list[dict], dict]:
    """Readings from every batch up to `until`, checked against the anchored roots.

    The forecast is based on the latest batch of the ledger it is recorded in, which must be `until`: the v3 ledger
    when `ledger3` is given, else `ledger` (v2). Days that ledger does not have come from the older ones (v2, then
    program v1's frozen archive): the lags of the first days come from the days before them.
    """
    if ledger3 is not None:
        entries = v3_entries(ledger3, until)
        if not entries or date.fromisoformat(entries[-1]["day"]) != until:
            raise SystemExit(f"o último lote do livro v3 não é de {until}")
        have = {e["day"] for e in entries}
        older2 = [e for e in checked_entries(ledger, until) if e["day"] not in have] if ledger.exists() else []
        have |= {e["day"] for e in older2}
        older1 = [e for e in checked_entries(archive, until) if e["day"] not in have] if archive and archive.exists() else []
        readings = [r for e in older1 + older2 + entries for r in e["readings"]]
        return readings, entries[-1]
    entries = checked_entries(ledger, until)
    if not entries or date.fromisoformat(entries[-1]["day"]) != until:
        raise SystemExit(f"o último lote ancorado não é de {until}")
    have = {e["day"] for e in entries}
    older = [e for e in checked_entries(archive, until) if e["day"] not in have] if archive and archive.exists() else []
    readings = [r for e in older + entries for r in e["readings"]]
    return readings, entries[-1]


def forecast(readings: list[dict], last: dict, day: date, models, widen, manifest) -> dict | None:
    # Types inferred from every row: older readings have no `datum_id` (rain, discharge), the river gauges do,
    # and the first ones only show up after hundreds of rows.
    table = feature_table(daily_table(pl.DataFrame(readings, infer_schema_length=None))).filter(pl.col("day") == day)
    if table.is_empty() or table["lq0"][0] is None:
        return None  # no Guaíba discharge on that day: there is no honest forecast to make

    # q0 (the day's anchored reading) goes along: it is the basis for converting to m³/s.
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
    ap.add_argument("--archive", type=Path, default=ROOT / "data/ledger-v1", help="ledger congelado da v1 (histórico)")
    ap.add_argument("--ledger-v3", type=Path, default=None, help="livro v3: a previsão parte do último lote dele")
    ap.add_argument("--models", type=Path, default=ROOT / "ml/models")
    ap.add_argument("--out", type=Path, default=ROOT / "data/forecasts")
    args = ap.parse_args()

    models, widen, manifest = model.load(args.models)
    readings, last = load_ledger(args.ledger, args.day, args.archive, args.ledger_v3)
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
