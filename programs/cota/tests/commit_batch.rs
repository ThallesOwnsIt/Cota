//! Testes do programa compilado (cota.so) rodando no LiteSVM, com o precompile Ed25519 real.

use {
    anchor_lang::{
        prelude::Pubkey, solana_program::instruction::Instruction, solana_program::system_program,
        AccountDeserialize, InstructionData, ToAccountMetas,
    },
    cota::{state::BatchCommitment, BatchHeader, StationArgs},
    cota_core::precompile::ed25519_instruction_data,
    ed25519_dalek::{Signer as _, SigningKey},
    litesvm::LiteSVM,
    solana_keypair::Keypair,
    solana_message::{Message, VersionedMessage},
    solana_signer::Signer,
    solana_transaction::{versioned::VersionedTransaction, InstructionError, TransactionError},
};

/// Código da primeira variante de `#[error_code]` no Anchor.
const ANCHOR_ERROR_OFFSET: u32 = 6000;

struct Env {
    svm: LiteSVM,
    authority: Keypair,
    oracles: Vec<SigningKey>,
}

impl Env {
    fn new() -> Self {
        let mut svm = LiteSVM::new();
        let bytes = include_bytes!(concat!(env!("CARGO_TARGET_TMPDIR"), "/../deploy/cota.so"));
        svm.add_program(cota::id(), bytes).unwrap();
        let authority = Keypair::new();
        svm.airdrop(&authority.pubkey(), 10_000_000_000).unwrap();
        let oracles = (1..=3u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
        let mut env = Self { svm, authority, oracles };
        let members = env.oracles.iter().map(oracle_pubkey).collect();
        env.init_committee(members, 2).unwrap();
        env
    }

    fn send(&mut self, ixs: &[Instruction]) -> Result<(), TransactionError> {
        let payer = self.authority.pubkey();
        let msg = Message::new_with_blockhash(ixs, Some(&payer), &self.svm.latest_blockhash());
        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &[&self.authority]).unwrap();
        let res = self.svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err);
        self.svm.expire_blockhash();
        res
    }

    fn init_committee(&mut self, members: Vec<Pubkey>, threshold: u8) -> Result<(), TransactionError> {
        let ix = Instruction::new_with_bytes(
            cota::id(),
            &cota::instruction::InitCommittee { members, threshold }.data(),
            cota::accounts::InitCommittee {
                authority: self.authority.pubkey(),
                committee: committee_pda(),
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ix])
    }

    /// Envia [precompile Ed25519 assinado por `signers`, commit_batch(header)].
    fn commit(&mut self, header: &BatchHeader, signers: &[usize]) -> Result<(), TransactionError> {
        let ed = self.ed25519_ix(&header.signing_message(), signers);
        let ix = commit_ix(self.authority.pubkey(), header);
        self.send(&[ed, ix])
    }

    fn ed25519_ix(&self, message: &[u8], signers: &[usize]) -> Instruction {
        let sigs: Vec<_> = signers
            .iter()
            .map(|&i| (self.oracles[i].verifying_key().to_bytes(), self.oracles[i].sign(message).to_bytes()))
            .collect();
        Instruction::new_with_bytes(
            solana_sdk_ids::ed25519_program::ID,
            &ed25519_instruction_data(message, &sigs),
            vec![],
        )
    }

    fn batch(&self, id: u64) -> BatchCommitment {
        let acc = self.svm.get_account(&batch_pda(id)).expect("lote não existe");
        BatchCommitment::try_deserialize(&mut acc.data.as_slice()).unwrap()
    }
}

fn oracle_pubkey(k: &SigningKey) -> Pubkey {
    Pubkey::new_from_array(k.verifying_key().to_bytes())
}

fn committee_pda() -> Pubkey {
    Pubkey::find_program_address(&[cota::COMMITTEE_SEED], &cota::id()).0
}

fn batch_pda(id: u64) -> Pubkey {
    Pubkey::find_program_address(&[cota::BATCH_SEED, &id.to_le_bytes()], &cota::id()).0
}

fn commit_ix(submitter: Pubkey, header: &BatchHeader) -> Instruction {
    Instruction::new_with_bytes(
        cota::id(),
        &cota::instruction::CommitBatch { header: header.clone() }.data(),
        cota::accounts::CommitBatch {
            submitter,
            committee: committee_pda(),
            batch: batch_pda(header.batch_id),
            instructions: solana_instructions_sysvar::ID,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
    )
}

fn header(batch_id: u64, prev: [u8; 32], t_start: i64) -> BatchHeader {
    BatchHeader {
        batch_id,
        t_start,
        t_end: t_start + 3600,
        leaf_count: 42,
        merkle_root: [batch_id as u8; 32],
        station_set_hash: [9; 32],
        prev_batch_root: prev,
    }
}

fn custom_error(res: Result<(), TransactionError>) -> u32 {
    match res {
        Err(TransactionError::InstructionError(_, InstructionError::Custom(c))) => c,
        other => panic!("esperava erro customizado, veio {other:?}"),
    }
}

fn code(e: cota::error::CotaError) -> u32 {
    ANCHOR_ERROR_OFFSET + e as u32
}

use cota::error::CotaError;

#[test]
fn header_bytes_match_cota_core() {
    let h = header(7, [3; 32], 1_714_521_600);
    let core = cota_core::BatchHeader {
        batch_id: h.batch_id,
        t_start: h.t_start,
        t_end: h.t_end,
        leaf_count: h.leaf_count,
        merkle_root: h.merkle_root,
        station_set_hash: h.station_set_hash,
        prev_batch_root: h.prev_batch_root,
    };
    assert_eq!(h.signing_message(), core.signing_message());
}

#[test]
fn three_of_three_commits_and_records_signers() {
    let mut env = Env::new();
    let h = header(1, [0; 32], 1_714_521_600);
    env.commit(&h, &[0, 1, 2]).unwrap();

    let b = env.batch(1);
    assert_eq!(b.merkle_root, h.merkle_root);
    assert_eq!(b.leaf_count, 42);
    assert_eq!(b.signers, 0b111);
    assert_eq!(b.signer_count, 3);
    assert_eq!(b.submitter, env.authority.pubkey());
}

#[test]
fn two_of_three_is_enough_and_marks_absent_node() {
    let mut env = Env::new();
    env.commit(&header(1, [0; 32], 0), &[0, 2]).unwrap();
    let b = env.batch(1);
    assert_eq!(b.signers, 0b101);
    assert_eq!(b.signer_count, 2);
}

#[test]
fn one_of_three_is_rejected() {
    let mut env = Env::new();
    assert_eq!(custom_error(env.commit(&header(1, [0; 32], 0), &[1])), code(CotaError::ThresholdNotMet));
}

#[test]
fn same_signer_twice_does_not_count_twice() {
    let mut env = Env::new();
    assert_eq!(custom_error(env.commit(&header(1, [0; 32], 0), &[1, 1])), code(CotaError::ThresholdNotMet));
}

#[test]
fn signature_over_other_header_is_rejected() {
    let mut env = Env::new();
    let signed = header(1, [0; 32], 0);
    let mut sent = signed.clone();
    sent.merkle_root = [0xAA; 32];
    let ed = env.ed25519_ix(&signed.signing_message(), &[0, 1, 2]);
    let ix = commit_ix(env.authority.pubkey(), &sent);
    assert_eq!(custom_error(env.send(&[ed, ix])), code(CotaError::MessageMismatch));
}

#[test]
fn forged_signature_is_rejected_by_precompile() {
    let mut env = Env::new();
    let h = header(1, [0; 32], 0);
    let mut ed = env.ed25519_ix(&h.signing_message(), &[0, 1]);
    let last = ed.data.len() - 1;
    ed.data[last] ^= 0x01; // altera um bit da última assinatura
    let ix = commit_ix(env.authority.pubkey(), &h);
    let err = env.send(&[ed, ix]).unwrap_err();
    assert!(matches!(err, TransactionError::InstructionError(0, _)), "falha deve vir do precompile: {err:?}");
}

#[test]
fn outsider_key_is_rejected() {
    let mut env = Env::new();
    env.oracles.push(SigningKey::from_bytes(&[99; 32]));
    assert_eq!(custom_error(env.commit(&header(1, [0; 32], 0), &[0, 3])), code(CotaError::UnknownSigner));
}

#[test]
fn missing_precompile_is_rejected() {
    let mut env = Env::new();
    let ix = commit_ix(env.authority.pubkey(), &header(1, [0; 32], 0));
    assert_eq!(custom_error(env.send(&[ix])), code(CotaError::MissingEd25519Instruction));
}

#[test]
fn hash_chain_sequence_and_window_are_enforced() {
    let mut env = Env::new();
    let h1 = header(1, [0; 32], 1000);
    env.commit(&h1, &[0, 1]).unwrap();

    // Repetir o lote 1.
    assert!(env.commit(&h1, &[0, 1]).is_err());
    // Pular para o lote 3.
    assert_eq!(custom_error(env.commit(&header(3, h1.merkle_root, 5000), &[0, 1])), code(CotaError::BatchOutOfSequence));
    // Lote 2 que não encadeia na raiz do lote 1.
    assert_eq!(custom_error(env.commit(&header(2, [7; 32], 5000), &[0, 1])), code(CotaError::BrokenHashChain));
    // Lote 2 com janela sobreposta ao lote 1.
    assert_eq!(custom_error(env.commit(&header(2, h1.merkle_root, 2000), &[0, 1])), code(CotaError::InvalidWindow));

    env.commit(&header(2, h1.merkle_root, h1.t_end), &[1, 2]).unwrap();
    assert_eq!(env.batch(2).prev_batch_root, h1.merkle_root);
}

#[test]
fn rotated_key_stops_counting() {
    let mut env = Env::new();
    let old = oracle_pubkey(&env.oracles[0]);
    let replacement = SigningKey::from_bytes(&[42; 32]);
    let ix = Instruction::new_with_bytes(
        cota::id(),
        &cota::instruction::RotateMember { old, new: oracle_pubkey(&replacement) }.data(),
        cota::accounts::RotateMember { authority: env.authority.pubkey(), committee: committee_pda() }
            .to_account_metas(None),
    );
    env.send(&[ix]).unwrap();

    assert_eq!(custom_error(env.commit(&header(1, [0; 32], 0), &[0, 1])), code(CotaError::UnknownSigner));
    env.oracles[0] = replacement;
    env.commit(&header(1, [0; 32], 0), &[0, 1]).unwrap();
    assert_eq!(env.batch(1).signers, 0b011);
}

#[test]
fn only_authority_registers_stations() {
    let mut env = Env::new();
    let args = StationArgs {
        station_id: "A801".into(),
        name: "Porto Alegre - Jardim Botânico".into(),
        source: 0,
        variable: 0,
        lat_e6: -30_053_611,
        lon_e6: -51_174_722,
        datum_id: None,
    };
    let station = Pubkey::find_program_address(&[cota::STATION_SEED, b"A801"], &cota::id()).0;
    let ix_for = |authority: Pubkey| {
        Instruction::new_with_bytes(
            cota::id(),
            &cota::instruction::RegisterStation { args: args.clone() }.data(),
            cota::accounts::RegisterStation {
                authority,
                committee: committee_pda(),
                station,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        )
    };

    let intruder = Keypair::new();
    env.svm.airdrop(&intruder.pubkey(), 1_000_000_000).unwrap();
    let msg = Message::new_with_blockhash(&[ix_for(intruder.pubkey())], Some(&intruder.pubkey()), &env.svm.latest_blockhash());
    let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &[&intruder]).unwrap();
    let err = env.svm.send_transaction(tx).unwrap_err().err;
    assert_eq!(custom_error(Err(err)), code(CotaError::Unauthorized));

    let ix = ix_for(env.authority.pubkey());
    env.send(&[ix]).unwrap();
    let acc = env.svm.get_account(&station).unwrap();
    let s = cota::state::Station::try_deserialize(&mut acc.data.as_slice()).unwrap();
    assert_eq!(s.station_id, "A801");
    assert_eq!(s.lat_e6, -30_053_611);
}
