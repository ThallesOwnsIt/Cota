//! Cota v2: the oracle committee and one append-only accumulator (Merkle Mountain Range) holding every
//! record the program accepts: stations, batches, degradations, forecasts, official warnings and rain
//! outlooks. Records are leaves, not accounts, so running it costs transaction fees only.
//! See `docs/v2-accumulator.md`.

pub mod constants;
pub mod ed25519;
pub mod error;
pub mod events;
pub mod instructions;
pub mod mmr;
pub mod state;

use anchor_lang::prelude::*;

pub use constants::*;
pub use instructions::*;
pub use state::*;

declare_id!("FnjR7wG5zicqWJ6KQuWQ5UcqqhcSghrYozVgMmmDFNwR");

#[program]
pub mod cota_v2 {
    use super::*;

    /// Creates the registry: committee `members` with threshold `threshold` (k of n), empty accumulator.
    pub fn init_registry(ctx: Context<InitRegistry>, members: Vec<Pubkey>, threshold: u8) -> Result<()> {
        instructions::handle_init_registry(ctx, members, threshold)
    }

    /// Replaces a member's key, keeping its position.
    pub fn rotate_member(ctx: Context<AdminOnly>, old: Pubkey, new: Pubkey) -> Result<()> {
        instructions::handle_rotate_member(ctx, old, new)
    }

    /// Hands the operator role to another key (admin only).
    pub fn set_authority(ctx: Context<AdminOnly>, new_authority: Pubkey) -> Result<()> {
        instructions::handle_set_authority(ctx, new_authority)
    }

    /// Registers a station (or grid point) as a record.
    pub fn register_station(ctx: Context<AuthorityOnly>, station: StationArgs) -> Result<()> {
        instructions::handle_register_station(ctx, station)
    }

    /// Sets (once) a station's flow-alert threshold.
    pub fn set_alert_rule(ctx: Context<SetAlertRule>, station_id: String, threshold_milli: i64, level: u8) -> Result<()> {
        instructions::handle_set_alert_rule(ctx, station_id, threshold_milli, level)
    }

    /// Sets (once) a region's early-warning rule based on forecast rain.
    pub fn set_outlook_rule(ctx: Context<SetOutlookRule>, args: OutlookRuleArgs) -> Result<()> {
        instructions::handle_set_outlook_rule(ctx, args)
    }

    /// Anchors a batch signed by the committee; requires the Ed25519 precompile as the previous instruction.
    pub fn commit_batch(ctx: Context<CommitteeSigned>, header: BatchHeader) -> Result<()> {
        instructions::handle_commit_batch(ctx, header)
    }

    /// Records that a source lost confidence in the most recent batch.
    pub fn report_degradation(
        ctx: Context<AuthorityOnly>,
        batch_id: u64,
        station_id: String,
        score_milli: u16,
        reason: u8,
    ) -> Result<()> {
        instructions::handle_report_degradation(ctx, batch_id, station_id, score_milli, reason)
    }

    /// Records a P10/P50/P90 forecast before the forecast reading is anchored, and its alert, if any.
    pub fn commit_forecast(ctx: Context<CommitForecast>, args: ForecastArgs) -> Result<()> {
        instructions::handle_commit_forecast(ctx, args)
    }

    /// Records an official warning (e.g. INMET) attested by ≥ k oracles.
    pub fn record_public_alert(ctx: Context<CommitteeSigned>, header: PublicAlertHeader) -> Result<()> {
        instructions::handle_record_public_alert(ctx, header)
    }

    /// Records the forecast rain attested by ≥ k oracles, with the result of the region's rule.
    pub fn commit_outlook(ctx: Context<CommitOutlook>, header: OutlookHeader) -> Result<()> {
        instructions::handle_commit_outlook(ctx, header)
    }

    /// Creates a parametric vault that pays a beneficiary when Cota records a trigger.
    pub fn create_vault(ctx: Context<CreateVault>, args: VaultArgs) -> Result<()> {
        instructions::handle_create_vault(ctx, args)
    }

    /// Releases a vault payout for a trigger record proven to be in the accumulator (anyone can crank).
    pub fn trigger_payout(ctx: Context<TriggerPayout>, proof: TriggerProof) -> Result<()> {
        instructions::handle_trigger_payout(ctx, proof)
    }

    /// Closes an expired vault and returns the balance to its creator.
    pub fn close_vault(ctx: Context<CloseVault>) -> Result<()> {
        instructions::handle_close_vault(ctx)
    }
}
