"""Daily table and features for the Guaíba discharge forecast.

The target is the daily discharge modelled by GloFAS at the Guaíba point, 1 and 2 days ahead
(the briefing: without ANA, forecast daily discharge 1 and 2 days ahead). The model predicts the
log change ln Q(D+h) − ln Q(D), not the absolute value: trees do not extrapolate beyond the
training range, but a relative change can lead to discharges never seen before.

Training and forecasting use exactly these functions; the only difference is where the
`Reading`s come from (historical ingestion or anchored batches).
"""

from __future__ import annotations

import math

import polars as pl

TARGET = "GLOFAS-GUAIBA"
UPSTREAM = ["GLOFAS-JACUI", "GLOFAS-TAQUARI", "GLOFAS-CAI", "GLOFAS-SINOS", "GLOFAS-GRAVATAI"]
RAIN_STATIONS = ["A801", "B807", "A840", "A879", "A882", "A837"]
HORIZONS = [1, 2]  # days
QUANTILES = [0.1, 0.5, 0.9]
# A day of rain only counts if it has at least this many observed hours.
MIN_RAIN_HOURS = 20

FEATURES = (
    ["lq0", "dlq1", "dlq2"]
    + [f"up_{s.removeprefix('GLOFAS-').lower()}_rel" for s in UPSTREAM]
    + [f"up_{s.removeprefix('GLOFAS-').lower()}_d1" for s in UPSTREAM]
    + ["rain0", "rain1", "rain3", "rain5", "rainmax0", "doy_sin", "doy_cos"]
)


def daily_table(readings: pl.DataFrame) -> pl.DataFrame:
    """One row per UTC day: discharge at each GloFAS point and daily rain at each station."""
    df = readings.with_columns(
        day=pl.from_epoch("ts_utc").dt.date(),
        value=pl.col("value_milli") / 1000,
    )
    q = (
        df.filter(pl.col("variable") == "river_discharge")
        .pivot(on="station_id", index="day", values="value", aggregate_function="first")
    )
    rain = (
        df.filter(pl.col("variable") == "precipitation")
        .group_by("day", "station_id")
        .agg(pl.col("value").sum().alias("mm"), pl.len().alias("hours"))
        .with_columns(pl.when(pl.col("hours") >= MIN_RAIN_HOURS).then(pl.col("mm")).alias("mm"))
        .pivot(on="station_id", index="day", values="mm", aggregate_function="first")
        .rename(lambda c: c if c == "day" else f"rain_{c}")
    )
    first, last = df["day"].min(), df["day"].max()
    days = pl.DataFrame({"day": pl.date_range(first, last, "1d", eager=True)})
    out = days.join(q, on="day", how="left").join(rain, on="day", how="left")
    for s in [TARGET, *UPSTREAM]:
        if s not in out.columns:
            out = out.with_columns(pl.lit(None, dtype=pl.Float64).alias(s))
    for s in RAIN_STATIONS:
        if f"rain_{s}" not in out.columns:
            out = out.with_columns(pl.lit(None, dtype=pl.Float64).alias(f"rain_{s}"))
    return out.sort("day")


def _ln(c: str) -> pl.Expr:
    return pl.col(c).log()


def feature_table(daily: pl.DataFrame) -> pl.DataFrame:
    """Features for issue day D (using only data up to the end of D) and targets at D+h."""
    rain_cols = [f"rain_{s}" for s in RAIN_STATIONS]
    d = daily.with_columns(
        rain_mean=pl.mean_horizontal(rain_cols),
        rain_max=pl.max_horizontal(rain_cols),
    )
    exprs = [
        _ln(TARGET).alias("lq0"),
        (_ln(TARGET) - _ln(TARGET).shift(1)).alias("dlq1"),
        (_ln(TARGET).shift(1) - _ln(TARGET).shift(2)).alias("dlq2"),
    ]
    for s in UPSTREAM:
        k = s.removeprefix("GLOFAS-").lower()
        exprs.append((_ln(s) - _ln(TARGET)).alias(f"up_{k}_rel"))
        exprs.append((_ln(s) - _ln(s).shift(1)).alias(f"up_{k}_d1"))
    # Rolling sums require every day in the window; one missing day makes the feature null.
    exprs += [
        pl.col("rain_mean").alias("rain0"),
        pl.col("rain_mean").shift(1).alias("rain1"),
        pl.col("rain_mean").rolling_sum(3).alias("rain3"),
        pl.col("rain_mean").rolling_sum(5).alias("rain5"),
        pl.col("rain_max").alias("rainmax0"),
        (2 * math.pi * pl.col("day").dt.ordinal_day() / 365.25).sin().alias("doy_sin"),
        (2 * math.pi * pl.col("day").dt.ordinal_day() / 365.25).cos().alias("doy_cos"),
    ]
    for h in HORIZONS:
        exprs.append((_ln(TARGET).shift(-h) - _ln(TARGET)).alias(f"y{h}"))
        exprs.append(pl.col(TARGET).shift(-h).alias(f"q{h}"))
    out = d.select("day", pl.col(TARGET).alias("q0"), *exprs)
    # Exactly the values that are committed on-chain (×1000 scale): the model
    # trains and predicts with the same numbers anyone can reproduce.
    return out.with_columns([(pl.col(f) * 1000).round().cast(pl.Int64).alias(f"{f}_milli") for f in FEATURES]).with_columns(
        [(pl.col(f"{f}_milli") / 1000).alias(f) for f in FEATURES]
    )

