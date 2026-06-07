use chrono::{Duration, Utc};
use codex_companion_core::{HealthFailureKind, HealthStatusKind, ProviderHealth};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureClassification {
    pub kind: HealthFailureKind,
    pub retryable: bool,
    pub cooldown: bool,
}

pub fn classify_failure(status: Option<u16>, body: &str) -> FailureClassification {
    let lower = body.to_ascii_lowercase();

    if matches!(status, Some(401 | 403)) {
        return class(HealthFailureKind::AuthFailed, false, true);
    }
    if lower.contains("insufficient_quota")
        || lower.contains("quota exceeded")
        || lower.contains("billing")
        || lower.contains("额度")
    {
        return class(HealthFailureKind::QuotaExhausted, true, true);
    }
    if matches!(status, Some(429))
        || lower.contains("rate limit")
        || lower.contains("too many requests")
    {
        return class(HealthFailureKind::RateLimited, true, true);
    }
    if matches!(status, Some(404)) && lower.contains("model") {
        return class(HealthFailureKind::ModelMissing, true, true);
    }
    if lower.contains("model_not_found") || lower.contains("model not found") {
        return class(HealthFailureKind::ModelMissing, true, true);
    }
    if status.is_some_and(|value| value >= 500) {
        return class(HealthFailureKind::UpstreamFailed, true, true);
    }
    if status.is_none() {
        return class(HealthFailureKind::NetworkFailed, true, true);
    }
    class(HealthFailureKind::Unknown, false, false)
}

pub fn mark_success(health: &mut ProviderHealth) {
    health.last_checked = Some(Utc::now());
    health.status = HealthStatusKind::Healthy;
    health.last_success = Some(Utc::now());
    health.last_error = None;
    health.last_failure_kind = None;
    health.cooldown_until = None;
    health.failure_count = 0;
}

pub fn mark_failure(health: &mut ProviderHealth, failure: &FailureClassification, message: String) {
    health.last_checked = Some(Utc::now());
    health.failure_count = health.failure_count.saturating_add(1);
    health.last_error = Some(message);
    health.last_failure_kind = Some(failure.kind.clone());

    health.status = match failure.kind {
        HealthFailureKind::AuthFailed => HealthStatusKind::AuthFailed,
        HealthFailureKind::QuotaExhausted => HealthStatusKind::QuotaExhausted,
        HealthFailureKind::RateLimited => HealthStatusKind::RateLimited,
        HealthFailureKind::ModelMissing => HealthStatusKind::ModelMissing,
        HealthFailureKind::NetworkFailed => HealthStatusKind::Offline,
        HealthFailureKind::UpstreamFailed | HealthFailureKind::Unknown => {
            HealthStatusKind::Degraded
        }
    };

    if failure.cooldown {
        let seconds = cooldown_seconds(health.failure_count);
        health.cooldown_until = Some(Utc::now() + Duration::seconds(seconds));
        if !matches!(
            health.status,
            HealthStatusKind::AuthFailed
                | HealthStatusKind::QuotaExhausted
                | HealthStatusKind::ModelMissing
        ) {
            health.status = HealthStatusKind::Cooldown;
        }
    }
}

pub fn cooldown_active(health: &ProviderHealth) -> bool {
    health
        .cooldown_until
        .is_some_and(|until| until > Utc::now())
}

pub fn normalize_expired_cooldown(health: &mut ProviderHealth) {
    if health
        .cooldown_until
        .is_some_and(|until| until <= Utc::now())
        && matches!(
            health.status,
            HealthStatusKind::Cooldown | HealthStatusKind::RateLimited
        )
    {
        health.status = HealthStatusKind::Degraded;
        health.cooldown_until = None;
    }
}

fn class(kind: HealthFailureKind, retryable: bool, cooldown: bool) -> FailureClassification {
    FailureClassification {
        kind,
        retryable,
        cooldown,
    }
}

fn cooldown_seconds(failure_count: u32) -> i64 {
    let exponent = failure_count.saturating_sub(1).min(6);
    30 * 2_i64.pow(exponent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_failures() {
        assert_eq!(
            classify_failure(Some(401), "").kind,
            HealthFailureKind::AuthFailed
        );
        assert_eq!(
            classify_failure(Some(429), "rate limit").kind,
            HealthFailureKind::RateLimited
        );
        assert_eq!(
            classify_failure(Some(400), "insufficient_quota").kind,
            HealthFailureKind::QuotaExhausted
        );
        assert_eq!(
            classify_failure(Some(404), "model_not_found").kind,
            HealthFailureKind::ModelMissing
        );
        assert_eq!(
            classify_failure(Some(502), "bad gateway").kind,
            HealthFailureKind::UpstreamFailed
        );
        assert_eq!(
            classify_failure(None, "timeout").kind,
            HealthFailureKind::NetworkFailed
        );
    }

    #[test]
    fn failure_starts_cooldown() {
        let failure = classify_failure(Some(429), "rate limit");
        let mut health = ProviderHealth::default();
        mark_failure(&mut health, &failure, "rate limit".to_string());
        assert!(cooldown_active(&health));
        assert_eq!(health.failure_count, 1);
    }

    #[test]
    fn success_clears_cooldown() {
        let failure = classify_failure(Some(429), "rate limit");
        let mut health = ProviderHealth::default();
        mark_failure(&mut health, &failure, "rate limit".to_string());
        mark_success(&mut health);
        assert!(!cooldown_active(&health));
        assert_eq!(health.status, HealthStatusKind::Healthy);
    }
}
