use anchor_lang::prelude::*;

use crate::{
    constants::*,
    error::CotaError,
    events::{AlertIssued, AlertRuleSet},
    state::{AlertReceipt, AlertRule, Committee, ForecastCommitment, Station},
};

/// Fixa o limiar de alerta de uma estação. `init` torna a regra imutável.
#[derive(Accounts)]
#[instruction(station_id: String)]
pub struct SetAlertRule<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(seeds = [COMMITTEE_SEED], bump = committee.bump, has_one = authority @ CotaError::Unauthorized)]
    pub committee: Account<'info, Committee>,
    #[account(seeds = [STATION_SEED, station_id.as_bytes()], bump = station.bump)]
    pub station: Account<'info, Station>,
    #[account(
        init,
        payer = authority,
        space = 8 + AlertRule::INIT_SPACE,
        seeds = [ALERT_RULE_SEED, station_id.as_bytes()],
        bump
    )]
    pub rule: Account<'info, AlertRule>,
    pub system_program: Program<'info, System>,
}

pub fn handle_set_alert_rule(ctx: Context<SetAlertRule>, station_id: String, threshold_milli: i64, level: u8) -> Result<()> {
    let rule = &mut ctx.accounts.rule;
    rule.station_id = station_id.clone();
    rule.threshold_milli = threshold_milli;
    rule.level = level;
    rule.set_at = Clock::get()?.unix_timestamp;
    rule.bump = ctx.bumps.rule;

    emit!(AlertRuleSet { station_id, threshold_milli, level });
    Ok(())
}

/// Emite o recibo de alerta de uma previsão. Qualquer conta pode chamar: o programa só
/// aceita se a P90 gravada cruza o limiar e se a observação ainda não foi ancorada.
#[derive(Accounts)]
pub struct IssueAlert<'info> {
    #[account(mut)]
    pub submitter: Signer<'info>,
    #[account(seeds = [COMMITTEE_SEED], bump = committee.bump)]
    pub committee: Account<'info, Committee>,
    pub forecast: Account<'info, ForecastCommitment>,
    #[account(seeds = [ALERT_RULE_SEED, forecast.station_id.as_bytes()], bump = rule.bump)]
    pub rule: Account<'info, AlertRule>,
    #[account(
        init,
        payer = submitter,
        space = 8 + AlertReceipt::INIT_SPACE,
        seeds = [ALERT_SEED, forecast.key().as_ref()],
        bump
    )]
    pub alert: Account<'info, AlertReceipt>,
    pub system_program: Program<'info, System>,
}

pub fn handle_issue_alert(ctx: Context<IssueAlert>) -> Result<()> {
    let forecast = &ctx.accounts.forecast;
    let rule = &ctx.accounts.rule;
    require!(forecast.target_ts >= ctx.accounts.committee.last_t_end, CotaError::TargetAlreadyAnchored);
    require!(forecast.p90 >= rule.threshold_milli, CotaError::BelowAlertThreshold);

    let clock = Clock::get()?;
    let alert = &mut ctx.accounts.alert;
    alert.forecast = forecast.key();
    alert.station_id = forecast.station_id.clone();
    alert.issued_at = forecast.issued_at;
    alert.target_ts = forecast.target_ts;
    alert.p90 = forecast.p90;
    alert.threshold_milli = rule.threshold_milli;
    alert.level = rule.level;
    alert.slot = clock.slot;
    alert.committed_at = clock.unix_timestamp;
    alert.submitter = ctx.accounts.submitter.key();
    alert.bump = ctx.bumps.alert;

    emit!(AlertIssued {
        alert: alert.key(),
        forecast: forecast.key(),
        station_id: forecast.station_id.clone(),
        target_ts: forecast.target_ts,
        p90: forecast.p90,
        threshold_milli: rule.threshold_milli,
        level: rule.level,
    });
    Ok(())
}
