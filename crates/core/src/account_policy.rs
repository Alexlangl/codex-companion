use crate::{AccountProtection, ProviderConfig, ProviderKind};
use chrono::{DateTime, NaiveDate, Utc};

pub fn base_model_key(model: &str) -> &str {
    let model = model.trim();
    if let Some(index) = model.len().checked_sub(11) {
        if model.is_char_boundary(index)
            && model.as_bytes()[index] == b'-'
            && NaiveDate::parse_from_str(&model[index + 1..], "%Y-%m-%d").is_ok()
        {
            return &model[..index];
        }
    }
    model
}

pub fn model_matches_rule(model: &str, rule: &str) -> bool {
    let model = model.to_ascii_lowercase();
    let rule = rule.trim().to_ascii_lowercase();
    if rule.is_empty() {
        return false;
    }
    if !rule.contains('*') {
        return model == rule;
    }
    let mut rest = model.as_str();
    for (index, part) in rule.split('*').enumerate() {
        if part.is_empty() {
            continue;
        }
        let Some(found) = rest.find(part) else {
            return false;
        };
        if index == 0 && found != 0 {
            return false;
        }
        rest = &rest[found + part.len()..];
    }
    rule.ends_with('*')
        || rule
            .rsplit('*')
            .find(|p| !p.is_empty())
            .is_some_and(|part| model.ends_with(part))
}

pub fn account_policy_block_reason(
    policy: &AccountProtection,
    provider: &ProviderConfig,
    model: Option<&str>,
    now: DateTime<Utc>,
) -> Option<&'static str> {
    let account_policy = policy.providers.get(&provider.id);
    if let Some(model) = model {
        let mapped = provider
            .model_map
            .get(model)
            .map(String::as_str)
            .unwrap_or(model);
        if policy
            .excluded_models
            .iter()
            .chain(
                account_policy
                    .into_iter()
                    .flat_map(|p| p.excluded_models.iter()),
            )
            .any(|rule| {
                [model, mapped, base_model_key(model), base_model_key(mapped)]
                    .iter()
                    .any(|m| model_matches_rule(m, rule))
            })
        {
            return Some("account_model_excluded");
        }
    }
    let reserve = account_policy?.quota_reserve.as_ref()?;
    if provider.kind != ProviderKind::OfficialCodex {
        return None;
    }
    let Some(account) = provider.account.as_ref() else {
        return Some("quota_snapshot_unknown");
    };
    let Some(at) = account
        .last_refresh_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
    else {
        return Some("quota_snapshot_unknown");
    };
    let age = now.signed_duration_since(at).num_seconds();
    if !(0..=180).contains(&age) {
        return Some("quota_snapshot_stale");
    }
    for (label, threshold, present) in [
        (
            "5h",
            reserve.hourly_threshold_percent,
            account.quota_hourly_present,
        ),
        (
            "Week",
            reserve.weekly_threshold_percent,
            account.quota_weekly_present,
        ),
    ] {
        if present == Some(false) {
            continue;
        }
        let Some(window) = account
            .quota_windows
            .iter()
            .find(|w| w.label.eq_ignore_ascii_case(label))
        else {
            return Some("quota_window_unknown");
        };
        if !(1..=100).contains(&threshold)
            || !window.remaining_percent.is_finite()
            || !(0.0..=100.0).contains(&window.remaining_percent)
        {
            return Some("quota_window_invalid");
        }
        if window.remaining_percent <= f64::from(threshold) {
            return Some("quota_reserve_reached");
        }
    }
    None
}

pub fn account_concurrency_key(provider: &ProviderConfig) -> String {
    if provider.kind == ProviderKind::OfficialCodex {
        if let Some(account) = &provider.account {
            if let (Some(user), Some(workspace)) = (&account.user_id, &account.account_id) {
                if !user.is_empty() && !workspace.is_empty() {
                    return format!("oauth:{}:{user}:{workspace}", user.len());
                }
            }
        }
    }
    // This key stays in memory; never expose credential references in errors.
    crate::provider_relay_auth_ref(provider)
        .map(|auth| format!("auth:{auth}"))
        .unwrap_or_else(|| format!("provider:{}", provider.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccountPolicy, ProviderAccountInfo, ProviderQuotaWindow, QuotaReserve};

    fn provider() -> ProviderConfig {
        serde_json::from_value(serde_json::json!({"id":"a","name":"a","kind":"official_codex","baseUrl":"https://example.test","authRef":null,"modelMap":{},"priority":0,"enabled":true})).unwrap()
    }

    #[test]
    fn reserve_fails_closed_for_stale_unknown_and_low_quota() {
        let now = Utc::now();
        let mut p = provider();
        let mut policy = AccountProtection::default();
        policy.providers.insert(
            "a".into(),
            AccountPolicy {
                quota_reserve: Some(QuotaReserve {
                    hourly_threshold_percent: 10,
                    weekly_threshold_percent: 20,
                }),
                ..Default::default()
            },
        );
        assert_eq!(
            account_policy_block_reason(&policy, &p, None, now),
            Some("quota_snapshot_unknown")
        );
        p.account = Some(ProviderAccountInfo {
            last_refresh_at: Some(now.to_rfc3339()),
            quota_weekly_present: Some(false),
            quota_windows: vec![ProviderQuotaWindow {
                label: "5h".into(),
                remaining_percent: 10.0,
                ..Default::default()
            }],
            ..Default::default()
        });
        assert_eq!(
            account_policy_block_reason(&policy, &p, None, now),
            Some("quota_reserve_reached")
        );
        p.account.as_mut().unwrap().quota_windows[0].remaining_percent = 11.0;
        assert_eq!(account_policy_block_reason(&policy, &p, None, now), None);
        assert_eq!(
            account_policy_block_reason(&policy, &p, None, now + chrono::Duration::seconds(181)),
            Some("quota_snapshot_stale")
        );
        p.account.as_mut().unwrap().quota_weekly_present = None;
        assert_eq!(
            account_policy_block_reason(&policy, &p, None, now),
            Some("quota_window_unknown")
        );
    }

    #[test]
    fn model_rules_apply_to_aliases_dates_and_wildcards() {
        let mut p = provider();
        p.model_map
            .insert("friendly".into(), "gpt-test-2026-09-20".into());
        let policy = AccountProtection {
            excluded_models: vec!["GPT-TEST".into()],
            ..Default::default()
        };
        assert_eq!(
            account_policy_block_reason(&policy, &p, Some("friendly"), Utc::now()),
            Some("account_model_excluded")
        );
        assert_eq!(base_model_key("gpt-test-2026-99-99"), "gpt-test-2026-99-99");
        assert!(model_matches_rule("abxb", "a*b"));
        assert!(model_matches_rule("gpt-test", "*test"));
        assert!(!model_matches_rule("other-gpt", "gpt*"));
    }
}
