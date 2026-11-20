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

After the cutover to the v3 ledger (COTA_LEDGER=v3, or `--v3 DIR` to preview a ledger such as the rehearsal), the
page's data comes from the v3 ledger only, each item with its entry (index, leaf), signers and covering checkpoint;
the Solana history (v2 and v1) goes to dashboard/data/archive/, which the archive page reads. The parametric vaults
were retired on 13/11/2026 and are no longer exported.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tomllib
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LEDGER = ROOT / "data/ledger"
# Program v1's ledger, frozen at the switch to v2 (docs/v2-accumulator.md).
ARCHIVE = ROOT / "data/ledger-v1"
OUT = ROOT / "dashboard/data"
# `--out DIR`: write somewhere else (a preview), never the committed data.
if "--out" in __import__("sys").argv:
    OUT = Path(__import__("sys").argv[__import__("sys").argv.index("--out") + 1]).resolve()
TARGET = "GLOFAS-GUAIBA"
# Cais Mauá gauge (ANA): the level Porto Alegre's civil defense follows.
STAGE_MAIN = "ANA-87450004"
# Places and their official levels (config/locations.toml, checked by scripts/check_locations.py).
LOCATIONS = tomllib.loads((ROOT / "config/locations.toml").read_text())["location"] if (ROOT / "config/locations.toml").exists() else []
# River-level plausibility (hand-written; the same file and rule as cota-quality, see stage_spikes).
STAGE_LIMITS = tomllib.loads((ROOT / "config/stage-limits.toml").read_text()) if (ROOT / "config/stage-limits.toml").exists() else {}
# Days of Cais Mauá hourly readings in the live-mode chart.
STAGE_SERIES_DAYS = 60
RPC = "https://api.devnet.solana.com"
LIVE_FROM = "2025-01-01"


def day_of(ts: int) -> str:
    return datetime.fromtimestamp(ts, timezone.utc).date().isoformat()


def main() -> None:
    v3 = v3_source()
    if v3 is None:
        write_v2_bundle()
        return
    # The Solana history, frozen, for the archive page; then the page's own data from the v3 ledger.
    global OUT
    main_out = OUT
    OUT = main_out / "archive"
    write_v2_bundle()
    OUT = main_out
    export_v3(OUT, v3)


def v3_source() -> Path | None:
    """The v3 ledger the page reads: `--v3 DIR`, or V3_LEDGER (default data/ledger-v3) when COTA_LEDGER=v3."""
    if "--v3" in sys.argv:
        return ROOT / sys.argv[sys.argv.index("--v3") + 1]
    if os.environ.get("COTA_LEDGER") == "v3":
        return ROOT / os.environ.get("V3_LEDGER", "data/ledger-v3")
    return None


def write_v2_bundle() -> None:
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
        outlook_days = {(o["region"], o["day"]) for o in outlooks}
        outlooks += [o for o in part["outlooks"] if (o["region"], o["day"]) not in outlook_days]
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
    gauges_now = {k: v for part in parts for k, v in part["gauges_now"].items()}
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
            "locations": export_locations(gauges_now),
        },
        "outlook": export_outlook(outlooks, parts),
        "external_forecasts": export_external(),
        "timestamps": timestamps,
        # Copy of the ledger on Arweave (scripts/archive/archive.mjs): how much, and who signs the uploads.
        "archive": archive_summary(),
        # Who can change program v2: a multisig (scripts/squads/squads.mjs). The browser checks it live.
        "upgrade": json.loads((ROOT / "config/upgrade-multisig.json").read_text()) if (ROOT / "config/upgrade-multisig.json").exists() else None,
        # Retired on 13/11/2026 (decision of the project owner): no longer on the page.
        "vaults": [],
    }
    (OUT / "bundle.json").write_text(json.dumps(bundle, separators=(",", ":"), ensure_ascii=False))
    size = sum(p.stat().st_size for p in OUT.rglob("*") if p.is_file() and "archive" not in p.relative_to(OUT).parts[:-1])
    print(
        f"{' + '.join(p['proto'] for p in parts)} · replay: {len(replay_days)} lotes, {len(replay_issues)} previsões · ao vivo: {len(live_days)} lotes, "
        f"{len(live_issues)} previsões, {len(alerts)} avisos oficiais, {len(outlooks)} perspectivas de chuva "
        f"→ {OUT} ({size / 1024:.0f} KiB)"
    )


def stage_spikes(sid: str, points: list[tuple[int, int]]) -> set[int]:
    """Timestamps of isolated spikes in one gauge's sorted series, by the same rule as cota-quality's stage_spikes:
    reaching the reading from the previous plausible one and coming back from it to the next would both need the
    river to move faster than its limit, in opposite directions, and by more than the floor."""
    floor = STAGE_LIMITS.get("floor_mm", 300)
    rate = STAGE_LIMITS.get("rate_mm_h", {}).get(sid, STAGE_LIMITS.get("default_rate_mm_h", 2000))
    too_fast = lambda dv, dt: abs(dv) > floor and abs(dv) > rate * dt / 3600  # noqa: E731
    out: set[int] = set()
    prev = None
    for i, (t, v) in enumerate(points):
        if prev is None:
            prev = (t, v)
            continue
        if i + 1 >= len(points):
            break
        tn, vn = points[i + 1]
        up, down = v - prev[1], vn - v
        if up * down < 0 and too_fast(up, t - prev[0]) and too_fast(down, tn - t):
            out.add(t)
        else:
            prev = (t, v)
    return out


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

    # Implausible river levels, judged over each gauge's whole series (a spike at midnight needs the next day's
    # first reading): left out of the levels below, and listed per day.
    gauges: dict[str, list[tuple[int, int]]] = {}
    for path in sorted((ledger / "batches").glob("*.json")):
        for r in json.loads(path.read_text())["readings"]:
            if r["variable"] == "river_stage":
                gauges.setdefault(r["station_id"], []).append((r["ts_utc"], r["value_milli"]))
    implausible = {(sid, t) for sid, pts in gauges.items() for t in stage_spikes(sid, sorted(pts))}
    # Each gauge's latest plausible reading and the one about 24 h before it (within 3 h), for the places.
    gauges_now = {}
    for sid, pts in gauges.items():
        good = sorted(p for p in pts if (sid, p[0]) not in implausible)
        if good:
            t, v = good[-1]
            near = [pv for pt, pv in good if abs(pt - (t - 86_400)) <= 3 * 3600]
            gauges_now[sid] = {"ts": t, "value_milli": v, "change_24h_milli": v - near[0] if near else None}

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
        dropped: list[dict] = []
        flow: dict[str, int] = {}
        for r in e["readings"]:
            if r["variable"] == "river_stage" and (r["station_id"], r["ts_utc"]) in implausible:
                dropped.append({"station_id": r["station_id"], "ts_utc": r["ts_utc"], "value_milli": r["value_milli"]})
            elif r["variable"] == "river_stage":
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
                # Readings left out as isolated spikes (still in the batch: it is what the source published).
                **({"stage_implausible": dropped} if dropped else {}),
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
                "region": h.get("region", "guaiba"),
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
        "gauges_now": gauges_now,
        "leaves": [r["leaf"] for r in stored],
    }


def export_locations(gauges_now: dict) -> list[dict]:
    """Each place with its gauge's latest plausible level, the change in 24 h and, only when its levels come from an
    official document, its situation: "inundacao" at or above flood, "alerta", "atencao", else "normal"."""
    out = []
    for p in LOCATIONS:
        levels = {k[:-2]: round(p[k] * 1000) for k in ("attention_m", "alert_m", "flood_m", "record_m") if k in p}
        now = gauges_now.get(p["gauge"])
        state = None
        if p.get("confirmed") and now and levels:
            v = now["value_milli"]
            state = next((name for key, name in (("flood", "inundacao"), ("alert", "alerta"), ("attention", "atencao")) if key in levels and v >= levels[key]), "normal")
        out.append(
            {
                **{k: p[k] for k in ("id", "name", "river", "gauge") },
                "confirmed": bool(p.get("confirmed")),
                "levels_milli": levels,
                **{k: p[k] for k in ("source", "source_url", "checked", "note") if k in p},
                "now": now,
                "state": state,
            }
        )
    return out


def archive_summary() -> dict | None:
    p = LEDGER / "archive.json"
    if not p.exists():
        return None
    m = json.loads(p.read_text())
    return {"signer": m.get("signer"), "files": len(m["files"]) + len(m["records"]), "bytes": m["bytes"], "gateway": m["gateway"]}


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
        "live": [o for o in live if o["region"] == "guaiba"],
        "subbasins": export_subbasins(live),
    }


def export_subbasins(live: list[dict]) -> list[dict]:
    """Sub-basin rules (ml/models/outlook-rules), their out-of-sample test and their anchored outlooks. A rule not
    on-chain yet has no outlooks: it goes through the multisig first."""
    report = ROOT / "ml/reports/subbasin-rules.json"
    names = {k: v["name"] for k, v in json.loads(report.read_text())["subbasins"].items()} if report.exists() else {}
    out = []
    for path in sorted((ROOT / "ml/models/outlook-rules").glob("*.json")):
        rule = json.loads(path.read_text())
        spec = rule["spec"]
        region = spec["region"]
        out.append(
            {
                "region": region,
                "name": names.get(region, region),
                "rule_sha256": rule["rule_sha256"],
                "threshold_mm": spec["threshold_mm"],
                "window_days": spec["window_days"],
                "points": [p["id"] for p in spec["points"]],
                "target": spec["target"],
                "calibration": {k: spec["calibration"][k] for k in ("years", "data", "threshold_mm", "csi")},
                "backtest": spec.get("backtest"),
                "live": [o for o in live if o["region"] == region],
            }
        )
    return out


def export_external() -> dict | None:
    """Third-party forecasts kept in the ledger (external_forecasts/), signed by the committee: the latest one in
    full, a short history, and the files themselves (with their Bitcoin proofs) for download."""
    src = LEDGER / "external_forecasts" / "glofas"
    files = sorted(src.glob("*.json")) if src.exists() else []
    if not files:
        return None
    dest = OUT / "external_forecasts" / "glofas"
    dest.mkdir(parents=True, exist_ok=True)
    history = []
    for f in files:
        for g in (f, f.with_name(f.name + ".ots")):
            if g.exists():
                (dest / g.name).write_bytes(g.read_bytes())
        e = json.loads(f.read_text())
        c = json.loads(e["content"])
        outlet = next((p for p in c["points"] if p["id"] == TARGET), c["points"][0])
        history.append(
            {
                "day": e["day"],
                "quorum": e["quorum"],
                "signatures": len(e["signatures"]),
                "stamped": f.with_name(f.name + ".ots").exists(),
                "file": f"external_forecasts/glofas/{f.name}",
                "peak_median_milli": max((d["median"] for d in outlet["days"] if d["median"] is not None), default=None),
                "peak_max_milli": max((d["max"] for d in outlet["days"] if d["max"] is not None), default=None),
            }
        )
    latest = json.loads(files[-1].read_text())
    content = json.loads(latest["content"])
    return {
        "source": "glofas",
        "provider": content["provider"],
        "latest": {**history[-1], "threshold": latest["threshold"], "content_sha256": latest["header"]["content_sha256"], "points": content["points"]},
        "history": history[-30:],
    }


# ---- the v3 ledger (after the cutover) ------------------------------------------------------------------------

BITCOIN_TAG = bytes.fromhex("0588960d73d71901")  # OpenTimestamps: attestation in a Bitcoin block header


def summarize_readings(readings: list[dict], implausible: set, stage_series: list) -> dict:
    """Rain, measured level and discharge of one batch, as the page draws them (the same as v2's days)."""
    rain: dict[str, int] = {}
    hours: dict[str, int] = {}
    guaiba = None
    stage: dict[str, list[int]] = {}
    dropped: list[dict] = []
    flow: dict[str, int] = {}
    for r in readings:
        if r["variable"] == "river_stage" and (r["station_id"], r["ts_utc"]) in implausible:
            dropped.append({"station_id": r["station_id"], "ts_utc": r["ts_utc"], "value_milli": r["value_milli"]})
        elif r["variable"] == "river_stage":
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
    return {
        "guaiba_milli": guaiba,
        "flow": flow,
        **({"stage": {sid: {"first": v[0], "last": v[-1], "max": max(v), "hours": len(v)} for sid, v in stage.items()}} if stage else {}),
        **({"stage_implausible": dropped} if dropped else {}),
        "rain_milli": rain,
        "rain_hours": hours,
    }


def export_v3(out: Path, ledger3: Path) -> None:
    """The page's data from the v3 ledger: only what the committee signed into it. Each item carries its entry
    (index, leaf), its signers and the checkpoint that covers it; the browser checks them with the same rules that
    built the ledger (verifier/cota_verify.wasm)."""
    binary = Path(os.environ.get("BIN", ROOT / "target/release")) / "cota-ledger3"
    tmp = out / ".ledger-v3.json"
    out.mkdir(parents=True, exist_ok=True)
    subprocess.run([str(binary), "--ledger", str(ledger3), "export", "--out", str(tmp)], check=True, cwd=ROOT)
    led = json.loads(tmp.read_text())
    tmp.unlink()
    if led["genesis_index"] is None:
        raise SystemExit(f"{ledger3} ainda não tem registro de abertura")
    members = led["committee"]["members"]
    manifest = json.loads((ROOT / "ml/models/manifest.json").read_text())
    metrics = json.loads((ROOT / "ml/reports/metrics.json").read_text())
    live_stations = tomllib.loads((ROOT / "config/stations-live.toml").read_text())["station"]

    def provenance(e: dict) -> dict:
        return {
            "protocol": "v3",
            "index": e["index"],
            "leaf": e["leaf"],
            "checkpoint": e["checkpoint"],
            "signer_positions": e["signers"],
            "signers": sum(1 << p for p in e["signers"]),
        }

    entries = led["entries"]
    batches = [e for e in entries if e["kind"] == "batch" and e["body"]["stream"] == "guaiba"]
    # The page's folder holds only the v3 ledger's data (the Solana history is in archive/).
    import shutil

    for sub in ("batches", "public_alerts", "outlooks", "external_forecasts", "checkpoints", "timestamps"):
        shutil.rmtree(out / sub, ignore_errors=True)
    for sub in ("batches", "public_alerts", "outlooks"):
        (out / sub).mkdir(parents=True, exist_ok=True)
    # v2's leaves are the first leaves of the v3 accumulator (up to Genesis): the browser's check needs them.
    if (out / "archive" / "records-v2.json").exists():
        (out / "records-v2.json").write_bytes((out / "archive" / "records-v2.json").read_bytes())

    # Readings of every batch, and the implausible river levels judged over each gauge's whole series.
    files = {}
    gauges: dict[str, list[tuple[int, int]]] = {}
    for e in batches:
        b = e["body"]
        f = json.loads((ledger3 / "batches" / f"{b['stream']}-{b['batch_id']:05d}.json").read_text())
        files[b["batch_id"]] = f
        for r in f["readings"]:
            if r["variable"] == "river_stage":
                gauges.setdefault(r["station_id"], []).append((r["ts_utc"], r["value_milli"]))
    implausible = {(sid, t) for sid, pts in gauges.items() for t in stage_spikes(sid, sorted(pts))}
    gauges_now = {}
    for sid, pts in gauges.items():
        good = sorted(p for p in pts if (sid, p[0]) not in implausible)
        if good:
            t, v = good[-1]
            near = [pv for pt, pv in good if abs(pt - (t - 86_400)) <= 3 * 3600]
            gauges_now[sid] = {"ts": t, "value_milli": v, "change_24h_milli": v - near[0] if near else None}

    days, stage_series = [], []
    batch_index_of_day = {}
    for e in batches:
        b = e["body"]
        f = files[b["batch_id"]]
        q = f.get("quality") or {"stations": [], "dissents": []}
        day = day_of(b["t_start"])
        batch_index_of_day[day] = e["index"]
        name = f"v3-{b['stream']}-{b['batch_id']:05d}.json"
        (out / "batches" / name).write_text(json.dumps({"batch_id": b["batch_id"], "header": b, "readings": f["readings"]}, separators=(",", ":")))
        days.append(
            {
                "batch_id": b["batch_id"],
                "day": day,
                "t_start": b["t_start"],
                "t_end": b["t_end"],
                "leaf_count": b["leaf_count"],
                "merkle_root": b["merkle_root"],
                "prev_batch_root": b["prev_batch_root"],
                **provenance(e),
                "batch_file": f"batches/{name}",
                "members": [{"member": m, "status": "signed" if i in e["signers"] else "absent"} for i, m in enumerate(members)],
                **summarize_readings(f["readings"], implausible, stage_series),
                "quality": [
                    {
                        "station_id": st["station_id"],
                        "score": round(st["score"], 4),
                        "raw": round(st["raw"], 4),
                        "availability": round(st["availability"], 4),
                        "present": st["present"],
                        "expected": st["expected"],
                        "flags": [fl["reason"] for fl in st["flags"]],
                        "degraded": st["degraded"],
                    }
                    for st in q["stations"]
                ],
                "dissents": q["dissents"],
                "degradations": [],
            }
        )
    guaiba_of_day = {d["day"]: d["guaiba_milli"] for d in days}

    # Flow forecasts, one issue per day, with what happened after: the observation and whether the forecast's entry
    # comes before the batch that observed it (the ledger's order proves it was made first).
    issues: dict[str, dict] = {}
    for e in (x for x in entries if x["kind"] == "forecast"):
        f = e["body"]
        issue_day = day_of(f["target_ts"] - f["horizon_hours"] * 3600)
        target_day = day_of(f["target_ts"])
        run_path = ledger3 / "forecasts" / f"{issue_day}.json"
        run = json.loads(run_path.read_text()) if run_path.exists() else {}
        observed = guaiba_of_day.get(target_day)
        obs_index = batch_index_of_day.get(target_day)
        issue = issues.setdefault(
            issue_day,
            {
                "issue_day": issue_day,
                "issued_at": f["target_ts"] - f["horizon_hours"] * 3600 + 86_400,
                "based_on_batch_id": f["based_on_batch_id"],
                "based_on_root": f["based_on_root"],
                "model_sha256": f["model_sha256"],
                "features_root": f["features_root"],
                "features": run.get("features", []),
                "horizons": [],
            },
        )
        alert = (e["outcome"] or {}).get("alert")
        issue["horizons"].append(
            {
                "horizon_hours": f["horizon_hours"],
                "target_ts": f["target_ts"],
                "target_day": target_day,
                "p10": f["p10"],
                "p50": f["p50"],
                "p90": f["p90"],
                **provenance(e),
                "observed_milli": observed,
                "recorded_before_observation": None if obs_index is None else e["index"] < obs_index,
                "inside_p10_p90": None if observed is None else f["p10"] <= observed <= f["p90"],
                "alert": None if alert is None else {"threshold_milli": alert["threshold_milli"], "recorded_before_observation": None if obs_index is None else e["index"] < obs_index},
            }
        )
    issues_list = sorted(issues.values(), key=lambda i: i["issue_day"])

    alerts = []
    for e in (x for x in entries if x["kind"] == "public_alert"):
        a = e["body"]
        c = json.loads((ledger3 / "public_alerts" / f"{a['content_sha256']}.json").read_text())
        (out / "public_alerts" / f"{a['content_sha256']}.txt").write_text(c["content"], encoding="utf-8")
        alerts.append({**provenance(e), **{k: a[k] for k in ("alert_id", "issuer", "content_sha256", "severity", "onset", "expires")},
                       "severity_label": c.get("severity_label", ""), "description": c.get("description", ""), "states": c.get("states", "")})
    alerts.sort(key=lambda a: a["index"], reverse=True)

    batch_id_ending = {e["body"]["t_end"]: e["body"]["batch_id"] for e in batches}
    # The page shows the Guaíba's rules (basin and sub-basins); the rest of RS's (Etapa 5) wait for its region pages.
    guaiba_regions = {e["body"]["region"] for e in entries if e["kind"] == "rule_set" and e["body"].get("rule") == "outlook" and e["body"]["stream"] == "guaiba"}
    outlooks = []
    for e in (x for x in entries if x["kind"] == "outlook" and x["body"]["region"] in guaiba_regions):
        o = e["body"]
        day = day_of(o["issued_at"] - 1)
        c = json.loads((ledger3 / "outlooks" / f"{o['region']}-{day}.json").read_text())
        (out / "outlooks" / f"{o['content_sha256']}.txt").write_text(c["content"], encoding="utf-8")
        outlooks.append(
            {
                "day": day,
                "region": o["region"],
                **provenance(e),
                **{k: o[k] for k in ("issued_at", "window_end", "models", "rain_mean_milli", "rain_min_milli", "rain_max_milli", "content_sha256")},
                "threshold_milli": e["outcome"]["threshold_milli"],
                "triggered": e["outcome"]["triggered"],
                "based_on_batch_id": batch_id_ending.get(o["issued_at"]),
            }
        )
    outlooks.sort(key=lambda o: o["day"], reverse=True)

    # Third-party forecasts: their content files, committed by hash.
    ext = None
    src = ledger3 / "external_forecasts" / "glofas"
    ext_entries = {(day_of(e["body"]["issued_at"] - 1)): e for e in entries if e["kind"] == "external_forecast"}
    files_ext = sorted(src.glob("*.json")) if src.exists() else []
    if files_ext:
        dest = out / "external_forecasts" / "glofas"
        dest.mkdir(parents=True, exist_ok=True)
        history = []
        for f in files_ext:
            (dest / f.name).write_bytes(f.read_bytes())
            e = json.loads(f.read_text())
            c = json.loads(e["content"])
            outlet = next((p for p in c["points"] if p["id"] == TARGET), c["points"][0])
            day = f.stem
            entry = ext_entries.get(day)
            history.append(
                {
                    "day": day,
                    "quorum": True,
                    "signatures": len(entry["signers"]) if entry else 0,
                    "stamped": entry is not None and entry["checkpoint"] is not None,
                    "file": f"external_forecasts/glofas/{f.name}",
                    **(provenance(entry) if entry else {}),
                    "peak_median_milli": max((d["median"] for d in outlet["days"] if d["median"] is not None), default=None),
                    "peak_max_milli": max((d["max"] for d in outlet["days"] if d["max"] is not None), default=None),
                }
            )
        latest = json.loads(files_ext[-1].read_text())
        content = json.loads(latest["content"])
        threshold = next((e["body"]["threshold_milli"] for e in entries if e["kind"] == "rule_set" and e["body"].get("rule") == "flow_alert"), None)
        ext = {
            "source": "glofas",
            "provider": content["provider"],
            "latest": {**history[-1], "threshold": threshold, "content_sha256": latest["content_sha256"], "points": content["points"]},
            "history": history[-30:],
        }

    # Checkpoints and their Bitcoin proofs.
    cps = []
    cp_dir = ledger3 / "checkpoints"
    if cp_dir.exists():
        (out / "checkpoints").mkdir(exist_ok=True)
        for f in sorted(cp_dir.glob("*.json")):
            proof = f.with_name(f.name + ".ots")
            for g in (f, proof):
                if g.exists():
                    (out / "checkpoints" / g.name).write_bytes(g.read_bytes())
            c = json.loads(f.read_text())
            cps.append({"size": c["size"], "root": c["root"], "signed_at": c["signed_at"], "epoch": c["epoch"], "signers": len(c["signatures"]),
                        "file": f"checkpoints/{f.name}", "stamped": proof.exists(), "bitcoin": proof.exists() and BITCOIN_TAG in proof.read_bytes()})

    test = metrics["test_2024"]
    bundle = {
        "meta": {
            "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
            "protocol": "v3",
            "ledger": {k: led[k] for k in ("genesis_index", "size", "root", "clock", "epoch")},
            "rpc": RPC,
            "members": members,
            "threshold": led["committee"]["threshold"],
            "target": manifest["target"],
            "model_sha256": manifest["model_sha256"],
            "alert_threshold_milli": manifest["alert_rule"]["threshold_milli"],
            "train_years": manifest["train_years"],
            # The Solana history (v2, and v1 inside it), for the archive page.
            "archive": "archive/bundle.json",
        },
        "metrics": {
            "test": {h: {k: test[h][k] for k in ("n", "crps", "coverage_p10_p90", "mean_width", "nse_p50", "nse_persistence", "kge_p50", "alerts")} for h in test},
            "episodes": metrics["episodes"],
            "conformal_margin_ln": metrics["conformal_margin_ln"],
        },
        "stations": [],
        "days": [],
        "issues": [],
        "live": {
            "days": days,
            "issues": issues_list,
            "public_alerts": alerts,
            "stage_stations": [{"id": st["id"], "name": st["name"]} for st in live_stations if st["source"] == "ana"],
            "stations": [{k: st[k] for k in ("id", "name", "source", "variable", "lat", "lon")} for st in live_stations],
            "stage_main": STAGE_MAIN,
            "stage_series": stage_series_tail(stage_series),
            "locations": export_locations(gauges_now),
        },
        "outlook": export_outlook(outlooks, [{"outlook_rule_account": None}]),
        "external_forecasts": ext,
        "checkpoints": cps,
        "timestamps": [],
        "archive": None,
        "upgrade": None,
        "vaults": [],
    }
    (out / "bundle.json").write_text(json.dumps(bundle, separators=(",", ":"), ensure_ascii=False))
    print(
        f"v3 ({ledger3}): {len(days)} lotes, {len(issues_list)} previsões, {len(alerts)} avisos oficiais, "
        f"{len(outlooks)} perspectivas de chuva, {len(cps)} pontos de controle → {out}"
    )


if __name__ == "__main__":
    main()
