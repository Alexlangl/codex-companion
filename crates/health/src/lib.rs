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

    if lower.contains("deactivated_workspace") {
        return class(HealthFailureKind::AuthFailed, false, true);
    }
    if lower.contains("insufficient_quota")
        || lower.contains("insufficient_balance")
        || lower.contains("insufficient account balance")
        || lower.contains("usage_limit_reached")
        || lower.contains("usage limit has been reached")
        || lower.contains("usage limit reached")
        || lower.contains("quota exceeded")
        || lower.contains("billing")
        || lower.contains("额度耗尽")
        || lower.contains("额度不足")
        || lower.contains("余额不足")
        || lower.contains("超出额度")
    {
        return class(HealthFailureKind::QuotaExhausted, true, true);
    }
    if matches!(status, Some(429))
        || lower.contains("rate_limit_exceeded")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("concurrency limit exceeded")
    {
        return class(HealthFailureKind::RateLimited, true, true);
    }
    if matches!(status, Some(404)) && lower.contains("model") {
        return class(HealthFailureKind::ModelMissing, true, true);
    }
    if lower.contains("model_not_found") || lower.contains("model not found") {
        return class(HealthFailureKind::ModelMissing, true, true);
    }
    if lower.contains("content_policy_violation")
        || lower.contains("content policy violation")
        || lower.contains("content filter")
        || lower.contains("safety policy")
    {
        return class(HealthFailureKind::RequestRejected, true, false);
    }
    if matches!(status, Some(401)) || (matches!(status, Some(403)) && explicit_auth_failure(&lower))
    {
        return class(HealthFailureKind::AuthFailed, false, true);
    }
    if matches!(status, Some(403)) {
        return class(HealthFailureKind::RequestRejected, true, false);
    }
    if lower.contains("upstream semantic failure")
        || lower.contains("response.failed")
        || lower.contains("upstream_stream_incomplete")
        || (lower.contains("json")
            && (lower.contains("解析") || lower.contains("parse") || lower.contains("decode")))
    {
        return class(HealthFailureKind::UpstreamFailed, true, true);
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
    if matches!(&failure.kind, HealthFailureKind::RequestRejected) {
        return;
    }
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
        HealthFailureKind::RequestRejected
        | HealthFailureKind::UpstreamFailed
        | HealthFailureKind::Unknown => HealthStatusKind::Degraded,
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

pub fn mark_model_failure(
    health: &mut ProviderHealth,
    failure: &FailureClassification,
    message: String,
) {
    mark_failure(health, failure, message);
    health.cooldown_until = None;
    if matches!(health.status, HealthStatusKind::Cooldown) {
        health.status = HealthStatusKind::Degraded;
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

pub fn repair_legacy_auth_misclassification(health: &mut ProviderHealth) -> bool {
    if !matches!(health.status, HealthStatusKind::AuthFailed) {
        return false;
    }
    let Some(message) = health.last_error.as_deref() else {
        return false;
    };
    let failure = classify_failure(None, message);
    let request_rejected = matches!(&failure.kind, HealthFailureKind::RequestRejected);
    health.status = match &failure.kind {
        HealthFailureKind::QuotaExhausted => HealthStatusKind::QuotaExhausted,
        HealthFailureKind::RateLimited => HealthStatusKind::RateLimited,
        HealthFailureKind::ModelMissing => HealthStatusKind::ModelMissing,
        HealthFailureKind::RequestRejected => {
            health.last_error = None;
            health.last_failure_kind = None;
            health.cooldown_until = None;
            health.failure_count = 0;
            if health.last_success.is_some() {
                HealthStatusKind::Healthy
            } else {
                HealthStatusKind::Unknown
            }
        }
        _ => return false,
    };
    if !request_rejected {
        health.last_failure_kind = Some(failure.kind);
    }
    true
}

fn explicit_auth_failure(body: &str) -> bool {
    [
        "invalid_api_key",
        "invalid api key",
        "incorrect api key",
        "api key is invalid",
        "invalid bearer token",
        "invalid access token",
        "token is invalid",
        "token expired",
        "expired token",
        "authentication_error",
        "authentication failed",
        "authentication required",
        "unauthorized",
        "凭证无效",
        "认证失败",
        "鉴权失败",
        "令牌无效",
    ]
    .iter()
    .any(|pattern| body.contains(pattern))
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
            classify_failure(Some(400), "deactivated_workspace").kind,
            HealthFailureKind::AuthFailed
        );
        assert_eq!(
            classify_failure(Some(400), "insufficient_quota").kind,
            HealthFailureKind::QuotaExhausted
        );
        assert_eq!(
            classify_failure(
                Some(403),
                r#"{"error":{"code":"INSUFFICIENT_BALANCE","message":"Insufficient account balance"}}"#
            )
            .kind,
            HealthFailureKind::QuotaExhausted
        );
        assert_eq!(
            classify_failure(
                Some(403),
                r#"{"error":{"code":"content_policy_violation"}}"#
            )
            .kind,
            HealthFailureKind::RequestRejected
        );
        assert_eq!(
            classify_failure(Some(403), r#"{"error":{"message":"invalid api key"}}"#).kind,
            HealthFailureKind::AuthFailed
        );
        assert_eq!(
            classify_failure(
                Some(429),
                r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached"}}"#
            )
            .kind,
            HealthFailureKind::QuotaExhausted
        );
        assert_eq!(
            classify_failure(
                None,
                r#"{"type":"response.failed","response":{"error":{"code":"rate_limit_exceeded","message":"Concurrency limit exceeded for account, please retry later"}}}"#
            )
            .kind,
            HealthFailureKind::RateLimited
        );
        assert_eq!(
            classify_failure(None, "额度不足").kind,
            HealthFailureKind::QuotaExhausted
        );
        assert_eq!(
            classify_failure(
                None,
                "解析 Codex 额度 JSON 失败: invalid type: null, expected a sequence"
            )
            .kind,
            HealthFailureKind::UpstreamFailed
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
            classify_failure(None, "upstream semantic failure: overloaded").kind,
            HealthFailureKind::UpstreamFailed
        );
        assert_eq!(
            classify_failure(None, "timeout").kind,
            HealthFailureKind::NetworkFailed
        );
    }

    #[test]
    fn request_rejection_does_not_change_provider_health() {
        let failure = classify_failure(Some(403), "content_policy_violation");
        let mut health = ProviderHealth::default();
        mark_failure(&mut health, &failure, "blocked prompt".to_string());

        assert_eq!(health.status, HealthStatusKind::Unknown);
        assert_eq!(health.failure_count, 0);
        assert!(health.last_error.is_none());
    }

    #[test]
    fn repairs_legacy_403_misclassifications() {
        let mut quota = ProviderHealth {
            status: HealthStatusKind::AuthFailed,
            last_error: Some("上游返回 403: INSUFFICIENT_BALANCE".to_string()),
            last_failure_kind: Some(HealthFailureKind::AuthFailed),
            ..ProviderHealth::default()
        };
        assert!(repair_legacy_auth_misclassification(&mut quota));
        assert_eq!(quota.status, HealthStatusKind::QuotaExhausted);
        assert_eq!(
            quota.last_failure_kind,
            Some(HealthFailureKind::QuotaExhausted)
        );

        let mut policy = ProviderHealth {
            status: HealthStatusKind::AuthFailed,
            last_error: Some("上游返回 403: content_policy_violation".to_string()),
            last_failure_kind: Some(HealthFailureKind::AuthFailed),
            failure_count: 1,
            ..ProviderHealth::default()
        };
        assert!(repair_legacy_auth_misclassification(&mut policy));
        assert_eq!(policy.status, HealthStatusKind::Unknown);
        assert!(policy.last_failure_kind.is_none());
        assert!(policy.last_error.is_none());
        assert_eq!(policy.failure_count, 0);
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

    #[test]
    fn model_failure_does_not_cool_down_unrelated_models() {
        let failure = classify_failure(Some(429), "rate limit");
        let mut health = ProviderHealth::default();
        mark_model_failure(&mut health, &failure, "model rate limit".to_string());
        assert!(!cooldown_active(&health));
        assert_eq!(health.failure_count, 1);
    }
}
