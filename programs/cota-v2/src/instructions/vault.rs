use anchor_lang::{prelude::*, system_program};

use crate::{
    constants::*,
    error::CotaError,
    events::{PayoutReleased, VaultClosed, VaultCreated},
    mmr,
    state::{Payout, Record, Registry, Vault},
};

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct VaultArgs {
    pub vault_id: u64,
    pub beneficiary: Pubkey,
    pub kind: u8,
    pub subject: String,
    pub payout_lamports: u64,
    pub cooldown_secs: i64,
    pub accept_from: i64,
    pub expires_at: i64,
    /// Initial deposit (more SOL can be sent to the vault address at any time).
    pub deposit_lamports: u64,
}

/// Creates a parametric vault. Any account may create its own; the conditions are fixed.
#[derive(Accounts)]
#[instruction(args: VaultArgs)]
pub struct CreateVault<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,
    #[account(
        init,
        payer = creator,
        space = 8 + Vault::INIT_SPACE,
        seeds = [VAULT_SEED, creator.key().as_ref(), &args.vault_id.to_le_bytes()],
        bump
    )]
    pub vault: Account<'info, Vault>,
    pub system_program: Program<'info, System>,
}

pub fn handle_create_vault(ctx: Context<CreateVault>, args: VaultArgs) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(!args.subject.is_empty() && args.subject.len() <= MAX_STATION_ID_LEN, CotaError::StringTooLong);
    require!(
        args.kind <= TRIGGER_FLOW_ALERT && args.payout_lamports > 0 && args.cooldown_secs >= 0 && args.expires_at > now,
        CotaError::InvalidVault
    );
    let v = &mut ctx.accounts.vault;
    v.creator = ctx.accounts.creator.key();
    v.vault_id = args.vault_id;
    v.beneficiary = args.beneficiary;
    v.kind = args.kind;
    v.subject = args.subject.clone();
    v.payout_lamports = args.payout_lamports;
    v.cooldown_secs = args.cooldown_secs;
    v.accept_from = args.accept_from;
    v.expires_at = args.expires_at;
    v.last_event_at = 0;
    v.payouts = 0;
    v.total_paid = 0;
    v.created_at = now;
    v.bump = ctx.bumps.vault;
    if args.deposit_lamports > 0 {
        system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.key(),
                system_program::Transfer { from: ctx.accounts.creator.to_account_info(), to: v.to_account_info() },
            ),
            args.deposit_lamports,
        )?;
    }
    emit!(VaultCreated {
        vault: v.key(),
        creator: v.creator,
        beneficiary: args.beneficiary,
        kind: args.kind,
        subject: args.subject,
        payout_lamports: args.payout_lamports,
        cooldown_secs: args.cooldown_secs,
        accept_from: args.accept_from,
        expires_at: args.expires_at,
    });
    Ok(())
}

/// The trigger record and its place in the accumulator.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct TriggerProof {
    /// Borsh of the `Record`, exactly as it was appended.
    pub record: Vec<u8>,
    pub leaf_index: u64,
    /// Siblings from the leaf up to its peak (`cota_core::mmr::Mmr::proof`).
    pub siblings: Vec<[u8; 32]>,
}

/// Pays the beneficiary when Cota has recorded a trigger. Any account may crank it: the program checks
/// that the record is in its own accumulator, then decides on its own.
#[derive(Accounts)]
#[instruction(proof: TriggerProof)]
pub struct TriggerPayout<'info> {
    #[account(mut)]
    pub submitter: Signer<'info>,
    #[account(seeds = [REGISTRY_SEED], bump = registry.bump)]
    pub registry: Account<'info, Registry>,
    #[account(mut, seeds = [VAULT_SEED, vault.creator.as_ref(), &vault.vault_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    /// CHECK: must be the beneficiary recorded in the vault.
    #[account(mut, address = vault.beneficiary @ CotaError::TriggerMismatch)]
    pub beneficiary: UncheckedAccount<'info>,
    #[account(
        init,
        payer = submitter,
        space = 8 + Payout::INIT_SPACE,
        seeds = [PAYOUT_SEED, vault.key().as_ref(), &mmr::hash_leaf(&proof.record)],
        bump
    )]
    pub payout: Account<'info, Payout>,
    pub system_program: Program<'info, System>,
}

pub fn handle_trigger_payout(ctx: Context<TriggerPayout>, proof: TriggerProof) -> Result<()> {
    let clock = Clock::get()?;
    let registry = &ctx.accounts.registry;
    let vault = &ctx.accounts.vault;
    require!(clock.unix_timestamp < vault.expires_at, CotaError::VaultExpired);

    // 1. The record is one this program appended.
    let leaf = mmr::hash_leaf(&proof.record);
    require!(
        mmr::contains(&registry.peaks, registry.leaf_count, &leaf, proof.leaf_index, &proof.siblings),
        CotaError::NotInAccumulator
    );

    // 2. It is a trigger of this vault's kind that actually fired.
    let record = Record::try_from_slice(&proof.record).map_err(|_| error!(CotaError::TriggerMismatch))?;
    let (subject, event_at, recorded_at) = match (vault.kind, record) {
        (TRIGGER_EARLY_WARNING, Record::Outlook(o)) if o.triggered => (o.header.region, o.header.issued_at, o.committed_at),
        (TRIGGER_FLOW_ALERT, Record::Forecast(f)) if f.alert.is_some() => (f.station_id, f.issued_at, f.committed_at),
        _ => return err!(CotaError::TriggerMismatch),
    };
    require!(subject == vault.subject, CotaError::TriggerMismatch);
    require!(recorded_at >= vault.accept_from, CotaError::TriggerTooEarly);
    // One payout per event: a later event, past the cooldown. Strictly later, so that a second record of the
    // same event (an outlook resubmitted before the next batch, or the +24 h and +48 h forecasts of the same
    // day) never pays twice, even with a zero cooldown.
    require!(
        vault.payouts == 0
            || (event_at > vault.last_event_at && event_at >= vault.last_event_at.saturating_add(vault.cooldown_secs)),
        CotaError::CooldownActive
    );

    // 3. Pays what is above the rent floor, up to the agreed amount.
    let vault_info = ctx.accounts.vault.to_account_info();
    let floor = Rent::get()?.minimum_balance(vault_info.data_len());
    let amount = vault.payout_lamports.min(vault_info.lamports().saturating_sub(floor));
    require!(amount > 0, CotaError::VaultEmpty);
    **vault_info.try_borrow_mut_lamports()? -= amount;
    **ctx.accounts.beneficiary.try_borrow_mut_lamports()? += amount;

    let vault = &mut ctx.accounts.vault;
    vault.last_event_at = event_at;
    vault.payouts += 1;
    vault.total_paid += amount;
    let p = &mut ctx.accounts.payout;
    p.vault = vault.key();
    p.trigger_leaf = leaf;
    p.trigger_index = proof.leaf_index;
    p.event_at = event_at;
    p.amount = amount;
    p.beneficiary = vault.beneficiary;
    p.slot = clock.slot;
    p.paid_at = clock.unix_timestamp;
    p.submitter = ctx.accounts.submitter.key();
    p.bump = ctx.bumps.payout;
    emit!(PayoutReleased {
        vault: vault.key(),
        trigger_leaf: leaf,
        trigger_index: proof.leaf_index,
        beneficiary: vault.beneficiary,
        amount,
        event_at,
    });
    Ok(())
}

/// After expiry, the creator closes the vault and recovers the remaining balance.
#[derive(Accounts)]
pub struct CloseVault<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,
    #[account(
        mut,
        seeds = [VAULT_SEED, creator.key().as_ref(), &vault.vault_id.to_le_bytes()],
        bump = vault.bump,
        has_one = creator @ CotaError::Unauthorized,
        close = creator
    )]
    pub vault: Account<'info, Vault>,
}

pub fn handle_close_vault(ctx: Context<CloseVault>) -> Result<()> {
    require!(Clock::get()?.unix_timestamp >= ctx.accounts.vault.expires_at, CotaError::VaultActive);
    emit!(VaultClosed { vault: ctx.accounts.vault.key(), returned: ctx.accounts.vault.to_account_info().lamports() });
    Ok(())
}
