use anchor_lang::prelude::*;
use solana_instructions_sysvar::ID as INSTRUCTIONS_SYSVAR_ID;

use crate::{
    constants::*,
    ed25519,
    error::CotaError,
    events::PublicAlertRecorded,
    state::{Committee, PublicAlertHeader, PublicAlertRecord},
};

/// Registra um aviso oficial visto por ≥ k oráculos com o mesmo conteúdo. Qualquer conta
/// pode submeter; a autorização vem das assinaturas Ed25519 da instrução anterior.
/// Cada versão do aviso (conteúdo diferente) gera uma conta própria, e `init` impede
/// regravar uma versão com outro horário.
#[derive(Accounts)]
#[instruction(header: PublicAlertHeader)]
pub struct RecordPublicAlert<'info> {
    #[account(mut)]
    pub submitter: Signer<'info>,
    #[account(seeds = [COMMITTEE_SEED], bump = committee.bump)]
    pub committee: Account<'info, Committee>,
    #[account(
        init,
        payer = submitter,
        space = 8 + PublicAlertRecord::INIT_SPACE,
        seeds = [PUBLIC_ALERT_SEED, header.content_sha256.as_ref()],
        bump
    )]
    pub record: Account<'info, PublicAlertRecord>,
    /// CHECK: endereço conferido contra o ID do sysvar Instructions.
    #[account(address = INSTRUCTIONS_SYSVAR_ID)]
    pub instructions: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

pub fn handle_record_public_alert(ctx: Context<RecordPublicAlert>, header: PublicAlertHeader) -> Result<()> {
    require!(
        !header.issuer.is_empty()
            && header.issuer.len() <= MAX_ISSUER_LEN
            && !header.alert_id.is_empty()
            && header.alert_id.len() <= MAX_ALERT_ID_LEN,
        CotaError::StringTooLong
    );
    require!(header.expires >= header.onset && header.severity <= 3, CotaError::InvalidPublicAlert);

    let committee = &ctx.accounts.committee;
    let (signers, signer_count) = ed25519::committee_signers(
        &ctx.accounts.instructions.to_account_info(),
        &committee.members,
        committee.threshold,
        &header.signing_message(),
    )?;

    let clock = Clock::get()?;
    let r = &mut ctx.accounts.record;
    r.issuer = header.issuer.clone();
    r.alert_id = header.alert_id.clone();
    r.content_sha256 = header.content_sha256;
    r.severity = header.severity;
    r.onset = header.onset;
    r.expires = header.expires;
    r.signers = signers;
    r.signer_count = signer_count;
    r.slot = clock.slot;
    r.committed_at = clock.unix_timestamp;
    r.submitter = ctx.accounts.submitter.key();
    r.bump = ctx.bumps.record;

    emit!(PublicAlertRecorded {
        record: r.key(),
        issuer: header.issuer,
        alert_id: header.alert_id,
        content_sha256: header.content_sha256,
        severity: header.severity,
        onset: header.onset,
        expires: header.expires,
        signers,
        slot: clock.slot,
    });
    Ok(())
}
