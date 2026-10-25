import json
from datetime import date
from pathlib import Path

import polars as pl
import pytest

from cota_ml import model
from cota_ml.canonical import feature_leaf, hash_leaf, hash_node, merkle_root, reading_bytes
from cota_ml.features import FEATURES, daily_table, feature_table

ROOT = Path(__file__).resolve().parents[2]
LEDGER = ROOT / "data/ledger/batches"
REPLAY = ROOT / "data/readings/replay-2024/readings.jsonl"


def test_merkle_shape_matches_rfc9162():
    a, b, c = (hash_leaf(x) for x in (b"a", b"b", b"c"))
    assert merkle_root([b"a"]) == a
    assert merkle_root([b"a", b"b", b"c"]) == hash_node(hash_node(a, b), c)


@pytest.mark.skipif(not LEDGER.exists(), reason="sem ledger local")
def test_python_reproduces_anchored_roots():
    for path in sorted(LEDGER.glob("*.json")):
        e = json.loads(path.read_text())
        assert merkle_root([reading_bytes(r) for r in e["readings"]]).hex() == e["header"]["merkle_root"]


def test_feature_leaf_format():
    assert feature_leaf("rain0", 1500) == b"rain0=1500"
    assert feature_leaf("rain5", None) == b"rain5=null"


@pytest.mark.skipif(not REPLAY.exists(), reason="sem leituras do replay")
def test_features_do_not_look_ahead():
    """As features do dia D não podem mudar quando os dados depois de D são removidos."""
    readings = pl.read_ndjson(REPLAY)
    day = date(2024, 5, 3)
    cutoff = 1714780800  # 2024-05-04T00:00Z
    full = feature_table(daily_table(readings)).filter(pl.col("day") == day)
    past = feature_table(daily_table(readings.filter(pl.col("ts_utc") < cutoff))).filter(pl.col("day") == day)
    assert full.select(FEATURES).rows() == past.select(FEATURES).rows()
    # ... e os alvos de fato vêm do futuro.
    assert full["q1"][0] is not None and past["q1"][0] is None


@pytest.mark.skipif(not (ROOT / "ml/models/manifest.json").exists(), reason="modelos não treinados")
def test_tampered_model_is_rejected(tmp_path):
    src = ROOT / "ml/models"
    for f in src.iterdir():
        (tmp_path / f.name).write_bytes(f.read_bytes())
    models, widen, manifest = model.load(tmp_path)
    assert set(widen) == {1, 2}
    target = tmp_path / manifest["files"][0]["file"]
    target.write_bytes(target.read_bytes().replace(b"learning_rate", b"learning_ratf", 1))
    with pytest.raises(ValueError):
        model.load(tmp_path)


def test_outlook_rule_hash_and_no_lookahead():
    """O hash publicado é o do JSON canônico da regra, e o backtest só usa rodadas ≥ (k+1) dias antes."""
    import hashlib
    import inspect
    import json

    from cota_ml import outlook

    rule = json.loads((outlook.ROOT / "ml/models/outlook-rule.json").read_text())
    assert hashlib.sha256(outlook.canonical(rule["spec"])).hexdigest() == rule["rule_sha256"]
    assert rule["spec"]["calibration"]["years"][1] < 2024, "a calibração não pode ver o período do backtest"
    src = inspect.getsource(outlook.archived_forecasts)
    assert 'precipitation_previous_day{k + 1}' in src and "range(2, WINDOW_DAYS + 2)" in src
