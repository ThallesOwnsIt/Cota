//! The v3 round's tools against real files.

#[test]
fn features_root_matches_the_model_on_a_real_forecast() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/ledger/forecasts/2026-11-11.json");
    let f: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let root = cota_oracle::ledger3::features_root(&f["features"]).unwrap();
    assert_eq!(hex::encode(root), f["features_root"].as_str().unwrap());
}

/// The catalogs of the whole state (Etapa 5): every station goes through the ledger's own rules (ids, name and datum
/// sizes, an existing stream), and the 20 stations the ledger opened with stay byte for byte as they were, so they
/// are not registered again.
#[test]
fn every_catalog_station_is_accepted_by_the_ledger_and_the_opening_ones_do_not_change() {
    use cota_ledger::*;
    use ed25519_dalek::{Signer as _, SigningKey};
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    std::env::set_current_dir(root).unwrap();
    let cfg = cota_oracle::ledger3::load_config(std::path::Path::new("config/ledger-v3.toml")).unwrap();
    assert!(!cfg.regional_catalogs.is_empty());
    let stations = cota_oracle::ledger3::catalog_stations(&cfg).unwrap();

    let keys: Vec<SigningKey> = (1..=3u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    let sign = |body: Body| {
        let msg = body.signing_message();
        let signatures = (0..2).map(|p| Signature { member: p as u16, signature: keys[p].sign(&msg).to_bytes() }).collect();
        Entry { body, quorum: Quorum { committee: MAIN_COMMITTEE, epoch: 0, signatures } }
    };
    let mut ledger = Ledger::new();
    let start = 1_795_132_800;
    let genesis = Genesis {
        v2_program: [1; 32],
        v2_registry: [2; 32],
        v2_leaf_count: 0,
        v2_root: [0; 32],
        v1_archive_sha256: [3; 32],
        members: keys.iter().map(|k| k.verifying_key().to_bytes()).collect(),
        threshold: 2,
        cutover_at: start,
    };
    ledger.apply(&sign(Body::Genesis(genesis))).unwrap();
    for s in &cfg.streams {
        ledger.apply(&sign(Body::StreamCreate(StreamCreate { id: s.clone(), starts_at: start }))).unwrap();
    }
    let mut per_stream = std::collections::BTreeMap::<String, usize>::new();
    for s in &stations {
        ledger.apply(&sign(Body::Station(s.clone()))).unwrap_or_else(|e| panic!("{}: {e:?}", s.station_id));
        *per_stream.entry(s.stream.clone()).or_default() += 1;
    }
    assert!(per_stream["guaiba"] > 200 && per_stream["uruguai"] > 150 && per_stream["litoranea"] > 30, "{per_stream:?}");

    // The opening's stations, as the ledger on the repository has them (the real one after the cutover, the rehearsal
    // before): nothing to sign again for them.
    let dir = ["data/ledger-v3", "data/ledger-v3-ensaio"].into_iter().map(std::path::Path::new).find(|d| d.join("entries.jsonl").is_file());
    if let Some(dir) = dir {
        let book = cota_oracle::ledger3::Book::open(dir, std::path::Path::new("data/ledger")).unwrap();
        assert!(!book.stations.is_empty());
        let again: Vec<_> = cota_oracle::ledger3::station_bodies(&cfg, &book)
            .unwrap()
            .into_iter()
            .filter_map(|b| match b {
                Body::Station(s) if book.stations.contains_key(&s.station_id) => Some(s.station_id),
                _ => None,
            })
            .collect();
        assert!(again.is_empty(), "estações da abertura assinadas de novo: {again:?}");
    }
}

/// Each stream's batch carries its own stations (every station in exactly one), and its files go to its own folder:
/// the main stream's where v2 put them, the others in a subfolder.
#[test]
fn each_stream_has_its_stations_and_its_folder() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    std::env::set_current_dir(root).unwrap();
    let cfg = cota_oracle::ledger3::load_config(std::path::Path::new("config/ledger-v3.toml")).unwrap();
    let all = cfg.stations().unwrap();
    let mut total = 0;
    for stream in &cfg.streams {
        let st = cfg.stream_stations(stream).unwrap();
        assert!(!st.is_empty(), "{stream}");
        assert!(st.iter().all(|s| cfg.stream_of(s) == stream));
        total += st.len();
    }
    assert_eq!(total, all.len());
    let main = cfg.stream_stations(&cfg.main_stream).unwrap();
    assert!(main.iter().any(|s| s.id == "ANA-87450004") && main.iter().any(|s| s.id == "GLOFAS-GUAIBA"));
    assert!(cfg.stream_stations("uruguai").unwrap().iter().any(|s| s.id == "ANA-77150000"));
    assert!(cfg.stream_stations("nada").is_err());
    let node = std::path::Path::new("data/nodes/node-1");
    assert_eq!(cfg.stream_dir(node, "guaiba"), node);
    assert_eq!(cfg.stream_dir(node, "uruguai"), node.join("uruguai"));
}
