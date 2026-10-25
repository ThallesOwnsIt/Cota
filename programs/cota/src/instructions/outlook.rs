use anchor_lang::prelude::*;
use solana_instructions_sysvar::ID as INSTRUCTIONS_SYSVAR_ID;

use crate::{
    constants::*,
    ed25519,
    error::CotaError,
    events::{EarlyWarning, OutlookCommitted, OutlookRuleSet},
    state::{Committee, OutlookHeader, OutlookRecord, OutlookRule},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct OutlookRuleArgs {
    pub region: String,
    pub threshold_milli: i64,
    pub window_secs: i64,
    pub min_models: u8,
    pub rule_sha256: [u8; 32],
}

/// Fixa a regra do alerta antecipado de uma região. `init` torna a regra imutável.
#[derive(Accounts)]
#[instruction(args: OutlookRuleArgs)]
pub struct SetOutlookRule<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(seeds = [COMMITTEE_SEED], bump = committee.bump, has_one = authority @ CotaError::Unauthorized)]
    pub committee: Account<'info, Committee>,
    #[account(
        init,
        payer = authority,
        space = 8 + OutlookRule::INIT_SPACE,
        seeds = [OUTLOOK_RULE_SEED, args.region.as_bytes()],
        bump
    )]
    pub rule: Account<'info, OutlookRule>,
    pub system_program: Program<'info, System>,
}

pub fn handle_set_outlook_rule(ctx: Context<SetOutlookRule>, args: OutlookRuleArgs) -> Result<()> {
    require!(!args.region.is_empty() && args.region.len() <= MAX_REGION_LEN, CotaError::StringTooLong);
    require!(
        args.threshold_milli > 0 && args.window_secs > 0 && args.min_models > 0,
        CotaError::InvalidOutlook
    );
    let rule = &mut ctx.accounts.rule;
    rule.region = args.region.clone();
    rule.threshold_milli = args.threshold_milli;
    rule.window_secs = args.window_secs;
    rule.min_models = args.min_models;
    rule.rule_sha256 = args.rule_sha256;
    rule.set_at = Clock::get()?.unix_timestamp;
    rule.bump = ctx.bumps.rule;

    emit!(OutlookRuleSet {
        region: args.region,
        threshold_milli: args.threshold_milli,
        window_secs: args.window_secs,
        min_models: args.min_models,
        rule_sha256: args.rule_sha256,
    });
    Ok(())
}

/// Registra a chuva prevista vista por ≥ k oráculos e aplica a regra. Qualquer conta pode
/// submeter; a autorização vem das assinaturas Ed25519 da instrução anterior.
///
/// A emissão precisa ser o fim do último lote ancorado: a perspectiva fica presa ao dia que
/// acabou de fechar, não pode ser antecipada nem registrada depois do lote seguinte.
#[derive(Accounts)]
#[instruction(header: OutlookHeader)]
pub struct CommitOutlook<'info> {
    #[account(mut)]
    pub submitter: Signer<'info>,
    #[account(seeds = [COMMITTEE_SEED], bump = committee.bump)]
    pub committee: Account<'info, Committee>,
    #[account(seeds = [OUTLOOK_RULE_SEED, header.region.as_bytes()], bump = rule.bump)]
    pub rule: Account<'info, OutlookRule>,
    #[account(
        init,
        payer = submitter,
        space = 8 + OutlookRecord::INIT_SPACE,
        seeds = [OUTLOOK_SEED, header.region.as_bytes(), &header.issued_at.to_le_bytes()],
        bump
    )]
    pub outlook: Account<'info, OutlookRecord>,
    /// CHECK: endereço conferido contra o ID do sysvar Instructions.
    #[account(address = INSTRUCTIONS_SYSVAR_ID)]
    pub instructions: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

pub fn handle_commit_outlook(ctx: Context<CommitOutlook>, header: OutlookHeader) -> Result<()> {
    let committee = &ctx.accounts.committee;
    let rule = &ctx.accounts.rule;
    require!(committee.next_batch_id > 1, CotaError::NoAnchoredBatch);
    require!(
        header.issued_at == committee.last_t_end
            && header.window_end - header.issued_at == rule.window_secs
            && header.models >= rule.min_models
            && header.rain_min_milli >= 0
            && header.rain_min_milli <= header.rain_mean_milli
            && header.rain_mean_milli <= header.rain_max_milli,
        CotaError::InvalidOutlook
    );

    let (signers, signer_count) = ed25519::committee_signers(
        &ctx.accounts.instructions.to_account_info(),
        &committee.members,
        committee.threshold,
        &header.signing_message(),
    )?;

    let clock = Clock::get()?;
    let triggered = header.rain_mean_milli >= rule.threshold_milli;
    let o = &mut ctx.accounts.outlook;
    o.region = header.region.clone();
    o.based_on_batch_id = committee.next_batch_id - 1;
    o.issued_at = header.issued_at;
    o.window_end = header.window_end;
    o.models = header.models;
    o.rain_mean_milli = header.rain_mean_milli;
    o.rain_min_milli = header.rain_min_milli;
    o.rain_max_milli = header.rain_max_milli;
    o.content_sha256 = header.content_sha256;
    o.threshold_milli = rule.threshold_milli;
    o.triggered = triggered;
    o.signers = signers;
    o.signer_count = signer_count;
    o.slot = clock.slot;
    o.committed_at = clock.unix_timestamp;
    o.submitter = ctx.accounts.submitter.key();
    o.bump = ctx.bumps.outlook;

    emit!(OutlookCommitted {
        outlook: o.key(),
        region: header.region.clone(),
        issued_at: header.issued_at,
        window_end: header.window_end,
        rain_mean_milli: header.rain_mean_milli,
        content_sha256: header.content_sha256,
        signers,
    });
    if triggered {
        emit!(EarlyWarning {
            outlook: o.key(),
            region: header.region,
            issued_at: header.issued_at,
            window_end: header.window_end,
            rain_mean_milli: header.rain_mean_milli,
            threshold_milli: rule.threshold_milli,
        });
    }
    Ok(())
}
