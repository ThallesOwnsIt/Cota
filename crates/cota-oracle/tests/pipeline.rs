//! Full pipeline without a network: nodes sign → aggregator picks → real instructions in LiteSVM (program
//! v2) → records read back from the logs into `records.jsonl` → audit-style rebuild and a vault payout
//! with a proof built from the ledger.

use anchor_lang::prelude::Pubkey;
use chrono::NaiveDate;
use cota_core::{Batch, Hash, Reading, Source, Variable};
use cota_oracle::{
    aggregate::{aggregate, NodeStatus},
    chain::{self, Record},
    keys::pubkey_base58,
    node::SignedBatch,
    quality, records, PROTOCOL,
};
use cota_quality::{Limits, Reason};
use ed25519_dalek::{Signer as _, SigningKey};
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::Transaction;

const DAY: i64 = 86_400;

fn readings(day: i64, tamper: bool) -> Vec<Reading> {
    (0..24)
        .map(|h| Reading {
            station_id: "A801".into(),
            variable: Variable::Precipitation,
            ts_utc: day * DAY + h * 3600,
            source: Source::Inmet,
            value_milli: if tamper && h == 3 { 99_000 } else { h * 100 },
            datum_id: None,
            raw_payload_sha256: [h as u8; 32],
        })
        .collect()
}

fn sign(key: &SigningKey, batch_id: u64, day: i64, prev: Hash, tamper: bool) -> SignedBatch {
    let b = Batch::build(batch_id, day * DAY, (day + 1) * DAY, prev, readings(day, tamper)).unwrap();
    SignedBatch {
        node: pubkey_base58(key),
        day: NaiveDate::from_ymd_opt(2024, 5, 1).unwrap(),
        signature: key.sign(&b.header.signing_message_for(PROTOCOL)).to_bytes(),
        header: b.header,
        readings: b.readings,
        ingest: vec![],
    }
}

/// Sends like the RPC client does and stores the appended records exactly as `records::send` would.
fn send(svm: &mut LiteSVM, payer: &Keypair, ledger: &std::path::Path, ixs: &[anchor_lang::solana_program::instruction::Instruction]) -> Vec<records::Appended> {
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &[payer], svm.latest_blockhash());
    let wire = bincode::serialize(&tx).unwrap();
    let decoded: Transaction = bincode::deserialize(&wire).unwrap();
    let meta = svm.send_transaction(decoded).map_err(|e| format!("{:?}\n{}", e.err, e.meta.logs.join("\n"))).unwrap();
    svm.expire_blockhash();
    let appended = records::parse_logs(&meta.logs).unwrap();
    records::store(ledger, &tx.signatures[0].to_string(), &appended).unwrap();
    appended
}

fn registry(svm: &LiteSVM) -> chain::Registry {
    chain::decode_registry(&svm.get_account(&chain::registry_pda()).unwrap().data).unwrap()
}

#[test]
fn three_days_with_silent_and_tampering_nodes() {
    let mut svm = LiteSVM::new();
    svm.add_program(chain::PROGRAM_ID, include_bytes!("../../../target/deploy/cota_v2.so")).unwrap();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 20_000_000_000).unwrap();
    let ledger = std::env::temp_dir().join(format!("cota-ledger-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ledger);

    let nodes: Vec<SigningKey> = (1..=3u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    let members: Vec<[u8; 32]> = nodes.iter().map(|k| k.verifying_key().to_bytes()).collect();
    let member_keys = members.iter().map(|m| Pubkey::new_from_array(*m)).collect();
    send(&mut svm, &payer, &ledger, &[chain::init_registry_instruction(payer.pubkey(), member_keys, 2)]);
    let a801 = cota_ingest::config::StationConfig {
        id: "A801".into(),
        name: "Porto Alegre".into(),
        source: Source::Inmet,
        variable: Variable::Precipitation,
        lat: -30.05,
        lon: -51.17,
        datum_id: None,
    };
    send(&mut svm, &payer, &ledger, &[chain::register_station_instruction(payer.pubkey(), &a801)]);
    let catalog = vec![("A801".to_string(), Source::Inmet)];
    let state = ledger.join("quality-state.json");

    // Day 1: all honest. Day 2: node 3 silent. Day 3: node 2 tampers with a reading.
    let mut prev = [0u8; 32];
    for (batch_id, day) in [(1u64, 100i64), (2, 101), (3, 102)] {
        let subs: Vec<SignedBatch> = match batch_id {
            1 => nodes.iter().map(|k| sign(k, 1, day, prev, false)).collect(),
            2 => nodes[..2].iter().map(|k| sign(k, 2, day, prev, false)).collect(),
            _ => vec![sign(&nodes[0], 3, day, prev, false), sign(&nodes[1], 3, day, prev, true), sign(&nodes[2], 3, day, prev, false)],
        };
        let agg = aggregate(&members, 2, &subs).unwrap();
        let appended = send(&mut svm, &payer, &ledger, &chain::commit_batch_instructions(payer.pubkey(), &agg.header, &agg.signatures));
        let Ok(Record::Batch(rec)) = appended[0].record() else { panic!("commit_batch must append a batch record") };
        assert_eq!(rec.header, chain::to_program_header(&agg.header));
        assert_eq!(rec.header.prev_batch_root, prev);

        let q = quality::evaluate(&state, &Limits::default(), &catalog, &agg).unwrap();
        for st in q.stations.iter().filter(|s| s.degraded.is_some()) {
            let ix = chain::report_degradation_instruction(payer.pubkey(), batch_id, &st.station_id, 0, st.degraded.unwrap() as u8);
            send(&mut svm, &payer, &ledger, &[ix]);
        }
        match batch_id {
            1 => {
                assert_eq!(rec.signers, 0b111);
                assert!(q.stations[0].degraded.is_none() && q.dissents.is_empty());
            }
            2 => {
                assert_eq!(rec.signers, 0b011);
                assert_eq!(agg.members[2].status, NodeStatus::Absent);
                // A silent node is not a data disagreement: the bitmap already shows it.
                assert!(q.stations[0].degraded.is_none());
            }
            _ => {
                assert_eq!(rec.signers, 0b101);
                assert!(matches!(agg.members[1].status, NodeStatus::Disagreed { .. }));
                // The recorded reading is the honest one.
                assert_eq!(agg.batch.readings[3].value_milli, 300);
                // MAD points at node 2, on that reading, and the station is declared in disagreement.
                assert_eq!(q.dissents.len(), 1);
                assert_eq!(q.dissents[0].member, pubkey_base58(&nodes[1]));
                assert_eq!((q.dissents[0].dissent.value_milli, q.dissents[0].dissent.median_milli), (Some(99_000), Some(300)));
                assert_eq!(q.stations[0].degraded, Some(Reason::Disagreement));
            }
        }
        prev = agg.header.merkle_root;
    }

    // The ledger's copy rebuilds exactly the on-chain accumulator.
    let reg = registry(&svm);
    assert_eq!((reg.next_batch_id, reg.last_batch_root), (4, prev));
    let stored = records::load(&ledger).unwrap();
    let kinds: Vec<_> = stored.iter().map(|s| s.kind.as_str()).collect();
    assert_eq!(kinds, ["station", "batch", "batch", "batch", "degradation"]);
    let tree = records::mmr(&stored).unwrap();
    assert_eq!((tree.len(), tree.peaks()), (reg.leaf_count, reg.peaks));

    // A gap (a record appended by someone else, not in our file) is refused instead of silently skipped.
    let fake = records::Appended { index: reg.leaf_count + 1, leaf: [0; 32], bytes: vec![] };
    assert!(records::store(&ledger, "x", &[fake]).is_err());

    let _ = std::fs::remove_dir_all(&ledger);
}

#[test]
fn vault_payout_with_a_proof_from_the_ledger() {
    let mut svm = LiteSVM::new();
    svm.add_program(chain::PROGRAM_ID, include_bytes!("../../../target/deploy/cota_v2.so")).unwrap();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 20_000_000_000).unwrap();
    let ledger = std::env::temp_dir().join(format!("cota-ledger-vault-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ledger);
    let nodes: Vec<SigningKey> = (1..=3u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    let members: Vec<[u8; 32]> = nodes.iter().map(|k| k.verifying_key().to_bytes()).collect();
    send(&mut svm, &payer, &ledger, &[chain::init_registry_instruction(payer.pubkey(), members.iter().map(|m| Pubkey::new_from_array(*m)).collect(), 2)]);
    send(&mut svm, &payer, &ledger, &[chain::set_alert_rule_instruction(payer.pubkey(), "GLOFAS-GUAIBA", 9_000_000, 1)]);
    let ben = Keypair::new().pubkey();
    let vault_args = chain::VaultArgs {
        vault_id: 1,
        beneficiary: ben,
        kind: chain::TRIGGER_FLOW_ALERT,
        subject: "GLOFAS-GUAIBA".into(),
        payout_lamports: 1_000_000_000,
        cooldown_secs: 3 * DAY,
        accept_from: 0,
        expires_at: i64::MAX,
        deposit_lamports: 3_000_000_000,
    };
    send(&mut svm, &payer, &ledger, &[chain::create_vault_instruction(payer.pubkey(), vault_args)]);
    let vault = chain::vault_pda(&payer.pubkey(), 1);

    // Ten days, each with a batch and a forecast; the forecast on day 4 crosses the rule.
    let mut prev = [0u8; 32];
    let mut alert_index = None;
    for d in 0..10u64 {
        let day = 200 + d as i64;
        let subs: Vec<SignedBatch> = nodes.iter().map(|k| sign(k, d + 1, day, prev, false)).collect();
        let agg = aggregate(&members, 2, &subs).unwrap();
        send(&mut svm, &payer, &ledger, &chain::commit_batch_instructions(payer.pubkey(), &agg.header, &agg.signatures));
        prev = agg.header.merkle_root;
        let args = chain::ForecastArgs {
            station_id: "GLOFAS-GUAIBA".into(),
            target_ts: (day + 2) * DAY,
            horizon_hours: 24,
            p10: 1,
            p50: 2,
            p90: if d == 4 { 9_500_000 } else { 5_000_000 },
            model_sha256: [0; 32],
            features_root: [0; 32],
        };
        let a = send(&mut svm, &payer, &ledger, &[chain::commit_forecast_instruction(payer.pubkey(), args)]);
        if let Ok(Record::Forecast(f)) = a[0].record() {
            if f.alert.is_some() {
                alert_index = Some(a[0].index);
            }
        }
    }
    let alert_index = alert_index.expect("the forecast above the rule carries an alert");

    // The proof is built from the ledger alone, after many more records were appended.
    let proof = records::proof(&records::load(&ledger).unwrap(), alert_index).unwrap();
    send(&mut svm, &payer, &ledger, &[chain::trigger_payout_instruction(payer.pubkey(), vault, ben, proof)]);
    // For the dashboard's JS check (dashboard/verify.js against the same bytes): COTA_KEEP_LEDGER=dir.
    if let Ok(dir) = std::env::var("COTA_KEEP_LEDGER") {
        let reg = registry(&svm);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(ledger.join("records.jsonl"), format!("{dir}/records.jsonl")).unwrap();
        let peaks: Vec<String> = reg.peaks.iter().map(hex::encode).collect();
        std::fs::write(format!("{dir}/registry.json"), serde_json::json!({"leaf_count": reg.leaf_count, "peaks": peaks}).to_string()).unwrap();
    }
    assert_eq!(svm.get_balance(&ben), Some(1_000_000_000));
    let v = chain::decode_vault(&svm.get_account(&vault).unwrap().data).unwrap();
    assert_eq!((v.payouts, v.total_paid), (1, 1_000_000_000));
    let _ = std::fs::remove_dir_all(&ledger);
}
