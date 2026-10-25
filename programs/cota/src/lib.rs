pub mod constants;
pub mod ed25519;
pub mod error;
pub mod events;
pub mod instructions;
pub mod state;

use anchor_lang::prelude::*;

pub use constants::*;
pub use instructions::*;
pub use state::*;

declare_id!("4FChu6PkKzsFQRAtQt2oiGt7TuC4uZhBNSSHqDoVNFsa");

#[program]
pub mod cota {
    use super::*;

    /// Cria o comitê de oráculos com `members` e limiar `threshold` (k de n).
    pub fn init_committee(ctx: Context<InitCommittee>, members: Vec<Pubkey>, threshold: u8) -> Result<()> {
        crate::instructions::init_committee::handle_init_committee(ctx, members, threshold)
    }

    /// Substitui a chave de um membro, mantendo sua posição.
    pub fn rotate_member(ctx: Context<RotateMember>, old: Pubkey, new: Pubkey) -> Result<()> {
        crate::instructions::rotate_member::handle_rotate_member(ctx, old, new)
    }

    /// Passa a autoridade do comitê para outra chave.
    pub fn set_authority(ctx: Context<SetAuthority>, new_authority: Pubkey) -> Result<()> {
        crate::instructions::set_authority::handle_set_authority(ctx, new_authority)
    }

    /// Registra uma estação (ou ponto de grade) com coordenadas e datum.
    pub fn register_station(ctx: Context<RegisterStation>, args: StationArgs) -> Result<()> {
        crate::instructions::register_station::handle_register_station(ctx, args)
    }

    /// Ancora um lote assinado; exige o precompile Ed25519 como instrução anterior.
    pub fn commit_batch(ctx: Context<CommitBatch>, header: BatchHeader) -> Result<()> {
        crate::instructions::commit_batch::handle_commit_batch(ctx, header)
    }

    /// Emite `SourceDegraded` para uma estação no lote mais recente.
    pub fn report_degradation(
        ctx: Context<ReportDegradation>,
        batch_id: u64,
        station_id: String,
        score_milli: u16,
        reason: u8,
    ) -> Result<()> {
        crate::instructions::report_degradation::handle_report_degradation(ctx, batch_id, station_id, score_milli, reason)
    }

    /// Grava uma previsão P10/P50/P90 antes de a leitura prevista ser ancorada.
    pub fn commit_forecast(ctx: Context<CommitForecast>, args: ForecastArgs) -> Result<()> {
        crate::instructions::commit_forecast::handle_commit_forecast(ctx, args)
    }

    /// Fixa (uma única vez) o limiar de alerta de uma estação.
    pub fn set_alert_rule(ctx: Context<SetAlertRule>, station_id: String, threshold_milli: i64, level: u8) -> Result<()> {
        crate::instructions::alerts::handle_set_alert_rule(ctx, station_id, threshold_milli, level)
    }

    /// Registra um aviso oficial (ex.: INMET) atestado por ≥ k oráculos.
    pub fn record_public_alert(ctx: Context<RecordPublicAlert>, header: PublicAlertHeader) -> Result<()> {
        crate::instructions::record_public_alert::handle_record_public_alert(ctx, header)
    }

    /// Fixa (uma única vez) a regra do alerta antecipado por chuva prevista de uma região.
    pub fn set_outlook_rule(ctx: Context<SetOutlookRule>, args: OutlookRuleArgs) -> Result<()> {
        crate::instructions::outlook::handle_set_outlook_rule(ctx, args)
    }

    /// Registra a chuva prevista atestada por ≥ k oráculos e emite `EarlyWarning` se cruzar a regra.
    pub fn commit_outlook(ctx: Context<CommitOutlook>, header: OutlookHeader) -> Result<()> {
        crate::instructions::outlook::handle_commit_outlook(ctx, header)
    }

    /// Emite o recibo de alerta de uma previsão cuja P90 cruzou o limiar.
    pub fn issue_alert(ctx: Context<IssueAlert>) -> Result<()> {
        crate::instructions::alerts::handle_issue_alert(ctx)
    }
}
