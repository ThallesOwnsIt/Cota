"""Treina, avalia e exporta os modelos quantílicos.

    uv run python -m cota_ml.train

1. Validação: treina até 2022 e avalia em 2023, para escolher o início do histórico.
2. Final: treina até 2023 e testa em abr–mai/2024 (dados do replay, nunca vistos).
3. Exporta modelos + manifesto (model_sha256, limiar de alerta) e os limites de salto
   que as regras de qualidade em Rust usam.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

import numpy as np
import polars as pl

from . import metrics, model
from .features import FEATURES, HORIZONS, QUANTILES, TARGET, UPSTREAM, daily_table, feature_table

ROOT = Path(__file__).resolve().parents[2]
ALERT_QUANTILE = 0.99
JUMP_QUANTILE = 0.999
TARGET_COVERAGE = 0.8
RAIN_FEATURES = ["rain0", "rain1", "rain3", "rain5", "rainmax0"]
SCENARIOS = [0.10, 0.20, 0.30]


def load_readings(*dirs: Path) -> pl.DataFrame:
    return pl.concat([pl.read_ndjson(d / "readings.jsonl") for d in dirs], how="vertical_relaxed")


def evaluate(models, table: pl.DataFrame, threshold: float, widen: dict[int, float] | None = None) -> dict:
    out = {}
    for h in HORIZONS:
        rows = table.filter(pl.col(f"q{h}").is_not_null() & pl.col("lq0").is_not_null())
        obs = rows[f"q{h}"].to_numpy()
        preds = model.predict(models, rows, h, (widen or {}).get(h, 0.0))
        alert = preds[0.9] >= threshold
        event = obs >= threshold
        out[f"h{h}"] = {
            "n": len(obs),
            "pinball": {str(t): round(metrics.pinball(obs, preds[t], t), 1) for t in QUANTILES},
            "crps": round(metrics.crps_approx(obs, preds), 1),
            "coverage_p10_p90": round(metrics.coverage(obs, preds[0.1], preds[0.9]), 3),
            "mean_width": round(float(np.mean(preds[0.9] - preds[0.1])), 1),
            "nse_p50": round(metrics.nse(obs, preds[0.5]), 3),
            "kge_p50": round(metrics.kge(obs, preds[0.5]), 3),
            "nse_persistence": round(metrics.nse(obs, rows["q0"].to_numpy()), 3),
            "alerts": {k: (round(v, 3) if isinstance(v, float) else v) for k, v in metrics.contingency(alert, event).items()},
        }
    return out


def lead_times(models, table: pl.DataFrame, threshold: float, widen: dict[int, float]) -> list[dict]:
    """Para cada episódio (dias seguidos com Q ≥ limiar): antecedência do primeiro alerta
    que apontava para o dia do cruzamento.

    Emissão = fim do dia D (quando os dados de D foram ancorados); o cruzamento é conhecido
    no fim do primeiro dia do episódio. Antecedência = diferença entre os dois.
    """
    alerts_for = {}  # dia-alvo -> dias de emissão com P90 ≥ limiar
    rows = table.filter(pl.col("lq0").is_not_null())
    for h in HORIZONS:
        p90 = model.predict(models, rows, h, widen[h])[0.9]
        for d, v in zip(rows["day"].to_list(), p90):
            if v >= threshold:
                alerts_for.setdefault(d + timedelta(days=h), []).append(d)
    q = table.select("day", "q0").drop_nulls()
    days, above = q["day"].to_list(), (q["q0"] >= threshold).to_list()
    episodes = []
    for i, d in enumerate(days):
        if above[i] and (i == 0 or not above[i - 1]):
            first = min(alerts_for.get(d, []), default=None)
            episodes.append(
                {
                    "crossing_day": d.isoformat(),
                    "first_alert_issued_end_of": first.isoformat() if first else None,
                    "lead_hours": (d - first).days * 24 if first else None,
                }
            )
    return episodes


def scenarios(models, table: pl.DataFrame, threshold: float, widen: dict[int, float], model_sha: str) -> list[dict]:
    """Delta change na chuva (P' = P·(1+δ)). Nunca são observações: cada um tem id e hash."""
    out = []
    for delta in SCENARIOS:
        t = table.with_columns([(pl.col(c) * (1 + delta)).alias(c) for c in RAIN_FEATURES])
        rows = []
        for h in HORIZONS:
            sub = t.filter(pl.col("lq0").is_not_null())
            base = model.predict(models, sub, h, widen[h])
            for d, p10, p50, p90 in zip(sub["day"].to_list(), base[0.1], base[0.5], base[0.9]):
                rows.append({"issue_day": d.isoformat(), "horizon_days": h, "p10": round(p10, 1), "p50": round(p50, 1), "p90": round(p90, 1)})
        sid = f"delta-rain-{round(delta * 100)}"
        body = json.dumps({"scenario_id": sid, "model_sha256": model_sha, "rows": rows}, sort_keys=True).encode()
        out.append(
            {
                "scenario_id": sid,
                "delta": delta,
                "sha256": hashlib.sha256(body).hexdigest(),
                "alerts": int(sum(r["p90"] >= threshold for r in rows)),
                "max_p90": max(r["p90"] for r in rows),
                "rows": rows,
            }
        )
    return out


def jump_limits(daily: pl.DataFrame) -> dict[str, float]:
    """r_max de cada ponto GloFAS: percentil 99,9 de |Δ ln Q| diário no histórico."""
    return {
        s: round(float(daily.select((pl.col(s).log() - pl.col(s).log().shift(1)).abs()).drop_nulls().quantile(JUMP_QUANTILE).item()), 3)
        for s in [TARGET, *UPSTREAM]
    }


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--history", nargs="+", type=Path, default=[ROOT / "data/readings/history-glofas", ROOT / "data/readings/history-inmet"])
    ap.add_argument("--test", type=Path, default=ROOT / "data/readings/replay-2024")
    ap.add_argument("--out", type=Path, default=ROOT / "ml/models")
    ap.add_argument("--reports", type=Path, default=ROOT / "ml/reports")
    args = ap.parse_args()

    hist_daily = daily_table(load_readings(*args.history))
    hist = feature_table(hist_daily)
    threshold = float(hist_daily[TARGET].drop_nulls().quantile(ALERT_QUANTILE))
    print(f"limiar de alerta (P{ALERT_QUANTILE * 100:g} de Q {TARGET}, 1997–2023): {threshold:.0f} m³/s")

    # 1. Validação em 2023 para escolher o início do histórico.
    valid = hist.filter(pl.col("day").dt.year() == 2023)
    choice = {}
    for since in (1997, 2015):
        m = model.train(hist.filter((pl.col("day").dt.year() >= since) & (pl.col("day").dt.year() <= 2022)))
        res = evaluate(m, valid, threshold)
        choice[since] = res
        print(f"validação 2023, treino {since}–2022: " + " | ".join(
            f"h{h}: CRPS {res[f'h{h}']['crps']}, cobertura {res[f'h{h}']['coverage_p10_p90']}, NSE {res[f'h{h}']['nse_p50']}" for h in HORIZONS
        ))
    since = min(choice, key=lambda s: sum(choice[s][f"h{h}"]["crps"] for h in HORIZONS))
    print(f"→ histórico a partir de {since}")

    # 2. Margem conformal (folds de 3 anos, fora da amostra), modelo final e teste em abr–mai/2024.
    train_rows = hist.filter((pl.col("day").dt.year() >= since) & (pl.col("day").dt.year() <= 2023))
    years = list(range(since, 2024))
    widen = {h: model.conformal_margin(train_rows, years, h, TARGET_COVERAGE) for h in HORIZONS}
    print("margem conformal (ln):", {h: round(w, 4) for h, w in widen.items()})
    final = model.train(train_rows)
    test = feature_table(daily_table(load_readings(args.test))).filter(pl.col("day") >= date(2024, 4, 25))
    res_raw = evaluate(final, test, threshold)
    res_test = evaluate(final, test, threshold, widen)
    print("sem calibração:", {k: v["coverage_p10_p90"] for k, v in res_raw.items()})
    for h in HORIZONS:
        r = res_test[f"h{h}"]
        print(f"teste 2024 h{h} (n={r['n']}): CRPS {r['crps']}, cobertura P10–P90 {r['coverage_p10_p90']}, "
              f"largura {r['mean_width']}, NSE P50 {r['nse_p50']} (persistência {r['nse_persistence']}), KGE {r['kge_p50']}, "
              f"POD {r['alerts']['pod']} FAR {r['alerts']['far']} CSI {r['alerts']['csi']}")

    # 3. Exportação.
    manifest = model.save(
        final,
        widen,
        args.out,
        {
            "target": {"station_id": TARGET, "variable": "river_discharge", "unit": "m3/s",
                       "note": "vazão diária modelada pelo GloFAS (Open-Meteo), não nível de régua"},
            "train_years": [since, 2023],
            "alert_rule": {"station_id": TARGET, "quantile": ALERT_QUANTILE, "threshold_milli": round(threshold * 1000), "level": 1},
        },
    )
    print(f"model_sha256 {manifest['model_sha256']}")

    args.reports.mkdir(parents=True, exist_ok=True)
    episodes = lead_times(final, test, threshold, widen)
    scen = scenarios(final, test, threshold, widen, manifest["model_sha256"])
    report = {
        "generated_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "model_sha256": manifest["model_sha256"],
        "threshold_m3s": round(threshold, 1),
        "validation_2023": {str(k): v for k, v in choice.items()},
        "history_since": since,
        "conformal_margin_ln": {f"h{h}": round(w, 6) for h, w in widen.items()},
        "test_2024_uncalibrated": res_raw,
        "test_2024": res_test,
        "episodes": episodes,
        "scenarios": [{k: v for k, v in s.items() if k != "rows"} for s in scen],
    }
    (args.reports / "metrics.json").write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
    (args.reports / "scenarios.json").write_text(json.dumps(scen, indent=2) + "\n")
    print("episódios:", episodes)
    for s in report["scenarios"]:
        print(f"cenário {s['scenario_id']}: {s['alerts']} alertas, P90 máx {s['max_p90']:.0f}")

    limits = jump_limits(hist_daily)
    lines = [
        "# Gerado por `uv run python -m cota_ml.train`. Não editar à mão.",
        f"# r_max = percentil {JUMP_QUANTILE * 100:g} de |Δ ln Q| diário (histórico até 2023).",
        "",
        "[jump_ln]",
        *[f'"{k}" = {v}' for k, v in limits.items()],
        "",
    ]
    (ROOT / "config/quality-limits.toml").write_text("\n".join(lines))
    print("limites de salto:", limits)


if __name__ == "__main__":
    main()
