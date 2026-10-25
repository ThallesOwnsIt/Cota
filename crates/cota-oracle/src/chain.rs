//! Instruções e contas do programa `cota`, usando os tipos gerados pelo Anchor.

use anchor_lang::{prelude::Pubkey, solana_program::instruction::Instruction, AccountDeserialize, InstructionData, ToAccountMetas};
use anyhow::{Context, Result};
use cota::state::{AlertReceipt, AlertRule, BatchCommitment, Committee, ForecastCommitment, OutlookRecord, OutlookRule, Payout, PublicAlertRecord, Vault};
use cota_core::precompile::ed25519_instruction_data;
use cota_ingest::config::StationConfig;

pub use cota::ID as PROGRAM_ID;

pub fn committee_pda() -> Pubkey {
    Pubkey::find_program_address(&[cota::COMMITTEE_SEED], &PROGRAM_ID).0
}

pub fn station_pda(station_id: &str) -> Pubkey {
    Pubkey::find_program_address(&[cota::STATION_SEED, station_id.as_bytes()], &PROGRAM_ID).0
}

pub fn batch_pda(batch_id: u64) -> Pubkey {
    Pubkey::find_program_address(&[cota::BATCH_SEED, &batch_id.to_le_bytes()], &PROGRAM_ID).0
}

pub fn to_program_header(h: &cota_core::BatchHeader) -> cota::BatchHeader {
    cota::BatchHeader {
        batch_id: h.batch_id,
        t_start: h.t_start,
        t_end: h.t_end,
        leaf_count: h.leaf_count,
        merkle_root: h.merkle_root,
        station_set_hash: h.station_set_hash,
        prev_batch_root: h.prev_batch_root,
    }
}

/// `[precompile Ed25519 com as assinaturas, commit_batch(header)]` — nessa ordem,
/// porque o programa lê a instrução imediatamente anterior.
pub fn commit_batch_instructions(
    submitter: Pubkey,
    header: &cota_core::BatchHeader,
    signatures: &[([u8; 32], [u8; 64])],
) -> [Instruction; 2] {
    let ed25519 = Instruction::new_with_bytes(
        solana_sdk_ids::ed25519_program::ID,
        &ed25519_instruction_data(&header.signing_message(), signatures),
        vec![],
    );
    let commit = Instruction::new_with_bytes(
        PROGRAM_ID,
        &cota::instruction::CommitBatch { header: to_program_header(header) }.data(),
        cota::accounts::CommitBatch {
            submitter,
            committee: committee_pda(),
            batch: batch_pda(header.batch_id),
            instructions: solana_instructions_sysvar::ID,
            system_program: anchor_lang::solana_program::system_program::ID,
        }
        .to_account_metas(None),
    );
    [ed25519, commit]
}

pub fn init_committee_instruction(authority: Pubkey, members: Vec<Pubkey>, threshold: u8) -> Instruction {
    Instruction::new_with_bytes(
        PROGRAM_ID,
        &cota::instruction::InitCommittee { members, threshold }.data(),
        cota::accounts::InitCommittee {
            authority,
            committee: committee_pda(),
            system_program: anchor_lang::solana_program::system_program::ID,
        }
        .to_account_metas(None),
    )
}

pub fn register_station_instruction(authority: Pubkey, st: &StationConfig) -> Instruction {
    let args = cota::StationArgs {
        station_id: st.id.clone(),
        name: st.name.clone(),
        source: st.source as u8,
        variable: st.variable as u8,
        lat_e6: (st.lat * 1e6).round() as i32,
        lon_e6: (st.lon * 1e6).round() as i32,
        datum_id: st.datum_id.clone(),
    };
    Instruction::new_with_bytes(
        PROGRAM_ID,
        &cota::instruction::RegisterStation { args }.data(),
        cota::accounts::RegisterStation {
            authority,
            committee: committee_pda(),
            station: station_pda(&st.id),
            system_program: anchor_lang::solana_program::system_program::ID,
        }
        .to_account_metas(None),
    )
}

pub fn decode_committee(data: &[u8]) -> Result<Committee> {
    Committee::try_deserialize(&mut &data[..]).context("conta do comitê inválida")
}

pub fn decode_batch(data: &[u8]) -> Result<BatchCommitment> {
    BatchCommitment::try_deserialize(&mut &data[..]).context("conta de lote inválida")
}

pub fn forecast_pda(station_id: &str, based_on_batch_id: u64, target_ts: i64) -> Pubkey {
    Pubkey::find_program_address(
        &[cota::FORECAST_SEED, station_id.as_bytes(), &based_on_batch_id.to_le_bytes(), &target_ts.to_le_bytes()],
        &PROGRAM_ID,
    )
    .0
}

pub fn alert_rule_pda(station_id: &str) -> Pubkey {
    Pubkey::find_program_address(&[cota::ALERT_RULE_SEED, station_id.as_bytes()], &PROGRAM_ID).0
}

pub fn alert_pda(forecast: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[cota::ALERT_SEED, forecast.as_ref()], &PROGRAM_ID).0
}

pub fn report_degradation_instruction(authority: Pubkey, batch_id: u64, station_id: &str, score_milli: u16, reason: u8) -> Instruction {
    Instruction::new_with_bytes(
        PROGRAM_ID,
        &cota::instruction::ReportDegradation { batch_id, station_id: station_id.to_string(), score_milli, reason }.data(),
        cota::accounts::ReportDegradation {
            authority,
            committee: committee_pda(),
            batch: batch_pda(batch_id),
            station: station_pda(station_id),
        }
        .to_account_metas(None),
    )
}

/// `based_on_batch_id` precisa ser o último lote ancorado (entra na semente da conta).
pub fn commit_forecast_instruction(authority: Pubkey, based_on_batch_id: u64, args: cota::ForecastArgs) -> Instruction {
    let forecast = forecast_pda(&args.station_id, based_on_batch_id, args.target_ts);
    let station = station_pda(&args.station_id);
    Instruction::new_with_bytes(
        PROGRAM_ID,
        &cota::instruction::CommitForecast { args }.data(),
        cota::accounts::CommitForecast {
            authority,
            committee: committee_pda(),
            station,
            forecast,
            system_program: anchor_lang::solana_program::system_program::ID,
        }
        .to_account_metas(None),
    )
}

pub fn set_alert_rule_instruction(authority: Pubkey, station_id: &str, threshold_milli: i64, level: u8) -> Instruction {
    Instruction::new_with_bytes(
        PROGRAM_ID,
        &cota::instruction::SetAlertRule { station_id: station_id.to_string(), threshold_milli, level }.data(),
        cota::accounts::SetAlertRule {
            authority,
            committee: committee_pda(),
            station: station_pda(station_id),
            rule: alert_rule_pda(station_id),
            system_program: anchor_lang::solana_program::system_program::ID,
        }
        .to_account_metas(None),
    )
}

pub fn issue_alert_instruction(submitter: Pubkey, forecast: Pubkey, station_id: &str) -> Instruction {
    Instruction::new_with_bytes(
        PROGRAM_ID,
        &cota::instruction::IssueAlert {}.data(),
        cota::accounts::IssueAlert {
            submitter,
            committee: committee_pda(),
            forecast,
            rule: alert_rule_pda(station_id),
            alert: alert_pda(&forecast),
            system_program: anchor_lang::solana_program::system_program::ID,
        }
        .to_account_metas(None),
    )
}

pub fn decode_forecast(data: &[u8]) -> Result<ForecastCommitment> {
    ForecastCommitment::try_deserialize(&mut &data[..]).context("conta de previsão inválida")
}

pub fn decode_alert_rule(data: &[u8]) -> Result<AlertRule> {
    AlertRule::try_deserialize(&mut &data[..]).context("conta de regra de alerta inválida")
}

pub fn decode_alert(data: &[u8]) -> Result<AlertReceipt> {
    AlertReceipt::try_deserialize(&mut &data[..]).context("conta de alerta inválida")
}

pub fn public_alert_pda(content_sha256: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[cota::PUBLIC_ALERT_SEED, content_sha256], &PROGRAM_ID).0
}

pub fn to_program_alert_header(h: &cota_core::PublicAlertHeader) -> cota::PublicAlertHeader {
    cota::PublicAlertHeader {
        issuer: h.issuer.clone(),
        alert_id: h.alert_id.clone(),
        content_sha256: h.content_sha256,
        severity: h.severity,
        onset: h.onset,
        expires: h.expires,
    }
}

/// `[precompile Ed25519, record_public_alert(header)]`, nessa ordem.
pub fn record_public_alert_instructions(
    submitter: Pubkey,
    header: &cota_core::PublicAlertHeader,
    signatures: &[([u8; 32], [u8; 64])],
) -> [Instruction; 2] {
    let ed25519 = Instruction::new_with_bytes(
        solana_sdk_ids::ed25519_program::ID,
        &ed25519_instruction_data(&header.signing_message(), signatures),
        vec![],
    );
    let record = Instruction::new_with_bytes(
        PROGRAM_ID,
        &cota::instruction::RecordPublicAlert { header: to_program_alert_header(header) }.data(),
        cota::accounts::RecordPublicAlert {
            submitter,
            committee: committee_pda(),
            record: public_alert_pda(&header.content_sha256),
            instructions: solana_instructions_sysvar::ID,
            system_program: anchor_lang::solana_program::system_program::ID,
        }
        .to_account_metas(None),
    );
    [ed25519, record]
}

pub fn decode_public_alert(data: &[u8]) -> Result<PublicAlertRecord> {
    PublicAlertRecord::try_deserialize(&mut &data[..]).context("conta de aviso oficial inválida")
}

pub fn set_authority_instruction(authority: Pubkey, new_authority: Pubkey) -> Instruction {
    Instruction::new_with_bytes(
        PROGRAM_ID,
        &cota::instruction::SetAuthority { new_authority }.data(),
        cota::accounts::SetAuthority { authority, committee: committee_pda() }.to_account_metas(None),
    )
}

pub fn outlook_rule_pda(region: &str) -> Pubkey {
    Pubkey::find_program_address(&[cota::OUTLOOK_RULE_SEED, region.as_bytes()], &PROGRAM_ID).0
}

pub fn outlook_pda(region: &str, issued_at: i64) -> Pubkey {
    Pubkey::find_program_address(&[cota::OUTLOOK_SEED, region.as_bytes(), &issued_at.to_le_bytes()], &PROGRAM_ID).0
}

pub fn set_outlook_rule_instruction(authority: Pubkey, args: cota::OutlookRuleArgs) -> Instruction {
    let rule = outlook_rule_pda(&args.region);
    Instruction::new_with_bytes(
        PROGRAM_ID,
        &cota::instruction::SetOutlookRule { args }.data(),
        cota::accounts::SetOutlookRule {
            authority,
            committee: committee_pda(),
            rule,
            system_program: anchor_lang::solana_program::system_program::ID,
        }
        .to_account_metas(None),
    )
}

pub fn to_program_outlook_header(h: &cota_core::OutlookHeader) -> cota::OutlookHeader {
    cota::OutlookHeader {
        region: h.region.clone(),
        issued_at: h.issued_at,
        window_end: h.window_end,
        models: h.models,
        rain_mean_milli: h.rain_mean_milli,
        rain_min_milli: h.rain_min_milli,
        rain_max_milli: h.rain_max_milli,
        content_sha256: h.content_sha256,
    }
}

/// `[precompile Ed25519, commit_outlook(header)]`, nessa ordem.
pub fn commit_outlook_instructions(
    submitter: Pubkey,
    header: &cota_core::OutlookHeader,
    signatures: &[([u8; 32], [u8; 64])],
) -> [Instruction; 2] {
    let ed25519 = Instruction::new_with_bytes(
        solana_sdk_ids::ed25519_program::ID,
        &ed25519_instruction_data(&header.signing_message(), signatures),
        vec![],
    );
    let commit = Instruction::new_with_bytes(
        PROGRAM_ID,
        &cota::instruction::CommitOutlook { header: to_program_outlook_header(header) }.data(),
        cota::accounts::CommitOutlook {
            submitter,
            committee: committee_pda(),
            rule: outlook_rule_pda(&header.region),
            outlook: outlook_pda(&header.region, header.issued_at),
            instructions: solana_instructions_sysvar::ID,
            system_program: anchor_lang::solana_program::system_program::ID,
        }
        .to_account_metas(None),
    );
    [ed25519, commit]
}

pub fn decode_outlook_rule(data: &[u8]) -> Result<OutlookRule> {
    OutlookRule::try_deserialize(&mut &data[..]).context("conta de regra de perspectiva inválida")
}

pub fn decode_outlook(data: &[u8]) -> Result<OutlookRecord> {
    OutlookRecord::try_deserialize(&mut &data[..]).context("conta de perspectiva inválida")
}

pub fn vault_pda(creator: &Pubkey, vault_id: u64) -> Pubkey {
    Pubkey::find_program_address(&[cota::VAULT_SEED, creator.as_ref(), &vault_id.to_le_bytes()], &PROGRAM_ID).0
}

pub fn payout_pda(vault: &Pubkey, trigger: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[cota::PAYOUT_SEED, vault.as_ref(), trigger.as_ref()], &PROGRAM_ID).0
}

pub fn create_vault_instruction(creator: Pubkey, args: cota::VaultArgs) -> Instruction {
    let vault = vault_pda(&creator, args.vault_id);
    Instruction::new_with_bytes(
        PROGRAM_ID,
        &cota::instruction::CreateVault { args }.data(),
        cota::accounts::CreateVault { creator, vault, system_program: anchor_lang::solana_program::system_program::ID }
            .to_account_metas(None),
    )
}

pub fn trigger_payout_instruction(submitter: Pubkey, vault: Pubkey, trigger: Pubkey, beneficiary: Pubkey) -> Instruction {
    Instruction::new_with_bytes(
        PROGRAM_ID,
        &cota::instruction::TriggerPayout {}.data(),
        cota::accounts::TriggerPayout {
            submitter,
            vault,
            trigger,
            beneficiary,
            payout: payout_pda(&vault, &trigger),
            system_program: anchor_lang::solana_program::system_program::ID,
        }
        .to_account_metas(None),
    )
}

pub fn close_vault_instruction(creator: Pubkey, vault: Pubkey) -> Instruction {
    Instruction::new_with_bytes(
        PROGRAM_ID,
        &cota::instruction::CloseVault {}.data(),
        cota::accounts::CloseVault { creator, vault }.to_account_metas(None),
    )
}

pub fn decode_vault(data: &[u8]) -> Result<Vault> {
    Vault::try_deserialize(&mut &data[..]).context("conta de cofre inválida")
}

pub fn decode_payout(data: &[u8]) -> Result<Payout> {
    Payout::try_deserialize(&mut &data[..]).context("conta de pagamento inválida")
}
