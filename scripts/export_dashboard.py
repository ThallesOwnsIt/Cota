#!/usr/bin/env python3
"""Builds the dashboard data from the ledger, the audit and the model report.

    python3 scripts/export_dashboard.py

Output in dashboard/data/:
- bundle.json        everything the page draws (days, forecasts, quality, metrics)
- batches/NNNNN.json each batch's readings, loaded on demand for the Merkle proof
- public_alerts/<sha>.txt canonical content of each official warning (the browser recomputes the SHA-256)
- outlooks/<sha>.txt  canonical content of each rain outlook (same)
(the parametric vaults go in the bundle itself)

The 2024 batches are the flood replay; those from 2025 on are live mode.
"""

from __future__ import annotations

import json
import tomllib
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LEDGER = ROOT / "data/ledger"
OUT = ROOT / "dashboard/data"
TARGET = "GLOFAS-GUAIBA"
# Cais Mauá gauge (ANA): the level Porto Alegre's civil defense follows.
STAGE_MAIN = "ANA-87450004"
# Days of Cais Mauá hourly readings in the live-mode chart.
STAGE_SERIES_DAYS = 60
RPC = "https://api.devnet.solana.com"
LIVE_FROM = "2025-01-01"


def day_of(ts: int) -> str:
    return datetime.fromtimestamp(ts, timezone.utc).date().isoformat()


def main() -> None:
    anchor = tomllib.loads((ROOT / "Anchor.toml").read_text())
    program = anchor["programs"]["devnet"]["cota"]
    stations = tomllib.loads((ROOT / "config/stations.toml").read_text())["station"]
    live_stations = tomllib.loads((ROOT / "config/stations-live.toml").read_text())["station"]
    manifest = json.loads((ROOT / "ml/models/manifest.json").read_text())
    metrics = json.loads((ROOT / "ml/reports/metrics.json").read_text())
    audit = json.loads((LEDGER / "audit.json").read_text())
    audit_batches = {b["batch_id"]: b for b in audit["batches"]}
    audit_forecasts = {f["account"]: f for f in audit["forecasts"]}

    (OUT / "batches").mkdir(parents=True, exist_ok=True)
    days = []
    stage_series: list[list[int]] = []
    members = None
    for path in sorted((LEDGER / "batches").glob("*.json")):
        e = json.loads(path.read_text())
        a = audit_batches[e["batch_id"]]
        members = members or [m["member"] for m in e["members"]]
        rain: dict[str, float | None] = {}
        hours: dict[str, int] = {}
        guaiba = None
        stage: dict[str, list[int]] = {}
        flow: dict[str, int] = {}
        for r in e["readings"]:
            if r["variable"] == "river_stage":
                stage.setdefault(r["station_id"], []).append(r["value_milli"])
                if r["station_id"] == STAGE_MAIN:
                    stage_series.append([r["ts_utc"], r["value_milli"]])
            elif r["variable"] == "precipitation":
                rain[r["station_id"]] = rain.get(r["station_id"], 0) + r["value_milli"]
                hours[r["station_id"]] = hours.get(r["station_id"], 0) + 1
            elif r["variable"] == "river_discharge":
                flow[r["station_id"]] = r["value_milli"]
                if r["station_id"] == TARGET:
                    guaiba = r["value_milli"]
        q = e["quality"]
        days.append(
            {
                "batch_id": e["batch_id"],
                "day": e["day"],
                "t_start": e["header"]["t_start"],
                "t_end": e["header"]["t_end"],
                "leaf_count": e["header"]["leaf_count"],
                "merkle_root": e["header"]["merkle_root"],
                "prev_batch_root": e["header"]["prev_batch_root"],
                "batch_account": e["batch_account"],
                "explorer": e["explorer"],
                "slot": a["slot"],
                "committed_at": a["committed_at"],
                "signers": a["signers"],
                "members": [{"member": m["member"], "status": m["status"]} for m in e["members"]],
                "guaiba_milli": guaiba,
                **({"flow": flow} if day_of(e["header"]["t_start"]) >= LIVE_FROM else {}),
                # Measured level (ANA): the day's first and last reading, its maximum and how many
                # hours came in.
                **(
                    {"stage": {sid: {"first": v[0], "last": v[-1], "max": max(v), "hours": len(v)} for sid, v in stage.items()}}
                    if stage
                    else {}
                ),
                "rain_milli": rain,
                "rain_hours": hours,
                "quality": [
                    {
                        "station_id": s["station_id"],
                        "score": round(s["score"], 4),
                        "raw": round(s["raw"], 4),
                        "availability": round(s["availability"], 4),
                        "present": s["present"],
                        "expected": s["expected"],
                        "flags": [f["reason"] for f in s["flags"]],
                        "degraded": s["degraded"],
                    }
                    for s in q["stations"]
                ],
                "dissents": q["dissents"],
                "degradations": [
                    {k: d[k] for k in ("station_id", "reason", "score_milli", "explorer", "tx_signature")} for d in q["degradations"]
                ],
            }
        )
        (OUT / "batches" / path.name).write_text(
            json.dumps({"batch_id": e["batch_id"], "header": e["header"], "readings": e["readings"]}, separators=(",", ":"))
        )

    issues = []
    for path in sorted((LEDGER / "forecasts").glob("*.json")):
        f = json.loads(path.read_text())
        horizons = []
        for rec in f["onchain"]:
            a = audit_forecasts[rec["account"]]
            alert = rec["alert"]
            horizons.append(
                {
                    "horizon_hours": rec["horizon_hours"],
                    "target_ts": rec["target_ts"],
                    "target_day": day_of(rec["target_ts"]),
                    "p10": rec["p10"],
                    "p50": rec["p50"],
                    "p90": rec["p90"],
                    "account": rec["account"],
                    "explorer": rec["explorer"],
                    "slot": a["slot"],
                    "observation_slot": a["observation_slot"],
                    "observed_milli": a["observed_milli"],
                    "recorded_before_observation": a["recorded_before_observation"],
                    "inside_p10_p90": a["inside_p10_p90"],
                    "alert": None
                    if alert is None
                    else {
                        "account": alert["account"],
                        "explorer": alert["explorer"],
                        "slot": a["alert"]["slot"],
                        "recorded_before_observation": a["alert"]["recorded_before_observation"],
                    },
                }
            )
        issues.append(
            {
                "issue_day": f["issue_day"],
                "issued_at": f["issued_at"],
                "based_on_batch_id": f["based_on_batch_id"],
                "based_on_root": f["based_on_root"],
                "model_sha256": f["model_sha256"],
                "features_root": f["features_root"],
                "features": f["features"],
                "horizons": horizons,
            }
        )

    alerts = []
    audit_pa = {a["record"]: a for a in audit.get("public_alerts", [])}
    (OUT / "public_alerts").mkdir(parents=True, exist_ok=True)
    pa_dir = LEDGER / "public_alerts"
    for path in sorted(pa_dir.glob("*.json")) if pa_dir.exists() else []:
        e = json.loads(path.read_text())
        h = e["header"]
        (OUT / "public_alerts" / f"{h['content_sha256']}.txt").write_text(e["content"], encoding="utf-8")
        a = audit_pa.get(e["record"], {})
        alerts.append(
            {
                "record": e["record"],
                "alert_id": h["alert_id"],
                "issuer": h["issuer"],
                "content_sha256": h["content_sha256"],
                "severity": h["severity"],
                "severity_label": e["severity_label"],
                "description": e["description"],
                "states": e["states"],
                "onset": h["onset"],
                "expires": h["expires"],
                "slot": e["slot"],
                "committed_at": e["committed_at"],
                "signers": e["signers"],
                "explorer": e["explorer"],
                "observations": e["observations"],
                "audit_ok": not a.get("problems", ["não auditado"]),
            }
        )
    alerts.sort(key=lambda a: a["slot"], reverse=True)

    outlook = export_outlook(audit)
    audited_vaults = {v["vault"]: v for v in audit.get("vaults", [])}
    vaults = []
    for path in sorted((LEDGER / "vaults").glob("*.json")) if (LEDGER / "vaults").exists() else []:
        e = json.loads(path.read_text())
        a = audited_vaults.get(e["vault"], {})
        vaults.append(
            {
                **{k: e[k] for k in ("name", "description", "vault", "created_explorer", "state", "payouts")},
                "description_en": e.get("description_en", ""),
                "audit_ok": not a.get("problems", ["não auditado"]),
            }
        )

    replay_days = [d for d in days if d["day"] < LIVE_FROM]
    live_days = [d for d in days if d["day"] >= LIVE_FROM]
    replay_issues = [i for i in issues if i["issue_day"] < LIVE_FROM]
    live_issues = [i for i in issues if i["issue_day"] >= LIVE_FROM]

    test = metrics["test_2024"]
    bundle = {
        "meta": {
            "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "program": program,
            "cluster": "devnet",
            "rpc": RPC,
            "members": members,
            "threshold": 2,
            "target": manifest["target"],
            "model_sha256": manifest["model_sha256"],
            "alert_threshold_milli": manifest["alert_rule"]["threshold_milli"],
            "train_years": manifest["train_years"],
        },
        "metrics": {
            "audit": audit["summary"],
            "test": {
                h: {k: test[h][k] for k in ("n", "crps", "coverage_p10_p90", "mean_width", "nse_p50", "nse_persistence", "kge_p50", "alerts")}
                for h in test
            },
            "episodes": metrics["episodes"],
            "conformal_margin_ln": metrics["conformal_margin_ln"],
        },
        "stations": [
            {k: s.get(k) for k in ("id", "name", "source", "variable", "lat", "lon")} for s in stations
        ],
        "days": replay_days,
        "issues": replay_issues,
        "live": {
            "days": live_days,
            "issues": live_issues,
            "public_alerts": alerts,
            "stage_stations": [{"id": s["id"], "name": s["name"]} for s in live_stations if s["source"] == "ana"],
            # Live-mode stations, for the basin map.
            "stations": [{k: s[k] for k in ("id", "name", "source", "variable", "lat", "lon")} for s in live_stations],
            "stage_main": STAGE_MAIN,
            "stage_series": stage_series_tail(stage_series),
        },
        "outlook": outlook,
        "vaults": vaults,
    }
    (OUT / "bundle.json").write_text(json.dumps(bundle, separators=(",", ":"), ensure_ascii=False))
    size = sum(p.stat().st_size for p in OUT.rglob("*") if p.is_file())
    print(
        f"replay: {len(replay_days)} lotes, {len(replay_issues)} previsões · ao vivo: {len(live_days)} lotes, "
        f"{len(live_issues)} previsões, {len(alerts)} avisos oficiais, {len(outlook['live'])} perspectivas de chuva, {len(vaults)} cofres "
        f"→ {OUT} ({size / 1024:.0f} KiB)"
    )


def stage_series_tail(series: list[list[int]]) -> list[list[int]]:
    """Cais Mauá hourly readings over the last STAGE_SERIES_DAYS anchored days."""
    if not series:
        return []
    series.sort()
    start = series[-1][0] - STAGE_SERIES_DAYS * 86_400
    return [p for p in series if p[0] > start]


def export_outlook(audit: dict) -> dict:
    """Early-warning rule, out-of-sample backtest and anchored outlooks."""
    rule = json.loads((ROOT / "ml/models/outlook-rule.json").read_text())
    bt = json.loads((ROOT / "ml/reports/outlook-backtest.json").read_text())
    spec = rule["spec"]
    audited = {o["record"]: o for o in audit.get("outlooks", [])}
    (OUT / "outlooks").mkdir(parents=True, exist_ok=True)
    live = []
    for path in sorted((LEDGER / "outlooks").glob("*.json")) if (LEDGER / "outlooks").exists() else []:
        e = json.loads(path.read_text())
        h = e["header"]
        (OUT / "outlooks" / f"{h['content_sha256']}.txt").write_text(e["content"], encoding="utf-8")
        a = audited.get(e["record"], {})
        live.append(
            {
                "day": e["day"],
                "record": e["record"],
                **{k: h[k] for k in ("issued_at", "window_end", "models", "rain_mean_milli", "rain_min_milli", "rain_max_milli", "content_sha256")},
                "threshold_milli": e["threshold_milli"],
                "triggered": e["triggered"],
                "slot": e["slot"],
                "committed_at": e["committed_at"],
                "signers": e["signers"],
                "based_on_batch_id": e["based_on_batch_id"],
                "explorer": e["explorer"],
                "next_batch_slot": a.get("next_batch_slot"),
                "audit_ok": not a.get("problems", ["não auditado"]),
            }
        )
    live.sort(key=lambda o: o["day"], reverse=True)
    rule_account = next((json.loads(p.read_text())["rule"] for p in (LEDGER / "outlooks").glob("*.json")), None) if live else None
    return {
        "rule": {
            "sha256": rule["rule_sha256"],
            "account": rule_account,
            "region": spec["region"],
            "threshold_mm": spec["threshold_mm"],
            "window_days": spec["window_days"],
            "min_models": spec["min_models"],
            "models": spec["models"],
            "points": [p["id"] for p in spec["points"]],
            "target": spec["target"],
            "calibration": spec["calibration"],
        },
        "backtest": {
            "period": bt["period"],
            "contingency": bt["contingency"],
            "floods": bt["floods"],
            "false_alarm_runs": bt["false_alarm_runs"],
            "days": [
                {"day": d["day"], "rain": d["rain_mm"], "models": d["models"], "q": None if d["q"] is None else round(d["q"]), "alert": d["alert"], "flood": d["flood_ahead"]}
                for d in bt["days"]
            ],
        },
        "live": live,
    }


if __name__ == "__main__":
    main()
