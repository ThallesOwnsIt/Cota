//! Pipeline completo sem rede: nós assinam → agregador escolhe → instruções reais no LiteSVM.

use anchor_lang::{prelude::Pubkey, AccountDeserialize};
use chrono::NaiveDate;
use cota::state::BatchCommitment;
use cota_core::{Batch, Hash, Reading, Source, Variable};
use cota_oracle::{aggregate::{aggregate, NodeStatus}, chain, keys::pubkey_base58, node::SignedBatch, quality};
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
        signature: key.sign(&b.header.signing_message()).to_bytes(),
        header: b.header,
        readings: b.readings,
        ingest: vec![],
    }
}

fn send(svm: &mut LiteSVM, payer: &Keypair, ixs: &[anchor_lang::solana_program::instruction::Instruction]) {
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &[payer], svm.latest_blockhash());
    // Mesmo formato de bytes que o cliente RPC envia.
    let wire = bincode::serialize(&tx).unwrap();
    let decoded: Transaction = bincode::deserialize(&wire).unwrap();
    svm.send_transaction(decoded).map_err(|e| format!("{:?}\n{}", e.err, e.meta.logs.join("\n"))).unwrap();
    svm.expire_blockhash();
}

#[test]
fn three_days_with_silent_and_tampering_nodes() {
    let mut svm = LiteSVM::new();
    svm.add_program(chain::PROGRAM_ID, include_bytes!("../../../target/deploy/cota.so")).unwrap();
    let payer = Keypair::new();
    svm.airdrop(&payer.pubkey(), 10_000_000_000).unwrap();

    let nodes: Vec<SigningKey> = (1..=3u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
    let members: Vec<[u8; 32]> = nodes.iter().map(|k| k.verifying_key().to_bytes()).collect();
    let member_keys = members.iter().map(|m| Pubkey::new_from_array(*m)).collect();
    send(&mut svm, &payer, &[chain::init_committee_instruction(payer.pubkey(), member_keys, 2)]);
    let a801 = cota_ingest::config::StationConfig {
        id: "A801".into(),
        name: "Porto Alegre".into(),
        source: Source::Inmet,
        variable: Variable::Precipitation,
        lat: -30.05,
        lon: -51.17,
        datum_id: None,
    };
    send(&mut svm, &payer, &[chain::register_station_instruction(payer.pubkey(), &a801)]);
    let catalog = vec![("A801".to_string(), Source::Inmet)];
    let state = std::env::temp_dir().join(format!("cota-quality-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&state);

    // Dia 1: todos honestos. Dia 2: nó 3 mudo. Dia 3: nó 2 adultera uma leitura.
    let mut prev = [0u8; 32];
    for (batch_id, day) in [(1u64, 100i64), (2, 101), (3, 102)] {
        let subs: Vec<SignedBatch> = match batch_id {
            1 => nodes.iter().map(|k| sign(k, 1, day, prev, false)).collect(),
            2 => nodes[..2].iter().map(|k| sign(k, 2, day, prev, false)).collect(),
            _ => vec![sign(&nodes[0], 3, day, prev, false), sign(&nodes[1], 3, day, prev, true), sign(&nodes[2], 3, day, prev, false)],
        };
        let agg = aggregate(&members, 2, &subs).unwrap();
        send(&mut svm, &payer, &chain::commit_batch_instructions(payer.pubkey(), &agg.header, &agg.signatures));

        let q = quality::evaluate(&state, &Limits::default(), &catalog, &agg).unwrap();
        for st in q.stations.iter().filter(|s| s.degraded.is_some()) {
            let ix = chain::report_degradation_instruction(payer.pubkey(), batch_id, &st.station_id, 0, st.degraded.unwrap() as u8);
            send(&mut svm, &payer, &[ix]);
        }

        let acc = svm.get_account(&chain::batch_pda(batch_id)).unwrap();
        let on_chain = BatchCommitment::try_deserialize(&mut acc.data.as_slice()).unwrap();
        assert_eq!(on_chain.merkle_root, agg.header.merkle_root);
        assert_eq!(on_chain.prev_batch_root, prev);
        match batch_id {
            1 => {
                assert_eq!(on_chain.signers, 0b111);
                assert!(q.stations[0].degraded.is_none() && q.dissents.is_empty());
            }
            2 => {
                assert_eq!(on_chain.signers, 0b011);
                assert_eq!(agg.members[2].status, NodeStatus::Absent);
                // Nó mudo não é divergência de dados: a ausência já está no OracleAbsent.
                assert!(q.stations[0].degraded.is_none());
            }
            _ => {
                assert_eq!(on_chain.signers, 0b101);
                assert!(matches!(agg.members[1].status, NodeStatus::Disagreed { .. }));
                // A leitura gravada é a honesta.
                assert_eq!(agg.batch.readings[3].value_milli, 300);
                // A MAD aponta o nó 2, naquela leitura, e a estação é declarada em divergência.
                assert_eq!(q.dissents.len(), 1);
                assert_eq!(q.dissents[0].member, pubkey_base58(&nodes[1]));
                assert_eq!((q.dissents[0].dissent.value_milli, q.dissents[0].dissent.median_milli), (Some(99_000), Some(300)));
                assert_eq!(q.stations[0].degraded, Some(Reason::Disagreement));
            }
        }
        prev = agg.header.merkle_root;
    }

    let committee = chain::decode_committee(&svm.get_account(&chain::committee_pda()).unwrap().data).unwrap();
    assert_eq!(committee.next_batch_id, 4);
    assert_eq!(committee.last_batch_root, prev);
    let _ = std::fs::remove_file(&state);
}
