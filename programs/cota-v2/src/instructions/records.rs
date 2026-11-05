use anchor_lang::prelude::*;
use solana_instructions_sysvar::ID as INSTRUCTIONS_SYSVAR_ID;

use crate::{
    constants::*,
    ed25519,
    error::CotaError,
    instructions::append_record,
    state::*,
};

/// Instructions authorized by the committee's Ed25519 signatures (verified in the previous instruction).
/// Any account may submit them and pay the fees.
#[derive(Accounts)]
pub struct CommitteeSigned<'info> {
    pub submitter: Signer<'info>,
    #[account(mut, seeds = [REGISTRY_SEED], bump = registry.bump)]
    pub registry: Account<'info, Registry>,
    /// CHECK: address checked against the Instructions sysvar ID.
    #[account(address = INSTRUCTIONS_SYSVAR_ID)]
    pub instructions: UncheckedAccount<'info>,
}

/// Anchors a batch signed by the committee.
pub fn handle_commit_batch(ctx: Context<CommitteeSigned>, header: BatchHeader) -> Result<()> {
    let r = &ctx.accounts.registry;
    // 1. Sequence and hash chain: prevents skipping, repeating or inserting batches in the past.
    require!(header.batch_id == r.next_batch_id, CotaError::BatchOutOfSequence);
    require!(header.prev_batch_root == r.last_batch_root, CotaError::BrokenHashChain);
    require!(header.t_end > header.t_start && header.t_start >= r.last_t_end, CotaError::InvalidWindow);

    // 2. Who signed exactly this header, and whether they are enough distinct members.
    let (signers, signer_count) =
        ed25519::committee_signers(&ctx.accounts.instructions.to_account_info(), &r.members, r.threshold, &header.signing_message())?;

    // 3. Advances the sequence and appends the record.
    let clock = Clock::get()?;
    let r = &mut ctx.accounts.registry;
    r.next_batch_id += 1;
    r.last_batch_root = header.merkle_root;
    r.last_t_end = header.t_end;
    append_record(
        r,
        Record::Batch(BatchRecord { header, signers, signer_count, slot: clock.slot, committed_at: clock.unix_timestamp }),
    )?;
    Ok(())
}

/// Records an official warning seen by ≥ k oracles with the same content. A version recorded twice is
/// harmless: the earliest record is the one that counts.
pub fn handle_record_public_alert(ctx: Context<CommitteeSigned>, header: PublicAlertHeader) -> Result<()> {
    require!(
        !header.issuer.is_empty()
            && header.issuer.len() <= MAX_ISSUER_LEN
            && !header.alert_id.is_empty()
            && header.alert_id.len() <= MAX_ALERT_ID_LEN,
        CotaError::StringTooLong
    );
    require!(header.expires >= header.onset && header.severity <= 3, CotaError::InvalidPublicAlert);
    let r = &ctx.accounts.registry;
    let (signers, signer_count) =
        ed25519::committee_signers(&ctx.accounts.instructions.to_account_info(), &r.members, r.threshold, &header.signing_message())?;
    let clock = Clock::get()?;
    append_record(
        &mut ctx.accounts.registry,
        Record::PublicAlert(PublicAlertRecord { header, signers, signer_count, slot: clock.slot, committed_at: clock.unix_timestamp }),
    )?;
    Ok(())
}

#[derive(Accounts)]
#[instruction(header: OutlookHeader)]
pub struct CommitOutlook<'info> {
    pub submitter: Signer<'info>,
    #[account(mut, seeds = [REGISTRY_SEED], bump = registry.bump)]
    pub registry: Account<'info, Registry>,
    #[account(seeds = [OUTLOOK_RULE_SEED, header.region.as_bytes()], bump = rule.bump)]
    pub rule: Account<'info, OutlookRule>,
    /// CHECK: address checked against the Instructions sysvar ID.
    #[account(address = INSTRUCTIONS_SYSVAR_ID)]
    pub instructions: UncheckedAccount<'info>,
}

/// Records the forecast rain seen by ≥ k oracles and applies the region's rule.
///
/// The issue time must be the end of the latest anchored batch: the outlook is tied to the day that has
/// just closed, and can be neither brought forward nor recorded after the next batch.
pub fn handle_commit_outlook(ctx: Context<CommitOutlook>, header: OutlookHeader) -> Result<()> {
    let r = &ctx.accounts.registry;
    let rule = &ctx.accounts.rule;
    require!(r.next_batch_id > 1, CotaError::NoAnchoredBatch);
    require!(
        header.issued_at == r.last_t_end
            && header.window_end - header.issued_at == rule.window_secs
            && header.models >= rule.min_models
            && header.rain_min_milli >= 0
            && header.rain_min_milli <= header.rain_mean_milli
            && header.rain_mean_milli <= header.rain_max_milli,
        CotaError::InvalidOutlook
    );
    let (signers, signer_count) =
        ed25519::committee_signers(&ctx.accounts.instructions.to_account_info(), &r.members, r.threshold, &header.signing_message())?;
    let clock = Clock::get()?;
    let record = OutlookRecord {
        triggered: header.rain_mean_milli >= rule.threshold_milli,
        threshold_milli: rule.threshold_milli,
        based_on_batch_id: r.next_batch_id - 1,
        header,
        signers,
        signer_count,
        slot: clock.slot,
        committed_at: clock.unix_timestamp,
    };
    append_record(&mut ctx.accounts.registry, Record::Outlook(record))?;
    Ok(())
}

/// Records that a source lost confidence in the most recent batch.
pub fn handle_report_degradation(
    ctx: Context<super::AuthorityOnly>,
    batch_id: u64,
    station_id: String,
    score_milli: u16,
    reason: u8,
) -> Result<()> {
    // Only the batch just anchored: the report is recorded "on the spot", not rewritten later.
    require!(batch_id + 1 == ctx.accounts.registry.next_batch_id, CotaError::StaleBatch);
    require!(score_milli <= 1000 && reason <= MAX_DEGRADATION_REASON, CotaError::InvalidDegradation);
    require!(!station_id.is_empty() && station_id.len() <= MAX_STATION_ID_LEN, CotaError::StringTooLong);
    let slot = Clock::get()?.slot;
    append_record(
        &mut ctx.accounts.registry,
        Record::Degradation(DegradationRecord { batch_id, station_id, score_milli, reason, slot }),
    )?;
    Ok(())
}

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

#[derive(Accounts)]
#[instruction(args: ForecastArgs)]
pub struct CommitForecast<'info> {
    pub authority: Signer<'info>,
    #[account(mut, seeds = [REGISTRY_SEED], bump = registry.bump, has_one = authority @ CotaError::Unauthorized)]
    pub registry: Account<'info, Registry>,
    /// CHECK: the station's `AlertRule` PDA, which may not exist. The address is enforced, so the
    /// submitter cannot dodge an existing rule by passing another account.
    #[account(seeds = [ALERT_RULE_SEED, args.station_id.as_bytes()], bump)]
    pub rule: UncheckedAccount<'info>,
}

/// Records a forecast tied to the latest anchored batch and, if the station has a rule, decides the alert
/// in the same step. The program guarantees that the forecast reading has not been anchored yet: that is
/// the proof the forecast was not adjusted afterwards.
pub fn handle_commit_forecast(ctx: Context<CommitForecast>, args: ForecastArgs) -> Result<()> {
    let r = &ctx.accounts.registry;
    require!(r.next_batch_id > 1, CotaError::NoAnchoredBatch);
    require!(args.target_ts >= r.last_t_end, CotaError::TargetAlreadyAnchored);
    require!(args.p10 <= args.p50 && args.p50 <= args.p90 && args.horizon_hours > 0, CotaError::InvalidForecast);
    require!(!args.station_id.is_empty() && args.station_id.len() <= MAX_STATION_ID_LEN, CotaError::StringTooLong);

    let rule_info = ctx.accounts.rule.to_account_info();
    let alert = if rule_info.data_is_empty() {
        None
    } else {
        require_keys_eq!(*rule_info.owner, crate::ID, CotaError::Unauthorized);
        let rule = AlertRule::try_deserialize(&mut &rule_info.try_borrow_data()?[..])?;
        (args.p90 >= rule.threshold_milli).then_some(ForecastAlert { threshold_milli: rule.threshold_milli, level: rule.level })
    };

    let clock = Clock::get()?;
    let record = ForecastRecord {
        station_id: args.station_id,
        based_on_batch_id: r.next_batch_id - 1,
        based_on_root: r.last_batch_root,
        issued_at: r.last_t_end,
        target_ts: args.target_ts,
        horizon_hours: args.horizon_hours,
        p10: args.p10,
        p50: args.p50,
        p90: args.p90,
        model_sha256: args.model_sha256,
        features_root: args.features_root,
        alert,
        slot: clock.slot,
        committed_at: clock.unix_timestamp,
    };
    append_record(&mut ctx.accounts.registry, Record::Forecast(record))?;
    Ok(())
}
