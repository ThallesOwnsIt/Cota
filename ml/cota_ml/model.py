"""Quantile LightGBM models: one per quantile and per horizon."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

import lightgbm as lgb
import numpy as np
import polars as pl

from .features import FEATURES, HORIZONS, QUANTILES

# deterministic + 1 thread: the same history produces the same model byte for byte,
# so anyone can reproduce `model_sha256`.
PARAMS = {
    "objective": "quantile",
    "learning_rate": 0.03,
    "num_leaves": 15,
    "min_data_in_leaf": 20,
    "feature_fraction": 0.9,
    "lambda_l2": 1.0,
    "deterministic": True,
    "force_row_wise": True,
    "num_threads": 1,
    "seed": 7,
    "verbose": -1,
}
ROUNDS = 400


def file_name(h: int, tau: float) -> str:
    return f"h{h}_q{round(tau * 100):02d}.txt"


def train(table: pl.DataFrame, horizons: list[int] = HORIZONS) -> dict[tuple[int, float], lgb.Booster]:
    models = {}
    for h in horizons:
        rows = table.filter(pl.col(f"y{h}").is_not_null() & pl.col("lq0").is_not_null())
        x = rows.select(FEATURES).to_numpy()
        y = rows[f"y{h}"].to_numpy()
        for tau in QUANTILES:
            ds = lgb.Dataset(x, y, feature_name=FEATURES, free_raw_data=False)
            models[(h, tau)] = lgb.train({**PARAMS, "alpha": tau}, ds, num_boost_round=ROUNDS)
    return models


def predict_ln(models: dict[tuple[int, float], lgb.Booster], table: pl.DataFrame, h: int, widen: float = 0.0) -> np.ndarray:
    """Quantiles of ln Q(D+h) − ln Q(D), rows = QUANTILES.

    Sorts them to fix crossings and widens P10/P90 by the conformal margin `widen`.
    """
    x = table.select(FEATURES).to_numpy()
    raw = np.stack([models[(h, tau)].predict(x) for tau in QUANTILES])
    raw.sort(axis=0)
    raw[0] -= widen
    raw[-1] += widen
    return raw


def predict(models: dict[tuple[int, float], lgb.Booster], table: pl.DataFrame, h: int, widen: float = 0.0) -> dict[float, np.ndarray]:
    """Discharge quantiles (m³/s) at D+h."""
    raw = predict_ln(models, table, h, widen)
    q0 = table["q0"].to_numpy()
    return {tau: q0 * np.exp(raw[i]) for i, tau in enumerate(QUANTILES)}


def conformal_margin(table: pl.DataFrame, years: list[int], h: int, coverage: float, fold_years: int = 3) -> float:
    """CQR margin (Romano et al., 2019) with folds made of blocks of years.

    Each block is predicted by a model trained without it; each day's score is
    max(P10 − y, y − P90) in ln. The margin is the quantile that would give `coverage` out of sample.
    """
    scores = []
    for i in range(0, len(years), fold_years):
        held = years[i : i + fold_years]
        in_fold = pl.col("day").dt.year().is_in(held)
        m = train(table.filter(~in_fold), horizons=[h])
        rows = table.filter(in_fold & pl.col(f"y{h}").is_not_null() & pl.col("lq0").is_not_null())
        raw = predict_ln(m, rows, h)
        y = rows[f"y{h}"].to_numpy()
        scores.append(np.maximum(raw[0] - y, y - raw[-1]))
    s = np.concatenate(scores)
    level = min(1.0, np.ceil((len(s) + 1) * coverage) / len(s))
    return float(np.quantile(s, level, method="higher"))


CALIBRATION_FILE = "calibration.json"


def save(models: dict[tuple[int, float], lgb.Booster], widen: dict[int, float], out: Path, extra: dict) -> dict:
    out.mkdir(parents=True, exist_ok=True)
    files = []
    whole = hashlib.sha256()
    for h in HORIZONS:
        for tau in QUANTILES:
            name = file_name(h, tau)
            text = models[(h, tau)].model_to_string().encode()
            (out / name).write_bytes(text)
            files.append({"file": name, "horizon_days": h, "quantile": tau, "sha256": hashlib.sha256(text).hexdigest()})
            whole.update(text)
    # Conformal margins are part of the model: they go into the same hash.
    text = (json.dumps({f"h{h}": round(w, 6) for h, w in widen.items()}, sort_keys=True) + "\n").encode()
    (out / CALIBRATION_FILE).write_bytes(text)
    files.append({"file": CALIBRATION_FILE, "sha256": hashlib.sha256(text).hexdigest()})
    whole.update(text)
    manifest = {
        # SHA-256 of the concatenated files (models + calibration), in the order of `files`.
        "model_sha256": whole.hexdigest(),
        "features": FEATURES,
        "horizons_days": HORIZONS,
        "quantiles": QUANTILES,
        "params": {**PARAMS, "num_boost_round": ROUNDS},
        "files": files,
        **extra,
    }
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n")
    return manifest


def load(out: Path) -> tuple[dict[tuple[int, float], lgb.Booster], dict[int, float], dict]:
    """Loads each file and checks it against the manifest and `model_sha256`."""
    manifest = json.loads((out / "manifest.json").read_text())
    whole = hashlib.sha256()
    models, widen = {}, {}
    for f in manifest["files"]:
        text = (out / f["file"]).read_bytes()
        if hashlib.sha256(text).hexdigest() != f["sha256"]:
            raise ValueError(f"{f['file']} não corresponde ao manifesto")
        whole.update(text)
        if f["file"] == CALIBRATION_FILE:
            widen = {int(k[1:]): v for k, v in json.loads(text).items()}
        else:
            models[(f["horizon_days"], f["quantile"])] = lgb.Booster(model_str=text.decode())
    if whole.hexdigest() != manifest["model_sha256"]:
        raise ValueError("model_sha256 não corresponde aos arquivos")
    return models, widen, manifest
