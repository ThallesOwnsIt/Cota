//! Cofres paramétricos (`create_vault`, `trigger_payout`, `close_vault`) no programa compilado.

use anchor_lang::{prelude::{Clock, Pubkey}, solana_program::instruction::Instruction};
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
const SOL: u64 = 1_000_000_000;
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
        svm.airdrop(&authority.pubkey(), 100 * SOL).unwrap();
        let nodes: Vec<SigningKey> = (1..=3u8).map(|i| SigningKey::from_bytes(&[i; 32])).collect();
        let members = nodes.iter().map(|k| Pubkey::new_from_array(k.verifying_key().to_bytes())).collect();
        let mut env = Self { svm, authority, nodes, prev: [0; 32], next_batch: 1 };
        let auth = env.authority.pubkey();
        env.ok(&[chain::init_committee_instruction(auth, members, 2)]);
        let st = StationConfig {
            id: STATION.into(),
            name: "Guaíba".into(),
            source: Source::OpenMeteoGlofas,
            variable: Variable::RiverDischarge,
            lat: -30.075,
            lon: -51.325,
            datum_id: None,
        };
        env.ok(&[chain::register_station_instruction(auth, &st)]);
        env.ok(&[chain::set_alert_rule_instruction(auth, STATION, 9_353_000, 1)]);
        let rule = cota::OutlookRuleArgs { region: REGION.into(), threshold_milli: 90_000, window_secs: 5 * DAY, min_models: 2, rule_sha256: [0xcc; 32] };
        env.ok(&[chain::set_outlook_rule_instruction(auth, rule)]);
        env
    }

    fn send_as(&mut self, payer: &Keypair, ixs: &[Instruction]) -> Result<(), TransactionError> {
        let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &[payer], self.svm.latest_blockhash());
        let res = self.svm.send_transaction(tx).map(|_| ()).map_err(|e| e.err);
        self.svm.expire_blockhash();
        res
    }

    fn send(&mut self, ixs: &[Instruction]) -> Result<(), TransactionError> {
        let auth = self.authority.insecure_clone();
        self.send_as(&auth, ixs)
    }

    fn ok(&mut self, ixs: &[Instruction]) {
        self.send(ixs).unwrap();
    }

    fn now(&self) -> i64 {
        self.svm.get_sysvar::<Clock>().unix_timestamp
    }

    fn warp(&mut self, secs: i64) {
        let mut c = self.svm.get_sysvar::<Clock>();
        c.unix_timestamp += secs;
        self.svm.set_sysvar(&c);
    }

    fn balance(&self, pk: &Pubkey) -> u64 {
        self.svm.get_account(pk).map_or(0, |a| a.lamports)
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
        let msg = b.header.signing_message();
        let sigs: Vec<_> = self.nodes[..2].iter().map(|k| (k.verifying_key().to_bytes(), k.sign(&msg).to_bytes())).collect();
        self.ok(&chain::commit_batch_instructions(self.authority.pubkey(), &b.header, &sigs));
        self.prev = b.header.merkle_root;
        self.next_batch += 1;
    }

    /// Ancora o dia e registra a perspectiva de chuva do fim dele; devolve a conta.
    fn outlook(&mut self, day: i64, mean_mm: i64) -> Pubkey {
        self.anchor_day(day);
        let issued_at = (day + 1) * DAY;
        let h = OutlookHeader {
            region: REGION.into(),
            issued_at,
            window_end: issued_at + 5 * DAY,
            models: 3,
            rain_mean_milli: mean_mm * 1000,
            rain_min_milli: mean_mm * 1000,
            rain_max_milli: mean_mm * 1000,
            content_sha256: content_hash(&format!("{day}:{mean_mm}")),
        };
        let msg = h.signing_message();
        let sigs: Vec<_> = self.nodes[..2].iter().map(|k| (k.verifying_key().to_bytes(), k.sign(&msg).to_bytes())).collect();
        self.ok(&chain::commit_outlook_instructions(self.authority.pubkey(), &h, &sigs));
        chain::outlook_pda(REGION, issued_at)
    }

    /// Ancora o dia, grava uma previsão acima do limiar e emite o `AlertReceipt`.
    fn flow_alert(&mut self, day: i64) -> Pubkey {
        self.anchor_day(day);
        let args = cota::ForecastArgs {
            station_id: STATION.into(),
            target_ts: (day + 1) * DAY,
            horizon_hours: 24,
            p10: 8_000_000,
            p50: 9_000_000,
            p90: 10_000_000,
            model_sha256: [0xaa; 32],
            features_root: [0xbb; 32],
        };
        let batch = self.next_batch - 1;
        self.ok(&[chain::commit_forecast_instruction(self.authority.pubkey(), batch, args)]);
        let forecast = chain::forecast_pda(STATION, batch, (day + 1) * DAY);
        self.ok(&[chain::issue_alert_instruction(self.authority.pubkey(), forecast, STATION)]);
        chain::alert_pda(&forecast)
    }

    /// Cofre de `creator` que paga 1 SOL por evento ao beneficiário `ben`, carência de 3 dias.
    fn create(&mut self, creator: &Keypair, id: u64, (kind, subject): (u8, &str), ben: Pubkey, accept_from: i64, deposit: u64) -> Pubkey {
        let args = cota::VaultArgs {
            vault_id: id,
            beneficiary: ben,
            kind,
            subject: subject.into(),
            payout_lamports: SOL,
            cooldown_secs: 3 * DAY,
            accept_from,
            expires_at: self.now() + 30 * DAY,
            deposit_lamports: deposit,
        };
        self.send_as(creator, &[chain::create_vault_instruction(creator.pubkey(), args)]).unwrap();
        chain::vault_pda(&creator.pubkey(), id)
    }

    fn pay(&mut self, vault: Pubkey, trigger: Pubkey, beneficiary: Pubkey) -> Result<(), TransactionError> {
        let crank = Keypair::new();
        self.svm.airdrop(&crank.pubkey(), SOL).unwrap();
        self.send_as(&crank, &[chain::trigger_payout_instruction(crank.pubkey(), vault, trigger, beneficiary)])
    }

    fn funder(&mut self) -> Keypair {
        let k = Keypair::new();
        self.svm.airdrop(&k.pubkey(), 20 * SOL).unwrap();
        k
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
fn early_warning_pays_once_per_event() {
    let mut env = Env::new();
    let fund = env.funder();
    let ben = Pubkey::new_unique();
    let vault = env.create(&fund, 1, (cota::TRIGGER_EARLY_WARNING, REGION), ben, 0, 3 * SOL);

    let high = env.outlook(100, 141);
    env.pay(vault, high, ben).unwrap();
    assert_eq!(env.balance(&ben), SOL);
    let p = chain::decode_payout(&env.svm.get_account(&chain::payout_pda(&vault, &high)).unwrap().data).unwrap();
    assert_eq!((p.trigger, p.amount, p.event_at, p.beneficiary), (high, SOL, 101 * DAY, ben));
    assert!(env.pay(vault, high, ben).is_err(), "um pagamento por gatilho");

    // Chuva abaixo da regra não é gatilho.
    let low = env.outlook(101, 40);
    assert_eq!(err(env.pay(vault, low, ben)), code(CotaError::TriggerMismatch));
    // O dia seguinte do mesmo evento cai na carência de 3 dias.
    let same_event = env.outlook(102, 120);
    assert_eq!(err(env.pay(vault, same_event, ben)), code(CotaError::CooldownActive));
    env.anchor_day(103);
    let next_event = env.outlook(104, 95);
    env.pay(vault, next_event, ben).unwrap();
    assert_eq!(env.balance(&ben), 2 * SOL);

    let v = chain::decode_vault(&env.svm.get_account(&vault).unwrap().data).unwrap();
    assert_eq!((v.payouts, v.total_paid, v.last_event_at), (2, 2 * SOL, 105 * DAY));
}

#[test]
fn only_matching_triggers_from_the_program() {
    let mut env = Env::new();
    let fund = env.funder();
    let ben = Pubkey::new_unique();
    let flow = env.create(&fund, 1, (cota::TRIGGER_FLOW_ALERT, STATION), ben, 0, 3 * SOL);
    let other_station = env.create(&fund, 2, (cota::TRIGGER_FLOW_ALERT, "GLOFAS-JACUI"), ben, 0, 3 * SOL);

    let outlook = env.outlook(100, 141);
    let alert = env.flow_alert(101);
    assert_eq!(err(env.pay(flow, outlook, ben)), code(CotaError::TriggerMismatch), "tipo errado");
    assert_eq!(err(env.pay(other_station, alert, ben)), code(CotaError::TriggerMismatch), "estação errada");
    assert_eq!(err(env.pay(flow, fund.pubkey(), ben)), code(CotaError::TriggerMismatch), "conta de fora do programa");
    assert_eq!(err(env.pay(flow, alert, Pubkey::new_unique())), code(CotaError::TriggerMismatch), "outro beneficiário");
    env.pay(flow, alert, ben).unwrap();
    assert_eq!(env.balance(&ben), SOL);
}

#[test]
fn no_retroactive_triggers_and_pays_what_is_left() {
    let mut env = Env::new();
    let fund = env.funder();
    let ben = Pubkey::new_unique();
    let old = env.outlook(100, 141);
    env.warp(60);
    let vault = env.create(&fund, 1, (cota::TRIGGER_EARLY_WARNING, REGION), ben, env.now(), SOL / 2);
    assert_eq!(err(env.pay(vault, old, ben)), code(CotaError::TriggerTooEarly));

    env.anchor_day(101);
    env.anchor_day(102);
    let fresh = env.outlook(103, 141);
    env.pay(vault, fresh, ben).unwrap();
    assert_eq!(env.balance(&ben), SOL / 2, "paga o que resta acima do aluguel");
    env.anchor_day(104);
    env.anchor_day(105);
    env.anchor_day(106);
    let later = env.outlook(107, 141);
    assert_eq!(err(env.pay(vault, later, ben)), code(CotaError::VaultEmpty));
}

#[test]
fn creator_recovers_funds_only_after_expiry() {
    let mut env = Env::new();
    let fund = env.funder();
    let ben = Pubkey::new_unique();
    let vault = env.create(&fund, 7, (cota::TRIGGER_EARLY_WARNING, REGION), ben, 0, 3 * SOL);
    let trigger = env.outlook(100, 141);

    assert_eq!(err(env.send_as(&fund, &[chain::close_vault_instruction(fund.pubkey(), vault)])), code(CotaError::VaultActive));
    let intruder = env.funder();
    assert!(env.send_as(&intruder, &[chain::close_vault_instruction(intruder.pubkey(), vault)]).is_err());

    env.warp(31 * DAY);
    assert_eq!(err(env.pay(vault, trigger, ben)), code(CotaError::VaultExpired));
    let before = env.balance(&fund.pubkey());
    env.send_as(&fund, &[chain::close_vault_instruction(fund.pubkey(), vault)]).unwrap();
    assert!(env.balance(&fund.pubkey()) > before + 3 * SOL - 10_000_000, "depósito devolvido");
    assert!(env.svm.get_account(&vault).is_none_or(|a| a.lamports == 0));
}
