//! Free-tier quotas (docs/SPEC-hosted-platform.md sections 5.2 and 11): an onboarding
//! daily allowance for the first days after a hub's token was created, then a lower
//! daily limit, plus monthly request and cost caps and an optional daily token cap.
//! All numbers are per-hub columns an operator can raise.

use chrono::{DateTime, Duration, Utc};

use crate::db::Hub;

/// Summed usage over a period (one day or one calendar month).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UsageTotals {
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_cents: f64,
}

/// The daily request allowance in force: the onboarding figure while the hub is
/// younger than `onboarding_days`, the regular limit afterwards.
pub fn effective_daily_limit(hub: &Hub, now: DateTime<Utc>) -> i64 {
    let created = DateTime::parse_from_rfc3339(&hub.created_at).map(|d| d.with_timezone(&Utc));
    match created {
        Ok(created)
            if now - created
                < Duration::try_days(hub.quota.onboarding_days).unwrap_or_else(Duration::zero) =>
        {
            hub.quota.onboarding_daily_requests
        }
        _ => hub.quota.daily_request_limit,
    }
}

/// `Err(reason)` when the next request must be refused.
pub fn check(
    hub: &Hub,
    today: &UsageTotals,
    month: &UsageTotals,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let daily = effective_daily_limit(hub, now);
    if today.requests >= daily {
        return Err(format!(
            "daily limit of {daily} model requests reached for this hub; it resets at 00:00 UTC"
        ));
    }
    if let Some(cap) = hub.quota.daily_token_limit
        && cap > 0
        && today.input_tokens + today.output_tokens >= cap
    {
        return Err(format!(
            "daily token limit of {cap} reached for this hub; it resets at 00:00 UTC"
        ));
    }
    if month.requests >= hub.quota.monthly_request_limit {
        return Err(format!(
            "monthly limit of {} model requests reached for this hub; it resets on the 1st",
            hub.quota.monthly_request_limit
        ));
    }
    if month.cost_cents >= hub.quota.monthly_cost_cap_cents as f64 {
        return Err(format!(
            "monthly cost cap of {} cents reached for this hub; it resets on the 1st",
            hub.quota.monthly_cost_cap_cents
        ));
    }
    Ok(())
}
