"""Camada C: alerta antecipado pela chuva prevista na bacia do Guaíba.

    uv run python -m cota_ml.outlook

Regra: se a média, entre modelos, da chuva prevista para os próximos 5 dias nos 6 pontos
da bacia passa de X mm, o Guaíba (GloFAS) pode cruzar o limiar P99 em até 7 dias.

1. Calibração (só 2015–2023): X que maximiza o CSI usando a chuva **observada** do INMET.
2. Backtest (fev/2024 → ontem): a mesma regra aplicada à chuva que os modelos **previam**
   (Open-Meteo Previous Runs). Para o dia alvo D+k usa-se a previsão feita (k+1)×24 h antes
   da hora válida: todas as rodadas usadas começaram no dia D−1 e já estavam publicadas no
   fim do dia D. Ao vivo o nó usa a rodada 00Z do dia D, que é mais nova: o backtest é
   conservador.
3. Exporta `ml/models/outlook-rule.json` (regra + `rule_sha256`, que vai on-chain) e
   `ml/reports/outlook-backtest.json` (série diária e tabela de contingência).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import time
import tomllib
import urllib.request
from datetime import date, timedelta
from pathlib import Path

import numpy as np
import polars as pl

from .features import RAIN_STATIONS, TARGET, daily_table
from .train import load_readings

ROOT = Path(__file__).resolve().parents[2]
CACHE = ROOT / "data" / "raw" / "outlook"
REGION = "guaiba"
MODELS = ["ecmwf_ifs025", "gfs_global", "icon_global"]
WINDOW_DAYS = 5
FLOOD_WITHIN_DAYS = 7
MIN_MODELS = 2
RUN_HOUR_UTC = 0
CALIBRATION_YEARS = (2015, 2023)
ARCHIVE_START = date(2024, 2, 15)
PREVIOUS_RUNS = "https://previous-runs-api.open-meteo.com/v1/forecast"
FLOOD = "https://flood-api.open-meteo.com/v1/flood"


def points() -> list[dict]:
    cfg = tomllib.loads((ROOT / "config" / "stations.toml").read_text())
    by_id = {s["id"]: s for s in cfg["station"]}
    return [{"id": s, "lat": by_id[s]["lat"], "lon": by_id[s]["lon"]} for s in RAIN_STATIONS]


def flood_threshold() -> float:
    m = json.loads((ROOT / "ml" / "models" / "manifest.json").read_text())
    return m["alert_rule"]["threshold_milli"] / 1000


def canonical(obj) -> bytes:
    return json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def _get(url: str, path: Path) -> dict:
    if not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        for attempt in range(5):
            try:
                with urllib.request.urlopen(url, timeout=120) as r:
                    path.write_bytes(r.read())
                break
            except Exception:
                if attempt == 4:
                    raise
                time.sleep(10 * (attempt + 1))
        time.sleep(1)
    return json.loads(path.read_text())


def calibrate(hist: pl.DataFrame, threshold: float) -> dict:
    """Limiar de chuva (mm em WINDOW_DAYS) que maximiza o CSI de 'Q ≥ limiar em até 7 dias'."""
    rain = hist.select(pl.mean_horizontal([f"rain_{s}" for s in RAIN_STATIONS])).to_series().to_numpy()
    q = hist[TARGET].to_numpy()
    xs, ys = [], []
    for d in range(len(q) - FLOOD_WITHIN_DAYS - 1):
        future_rain = rain[d + 1 : d + 1 + WINDOW_DAYS]
        future_q = q[d + 1 : d + 1 + FLOOD_WITHIN_DAYS]
        if np.isnan(q[d]) or q[d] >= threshold or np.isnan(future_rain).any() or np.isnan(future_q).all():
            continue
        xs.append(future_rain.sum())
        ys.append(np.nanmax(future_q) >= threshold)
    x, y = np.array(xs), np.array(ys)
    best = None
    for mm in range(20, 300, 5):
        a = int(((x >= mm) & y).sum())
        b = int(((x >= mm) & ~y).sum())
        c = int(((x < mm) & y).sum())
        csi = a / (a + b + c)
        if best is None or csi > best["csi"]:
            best = {"threshold_mm": mm, "csi": round(csi, 3), "hits": a, "false_alarms": b, "misses": c}
    return {**best, "days": len(x), "flood_days": int(y.sum())}


def rule_spec(cal: dict, threshold_q: float) -> dict:
    return {
        "region": REGION,
        "points": points(),
        "models": MODELS,
        "min_models": MIN_MODELS,
        "run_hour_utc": RUN_HOUR_UTC,
        "window_days": WINDOW_DAYS,
        "aggregation": "por modelo: soma horária de D+1..D+5 (UTC) em cada ponto, arredondada a 0,1 mm por dia; média dos pontos; depois média dos modelos",
        "threshold_mm": cal["threshold_mm"],
        "target": {"station_id": TARGET, "threshold_m3s": threshold_q, "within_days": FLOOD_WITHIN_DAYS},
        "calibration": {"years": list(CALIBRATION_YEARS), "data": "chuva observada INMET + vazão GloFAS", **cal},
    }


def archived_forecasts(end: date) -> dict:
    """{(modelo, ponto, dia alvo, k): mm} com a previsão feita (k+1)×24 h antes de cada hora."""
    out = {}
    hourly = ",".join(f"precipitation_previous_day{k}" for k in range(2, WINDOW_DAYS + 2))
    for p in points():
        for m in MODELS:
            url = (
                f"{PREVIOUS_RUNS}?latitude={p['lat']}&longitude={p['lon']}&start_date={ARCHIVE_START}"
                f"&end_date={end}&hourly={hourly}&models={m}&timezone=UTC"
            )
            h = _get(url, CACHE / f"prev_{p['id']}_{m}_{end}.json")["hourly"]
            for k in range(1, WINDOW_DAYS + 1):
                acc: dict[str, list[float]] = {}
                for t, v in zip(h["time"], h[f"precipitation_previous_day{k + 1}"]):
                    acc.setdefault(t[:10], []).append(v)
                for day, vals in acc.items():
                    if len(vals) == 24 and None not in vals:
                        out[(m, p["id"], day, k)] = round(sum(vals), 1)
    return out


def glofas(end: date) -> dict[str, float]:
    cfg = tomllib.loads((ROOT / "config" / "stations.toml").read_text())
    st = next(s for s in cfg["station"] if s["id"] == TARGET)
    url = f"{FLOOD}?latitude={st['lat']}&longitude={st['lon']}&daily=river_discharge&start_date=2024-01-01&end_date={end}"
    d = _get(url, CACHE / f"glofas_{end}.json")["daily"]
    return {t: v for t, v in zip(d["time"], d["river_discharge"]) if v is not None}


def backtest(spec: dict, end: date) -> dict:
    fc = archived_forecasts(end)
    q = glofas(end)
    thr_q = spec["target"]["threshold_m3s"]
    ids = [p["id"] for p in spec["points"]]
    rows = []
    day = ARCHIVE_START
    while day + timedelta(days=WINDOW_DAYS) <= end:
        per_model = {}
        for m in MODELS:
            totals = []
            for st in ids:
                s = [fc.get((m, st, (day + timedelta(days=k)).isoformat(), k)) for k in range(1, WINDOW_DAYS + 1)]
                if None not in s:
                    totals.append(sum(s))
            if len(totals) == len(ids):
                per_model[m] = round(float(np.mean(totals)), 1)
        mean = round(float(np.mean(list(per_model.values()))), 1) if len(per_model) >= MIN_MODELS else None
        future = [q.get((day + timedelta(days=k)).isoformat()) for k in range(1, FLOOD_WITHIN_DAYS + 1)]
        future = [v for v in future if v is not None]
        rows.append({
            "day": day.isoformat(),
            "models": per_model,
            "rain_mm": mean,
            "alert": mean is not None and mean >= spec["threshold_mm"],
            "q": q.get(day.isoformat()),
            "flood_ahead": bool(future) and max(future) >= thr_q,
            "complete": len(future) == FLOOD_WITHIN_DAYS,
        })
        day += timedelta(days=1)

    # Mesma pontuação da calibração: dias já acima do limiar não contam.
    scored = [r for r in rows if r["rain_mm"] is not None and r["complete"] and (r["q"] or 0) < thr_q]
    a = sum(r["alert"] and r["flood_ahead"] for r in scored)
    b = sum(r["alert"] and not r["flood_ahead"] for r in scored)
    c = sum(not r["alert"] and r["flood_ahead"] for r in scored)
    floods = flood_episodes(q, thr_q)
    for f in floods:
        start = date.fromisoformat(f["first_day"])
        first_alert = next(
            (r["day"] for r in rows if r["alert"] and start - timedelta(days=FLOOD_WITHIN_DAYS) <= date.fromisoformat(r["day"]) < start),
            None,
        )
        f["first_alert"] = first_alert
        f["lead_days"] = (start - date.fromisoformat(first_alert)).days if first_alert else None
    return {
        "period": [ARCHIVE_START.isoformat(), end.isoformat()],
        "offset": "previsão feita (k+1)×24 h antes da hora válida para o dia D+k",
        "contingency": {"days": len(scored), "hits": a, "false_alarms": b, "misses": c, "csi": round(a / max(a + b + c, 1), 3)},
        "floods": floods,
        "false_alarm_runs": alert_runs([r for r in rows if r["alert"] and not r["flood_ahead"]]),
        "days": rows,
    }


def flood_episodes(q: dict[str, float], thr: float) -> list[dict]:
    eps, cur = [], None
    for d in sorted(q):
        if q[d] >= thr:
            if cur and (date.fromisoformat(d) - date.fromisoformat(cur["last_day"])).days == 1:
                cur["last_day"], cur["peak_m3s"] = d, max(cur["peak_m3s"], round(q[d]))
            else:
                cur = {"first_day": d, "last_day": d, "peak_m3s": round(q[d])}
                eps.append(cur)
    return eps


def alert_runs(rows: list[dict]) -> list[list[str]]:
    runs: list[list[str]] = []
    for r in rows:
        if runs and (date.fromisoformat(r["day"]) - date.fromisoformat(runs[-1][1])).days == 1:
            runs[-1][1] = r["day"]
        else:
            runs.append([r["day"], r["day"]])
    return runs


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--end", type=date.fromisoformat, default=date.today() - timedelta(days=2))
    args = ap.parse_args()

    readings = ROOT / "data" / "readings"
    hist = daily_table(load_readings(readings / "history-glofas", readings / "history-inmet"))
    hist = hist.filter(pl.col("day").dt.year().is_between(*CALIBRATION_YEARS))
    thr_q = flood_threshold()
    spec = rule_spec(calibrate(hist, thr_q), thr_q)
    rule_sha = hashlib.sha256(canonical(spec)).hexdigest()
    (ROOT / "ml" / "models" / "outlook-rule.json").write_text(
        json.dumps({"rule_sha256": rule_sha, "spec": spec}, indent=2, ensure_ascii=False) + "\n"
    )
    print(f"regra: {spec['threshold_mm']} mm em {WINDOW_DAYS} dias  rule_sha256 {rule_sha[:16]}…  calibração {spec['calibration']}")

    bt = backtest(spec, args.end)
    bt["rule_sha256"] = rule_sha
    (ROOT / "ml" / "reports" / "outlook-backtest.json").write_text(json.dumps(bt, ensure_ascii=False) + "\n")
    print("backtest", bt["period"], bt["contingency"])
    for f in bt["floods"]:
        print(f"  cheia {f['first_day']}–{f['last_day']} (pico {f['peak_m3s']} m³/s): primeiro alerta {f['first_alert']}, antecedência {f['lead_days']} d")
    print("  alarmes falsos (sequências):", bt["false_alarm_runs"])


if __name__ == "__main__":
    main()
