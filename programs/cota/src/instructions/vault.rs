use anchor_lang::{prelude::*, system_program};

use crate::{
    constants::*,
    error::CotaError,
    events::{PayoutReleased, VaultClosed, VaultCreated},
    state::{AlertReceipt, OutlookRecord, Payout, Vault},
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
    /// Depósito inicial (mais SOL pode ser enviado ao endereço do cofre a qualquer momento).
    pub deposit_lamports: u64,
}

/// Cria um cofre paramétrico. Qualquer conta pode criar o seu; as condições ficam fixas.
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

/// Paga o beneficiário diante de um gatilho do Cota. Qualquer conta pode acionar: o programa
/// lê o gatilho (conta do próprio programa, com discriminador conferido) e decide sozinho.
#[derive(Accounts)]
pub struct TriggerPayout<'info> {
    #[account(mut)]
    pub submitter: Signer<'info>,
    #[account(mut, seeds = [VAULT_SEED, vault.creator.as_ref(), &vault.vault_id.to_le_bytes()], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    /// CHECK: só aceita contas do programa; o tipo é conferido pelo discriminador abaixo.
    #[account(owner = crate::ID @ CotaError::TriggerMismatch)]
    pub trigger: UncheckedAccount<'info>,
    /// CHECK: precisa ser o beneficiário gravado no cofre.
    #[account(mut, address = vault.beneficiary @ CotaError::TriggerMismatch)]
    pub beneficiary: UncheckedAccount<'info>,
    #[account(
        init,
        payer = submitter,
        space = 8 + Payout::INIT_SPACE,
        seeds = [PAYOUT_SEED, vault.key().as_ref(), trigger.key().as_ref()],
        bump
    )]
    pub payout: Account<'info, Payout>,
    pub system_program: Program<'info, System>,
}

pub fn handle_trigger_payout(ctx: Context<TriggerPayout>) -> Result<()> {
    let clock = Clock::get()?;
    let vault = &ctx.accounts.vault;
    require!(clock.unix_timestamp < vault.expires_at, CotaError::VaultExpired);

    let (subject, event_at, recorded_at) = {
        let data = ctx.accounts.trigger.try_borrow_data()?;
        match vault.kind {
            TRIGGER_EARLY_WARNING => {
                let o = OutlookRecord::try_deserialize(&mut &data[..]).map_err(|_| error!(CotaError::TriggerMismatch))?;
                require!(o.triggered, CotaError::TriggerMismatch);
                (o.region, o.issued_at, o.committed_at)
            }
            _ => {
                let a = AlertReceipt::try_deserialize(&mut &data[..]).map_err(|_| error!(CotaError::TriggerMismatch))?;
                (a.station_id, a.issued_at, a.committed_at)
            }
        }
    };
    require!(subject == vault.subject, CotaError::TriggerMismatch);
    require!(recorded_at >= vault.accept_from, CotaError::TriggerTooEarly);
    require!(
        vault.payouts == 0 || event_at >= vault.last_event_at.saturating_add(vault.cooldown_secs),
        CotaError::CooldownActive
    );

    let vault_info = ctx.accounts.vault.to_account_info();
    let floor = Rent::get()?.minimum_balance(vault_info.data_len());
    let available = vault_info.lamports().saturating_sub(floor);
    let amount = vault.payout_lamports.min(available);
    require!(amount > 0, CotaError::VaultEmpty);
    **vault_info.try_borrow_mut_lamports()? -= amount;
    **ctx.accounts.beneficiary.try_borrow_mut_lamports()? += amount;

    let vault = &mut ctx.accounts.vault;
    vault.last_event_at = event_at;
    vault.payouts += 1;
    vault.total_paid += amount;

    let p = &mut ctx.accounts.payout;
    p.vault = vault.key();
    p.trigger = ctx.accounts.trigger.key();
    p.event_at = event_at;
    p.amount = amount;
    p.beneficiary = vault.beneficiary;
    p.slot = clock.slot;
    p.paid_at = clock.unix_timestamp;
    p.submitter = ctx.accounts.submitter.key();
    p.bump = ctx.bumps.payout;

    emit!(PayoutReleased { vault: vault.key(), trigger: p.trigger, beneficiary: vault.beneficiary, amount, event_at });
    Ok(())
}

/// Depois do vencimento, o criador fecha o cofre e recupera o saldo restante.
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
