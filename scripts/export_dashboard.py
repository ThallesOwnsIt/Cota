#!/usr/bin/env python3
"""Builds the dashboard data from the ledger, the audit and the model report.

    python3 scripts/export_dashboard.py

Output in dashboard/data/:
- bundle.json        everything the page draws (days, forecasts, quality, metrics)
- batches/vN-NNNNN.json each batch's readings, loaded on demand for the Merkle proof
- records-v2.json     the leaves of program v2's accumulator, to check them against the on-chain peaks
- timestamps/         the accumulator's daily root and its OpenTimestamps proof (Bitcoin)
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
# Program v1's ledger, frozen at the switch to v2 (docs/v2-accumulator.md).
ARCHIVE = ROOT / "data/ledger-v1"
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
    stations = tomllib.loads((ROOT / "config/stations.toml").read_text())["station"]
    live_stations = tomllib.loads((ROOT / "config/stations-live.toml").read_text())["station"]
    manifest = json.loads((ROOT / "ml/models/manifest.json").read_text())
    metrics = json.loads((ROOT / "ml/reports/metrics.json").read_text())

    for sub in ("batches", "public_alerts", "outlooks"):
        (OUT / sub).mkdir(parents=True, exist_ok=True)
    for old in (OUT / "batches").glob("*.json"):
        old.unlink()

    # The current ledger first; program v1's frozen archive fills in the days the current one lacks.
    parts = [collect(LEDGER)] + ([collect(ARCHIVE)] if ARCHIVE.exists() else [])
    cur = parts[0]
    seen_days = set()
    days, issues, alerts, outlooks, vaults = [], [], [], [], []
    for part in parts:
        new_days = [d for d in part["days"] if d["day"] not in seen_days]
        seen_days |= {d["day"] for d in new_days}
        days += new_days
        issue_days = {i["issue_day"] for i in issues}
        issues += [i for i in part["issues"] if i["issue_day"] not in issue_days]
        alerts += part["alerts"]
        outlook_days = {o["day"] for o in outlooks}
        outlooks += [o for o in part["outlooks"] if o["day"] not in outlook_days]
        # Vaults only from the current program: v1's receive no more triggers, and v2's replay vault
        # reproduces the same payouts.
        if part is cur:
            vaults += part["vaults"]
    days.sort(key=lambda d: d["day"])
    issues.sort(key=lambda i: i["issue_day"])
    # The same warning version may be in both programs: the earliest record is the one that counts.
    earliest: dict[str, dict] = {}
    for a in alerts:
        k = a["content_sha256"]
        if k not in earliest or a["committed_at"] < earliest[k]["committed_at"]:
            earliest[k] = a
    alerts = sorted(earliest.values(), key=lambda a: a["committed_at"], reverse=True)
    outlooks.sort(key=lambda o: o["day"], reverse=True)

    stage_series = sorted(p for part in parts for p in part["stage_series"])
    replay_days = [d for d in days if d["day"] < LIVE_FROM]
    live_days = [d for d in days if d["day"] >= LIVE_FROM]
    replay_issues = [i for i in issues if i["issue_day"] < LIVE_FROM]
    live_issues = [i for i in issues if i["issue_day"] >= LIVE_FROM]

    programs = {
        part["proto"]: {
            "program": part["program"],
            "registry": part["registry"],
            "members": part["members"],
            "audit": part["audit_summary"],
            **({"leaves_file": "records-v2.json"} if part["proto"] == "v2" else {}),
        }
        for part in parts
    }
    for part in parts:
        if part["proto"] == "v2":
            (OUT / "records-v2.json").write_text(json.dumps({"leaves": part["leaves"]}, separators=(",", ":")))

    # OpenTimestamps: the accumulator's root, timestamped in Bitcoin (scripts/timestamp.py).
    timestamps = []
    ts_dir = LEDGER / "timestamps"
    if (ts_dir / "index.json").exists():
        (OUT / "timestamps").mkdir(exist_ok=True)
        for t in json.loads((ts_dir / "index.json").read_text()):
            for name in (t["file"], t["file"] + ".ots"):
                (OUT / "timestamps" / name).write_bytes((ts_dir / name).read_bytes())
            timestamps.append(t)
        timestamps.sort(key=lambda t: t["records"], reverse=True)

    test = metrics["test_2024"]
    bundle = {
        "meta": {
            "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            # Program of the current ledger; `programs` has both when the v1 archive is present.
            "program": cur["program"],
            "protocol": cur["proto"],
            "programs": programs,
            "cluster": "devnet",
            "rpc": RPC,
            "members": cur["members"],
            "threshold": 2,
            "target": manifest["target"],
            "model_sha256": manifest["model_sha256"],
            "alert_threshold_milli": manifest["alert_rule"]["threshold_milli"],
            "train_years": manifest["train_years"],
        },
        "metrics": {
            "audit": cur["audit_summary"],
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
        "outlook": export_outlook(outlooks, parts),
        "timestamps": timestamps,
        "vaults": vaults,
    }
    (OUT / "bundle.json").write_text(json.dumps(bundle, separators=(",", ":"), ensure_ascii=False))
    size = sum(p.stat().st_size for p in OUT.rglob("*") if p.is_file())
    print(
        f"{' + '.join(p['proto'] for p in parts)} · replay: {len(replay_days)} lotes, {len(replay_issues)} previsões · ao vivo: {len(live_days)} lotes, "
        f"{len(live_issues)} previsões, {len(alerts)} avisos oficiais, {len(outlooks)} perspectivas de chuva, {len(vaults)} cofres "
        f"→ {OUT} ({size / 1024:.0f} KiB)"
    )


def collect(ledger: Path) -> dict:
    """Everything one ledger contributes. v2 if it has the accumulator's records, v1 otherwise."""
    audit = json.loads((ledger / "audit.json").read_text())
    proto = "v2" if "accumulator" in audit else "v1"
    stored = []
    if proto == "v2":
        stored = [json.loads(l) for l in (ledger / "records.jsonl").read_text().splitlines() if l.strip()]

    def record_hex(index: int) -> str:
        return stored[index]["record"]

    key = "record_index" if proto == "v2" else "account"
    audit_batches = {b["batch_id"]: b for b in audit["batches"]}
    audit_forecasts = {f[key]: f for f in audit["forecasts"]}

    days = []
    stage_series: list[list[int]] = []
    members = None
    for path in sorted((ledger / "batches").glob("*.json")):
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
                "protocol": proto,
                **({"batch_account": e["batch_account"]} if proto == "v1" else {"record_index": e["record_index"], "record": record_hex(e["record_index"])}),
                "batch_file": f"batches/{proto}-{e['batch_id']:05d}.json",
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
        (OUT / "batches" / f"{proto}-{e['batch_id']:05d}.json").write_text(
            json.dumps({"batch_id": e["batch_id"], "header": e["header"], "readings": e["readings"]}, separators=(",", ":"))
        )


    issues = []
    for path in sorted((ledger / "forecasts").glob("*.json")) if (ledger / "forecasts").exists() else []:
        f = json.loads(path.read_text())
        horizons = []
        for rec in f["onchain"]:
            a = audit_forecasts[rec[key]]
            alert = rec["alert"]
            h = {
                "horizon_hours": rec["horizon_hours"],
                "target_ts": rec["target_ts"],
                "target_day": day_of(rec["target_ts"]),
                "p10": rec["p10"],
                "p50": rec["p50"],
                "p90": rec["p90"],
                "protocol": proto,
                "explorer": rec["explorer"],
                "slot": a["slot"],
                "observation_slot": a["observation_slot"],
                "observed_milli": a["observed_milli"],
                "recorded_before_observation": a["recorded_before_observation"],
                "inside_p10_p90": a["inside_p10_p90"],
            }
            if proto == "v1":
                h["account"] = rec["account"]
                h["alert"] = None if alert is None else {
                    "account": alert["account"],
                    "explorer": alert["explorer"],
                    "slot": a["alert"]["slot"],
                    "recorded_before_observation": a["alert"]["recorded_before_observation"],
                }
            else:
                h["record_index"] = rec["record_index"]
                h["record"] = record_hex(rec["record_index"])
                # In v2 the alert is part of the forecast record, decided in the same instruction.
                h["alert"] = None if alert is None else {
                    "threshold_milli": alert["threshold_milli"],
                    "explorer": rec["explorer"],
                    "slot": a["alert"]["slot"],
                    "recorded_before_observation": a["alert"]["recorded_before_observation"],
                }
            horizons.append(h)
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
    akey = "record_index" if proto == "v2" else "record"
    audit_pa = {a[akey]: a for a in audit.get("public_alerts", [])}
    for path in sorted((ledger / "public_alerts").glob("*.json")) if (ledger / "public_alerts").exists() else []:
        e = json.loads(path.read_text())
        h = e["header"]
        (OUT / "public_alerts" / f"{h['content_sha256']}.txt").write_text(e["content"], encoding="utf-8")
        a = audit_pa.get(e[akey], {})
        alerts.append(
            {
                "protocol": proto,
                **({"record": e["record"]} if proto == "v1" else {"record_index": e["record_index"], "record": record_hex(e["record_index"])}),
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

    audited_outlooks = {o[akey]: o for o in audit.get("outlooks", [])}
    outlooks = []
    rule_account = None
    for path in sorted((ledger / "outlooks").glob("*.json")) if (ledger / "outlooks").exists() else []:
        e = json.loads(path.read_text())
        h = e["header"]
        rule_account = rule_account or e.get("rule")
        (OUT / "outlooks" / f"{h['content_sha256']}.txt").write_text(e["content"], encoding="utf-8")
        a = audited_outlooks.get(e[akey], {})
        outlooks.append(
            {
                "day": e["day"],
                "protocol": proto,
                **({"record": e["record"]} if proto == "v1" else {"record_index": e["record_index"], "record": record_hex(e["record_index"])}),
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

    audited_vaults = {v["vault"]: v for v in audit.get("vaults", [])}
    vaults = []
    for path in sorted((ledger / "vaults").glob("*.json")) if (ledger / "vaults").exists() else []:
        e = json.loads(path.read_text())
        a = audited_vaults.get(e["vault"], {})
        if proto == "v2":
            # The browser proves each payout's trigger record against the on-chain peaks.
            for p in e["payouts"]:
                p["trigger_record"] = record_hex(p["trigger_index"])
        vaults.append(
            {
                **{k: e[k] for k in ("name", "description", "vault", "created_explorer", "state", "payouts")},
                "protocol": proto,
                "description_en": e.get("description_en", ""),
                "audit_ok": not a.get("problems", ["não auditado"]),
            }
        )

    return {
        "proto": proto,
        "program": audit.get("program"),
        "registry": audit.get("registry"),
        "audit_summary": audit["summary"],
        "members": members,
        "days": days,
        "issues": issues,
        "alerts": alerts,
        "outlooks": outlooks,
        "outlook_rule_account": rule_account,
        "vaults": vaults,
        "stage_series": stage_series,
        "leaves": [r["leaf"] for r in stored],
    }


def stage_series_tail(series: list[list[int]]) -> list[list[int]]:
    """Cais Mauá hourly readings over the last STAGE_SERIES_DAYS anchored days."""
    if not series:
        return []
    series.sort()
    start = series[-1][0] - STAGE_SERIES_DAYS * 86_400
    return [p for p in series if p[0] > start]


def export_outlook(live: list[dict], parts: list[dict]) -> dict:
    """Early-warning rule, out-of-sample backtest and anchored outlooks."""
    rule = json.loads((ROOT / "ml/models/outlook-rule.json").read_text())
    bt = json.loads((ROOT / "ml/reports/outlook-backtest.json").read_text())
    spec = rule["spec"]
    rule_account = next((p["outlook_rule_account"] for p in parts if p["outlook_rule_account"]), None)
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
