"""Tabela diária e features da previsão de vazão do Guaíba.

O alvo é a vazão diária modelada pelo GloFAS no ponto do Guaíba, 1 e 2 dias à frente
(o briefing: sem ANA, prever vazão diária a 1 e 2 dias). O modelo prevê a variação
logarítmica ln Q(D+h) − ln Q(D), não o valor absoluto: árvores não extrapolam além da
faixa de treino, mas uma variação relativa pode levar a vazões nunca vistas.

Treino e previsão usam exatamente estas funções; a única diferença é de onde vêm os
`Reading`s (ingestão histórica ou lotes ancorados).
"""

from __future__ import annotations

import math

import polars as pl

TARGET = "GLOFAS-GUAIBA"
UPSTREAM = ["GLOFAS-JACUI", "GLOFAS-TAQUARI", "GLOFAS-CAI", "GLOFAS-SINOS", "GLOFAS-GRAVATAI"]
RAIN_STATIONS = ["A801", "B807", "A840", "A879", "A882", "A837"]
HORIZONS = [1, 2]  # dias
QUANTILES = [0.1, 0.5, 0.9]
# Um dia de chuva só conta se tiver pelo menos esta quantidade de horas observadas.
MIN_RAIN_HOURS = 20

FEATURES = (
    ["lq0", "dlq1", "dlq2"]
    + [f"up_{s.removeprefix('GLOFAS-').lower()}_rel" for s in UPSTREAM]
    + [f"up_{s.removeprefix('GLOFAS-').lower()}_d1" for s in UPSTREAM]
    + ["rain0", "rain1", "rain3", "rain5", "rainmax0", "doy_sin", "doy_cos"]
)


def daily_table(readings: pl.DataFrame) -> pl.DataFrame:
    """Uma linha por dia UTC: vazão de cada ponto GloFAS e chuva diária de cada estação."""
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
    """Features do dia de emissão D (usam só dados até o fim de D) e alvos em D+h."""
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
    # Somas móveis exigem todos os dias da janela; um faltante deixa a feature nula.
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
    # Exatamente os valores que são comprometidos on-chain (escala ×1000): o modelo
    # treina e prevê com os mesmos números que qualquer um pode reproduzir.
    return out.with_columns([(pl.col(f) * 1000).round().cast(pl.Int64).alias(f"{f}_milli") for f in FEATURES]).with_columns(
        [(pl.col(f"{f}_milli") / 1000).alias(f) for f in FEATURES]
    )

