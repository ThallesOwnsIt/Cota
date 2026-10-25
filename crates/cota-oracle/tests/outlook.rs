//! `set_outlook_rule` e `commit_outlook` no programa compilado (LiteSVM).

use anchor_lang::{prelude::Pubkey, solana_program::instruction::Instruction, AnchorSerialize};
use cota::error::CotaError;
use cota_core::{public_alert::content_hash, Batch, OutlookHeader, Reading, Source, Variable};
use cota_ingest::config::StationConfig;
use cota_oracle::chain;
use ed25519_dalek::{Signer as _, SigningKey};
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::{InstructionError, Transaction, TransactionError};

const DAY: i64 = 86_400;
const STATION: &str = "GLOFAS-GUAIBA";
const REGION: &str = "guaiba";

struct Env {
    svm: LiteSVM,
    authority: Keypair,
    nodes: Vec<SigningKey>,
    prev: [u8; 32],
    next_batch: u64,
}

impl Env {
    fn new() -> Self {
        let mut svm = LiteSVM::new();
        svm.add_program(chain::PROGRAM_ID, include_bytes!("../../../target/deploy/cota.so")).unwrap();
        let authority = Keypair::new();
        svm.airdrop(&authority.pubkey(), 10_000_000_000).unwrap();
        let nodes: Vec<SigningKey> = (1..=3u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
        let members = nodes.iter().map(|k| Pubkey::new_from_array(k.verifying_key().to_bytes())).collect();
        let mut env = Self { svm, authority, nodes, prev: [0; 32], next_batch: 1 };
        env.send(&[chain::init_committee_instruction(env.authority.pubkey(), members, 2)]).unwrap();
        let st = StationConfig {
            id: STATION.into(),
            name: "Guaíba".into(),
            source: Source::OpenMeteoGlofas,
            variable: Variable::RiverDischarge,
            lat: -30.075,
            lon: -51.325,
            datum_id: None,
        };
        env.send(&[chain::register_station_instruction(env.authority.pubkey(), &st)]).unwrap();
        env
    }

    fn send(&mut self, ixs: &[Instruction]) -> Result<(), TransactionError> {
        let tx = Transaction::new_signed_with_payer(ixs, Some(&self.authority.pubkey()), &[&self.authority], self.svm.latest_blockhash());
        let res = self.svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err);
        self.svm.expire_blockhash();
        res
    }

    fn sigs(&self, who: &[usize], msg: &[u8]) -> Vec<([u8; 32], [u8; 64])> {
        who.iter().map(|&i| (self.nodes[i].verifying_key().to_bytes(), self.nodes[i].sign(msg).to_bytes())).collect()
    }

    fn anchor_day(&mut self, day: i64) {
        let reading = Reading {
            station_id: STATION.into(),
            variable: Variable::RiverDischarge,
            ts_utc: day * DAY,
            source: Source::OpenMeteoGlofas,
            value_milli: 5_000_000,
            datum_id: None,
            raw_payload_sha256: [7; 32],
        };
        let b = Batch::build(self.next_batch, day * DAY, (day + 1) * DAY, self.prev, vec![reading]).unwrap();
        let sigs = self.sigs(&[0, 1], &b.header.signing_message());
        self.send(&chain::commit_batch_instructions(self.authority.pubkey(), &b.header, &sigs)).unwrap();
        self.prev = b.header.merkle_root;
        self.next_batch += 1;
    }

    fn set_rule(&mut self) {
        let ix = chain::set_outlook_rule_instruction(self.authority.pubkey(), rule_args(90_000));
        self.send(&[ix]).unwrap();
    }

    fn commit(&mut self, who: &[usize], h: &OutlookHeader) -> Result<(), TransactionError> {
        let sigs = self.sigs(who, &h.signing_message());
        self.send(&chain::commit_outlook_instructions(self.authority.pubkey(), h, &sigs))
    }
}

fn rule_args(threshold_milli: i64) -> cota::OutlookRuleArgs {
    cota::OutlookRuleArgs { region: REGION.into(), threshold_milli, window_secs: 5 * DAY, min_models: 2, rule_sha256: [0xcc; 32] }
}

/// Perspectiva emitida no fim do dia `day`.
fn header(day: i64, mean_mm: i64) -> OutlookHeader {
    let issued_at = (day + 1) * DAY;
    OutlookHeader {
        region: REGION.into(),
        issued_at,
        window_end: issued_at + 5 * DAY,
        models: 3,
        rain_mean_milli: mean_mm * 1000,
        rain_min_milli: (mean_mm - 10).max(0) * 1000,
        rain_max_milli: (mean_mm + 10) * 1000,
        content_sha256: content_hash(&format!("{{\"dia\":{day},\"mm\":{mean_mm}}}")),
    }
}

fn err(res: Result<(), TransactionError>) -> u32 {
    match res {
        Err(TransactionError::InstructionError(_, InstructionError::Custom(c))) => c,
        other => panic!("esperava erro customizado, veio {other:?}"),
    }
}

fn code(e: CotaError) -> u32 {
    6000 + e as u32
}

#[test]
fn header_bytes_match_program() {
    let h = header(100, 95);
    let mut program = Vec::new();
    chain::to_program_outlook_header(&h).serialize(&mut program).unwrap();
    assert_eq!(h.signing_message(), [cota::OUTLOOK_DOMAIN, &program].concat());
}

#[test]
fn rule_is_immutable_and_only_the_authority_sets_it() {
    let mut env = Env::new();
    let intruder = Keypair::new();
    env.svm.airdrop(&intruder.pubkey(), 1_000_000_000).unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[chain::set_outlook_rule_instruction(intruder.pubkey(), rule_args(1))],
        Some(&intruder.pubkey()),
        &[&intruder],
        env.svm.latest_blockhash(),
    );
    assert_eq!(err(env.svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err)), code(CotaError::Unauthorized));

    env.set_rule();
    let r = chain::decode_outlook_rule(&env.svm.get_account(&chain::outlook_rule_pda(REGION)).unwrap().data).unwrap();
    assert_eq!((r.threshold_milli, r.window_secs, r.min_models, r.rule_sha256), (90_000, 5 * DAY, 2, [0xcc; 32]));
    assert!(env.send(&[chain::set_outlook_rule_instruction(env.authority.pubkey(), rule_args(50_000))]).is_err(), "regra imutável");
}

#[test]
fn program_applies_the_rule() {
    let mut env = Env::new();
    env.set_rule();
    env.anchor_day(100);

    let high = header(100, 141);
    env.commit(&[0, 2], &high).unwrap();
    let o = chain::decode_outlook(&env.svm.get_account(&chain::outlook_pda(REGION, high.issued_at)).unwrap().data).unwrap();
    assert!(o.triggered, "141 mm ≥ 90 mm: alerta antecipado");
    assert_eq!((o.based_on_batch_id, o.threshold_milli, o.signers, o.signer_count), (1, 90_000, 0b101, 2));
    assert_eq!(o.content_sha256, high.content_sha256);
    assert!(env.commit(&[0, 1, 2], &high).is_err(), "uma perspectiva por dia");

    env.anchor_day(101);
    let low = header(101, 40);
    env.commit(&[1, 2], &low).unwrap();
    let o = chain::decode_outlook(&env.svm.get_account(&chain::outlook_pda(REGION, low.issued_at)).unwrap().data).unwrap();
    assert!(!o.triggered);
}

#[test]
fn bound_to_the_day_that_just_closed() {
    let mut env = Env::new();
    env.set_rule();
    assert_eq!(err(env.commit(&[0, 1], &header(100, 95))), code(CotaError::NoAnchoredBatch));

    env.anchor_day(100);
    // Antecipar o dia seguinte (antes de o lote dele existir) não vale.
    assert_eq!(err(env.commit(&[0, 1], &header(101, 95))), code(CotaError::InvalidOutlook));
    env.anchor_day(101);
    // Nem registrar a do dia 100 depois que o lote do dia 101 entrou.
    assert_eq!(err(env.commit(&[0, 1], &header(100, 95))), code(CotaError::InvalidOutlook));
    env.commit(&[0, 1], &header(101, 95)).unwrap();
}

#[test]
fn rejects_bad_headers_and_signatures() {
    let mut env = Env::new();
    env.set_rule();
    env.anchor_day(100);
    let h = header(100, 95);

    let bad = [
        OutlookHeader { window_end: h.window_end + DAY, ..h.clone() },
        OutlookHeader { models: 1, ..h.clone() },
        OutlookHeader { rain_min_milli: h.rain_mean_milli + 1, ..h.clone() },
        OutlookHeader { rain_max_milli: h.rain_mean_milli - 1, ..h.clone() },
    ];
    for b in &bad {
        assert_eq!(err(env.commit(&[0, 1], b)), code(CotaError::InvalidOutlook), "{b:?}");
    }

    assert_eq!(err(env.commit(&[2], &h)), code(CotaError::ThresholdNotMet));

    // Assinaram outra chuva.
    let other = OutlookHeader { rain_mean_milli: 10_000, rain_min_milli: 0, ..h.clone() };
    let sigs = env.sigs(&[0, 1], &other.signing_message());
    let mut ixs = chain::commit_outlook_instructions(env.authority.pubkey(), &h, &sigs);
    ixs[0] = chain::commit_outlook_instructions(env.authority.pubkey(), &other, &sigs)[0].clone();
    assert_eq!(err(env.send(&ixs)), code(CotaError::MessageMismatch));

    // Os mesmos bytes com o domínio de aviso oficial.
    let mut body = Vec::new();
    chain::to_program_outlook_header(&h).serialize(&mut body).unwrap();
    let foreign = [cota::PUBLIC_ALERT_DOMAIN, &body].concat();
    let sigs = env.sigs(&[0, 1], &foreign);
    let mut ixs = chain::commit_outlook_instructions(env.authority.pubkey(), &h, &sigs);
    ixs[0] = Instruction::new_with_bytes(
        solana_sdk_ids::ed25519_program::ID,
        &cota_core::precompile::ed25519_instruction_data(&foreign, &sigs),
        vec![],
    );
    assert_eq!(err(env.send(&ixs)), code(CotaError::MessageMismatch));

    env.commit(&[0, 1], &h).unwrap();
}
