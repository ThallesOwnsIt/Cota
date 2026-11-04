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

    /// Creates the oracle committee with `members` and threshold `threshold` (k of n).
    pub fn init_committee(ctx: Context<InitCommittee>, members: Vec<Pubkey>, threshold: u8) -> Result<()> {
        crate::instructions::init_committee::handle_init_committee(ctx, members, threshold)
    }

    /// Replaces a member's key, keeping its position.
    pub fn rotate_member(ctx: Context<RotateMember>, old: Pubkey, new: Pubkey) -> Result<()> {
        crate::instructions::rotate_member::handle_rotate_member(ctx, old, new)
    }

    /// Hands the committee authority to another key.
    pub fn set_authority(ctx: Context<SetAuthority>, new_authority: Pubkey) -> Result<()> {
        crate::instructions::set_authority::handle_set_authority(ctx, new_authority)
    }

    /// Registers a station (or grid point) with coordinates and datum.
    pub fn register_station(ctx: Context<RegisterStation>, args: StationArgs) -> Result<()> {
        crate::instructions::register_station::handle_register_station(ctx, args)
    }

    /// Anchors a signed batch; requires the Ed25519 precompile as the previous instruction.
    pub fn commit_batch(ctx: Context<CommitBatch>, header: BatchHeader) -> Result<()> {
        crate::instructions::commit_batch::handle_commit_batch(ctx, header)
    }

    /// Emits `SourceDegraded` for a station in the most recent batch.
    pub fn report_degradation(
        ctx: Context<ReportDegradation>,
        batch_id: u64,
        station_id: String,
        score_milli: u16,
        reason: u8,
    ) -> Result<()> {
        crate::instructions::report_degradation::handle_report_degradation(ctx, batch_id, station_id, score_milli, reason)
    }

    /// Records a P10/P50/P90 forecast before the forecast reading is anchored.
    pub fn commit_forecast(ctx: Context<CommitForecast>, args: ForecastArgs) -> Result<()> {
        crate::instructions::commit_forecast::handle_commit_forecast(ctx, args)
    }

    /// Sets (once) a station's alert threshold.
    pub fn set_alert_rule(ctx: Context<SetAlertRule>, station_id: String, threshold_milli: i64, level: u8) -> Result<()> {
        crate::instructions::alerts::handle_set_alert_rule(ctx, station_id, threshold_milli, level)
    }

    /// Records an official warning (e.g. INMET) attested by ≥ k oracles.
    pub fn record_public_alert(ctx: Context<RecordPublicAlert>, header: PublicAlertHeader) -> Result<()> {
        crate::instructions::record_public_alert::handle_record_public_alert(ctx, header)
    }

    /// Sets (once) a region's early-warning rule based on forecast rain.
    pub fn set_outlook_rule(ctx: Context<SetOutlookRule>, args: OutlookRuleArgs) -> Result<()> {
        crate::instructions::outlook::handle_set_outlook_rule(ctx, args)
    }

    /// Records the forecast rain attested by ≥ k oracles and emits `EarlyWarning` if it crosses the
    /// rule.
    pub fn commit_outlook(ctx: Context<CommitOutlook>, header: OutlookHeader) -> Result<()> {
        crate::instructions::outlook::handle_commit_outlook(ctx, header)
    }

    /// Creates a parametric vault that pays a beneficiary when Cota records a trigger.
    pub fn create_vault(ctx: Context<CreateVault>, args: VaultArgs) -> Result<()> {
        crate::instructions::vault::handle_create_vault(ctx, args)
    }

    /// Releases a vault payout when there is a trigger (any account may crank it).
    pub fn trigger_payout(ctx: Context<TriggerPayout>) -> Result<()> {
        crate::instructions::vault::handle_trigger_payout(ctx)
    }

    /// Closes an expired vault and returns the balance to its creator.
    pub fn close_vault(ctx: Context<CloseVault>) -> Result<()> {
        crate::instructions::vault::handle_close_vault(ctx)
    }

    /// Issues the alert receipt for a forecast whose P90 crossed the threshold.
    pub fn issue_alert(ctx: Context<IssueAlert>) -> Result<()> {
        crate::instructions::alerts::handle_issue_alert(ctx)
    }
}
