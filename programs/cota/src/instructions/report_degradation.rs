use anchor_lang::prelude::*;

use crate::{
    constants::*,
    error::CotaError,
    events::SourceDegraded,
    state::{BatchCommitment, Committee, Station},
};

/// Records that a source lost confidence in the most recent batch. Creates no account:
/// the history lives in the transaction events.
#[derive(Accounts)]
#[instruction(batch_id: u64, station_id: String)]
pub struct ReportDegradation<'info> {
    pub authority: Signer<'info>,
    #[account(seeds = [COMMITTEE_SEED], bump = committee.bump, has_one = authority @ CotaError::Unauthorized)]
    pub committee: Account<'info, Committee>,
    #[account(seeds = [BATCH_SEED, &batch_id.to_le_bytes()], bump = batch.bump)]
    pub batch: Account<'info, BatchCommitment>,
    #[account(seeds = [STATION_SEED, station_id.as_bytes()], bump = station.bump)]
    pub station: Account<'info, Station>,
}

pub fn handle_report_degradation(
    ctx: Context<ReportDegradation>,
    batch_id: u64,
    station_id: String,
    score_milli: u16,
    reason: u8,
) -> Result<()> {
    // Only the batch just anchored: the report is recorded "on the spot", not rewritten later.
    require!(batch_id + 1 == ctx.accounts.committee.next_batch_id, CotaError::StaleBatch);
    require!(score_milli <= 1000 && reason <= MAX_DEGRADATION_REASON, CotaError::InvalidDegradation);

    emit!(SourceDegraded { batch_id, station_id, score_milli, reason });
    Ok(())
}
