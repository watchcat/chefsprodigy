//! Free-tier quotas (docs/SPEC-hosted-platform.md sections 5.2 and 11): an onboarding
//! daily allowance for the first days after the hub was registered (`hubs.created_at`;
//! a rotated token does not restart it), then a lower daily limit, plus monthly request
//! and cost caps and an optional daily token cap. All numbers are per-hub columns an
//! operator can change. Checked only before a vendor call: cache hits are free and BYOK
//! hubs skip the check.

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

/// The UTC day `now` falls on and the first and last day of its calendar month, as the
/// inclusive `YYYY-MM-DD` ranges `usage_daily` is summed over.
pub fn periods(now: DateTime<Utc>) -> (String, (String, String)) {
    let month = now.format("%Y-%m");
    (
        now.format("%Y-%m-%d").to_string(),
        (format!("{month}-01"), format!("{month}-31")),
    )
}

/// `Err(reason)` when the next request must be refused. `pending` counts requests already
/// admitted and still in flight (not yet in `usage_daily`), so concurrent calls cannot
/// overshoot the request limits; tokens and cost are only known afterwards.
pub fn check(
    hub: &Hub,
    today: &UsageTotals,
    month: &UsageTotals,
    pending: i64,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let daily = effective_daily_limit(hub, now);
    if today.requests + pending >= daily {
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
    if month.requests + pending >= hub.quota.monthly_request_limit {
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
