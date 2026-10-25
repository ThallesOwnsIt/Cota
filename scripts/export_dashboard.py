#!/usr/bin/env python3
"""Gera os dados do dashboard a partir do ledger, da auditoria e do relatório do modelo.

    python3 scripts/export_dashboard.py

Saída em dashboard/data/:
- bundle.json        tudo o que a página desenha (dias, previsões, qualidade, métricas)
- batches/NNNNN.json leituras de cada lote, carregadas sob demanda para a prova Merkle
- public_alerts/<sha>.txt conteúdo canônico de cada aviso oficial (o navegador refaz o SHA-256)
- outlooks/<sha>.txt  conteúdo canônico de cada perspectiva de chuva (idem)

Os lotes de 2024 são o replay da cheia; os de 2025 em diante, o modo ao vivo.
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
RPC = "https://api.devnet.solana.com"
LIVE_FROM = "2025-01-01"


def day_of(ts: int) -> str:
    return datetime.fromtimestamp(ts, timezone.utc).date().isoformat()


def main() -> None:
    anchor = tomllib.loads((ROOT / "Anchor.toml").read_text())
    program = anchor["programs"]["devnet"]["cota"]
    stations = tomllib.loads((ROOT / "config/stations.toml").read_text())["station"]
    manifest = json.loads((ROOT / "ml/models/manifest.json").read_text())
    metrics = json.loads((ROOT / "ml/reports/metrics.json").read_text())
    audit = json.loads((LEDGER / "audit.json").read_text())
    audit_batches = {b["batch_id"]: b for b in audit["batches"]}
    audit_forecasts = {f["account"]: f for f in audit["forecasts"]}

    (OUT / "batches").mkdir(parents=True, exist_ok=True)
    days = []
    members = None
    for path in sorted((LEDGER / "batches").glob("*.json")):
        e = json.loads(path.read_text())
        a = audit_batches[e["batch_id"]]
        members = members or [m["member"] for m in e["members"]]
        rain: dict[str, float | None] = {}
        hours: dict[str, int] = {}
        guaiba = None
        for r in e["readings"]:
            if r["variable"] == "precipitation":
                rain[r["station_id"]] = rain.get(r["station_id"], 0) + r["value_milli"]
                hours[r["station_id"]] = hours.get(r["station_id"], 0) + 1
            elif r["station_id"] == TARGET:
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
        "live": {"days": live_days, "issues": live_issues, "public_alerts": alerts},
        "outlook": outlook,
    }
    (OUT / "bundle.json").write_text(json.dumps(bundle, separators=(",", ":"), ensure_ascii=False))
    size = sum(p.stat().st_size for p in OUT.rglob("*") if p.is_file())
    print(
        f"replay: {len(replay_days)} lotes, {len(replay_issues)} previsões · ao vivo: {len(live_days)} lotes, "
        f"{len(live_issues)} previsões, {len(alerts)} avisos oficiais, {len(outlook['live'])} perspectivas de chuva "
        f"→ {OUT} ({size / 1024:.0f} KiB)"
    )


def export_outlook(audit: dict) -> dict:
    """Regra do alerta antecipado, backtest fora da amostra e perspectivas ancoradas."""
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
