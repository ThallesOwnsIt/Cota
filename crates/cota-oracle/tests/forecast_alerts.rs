//! Previsões, alertas e degradação no programa compilado (LiteSVM): cada regra que o
//! programa impõe, com o código de erro esperado.

use anchor_lang::{prelude::Pubkey, solana_program::instruction::Instruction};
use cota::error::CotaError;
use cota_core::{Batch, Reading, Source, Variable};
use cota_ingest::config::StationConfig;
use cota_oracle::chain;
use ed25519_dalek::{Signer as _, SigningKey};
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::{InstructionError, Transaction, TransactionError};

const DAY: i64 = 86_400;
const STATION: &str = "GLOFAS-GUAIBA";

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
        env.ok(&[chain::init_committee_instruction(env.authority.pubkey(), members, 2)]);
        let st = StationConfig {
            id: STATION.into(),
            name: "Guaíba".into(),
            source: Source::OpenMeteoGlofas,
            variable: Variable::RiverDischarge,
            lat: -30.075,
            lon: -51.325,
            datum_id: None,
        };
        env.ok(&[chain::register_station_instruction(env.authority.pubkey(), &st)]);
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

    /// Ancora o dia `day` assinado por 2 nós.
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
        let ixs = chain::commit_batch_instructions(self.authority.pubkey(), &b.header, &sigs);
        self.ok(&ixs);
        self.prev = b.header.merkle_root;
        self.next_batch += 1;
    }

    fn last_batch(&self) -> u64 {
        self.next_batch - 1
    }

    fn forecast_ix(&self, target_ts: i64, p: [i64; 3]) -> Instruction {
        chain::commit_forecast_instruction(self.authority.pubkey(), self.last_batch(), args(target_ts, p))
    }
}

fn args(target_ts: i64, [p10, p50, p90]: [i64; 3]) -> cota::ForecastArgs {
    cota::ForecastArgs {
        station_id: STATION.into(),
        target_ts,
        horizon_hours: 24,
        p10,
        p50,
        p90,
        model_sha256: [0xaa; 32],
        features_root: [0xbb; 32],
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
fn forecast_is_bound_to_the_latest_anchored_batch() {
    let mut env = Env::new();
    let ix = env.forecast_ix(101 * DAY, [1, 2, 3]);
    assert_eq!(err(env.send(&[ix])), code(CotaError::NoAnchoredBatch));

    env.anchor_day(100);
    env.ok(&[env.forecast_ix(101 * DAY, [4_000_000, 5_000_000, 9_000_000])]);

    let pda = chain::forecast_pda(STATION, 1, 101 * DAY);
    let f = chain::decode_forecast(&env.svm.get_account(&pda).unwrap().data).unwrap();
    assert_eq!(f.based_on_batch_id, 1);
    assert_eq!(f.based_on_root, env.prev, "a previsão aponta para a raiz que existia na emissão");
    assert_eq!(f.issued_at, 101 * DAY, "emitida no fim da janela ancorada");
    assert_eq!((f.p10, f.p50, f.p90), (4_000_000, 5_000_000, 9_000_000));
    assert_eq!(f.model_sha256, [0xaa; 32]);
    assert_eq!(f.variable, Variable::RiverDischarge as u8);

    // A mesma previsão não pode ser regravada.
    assert!(env.send(&[env.forecast_ix(101 * DAY, [1, 2, 3])]).is_err());
}

#[test]
fn cannot_forecast_what_was_already_anchored() {
    let mut env = Env::new();
    env.anchor_day(100);
    env.anchor_day(101);
    // Alvo dentro de uma janela já ancorada (o "previsto depois de ver").
    let ix = env.forecast_ix(101 * DAY, [1, 2, 3]);
    assert_eq!(err(env.send(&[ix])), code(CotaError::TargetAlreadyAnchored));
    let ix = env.forecast_ix(100 * DAY + 3_600, [1, 2, 3]);
    assert_eq!(err(env.send(&[ix])), code(CotaError::TargetAlreadyAnchored));
    env.ok(&[env.forecast_ix(102 * DAY, [1, 2, 3])]);
}

#[test]
fn forecast_validation_and_authority() {
    let mut env = Env::new();
    env.anchor_day(100);
    let ix = env.forecast_ix(101 * DAY, [5, 4, 6]);
    assert_eq!(err(env.send(&[ix])), code(CotaError::InvalidForecast), "quantis cruzados");

    let mut zero_h = args(101 * DAY, [1, 2, 3]);
    zero_h.horizon_hours = 0;
    let ix = chain::commit_forecast_instruction(env.authority.pubkey(), 1, zero_h);
    assert_eq!(err(env.send(&[ix])), code(CotaError::InvalidForecast), "horizonte nulo");

    let intruder = Keypair::new();
    env.svm.airdrop(&intruder.pubkey(), 1_000_000_000).unwrap();
    let ix = chain::commit_forecast_instruction(intruder.pubkey(), 1, args(101 * DAY, [1, 2, 3]));
    assert_eq!(err(env.send_as(&intruder, &[ix])), code(CotaError::Unauthorized));
}

#[test]
fn alert_requires_p90_over_threshold_before_observation() {
    let mut env = Env::new();
    env.ok(&[chain::set_alert_rule_instruction(env.authority.pubkey(), STATION, 9_353_000, 1)]);
    // A regra é imutável.
    assert!(env.send(&[chain::set_alert_rule_instruction(env.authority.pubkey(), STATION, 1, 1)]).is_err());

    env.anchor_day(100);
    env.ok(&[env.forecast_ix(101 * DAY, [5_000_000, 6_000_000, 9_000_000])]);
    env.ok(&[env.forecast_ix(102 * DAY, [6_000_000, 8_000_000, 9_996_000])]);
    let low = chain::forecast_pda(STATION, 1, 101 * DAY);
    let high = chain::forecast_pda(STATION, 1, 102 * DAY);

    // Qualquer conta pode emitir, mas só se a P90 cruza o limiar.
    let crank = Keypair::new();
    env.svm.airdrop(&crank.pubkey(), 1_000_000_000).unwrap();
    let ix = chain::issue_alert_instruction(crank.pubkey(), low, STATION);
    assert_eq!(err(env.send_as(&crank, &[ix])), code(CotaError::BelowAlertThreshold));
    env.send_as(&crank, &[chain::issue_alert_instruction(crank.pubkey(), high, STATION)]).unwrap();

    let a = chain::decode_alert(&env.svm.get_account(&chain::alert_pda(&high)).unwrap().data).unwrap();
    assert_eq!((a.forecast, a.p90, a.threshold_milli, a.target_ts), (high, 9_996_000, 9_353_000, 102 * DAY));
    assert!(env.send_as(&crank, &[chain::issue_alert_instruction(crank.pubkey(), high, STATION)]).is_err(), "um recibo por previsão");

    // Depois que o alvo foi ancorado, um alerta deixaria de ser alerta.
    env.anchor_day(101);
    env.ok(&[env.forecast_ix(102 * DAY, [6_000_000, 8_000_000, 9_500_000])]);
    let from_day_101 = chain::forecast_pda(STATION, 2, 102 * DAY);
    env.anchor_day(102);
    let ix = chain::issue_alert_instruction(crank.pubkey(), from_day_101, STATION);
    assert_eq!(err(env.send_as(&crank, &[ix])), code(CotaError::TargetAlreadyAnchored));
}

#[test]
fn degradation_only_for_latest_batch_by_authority() {
    let mut env = Env::new();
    env.anchor_day(100);
    env.anchor_day(101);
    let auth = env.authority.pubkey();
    env.ok(&[chain::report_degradation_instruction(auth, 2, STATION, 545, 0)]);

    let ix = chain::report_degradation_instruction(auth, 1, STATION, 545, 0);
    assert_eq!(err(env.send(&[ix])), code(CotaError::StaleBatch));
    let ix = chain::report_degradation_instruction(auth, 2, STATION, 545, 9);
    assert_eq!(err(env.send(&[ix])), code(CotaError::InvalidDegradation));
    let ix = chain::report_degradation_instruction(auth, 2, STATION, 1001, 0);
    assert_eq!(err(env.send(&[ix])), code(CotaError::InvalidDegradation));

    let intruder = Keypair::new();
    env.svm.airdrop(&intruder.pubkey(), 1_000_000_000).unwrap();
    let ix = chain::report_degradation_instruction(intruder.pubkey(), 2, STATION, 545, 0);
    assert_eq!(err(env.send_as(&intruder, &[ix])), code(CotaError::Unauthorized));
}

#[test]
fn authority_can_be_handed_to_an_operator() {
    let mut env = Env::new();
    let operator = Keypair::new();
    env.svm.airdrop(&operator.pubkey(), 1_000_000_000).unwrap();
    env.ok(&[chain::set_authority_instruction(env.authority.pubkey(), operator.pubkey())]);
    let c = chain::decode_committee(&env.svm.get_account(&chain::committee_pda()).unwrap().data).unwrap();
    assert_eq!(c.authority, operator.pubkey());

    // A antiga não manda mais; o operador sim.
    let ix = chain::set_alert_rule_instruction(env.authority.pubkey(), STATION, 1, 1);
    assert_eq!(err(env.send(&[ix])), code(CotaError::Unauthorized));
    env.send_as(&operator, &[chain::set_alert_rule_instruction(operator.pubkey(), STATION, 1, 1)]).unwrap();
    let ix = chain::set_authority_instruction(env.authority.pubkey(), env.authority.pubkey());
    assert_eq!(err(env.send(&[ix])), code(CotaError::Unauthorized));
}
