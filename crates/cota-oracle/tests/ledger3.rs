//! The v3 round's tools against real files.

#[test]
fn features_root_matches_the_model_on_a_real_forecast() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/ledger/forecasts/2026-11-11.json");
    let f: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let root = cota_oracle::ledger3::features_root(&f["features"]).unwrap();
    assert_eq!(hex::encode(root), f["features_root"].as_str().unwrap());
}
