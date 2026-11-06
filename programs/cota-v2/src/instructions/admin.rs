use anchor_lang::prelude::*;

use crate::{
    constants::*,
    error::CotaError,
    events::{AlertRuleSet, AuthorityChanged, MemberRotated, OutlookRuleSet, RegistryInitialized},
    instructions::append_record,
    state::{AlertRule, OutlookRule, Record, Registry, StationArgs, StationRecord},
};

#[derive(Accounts)]
pub struct InitRegistry<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(init, payer = authority, space = 8 + Registry::INIT_SPACE, seeds = [REGISTRY_SEED], bump)]
    pub registry: Account<'info, Registry>,
    pub system_program: Program<'info, System>,
}

pub fn validate_members(members: &[Pubkey], threshold: u8) -> Result<()> {
    require!(
        !members.is_empty() && members.len() <= MAX_MEMBERS && threshold >= 1 && threshold as usize <= members.len(),
        CotaError::InvalidCommittee
    );
    for (i, m) in members.iter().enumerate() {
        require!(!members[..i].contains(m), CotaError::InvalidCommittee);
    }
    Ok(())
}

pub fn handle_init_registry(ctx: Context<InitRegistry>, members: Vec<Pubkey>, threshold: u8) -> Result<()> {
    validate_members(&members, threshold)?;
    let r = &mut ctx.accounts.registry;
    r.authority = ctx.accounts.authority.key();
    r.members = members.clone();
    r.threshold = threshold;
    r.next_batch_id = 1;
    r.last_batch_root = [0; 32];
    r.last_t_end = i64::MIN;
    r.leaf_count = 0;
    r.peaks = [[0; 32]; MAX_PEAKS];
    r.bump = ctx.bumps.registry;
    emit!(RegistryInitialized { authority: r.authority, members, threshold });
    Ok(())
}

/// Routine instructions of the operator (the registry's `authority`): station records, forecasts,
/// degradation reports. None of them can change who signs or what the rules are.
#[derive(Accounts)]
pub struct AuthorityOnly<'info> {
    pub authority: Signer<'info>,
    #[account(mut, seeds = [REGISTRY_SEED], bump = registry.bump, has_one = authority @ CotaError::Unauthorized)]
    pub registry: Account<'info, Registry>,
}

/// Instructions that change who signs or who operates: only the program's upgrade authority (a multisig with a
/// time lock on devnet) may run them. The operator key, which signs every day from automation, cannot replace
/// committee members, so a leaked operator key cannot forge the committee's signatures.
#[derive(Accounts)]
pub struct AdminOnly<'info> {
    pub admin: Signer<'info>,
    #[account(mut, seeds = [REGISTRY_SEED], bump = registry.bump)]
    pub registry: Account<'info, Registry>,
    #[account(constraint = program.programdata_address()? == Some(program_data.key()) @ CotaError::Unauthorized)]
    pub program: Program<'info, crate::program::CotaV2>,
    #[account(constraint = program_data.upgrade_authority_address == Some(admin.key()) @ CotaError::Unauthorized)]
    pub program_data: Account<'info, ProgramData>,
}

/// Replaces a node's key (leaked key or replaced operator). Its position in the signer bitmap is kept.
pub fn handle_rotate_member(ctx: Context<AdminOnly>, old: Pubkey, new: Pubkey) -> Result<()> {
    let r = &mut ctx.accounts.registry;
    let index = r.members.iter().position(|m| *m == old).ok_or(CotaError::MemberNotFound)?;
    require!(!r.members.contains(&new), CotaError::InvalidCommittee);
    r.members[index] = new;
    emit!(MemberRotated { index: index as u8, old, new });
    Ok(())
}

/// Hands the operator role to another key. Admin only: the operator cannot pass its role on by itself.
pub fn handle_set_authority(ctx: Context<AdminOnly>, new_authority: Pubkey) -> Result<()> {
    let r = &mut ctx.accounts.registry;
    let old = r.authority;
    r.authority = new_authority;
    emit!(AuthorityChanged { old, new: new_authority });
    Ok(())
}

/// Registers a station (or grid point) as a record: nothing on-chain needs to read it later.
pub fn handle_register_station(ctx: Context<AuthorityOnly>, station: StationArgs) -> Result<()> {
    require!(
        !station.station_id.is_empty()
            && station.station_id.len() <= MAX_STATION_ID_LEN
            && station.name.len() <= MAX_STATION_NAME_LEN
            && station.datum_id.as_ref().is_none_or(|d| d.len() <= MAX_DATUM_ID_LEN),
        CotaError::StringTooLong
    );
    let clock = Clock::get()?;
    append_record(
        &mut ctx.accounts.registry,
        Record::Station(StationRecord { station, slot: clock.slot, registered_at: clock.unix_timestamp }),
    )?;
    Ok(())
}

/// Sets a station's flow-alert threshold. `init` makes the rule immutable. Admin only: rules decide when
/// vaults pay, so the operator cannot create or front-run them.
#[derive(Accounts)]
#[instruction(station_id: String)]
pub struct SetAlertRule<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(seeds = [REGISTRY_SEED], bump = registry.bump)]
    pub registry: Account<'info, Registry>,
    #[account(constraint = program.programdata_address()? == Some(program_data.key()) @ CotaError::Unauthorized)]
    pub program: Program<'info, crate::program::CotaV2>,
    #[account(constraint = program_data.upgrade_authority_address == Some(admin.key()) @ CotaError::Unauthorized)]
    pub program_data: Account<'info, ProgramData>,
    #[account(init, payer = admin, space = 8 + AlertRule::INIT_SPACE, seeds = [ALERT_RULE_SEED, station_id.as_bytes()], bump)]
    pub rule: Account<'info, AlertRule>,
    pub system_program: Program<'info, System>,
}

pub fn handle_set_alert_rule(ctx: Context<SetAlertRule>, station_id: String, threshold_milli: i64, level: u8) -> Result<()> {
    require!(!station_id.is_empty() && station_id.len() <= MAX_STATION_ID_LEN, CotaError::StringTooLong);
    let rule = &mut ctx.accounts.rule;
    rule.station_id = station_id.clone();
    rule.threshold_milli = threshold_milli;
    rule.level = level;
    rule.set_at = Clock::get()?.unix_timestamp;
    rule.bump = ctx.bumps.rule;
    emit!(AlertRuleSet { station_id, threshold_milli, level });
    Ok(())
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct OutlookRuleArgs {
    pub region: String,
    pub threshold_milli: i64,
    pub window_secs: i64,
    pub min_models: u8,
    pub rule_sha256: [u8; 32],
}

/// Sets a region's early-warning rule. `init` makes the rule immutable. Admin only, like the flow rule.
#[derive(Accounts)]
#[instruction(args: OutlookRuleArgs)]
pub struct SetOutlookRule<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(seeds = [REGISTRY_SEED], bump = registry.bump)]
    pub registry: Account<'info, Registry>,
    #[account(constraint = program.programdata_address()? == Some(program_data.key()) @ CotaError::Unauthorized)]
    pub program: Program<'info, crate::program::CotaV2>,
    #[account(constraint = program_data.upgrade_authority_address == Some(admin.key()) @ CotaError::Unauthorized)]
    pub program_data: Account<'info, ProgramData>,
    #[account(init, payer = admin, space = 8 + OutlookRule::INIT_SPACE, seeds = [OUTLOOK_RULE_SEED, args.region.as_bytes()], bump)]
    pub rule: Account<'info, OutlookRule>,
    pub system_program: Program<'info, System>,
}

pub fn handle_set_outlook_rule(ctx: Context<SetOutlookRule>, args: OutlookRuleArgs) -> Result<()> {
    require!(!args.region.is_empty() && args.region.len() <= MAX_REGION_LEN, CotaError::StringTooLong);
    require!(args.threshold_milli > 0 && args.window_secs > 0 && args.min_models > 0, CotaError::InvalidOutlook);
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
