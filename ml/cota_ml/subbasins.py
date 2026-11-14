"""Early-warning rules per sub-basin of the Guaíba (Etapa 3.3): calibration and out-of-sample test.

    uv run python -m cota_ml.subbasins [--end YYYY-MM-DD]

The same method as the basin-wide rule (cota_ml.outlook), one sub-basin at a time:

1. Flood of a sub-basin: GloFAS discharge at its point above that point's own 99th percentile (2015–2023) within
   7 days.
2. Calibration (2015–2023): the rain over the next 5 days, averaged over points inside the sub-basin, that
   maximizes CSI. The rain is the ERA5 reanalysis (Open-Meteo archive), because INMET's stations do not cover the
   sub-basins; the basin-wide rule is recalibrated the same way as a check on the method (it used INMET).
3. Out-of-sample test (Feb 2024 → `--end`): the same threshold on the rain the models actually forecast then
   (Open-Meteo Previous Runs, as in cota_ml.outlook).

Writes `ml/reports/subbasin-rules.json`. With `--write-rules A B …`, after review, also the rule files for the chain,
`ml/models/outlook-rules/<region>.json`, in the same format and hash as `ml/models/outlook-rule.json`.
"""

from __future__ import annotations

import argparse
import json
from datetime import date, timedelta
from pathlib import Path

import numpy as np

import hashlib

from .outlook import ARCHIVE_START, CACHE, FLOOD, FLOOD_WITHIN_DAYS, MIN_MODELS, MODELS, PREVIOUS_RUNS, RUN_HOUR_UTC, WINDOW_DAYS, _get, alert_runs, canonical, flood_episodes, points

ROOT = Path(__file__).resolve().parents[2]
ARCHIVE = "https://archive-api.open-meteo.com/v1/archive"
CAL_START, CAL_END = date(2015, 1, 1), date(2023, 12, 31)

# Points inside each sub-basin (towns, approximate), and the GloFAS point whose discharge defines its flood.
SUBBASINS = {
    "taquari": {
        "name": "Taquari-Antas",
        "target": ("GLOFAS-TAQUARI", -29.725, -51.975),
        "points": [("bom-jesus", -28.668, -50.432), ("vacaria", -28.512, -50.934), ("nova-prata", -28.783, -51.610),
                   ("bento-goncalves", -29.171, -51.519), ("soledade", -28.830, -52.510), ("encantado", -29.236, -51.870),
                   ("lajeado", -29.467, -51.961)],
    },
    "cai": {
        "name": "Caí",
        "target": ("GLOFAS-CAI", -29.775, -51.425),
        "points": [("sao-francisco-de-paula", -29.448, -50.584), ("canela", -29.365, -50.812), ("nova-petropolis", -29.376, -51.114),
                   ("feliz", -29.452, -51.306), ("montenegro", -29.689, -51.461)],
    },
    "sinos": {
        "name": "Sinos",
        "target": ("GLOFAS-SINOS", -29.875, -51.275),
        "points": [("caraa", -29.788, -50.432), ("rolante", -29.650, -50.576), ("taquara", -29.650, -50.780),
                   ("gramado", -29.379, -50.874), ("novo-hamburgo", -29.678, -51.130)],
    },
    "gravatai": {
        "name": "Gravataí",
        "target": ("GLOFAS-GRAVATAI", -29.975, -51.175),
        "points": [("santo-antonio-da-patrulha", -29.818, -50.520), ("glorinha", -29.879, -50.790), ("gravatai", -29.944, -50.992),
                   ("cachoeirinha", -29.951, -51.094)],
    },
    "jacui": {
        "name": "Jacuí",
        "target": ("GLOFAS-JACUI", -29.925, -51.625),
        "points": [("espumoso", -28.725, -52.850), ("julio-de-castilhos", -29.227, -53.681), ("santa-maria", -29.687, -53.807),
                   ("sobradinho", -29.421, -53.026), ("cachoeira-do-sul", -30.030, -52.894), ("santa-cruz-do-sul", -29.718, -52.426),
                   ("rio-pardo", -29.990, -52.378)],
    },
    # Check on the method: the basin-wide rule's 6 points and target, with ERA5 instead of INMET (it gave 90 mm).
    "guaiba-era5": {
        "name": "Guaíba (bacia inteira, conferência do método)",
        "target": ("GLOFAS-GUAIBA", -30.075, -51.325),
        "points": [(p["id"], p["lat"], p["lon"]) for p in points()],
    },
}


def era5_daily(pid: str, lat: float, lon: float) -> dict[str, float]:
    url = f"{ARCHIVE}?latitude={lat}&longitude={lon}&start_date={CAL_START}&end_date={CAL_END}&daily=precipitation_sum&timezone=UTC"
    d = _get(url, CACHE / f"era5_{pid}_{CAL_START.year}_{CAL_END.year}.json")["daily"]
    return {t: v for t, v in zip(d["time"], d["precipitation_sum"]) if v is not None}


def glofas_daily(tid: str, lat: float, lon: float, end: date) -> dict[str, float]:
    url = f"{FLOOD}?latitude={lat}&longitude={lon}&daily=river_discharge&start_date={CAL_START}&end_date={end}"
    d = _get(url, CACHE / f"glofas_{tid}_{lat}_{lon}_{end}.json")["daily"]  # the cell is part of the key
    return {t: v for t, v in zip(d["time"], d["river_discharge"]) if v is not None}


def days_between(a: date, b: date) -> list[str]:
    return [(a + timedelta(days=i)).isoformat() for i in range((b - a).days + 1)]


def calibrate(rain: dict[str, float], q: dict[str, float], thr: float) -> dict:
    """Rain threshold over the next 5 days that maximizes the CSI of 'discharge ≥ thr within 7 days'."""
    days = days_between(CAL_START, CAL_END - timedelta(days=FLOOD_WITHIN_DAYS + 1))
    xs, ys = [], []
    for d in days:
        d0 = date.fromisoformat(d)
        fr = [rain.get((d0 + timedelta(days=k)).isoformat()) for k in range(1, WINDOW_DAYS + 1)]
        fq = [q.get((d0 + timedelta(days=k)).isoformat()) for k in range(1, FLOOD_WITHIN_DAYS + 1)]
        if q.get(d) is None or q[d] >= thr or None in fr or all(v is None for v in fq):
            continue
        xs.append(sum(fr))
        ys.append(max(v for v in fq if v is not None) >= thr)
    x, y = np.array(xs), np.array(ys)
    best = None
    for mm in range(20, 400, 5):
        a, b, c = int(((x >= mm) & y).sum()), int(((x >= mm) & ~y).sum()), int(((x < mm) & y).sum())
        csi = a / max(a + b + c, 1)
        if best is None or csi > best["csi"]:
            best = {"threshold_mm": mm, "csi": round(csi, 3), "hits": a, "false_alarms": b, "misses": c}
    return {**best, "days": len(x), "flood_days": int(y.sum())}


def forecasts(pts: list[tuple[str, float, float]], end: date) -> dict:
    """{(model, point, target day, k): mm}, the forecast made (k+1)×24 h before each hour (as cota_ml.outlook)."""
    out = {}
    hourly = ",".join(f"precipitation_previous_day{k}" for k in range(2, WINDOW_DAYS + 2))
    for pid, lat, lon in pts:
        for m in MODELS:
            url = f"{PREVIOUS_RUNS}?latitude={lat}&longitude={lon}&start_date={ARCHIVE_START}&end_date={end}&hourly={hourly}&models={m}&timezone=UTC"
            h = _get(url, CACHE / f"prev_{pid}_{m}_{end}.json")["hourly"]
            for k in range(1, WINDOW_DAYS + 1):
                acc: dict[str, list[float]] = {}
                for t, v in zip(h["time"], h[f"precipitation_previous_day{k + 1}"]):
                    acc.setdefault(t[:10], []).append(v)
                for day, vals in acc.items():
                    if len(vals) == 24 and None not in vals:
                        out[(m, pid, day, k)] = round(sum(vals), 1)
    return out


def backtest(pts, thr_mm: float, q: dict[str, float], thr_q: float, end: date) -> dict:
    fc = forecasts(pts, end)
    ids = [p[0] for p in pts]
    rows, day = [], ARCHIVE_START
    while day + timedelta(days=WINDOW_DAYS) <= end:
        per_model = {}
        for m in MODELS:
            totals = []
            for pid in ids:
                s = [fc.get((m, pid, (day + timedelta(days=k)).isoformat(), k)) for k in range(1, WINDOW_DAYS + 1)]
                if None not in s:
                    totals.append(sum(s))
            if len(totals) == len(ids):
                per_model[m] = round(float(np.mean(totals)), 1)
        mean = round(float(np.mean(list(per_model.values()))), 1) if len(per_model) >= MIN_MODELS else None
        future = [q.get((day + timedelta(days=k)).isoformat()) for k in range(1, FLOOD_WITHIN_DAYS + 1)]
        future = [v for v in future if v is not None]
        rows.append({"day": day.isoformat(), "rain_mm": mean, "alert": mean is not None and mean >= thr_mm, "q": q.get(day.isoformat()),
                     "flood_ahead": bool(future) and max(future) >= thr_q, "complete": len(future) == FLOOD_WITHIN_DAYS})
        day += timedelta(days=1)
    scored = [r for r in rows if r["rain_mm"] is not None and r["complete"] and (r["q"] or 0) < thr_q]
    a = sum(r["alert"] and r["flood_ahead"] for r in scored)
    b = sum(r["alert"] and not r["flood_ahead"] for r in scored)
    c = sum(not r["alert"] and r["flood_ahead"] for r in scored)
    floods = [f for f in flood_episodes(q, thr_q) if f["first_day"] >= ARCHIVE_START.isoformat()]
    for f in floods:
        start = date.fromisoformat(f["first_day"])
        first = next((r["day"] for r in rows if r["alert"] and start - timedelta(days=FLOOD_WITHIN_DAYS) <= date.fromisoformat(r["day"]) < start), None)
        f["first_alert"], f["lead_days"] = first, (start - date.fromisoformat(first)).days if first else None
    return {"period": [ARCHIVE_START.isoformat(), end.isoformat()],
            "contingency": {"days": len(scored), "hits": a, "false_alarms": b, "misses": c, "csi": round(a / max(a + b + c, 1), 3)},
            "floods": floods, "false_alarm_runs": alert_runs([r for r in rows if r["alert"] and not r["flood_ahead"]])}


def write_rule(sid: str, sb: dict, cal: dict, thr_q: float, bt: dict, out_dir: Path | None = None) -> None:
    """The rule file for the chain, in the format of cota_ml.outlook's (the hash covers the canonical spec)."""
    tid = sb["target"][0]
    spec = {
        "region": sid,
        "points": [{"id": pid, "lat": lat, "lon": lon} for pid, lat, lon in sb["points"]],
        "models": MODELS,
        "min_models": MIN_MODELS,
        "run_hour_utc": RUN_HOUR_UTC,
        "window_days": WINDOW_DAYS,
        "aggregation": "por modelo: soma horária de D+1..D+5 (UTC) em cada ponto, arredondada a 0,1 mm por dia; média dos pontos; depois média dos modelos",
        "threshold_mm": cal["threshold_mm"],
        "target": {"station_id": tid, "threshold_m3s": round(thr_q, 1), "within_days": FLOOD_WITHIN_DAYS},
        "calibration": {"years": [CAL_START.year, CAL_END.year], "data": "chuva da reanálise ERA5 (Open-Meteo) + vazão GloFAS", **cal},
        "backtest": {"period": bt["period"], **bt["contingency"], "floods": len(bt["floods"]),
                     "floods_warned": sum(1 for f in bt["floods"] if f["first_alert"]), "false_alarm_runs": len(bt["false_alarm_runs"])},
    }
    sha = hashlib.sha256(canonical(spec)).hexdigest()
    out = (out_dir or ROOT / "ml" / "models" / "outlook-rules") / f"{sid}.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps({"rule_sha256": sha, "spec": spec}, indent=2, ensure_ascii=False) + "\n")
    print(f"  regra {sid}: {spec['threshold_mm']} mm, rule_sha256 {sha[:16]}… → {out.relative_to(ROOT)}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--end", type=date.fromisoformat, default=date.today() - timedelta(days=2))
    ap.add_argument("--only", nargs="*", help="sub-basins to run (default: all)")
    ap.add_argument("--write-rules", nargs="*", default=[], help="sub-basins whose rule files are written (after review)")
    args = ap.parse_args()
    report = {"method": __doc__.split("\n\n", 2)[2].split("\n\nWrites")[0], "subbasins": {}}
    for sid, sb in SUBBASINS.items():
        if args.only and sid not in args.only:
            continue
        tid, tlat, tlon = sb["target"]
        q = glofas_daily(tid, tlat, tlon, args.end)
        thr_q = float(np.quantile([v for d, v in q.items() if d <= CAL_END.isoformat()], 0.99))
        per_point = [era5_daily(*p) for p in sb["points"]]
        rain = {d: float(np.mean([pp[d] for pp in per_point])) for d in per_point[0] if all(d in pp for pp in per_point)}
        cal = calibrate(rain, q, thr_q)
        bt = backtest(sb["points"], cal["threshold_mm"], q, thr_q, args.end)
        report["subbasins"][sid] = {"name": sb["name"], "target": tid, "flood_m3s_p99": round(thr_q), "points": [p[0] for p in sb["points"]],
                                    "calibration": cal, "backtest": bt}
        if sid in args.write_rules:
            write_rule(sid, sb, cal, thr_q, bt)
        ct = bt["contingency"]
        print(f"{sb['name']:<44} P99 {thr_q:>7.0f} m³/s | regra {cal['threshold_mm']:>3} mm em 5 d | calibração CSI {cal['csi']:.2f} "
              f"| teste: {ct['hits']} acertos, {ct['false_alarms']} alarmes falsos, {ct['misses']} perdidos (CSI {ct['csi']:.2f}) "
              f"| cheias {len(bt['floods'])}, avisadas {sum(1 for f in bt['floods'] if f['first_alert'])}")
    out = ROOT / "ml" / "reports" / "subbasin-rules.json"
    out.write_text(json.dumps(report, ensure_ascii=False, indent=1) + "\n")
    print(f"→ {out.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
