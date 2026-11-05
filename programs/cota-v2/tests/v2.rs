//! Program v2 (cota_v2.so) in LiteSVM with the real Ed25519 precompile.
//!
//! Every record the program emits is replayed into the reference accumulator (`cota_core::mmr`): after
//! each transaction the peaks stored on-chain must be exactly the reference ones.

use {
    anchor_lang::{
        prelude::Pubkey, solana_program::instruction::Instruction, solana_program::system_program, AccountDeserialize,
        AnchorSerialize, InstructionData, ToAccountMetas,
    },
    base64::Engine as _,
    cota_core::{merkle::hash_leaf, mmr::Mmr, precompile::ed25519_instruction_data},
    cota_v2::{
        accounts as acc, instruction as ix, BatchHeader, ForecastArgs, OutlookHeader, OutlookRuleArgs, PublicAlertHeader,
        Record, Registry, StationArgs, TriggerProof, Vault, VaultArgs,
    },
    ed25519_dalek::{Signer as _, SigningKey},
    litesvm::LiteSVM,
    solana_keypair::Keypair,
    solana_message::{Message, VersionedMessage},
    solana_signer::Signer,
    solana_transaction::{versioned::VersionedTransaction, InstructionError, TransactionError},
};

const ANCHOR_ERROR_OFFSET: u32 = 6000;
const DAY: i64 = 86_400;
const T0: i64 = 1_714_521_600; // 2024-05-01T00:00Z

struct Env {
    svm: LiteSVM,
    authority: Keypair,
    oracles: Vec<SigningKey>,
    /// Reference accumulator, fed with the records the program emitted.
    mmr: Mmr,
    records: Vec<Vec<u8>>,
}

fn registry_pda() -> Pubkey {
    Pubkey::find_program_address(&[cota_v2::REGISTRY_SEED], &cota_v2::id()).0
}
fn alert_rule_pda(station: &str) -> Pubkey {
    Pubkey::find_program_address(&[cota_v2::ALERT_RULE_SEED, station.as_bytes()], &cota_v2::id()).0
}
fn outlook_rule_pda(region: &str) -> Pubkey {
    Pubkey::find_program_address(&[cota_v2::OUTLOOK_RULE_SEED, region.as_bytes()], &cota_v2::id()).0
}
fn vault_pda(creator: &Pubkey, id: u64) -> Pubkey {
    Pubkey::find_program_address(&[cota_v2::VAULT_SEED, creator.as_ref(), &id.to_le_bytes()], &cota_v2::id()).0
}
fn payout_pda(vault: &Pubkey, leaf: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[cota_v2::PAYOUT_SEED, vault.as_ref(), leaf], &cota_v2::id()).0
}

fn custom(code: u32) -> TransactionError {
    // The Ed25519 precompile is instruction 0, so program errors come from instruction 1 when it is used.
    TransactionError::InstructionError(1, InstructionError::Custom(ANCHOR_ERROR_OFFSET + code))
}
fn custom0(code: u32) -> TransactionError {
    TransactionError::InstructionError(0, InstructionError::Custom(ANCHOR_ERROR_OFFSET + code))
}
fn code(e: cota_v2::error::CotaError) -> u32 {
    e as u32
}

impl Env {
    fn new() -> Self {
        let mut svm = LiteSVM::new();
        let bytes = include_bytes!(concat!(env!("CARGO_TARGET_TMPDIR"), "/../deploy/cota_v2.so"));
        svm.add_program(cota_v2::id(), bytes).unwrap();
        let authority = Keypair::new();
        svm.airdrop(&authority.pubkey(), 100_000_000_000).unwrap();
        let oracles: Vec<SigningKey> = (1..=3u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
        let mut env = Self { svm, authority, oracles, mmr: Mmr::new(), records: vec![] };
        let members = env.oracles.iter().map(|k| Pubkey::new_from_array(k.verifying_key().to_bytes())).collect();
        let i = Instruction::new_with_bytes(
            cota_v2::id(),
            &ix::InitRegistry { members, threshold: 2 }.data(),
            acc::InitRegistry { authority: env.authority.pubkey(), registry: registry_pda(), system_program: system_program::ID }
                .to_account_metas(None),
        );
        env.send(&[i]).unwrap();
        env
    }

    /// Sends the instructions; replays every `RecordAppended` into the reference MMR and checks the peaks.
    fn send(&mut self, ixs: &[Instruction]) -> Result<(), TransactionError> {
        let payer = self.authority.pubkey();
        let msg = Message::new_with_blockhash(ixs, Some(&payer), &self.svm.latest_blockhash());
        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &[&self.authority]).unwrap();
        let res = self.svm.send_transaction(tx);
        self.svm.expire_blockhash();
        let meta = res.map_err(|e| e.err)?;
        for line in &meta.logs {
            let Some(b64) = line.strip_prefix("Program data: ") else { continue };
            let bytes = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
            // RecordAppended = discriminator (8) ‖ index (8) ‖ leaf (32) ‖ borsh(Record)
            if bytes.len() < 48 || bytes[..8] != record_appended_discriminator() {
                continue;
            }
            let index = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
            let record = bytes[48..].to_vec();
            assert_eq!(index, self.mmr.len(), "records arrive in order");
            assert_eq!(&bytes[16..48], &hash_leaf(&record), "event leaf = H(0x00 ‖ record)");
            self.mmr.push(&record);
            self.records.push(record);
        }
        let reg = self.registry();
        assert_eq!(reg.leaf_count, self.mmr.len());
        assert_eq!(reg.peaks, self.mmr.peaks(), "on-chain peaks = reference peaks");
        Ok(())
    }

    fn registry(&self) -> Registry {
        let a = self.svm.get_account(&registry_pda()).unwrap();
        Registry::try_deserialize(&mut a.data.as_slice()).unwrap()
    }

    fn ed25519_ix(&self, message: &[u8], signers: &[usize]) -> Instruction {
        let sigs: Vec<_> = signers
            .iter()
            .map(|&i| (self.oracles[i].verifying_key().to_bytes(), self.oracles[i].sign(message).to_bytes()))
            .collect();
        Instruction::new_with_bytes(solana_sdk_ids::ed25519_program::ID, &ed25519_instruction_data(message, &sigs), vec![])
    }

    fn signed_accounts(&self) -> Vec<anchor_lang::solana_program::instruction::AccountMeta> {
        acc::CommitteeSigned {
            submitter: self.authority.pubkey(),
            registry: registry_pda(),
            instructions: solana_instructions_sysvar::ID,
        }
        .to_account_metas(None)
    }

    fn authority_accounts(&self) -> Vec<anchor_lang::solana_program::instruction::AccountMeta> {
        acc::AuthorityOnly { authority: self.authority.pubkey(), registry: registry_pda() }.to_account_metas(None)
    }

    fn commit_signed(&mut self, header: &BatchHeader, message: &[u8], signers: &[usize]) -> Result<(), TransactionError> {
        let ed = self.ed25519_ix(message, signers);
        let c = Instruction::new_with_bytes(
            cota_v2::id(),
            &ix::CommitBatch { header: header.clone() }.data(),
            self.signed_accounts(),
        );
        self.send(&[ed, c])
    }

    fn commit(&mut self, header: &BatchHeader, signers: &[usize]) -> Result<(), TransactionError> {
        self.commit_signed(header, &header.signing_message(), signers)
    }

    /// Anchors the next day (1-day windows from T0).
    fn next_day(&mut self) -> BatchHeader {
        let reg = self.registry();
        let id = reg.next_batch_id;
        let h = BatchHeader {
            batch_id: id,
            t_start: T0 + (id as i64 - 1) * DAY,
            t_end: T0 + id as i64 * DAY,
            leaf_count: 1,
            merkle_root: [id as u8; 32],
            station_set_hash: [0; 32],
            prev_batch_root: reg.last_batch_root,
        };
        self.commit(&h, &[0, 1]).unwrap();
        h
    }

    fn forecast(&mut self, station: &str, target_ts: i64, p90: i64) -> Result<(), TransactionError> {
        let args = ForecastArgs {
            station_id: station.into(),
            target_ts,
            horizon_hours: 24,
            p10: p90 / 2,
            p50: p90 * 3 / 4,
            p90,
            model_sha256: [7; 32],
            features_root: [8; 32],
        };
        self.forecast_with_rule(args, alert_rule_pda(station))
    }

    fn forecast_with_rule(&mut self, args: ForecastArgs, rule: Pubkey) -> Result<(), TransactionError> {
        let i = Instruction::new_with_bytes(
            cota_v2::id(),
            &ix::CommitForecast { args }.data(),
            acc::CommitForecast { authority: self.authority.pubkey(), registry: registry_pda(), rule }.to_account_metas(None),
        );
        self.send(&[i])
    }

    fn set_alert_rule(&mut self, station: &str, threshold_milli: i64) {
        let i = Instruction::new_with_bytes(
            cota_v2::id(),
            &ix::SetAlertRule { station_id: station.into(), threshold_milli, level: 1 }.data(),
            acc::SetAlertRule {
                authority: self.authority.pubkey(),
                registry: registry_pda(),
                rule: alert_rule_pda(station),
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[i]).unwrap();
    }

    fn set_outlook_rule(&mut self) {
        let args =
            OutlookRuleArgs { region: "guaiba".into(), threshold_milli: 90_000, window_secs: 5 * DAY, min_models: 2, rule_sha256: [9; 32] };
        let i = Instruction::new_with_bytes(
            cota_v2::id(),
            &ix::SetOutlookRule { args }.data(),
            acc::SetOutlookRule {
                authority: self.authority.pubkey(),
                registry: registry_pda(),
                rule: outlook_rule_pda("guaiba"),
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[i]).unwrap();
    }

    fn outlook(&mut self, rain_mean_milli: i64) -> Result<(), TransactionError> {
        let issued_at = self.registry().last_t_end;
        let h = OutlookHeader {
            region: "guaiba".into(),
            issued_at,
            window_end: issued_at + 5 * DAY,
            models: 3,
            rain_mean_milli,
            rain_min_milli: rain_mean_milli / 2,
            rain_max_milli: rain_mean_milli * 2,
            content_sha256: [3; 32],
        };
        let ed = self.ed25519_ix(&h.signing_message(), &[0, 2]);
        let i = Instruction::new_with_bytes(
            cota_v2::id(),
            &ix::CommitOutlook { header: h }.data(),
            acc::CommitOutlook {
                submitter: self.authority.pubkey(),
                registry: registry_pda(),
                rule: outlook_rule_pda("guaiba"),
                instructions: solana_instructions_sysvar::ID,
            }
            .to_account_metas(None),
        );
        self.send(&[ed, i])
    }

    fn create_vault(&mut self, id: u64, kind: u8, subject: &str, beneficiary: Pubkey) -> Pubkey {
        let args = VaultArgs {
            vault_id: id,
            beneficiary,
            kind,
            subject: subject.into(),
            payout_lamports: 1_000_000_000,
            cooldown_secs: 3 * DAY,
            accept_from: 0,
            expires_at: i64::MAX,
            deposit_lamports: 5_000_000_000,
        };
        let creator = self.authority.pubkey();
        let i = Instruction::new_with_bytes(
            cota_v2::id(),
            &ix::CreateVault { args }.data(),
            acc::CreateVault { creator, vault: vault_pda(&creator, id), system_program: system_program::ID }.to_account_metas(None),
        );
        self.send(&[i]).unwrap();
        vault_pda(&creator, id)
    }

    fn payout_ix(&self, vault: Pubkey, beneficiary: Pubkey, proof: TriggerProof) -> Instruction {
        let leaf = hash_leaf(&proof.record);
        Instruction::new_with_bytes(
            cota_v2::id(),
            &ix::TriggerPayout { proof }.data(),
            acc::TriggerPayout {
                submitter: self.authority.pubkey(),
                registry: registry_pda(),
                vault,
                beneficiary,
                payout: payout_pda(&vault, &leaf),
                system_program: system_program::ID,
            }
            .to_account_metas(None),
        )
    }

    /// Proof for record `index`, built by the reference implementation from the ledger.
    fn proof(&self, index: u64) -> TriggerProof {
        let p = self.mmr.proof(index).unwrap();
        TriggerProof { record: self.records[index as usize].clone(), leaf_index: index, siblings: p.siblings }
    }

    fn last_record(&self) -> (u64, Record) {
        let i = self.records.len() - 1;
        (i as u64, borsh::from_slice(&self.records[i]).unwrap())
    }
}

fn ser<T: AnchorSerialize>(v: &T) -> Vec<u8> {
    let mut d = Vec::new();
    v.serialize(&mut d).unwrap();
    d
}

fn record_appended_discriminator() -> [u8; 8] {
    use anchor_lang::Discriminator;
    cota_v2::events::RecordAppended::DISCRIMINATOR.try_into().unwrap()
}

#[test]
fn headers_have_the_same_bytes_as_cota_core_but_v2_domains() {
    let v2 = BatchHeader {
        batch_id: 3,
        t_start: 10,
        t_end: 20,
        leaf_count: 5,
        merkle_root: [1; 32],
        station_set_hash: [2; 32],
        prev_batch_root: [3; 32],
    };
    let core = cota_core::BatchHeader {
        batch_id: 3,
        t_start: 10,
        t_end: 20,
        leaf_count: 5,
        merkle_root: [1; 32],
        station_set_hash: [2; 32],
        prev_batch_root: [3; 32],
    };
    assert_eq!(ser(&v2), borsh::to_vec(&core).unwrap());
    assert_eq!(v2.signing_message(), core.signing_message_for(cota_core::Protocol::V2));
    assert_ne!(v2.signing_message(), core.signing_message());
}

#[test]
fn batches_follow_sequence_chain_and_threshold() {
    let mut env = Env::new();
    let h1 = env.next_day();
    let mut h2 = BatchHeader { batch_id: 2, t_start: h1.t_end, t_end: h1.t_end + DAY, prev_batch_root: h1.merkle_root, ..h1.clone() };
    use cota_v2::error::CotaError::*;

    // Only one signature: below the 2-of-3 threshold.
    assert_eq!(env.commit(&h2, &[1]), Err(custom(code(ThresholdNotMet))));
    // Skipping, breaking the chain, overlapping the previous window.
    assert_eq!(env.commit(&BatchHeader { batch_id: 3, ..h2.clone() }, &[0, 1]), Err(custom(code(BatchOutOfSequence))));
    assert_eq!(env.commit(&BatchHeader { prev_batch_root: [9; 32], ..h2.clone() }, &[0, 1]), Err(custom(code(BrokenHashChain))));
    assert_eq!(env.commit(&BatchHeader { t_start: h1.t_end - 1, ..h2.clone() }, &[0, 1]), Err(custom(code(InvalidWindow))));
    // A signature made for program v1 (old domain) is not valid in v2.
    let core = cota_core::BatchHeader {
        batch_id: h2.batch_id,
        t_start: h2.t_start,
        t_end: h2.t_end,
        leaf_count: h2.leaf_count,
        merkle_root: h2.merkle_root,
        station_set_hash: h2.station_set_hash,
        prev_batch_root: h2.prev_batch_root,
    };
    assert_eq!(env.commit_signed(&h2, &core.signing_message(), &[0, 1]), Err(custom(code(MessageMismatch))));

    h2.leaf_count = 4;
    env.commit(&h2, &[0, 2]).unwrap();
    let reg = env.registry();
    assert_eq!((reg.next_batch_id, reg.last_batch_root, reg.last_t_end), (3, h2.merkle_root, h2.t_end));
    let (index, record) = env.last_record();
    assert_eq!(index, 1);
    match record {
        Record::Batch(b) => assert_eq!((b.header, b.signers, b.signer_count), (h2, 0b101, 2)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn forecasts_must_come_before_the_observation_and_carry_their_alert() {
    let mut env = Env::new();
    use cota_v2::error::CotaError::*;
    assert_eq!(env.forecast("GLOFAS-GUAIBA", T0, 1), Err(custom0(code(NoAnchoredBatch))));
    env.set_alert_rule("GLOFAS-GUAIBA", 9_353_060);
    let h = env.next_day();

    // Target inside the anchored window: "forecast after seeing it".
    assert_eq!(env.forecast("GLOFAS-GUAIBA", h.t_end - 1, 1), Err(custom0(code(TargetAlreadyAnchored))));

    // Below the rule: recorded, no alert.
    env.forecast("GLOFAS-GUAIBA", h.t_end + DAY, 5_000_000).unwrap();
    let Record::Forecast(f) = env.last_record().1 else { panic!() };
    assert_eq!((f.alert, f.based_on_batch_id, f.issued_at), (None, 1, h.t_end));

    // Above the rule: the alert is decided by the program, in the same instruction.
    env.forecast("GLOFAS-GUAIBA", h.t_end + 2 * DAY, 9_400_000).unwrap();
    let Record::Forecast(f) = env.last_record().1 else { panic!() };
    assert_eq!(f.alert.map(|a| a.threshold_milli), Some(9_353_060));

    // Station without a rule: no alert, whatever the P90.
    env.forecast("GLOFAS-JACUI", h.t_end + DAY, 1_000_000_000_000).unwrap();
    let Record::Forecast(f) = env.last_record().1 else { panic!() };
    assert_eq!(f.alert, None);

    // The submitter cannot dodge an existing rule by passing another account.
    let args = ForecastArgs {
        station_id: "GLOFAS-GUAIBA".into(),
        target_ts: h.t_end + DAY,
        horizon_hours: 24,
        p10: 1,
        p50: 2,
        p90: 9_999_999,
        model_sha256: [0; 32],
        features_root: [0; 32],
    };
    assert!(env.forecast_with_rule(args, alert_rule_pda("GLOFAS-JACUI")).is_err());
}

#[test]
fn outlooks_degradations_stations_and_warnings_are_records() {
    let mut env = Env::new();
    use cota_v2::error::CotaError::*;
    env.set_outlook_rule();
    let station = StationArgs {
        station_id: "ANA-87450004".into(),
        name: "Guaíba no Cais Mauá".into(),
        source: 2,
        variable: 1,
        lat_e6: -30_021_100,
        lon_e6: -51_221_100,
        datum_id: Some("ANA-87450004-regua".into()),
    };
    let i = Instruction::new_with_bytes(cota_v2::id(), &ix::RegisterStation { station: station.clone() }.data(), env.authority_accounts());
    env.send(&[i]).unwrap();
    assert!(matches!(env.last_record().1, Record::Station(s) if s.station == station));

    assert_eq!(env.outlook(50_000), Err(custom(code(NoAnchoredBatch))));
    env.next_day();
    env.outlook(61_794).unwrap();
    let Record::Outlook(o) = env.last_record().1 else { panic!() };
    assert!(!o.triggered);
    env.outlook(95_000).unwrap();
    let Record::Outlook(o) = env.last_record().1 else { panic!() };
    assert!(o.triggered && o.signers == 0b101);

    // Degradation only for the batch just anchored.
    let deg = |batch_id| {
        Instruction::new_with_bytes(
            cota_v2::id(),
            &ix::ReportDegradation { batch_id, station_id: "ANA-87450004".into(), score_milli: 200, reason: 0 }.data(),
            env.authority_accounts(),
        )
    };
    let (stale, fresh) = (deg(0), deg(1));
    assert_eq!(env.send(&[stale]), Err(custom0(code(StaleBatch))));
    env.send(&[fresh]).unwrap();
    assert!(matches!(env.last_record().1, Record::Degradation(d) if d.batch_id == 1 && d.score_milli == 200));

    // Official warning: recorded with the signers; the same version twice is allowed (the earliest counts).
    let w = PublicAlertHeader {
        issuer: "INMET".into(),
        alert_id: "urn:oid:2.49.0.1.76.0.2024.4.29.1".into(),
        content_sha256: [5; 32],
        severity: 3,
        onset: T0,
        expires: T0 + DAY,
    };
    for _ in 0..2 {
        let ed = env.ed25519_ix(&w.signing_message(), &[1, 2]);
        let i = Instruction::new_with_bytes(cota_v2::id(), &ix::RecordPublicAlert { header: w.clone() }.data(), env.signed_accounts());
        env.send(&[ed, i]).unwrap();
    }
    assert!(matches!(env.last_record().1, Record::PublicAlert(p) if p.header == w && p.signer_count == 2));
}

#[test]
fn vault_pays_once_per_proven_trigger() {
    let mut env = Env::new();
    use cota_v2::error::CotaError::*;
    env.set_outlook_rule();
    let ben = Keypair::new().pubkey();
    let vault = env.create_vault(1, cota_v2::TRIGGER_EARLY_WARNING, "guaiba", ben);

    env.next_day();
    env.outlook(40_000).unwrap(); // did not fire
    let quiet = env.last_record().0;
    env.next_day();
    env.outlook(95_000).unwrap(); // fired
    let fired = env.last_record().0;

    // A record that did not fire is not a trigger.
    let i = env.payout_ix(vault, ben, env.proof(quiet));
    assert_eq!(env.send(&[i]), Err(custom0(code(TriggerMismatch))));

    // A forged record (fired, but never appended) is not in the accumulator.
    let mut forged = env.proof(fired);
    let Ok(Record::Outlook(mut o)) = borsh::from_slice::<Record>(&forged.record) else { panic!() };
    o.header.rain_mean_milli += 1;
    forged.record = ser(&Record::Outlook(o));
    let i = env.payout_ix(vault, ben, forged);
    assert_eq!(env.send(&[i]), Err(custom0(code(NotInAccumulator))));

    // The real one pays, once.
    let i = env.payout_ix(vault, ben, env.proof(fired));
    env.send(&[i]).unwrap();
    assert_eq!(env.svm.get_balance(&ben), Some(1_000_000_000));
    let i = env.payout_ix(vault, ben, env.proof(fired));
    assert!(env.send(&[i]).is_err(), "same trigger twice");

    // Next day fires again, but falls inside the 3-day cooldown.
    env.next_day();
    env.outlook(99_000).unwrap();
    let again = env.last_record().0;
    let i = env.payout_ix(vault, ben, env.proof(again));
    assert_eq!(env.send(&[i]), Err(custom0(code(CooldownActive))));

    let v = env.svm.get_account(&vault).unwrap();
    let v = Vault::try_deserialize(&mut v.data.as_slice()).unwrap();
    assert_eq!((v.payouts, v.total_paid), (1, 1_000_000_000));
}

#[test]
fn stale_proofs_fail_and_fresh_ones_pass() {
    let mut env = Env::new();
    env.set_outlook_rule();
    let ben = Keypair::new().pubkey();
    let vault = env.create_vault(7, cota_v2::TRIGGER_EARLY_WARNING, "guaiba", ben);
    env.next_day(); // record 0
    env.outlook(95_000).unwrap(); // record 1: peaks of height 1 (records 0,1)
    let fired = env.last_record().0;
    let old = env.proof(fired);
    env.next_day(); // record 2
    env.next_day(); // record 3: the four leaves merge into one peak of height 2
    let i = env.payout_ix(vault, ben, old);
    assert_eq!(env.send(&[i]), Err(custom0(code(cota_v2::error::CotaError::NotInAccumulator))));
    let i = env.payout_ix(vault, ben, env.proof(fired));
    env.send(&[i]).unwrap();
}

#[test]
fn flow_alert_vault_uses_forecast_records() {
    let mut env = Env::new();
    env.set_alert_rule("GLOFAS-GUAIBA", 9_353_060);
    let ben = Keypair::new().pubkey();
    let vault = env.create_vault(2, cota_v2::TRIGGER_FLOW_ALERT, "GLOFAS-GUAIBA", ben);
    let h = env.next_day();
    env.forecast("GLOFAS-GUAIBA", h.t_end + DAY, 5_000_000).unwrap();
    let below = env.last_record().0;
    env.forecast("GLOFAS-GUAIBA", h.t_end + 2 * DAY, 9_400_000).unwrap();
    let above = env.last_record().0;
    let i = env.payout_ix(vault, ben, env.proof(below));
    assert_eq!(env.send(&[i]), Err(custom0(code(cota_v2::error::CotaError::TriggerMismatch))));
    let i = env.payout_ix(vault, ben, env.proof(above));
    env.send(&[i]).unwrap();
    assert_eq!(env.svm.get_balance(&ben), Some(1_000_000_000));
}

#[test]
fn many_records_keep_the_peaks_identical_to_the_reference() {
    let mut env = Env::new();
    env.set_alert_rule("GLOFAS-GUAIBA", 9_353_060);
    for d in 0..40 {
        let h = env.next_day();
        env.forecast("GLOFAS-GUAIBA", h.t_end + DAY, 9_000_000 + d * 20_000).unwrap();
    }
    // `send` already compared the peaks after every transaction; check the size too.
    assert_eq!(env.registry().leaf_count, 80);
    let a = env.svm.get_account(&registry_pda()).unwrap();
    assert!(a.data.len() < 1_500, "registry is fixed-size: {} bytes", a.data.len());
}
