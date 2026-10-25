//! `record_public_alert` no programa compilado (LiteSVM).

use anchor_lang::{prelude::Pubkey, solana_program::instruction::Instruction, AnchorSerialize};
use cota::error::CotaError;
use cota_core::{public_alert::content_hash, PublicAlertHeader};
use cota_oracle::chain;
use ed25519_dalek::{Signer as _, SigningKey};
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::{InstructionError, Transaction, TransactionError};

struct Env {
    svm: LiteSVM,
    payer: Keypair,
    nodes: Vec<SigningKey>,
}

impl Env {
    fn new() -> Self {
        let mut svm = LiteSVM::new();
        svm.add_program(chain::PROGRAM_ID, include_bytes!("../../../target/deploy/cota.so")).unwrap();
        let payer = Keypair::new();
        svm.airdrop(&payer.pubkey(), 10_000_000_000).unwrap();
        let nodes: Vec<SigningKey> = (1..=3u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
        let members = nodes.iter().map(|k| Pubkey::new_from_array(k.verifying_key().to_bytes())).collect();
        let mut env = Self { svm, payer, nodes };
        env.send(&[chain::init_committee_instruction(env.payer.pubkey(), members, 2)]).unwrap();
        env
    }

    fn send(&mut self, ixs: &[Instruction]) -> Result<(), TransactionError> {
        let tx = Transaction::new_signed_with_payer(ixs, Some(&self.payer.pubkey()), &[&self.payer], self.svm.latest_blockhash());
        let res = self.svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err);
        self.svm.expire_blockhash();
        res
    }

    fn sigs(&self, who: &[usize], msg: &[u8]) -> Vec<([u8; 32], [u8; 64])> {
        who.iter().map(|&i| (self.nodes[i].verifying_key().to_bytes(), self.nodes[i].sign(msg).to_bytes())).collect()
    }
}

fn header(content: &str) -> PublicAlertHeader {
    PublicAlertHeader {
        issuer: "inmet".into(),
        alert_id: "urn:oid:2.49.0.0.76.0.2024.100.1".into(),
        content_sha256: content_hash(content),
        severity: 3,
        onset: 1_714_395_600,
        expires: 1_714_482_000,
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
    let h = header("{}");
    let mut program = Vec::new();
    chain::to_program_alert_header(&h).serialize(&mut program).unwrap();
    assert_eq!(h.signing_message(), [cota::PUBLIC_ALERT_DOMAIN, &program].concat());
}

#[test]
fn two_of_three_records_once() {
    let mut env = Env::new();
    let h = header(r#"{"codigo":"a"}"#);
    let sigs = env.sigs(&[0, 2], &h.signing_message());
    env.send(&chain::record_public_alert_instructions(env.payer.pubkey(), &h, &sigs)).unwrap();

    let pda = chain::public_alert_pda(&h.content_sha256);
    let r = chain::decode_public_alert(&env.svm.get_account(&pda).unwrap().data).unwrap();
    assert_eq!((r.issuer.as_str(), r.alert_id.as_str(), r.severity), ("inmet", h.alert_id.as_str(), 3));
    assert_eq!((r.signers, r.signer_count), (0b101, 2));
    assert_eq!(r.content_sha256, h.content_sha256);

    // A mesma versão não pode ser regravada (com outro horário).
    let sigs = env.sigs(&[0, 1, 2], &h.signing_message());
    assert!(env.send(&chain::record_public_alert_instructions(env.payer.pubkey(), &h, &sigs)).is_err());

    // Outra versão do mesmo aviso (conteúdo diferente) é outra conta.
    let h2 = PublicAlertHeader { content_sha256: content_hash(r#"{"codigo":"a","encerrado":true}"#), ..h };
    let sigs = env.sigs(&[1, 2], &h2.signing_message());
    env.send(&chain::record_public_alert_instructions(env.payer.pubkey(), &h2, &sigs)).unwrap();
}

#[test]
fn rejects_below_threshold_mismatch_and_cross_domain() {
    let mut env = Env::new();
    let h = header(r#"{"codigo":"b"}"#);

    let sigs = env.sigs(&[1], &h.signing_message());
    assert_eq!(err(env.send(&chain::record_public_alert_instructions(env.payer.pubkey(), &h, &sigs))), code(CotaError::ThresholdNotMet));

    // Assinaram outra severidade.
    let other = PublicAlertHeader { severity: 2, ..h.clone() };
    let sigs = env.sigs(&[0, 1], &other.signing_message());
    let mut ixs = chain::record_public_alert_instructions(env.payer.pubkey(), &h, &sigs);
    ixs[0] = chain::record_public_alert_instructions(env.payer.pubkey(), &other, &sigs)[0].clone();
    assert_eq!(err(env.send(&ixs)), code(CotaError::MessageMismatch));

    // Os mesmos bytes Borsh sem o domínio de aviso (ex.: reaproveitar assinatura de lote).
    let mut no_domain = Vec::new();
    chain::to_program_alert_header(&h).serialize(&mut no_domain).unwrap();
    let mut batch_domain = cota::BATCH_DOMAIN.to_vec();
    batch_domain.extend(&no_domain);
    let sigs = env.sigs(&[0, 1], &batch_domain);
    let mut ixs = chain::record_public_alert_instructions(env.payer.pubkey(), &h, &sigs);
    ixs[0] = Instruction::new_with_bytes(
        solana_sdk_ids::ed25519_program::ID,
        &cota_core::precompile::ed25519_instruction_data(&batch_domain, &sigs),
        vec![],
    );
    assert_eq!(err(env.send(&ixs)), code(CotaError::MessageMismatch));

    let bad = PublicAlertHeader { expires: h.onset - 1, ..h };
    let sigs = env.sigs(&[0, 1], &bad.signing_message());
    assert_eq!(err(env.send(&chain::record_public_alert_instructions(env.payer.pubkey(), &bad, &sigs))), code(CotaError::InvalidPublicAlert));
}
