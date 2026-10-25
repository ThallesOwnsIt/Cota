use anchor_lang::prelude::*;

use crate::{
    constants::*,
    error::CotaError,
    events::ForecastCommitted,
    state::{Committee, ForecastCommitment, Station},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct ForecastArgs {
    pub station_id: String,
    pub target_ts: i64,
    pub horizon_hours: u16,
    pub p10: i64,
    pub p50: i64,
    pub p90: i64,
    pub model_sha256: [u8; 32],
    pub features_root: [u8; 32],
}

/// Grava uma previsão vinculada ao último lote ancorado. O programa garante que a leitura
/// prevista ainda não foi ancorada: é a prova de que a previsão não foi ajustada depois.
#[derive(Accounts)]
#[instruction(args: ForecastArgs)]
pub struct CommitForecast<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(seeds = [COMMITTEE_SEED], bump = committee.bump, has_one = authority @ CotaError::Unauthorized)]
    pub committee: Account<'info, Committee>,
    #[account(seeds = [STATION_SEED, args.station_id.as_bytes()], bump = station.bump)]
    pub station: Account<'info, Station>,
    #[account(
        init,
        payer = authority,
        space = 8 + ForecastCommitment::INIT_SPACE,
        seeds = [
            FORECAST_SEED,
            args.station_id.as_bytes(),
            &(committee.next_batch_id - 1).to_le_bytes(),
            &args.target_ts.to_le_bytes(),
        ],
        bump
    )]
    pub forecast: Account<'info, ForecastCommitment>,
    pub system_program: Program<'info, System>,
}

pub fn handle_commit_forecast(ctx: Context<CommitForecast>, args: ForecastArgs) -> Result<()> {
    let committee = &ctx.accounts.committee;
    require!(committee.next_batch_id > 1, CotaError::NoAnchoredBatch);
    require!(args.target_ts >= committee.last_t_end, CotaError::TargetAlreadyAnchored);
    require!(
        args.p10 <= args.p50 && args.p50 <= args.p90 && args.horizon_hours > 0,
        CotaError::InvalidForecast
    );

    let clock = Clock::get()?;
    let f = &mut ctx.accounts.forecast;
    f.station_id = args.station_id.clone();
    f.variable = ctx.accounts.station.variable;
    f.based_on_batch_id = committee.next_batch_id - 1;
    f.based_on_root = committee.last_batch_root;
    f.issued_at = committee.last_t_end;
    f.target_ts = args.target_ts;
    f.horizon_hours = args.horizon_hours;
    f.p10 = args.p10;
    f.p50 = args.p50;
    f.p90 = args.p90;
    f.model_sha256 = args.model_sha256;
    f.features_root = args.features_root;
    f.slot = clock.slot;
    f.committed_at = clock.unix_timestamp;
    f.submitter = ctx.accounts.authority.key();
    f.bump = ctx.bumps.forecast;

    emit!(ForecastCommitted {
        forecast: f.key(),
        station_id: args.station_id,
        based_on_batch_id: f.based_on_batch_id,
        issued_at: f.issued_at,
        target_ts: args.target_ts,
        horizon_hours: args.horizon_hours,
        p10: args.p10,
        p50: args.p50,
        p90: args.p90,
        model_sha256: args.model_sha256,
        features_root: args.features_root,
    });
    Ok(())
}
