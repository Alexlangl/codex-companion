use crate::auth::resolve_auth_token;
use crate::codex_oauth::ensure_codex_auth_snapshot;
use chrono::{DateTime, Utc};
use codex_companion_core::{
    CompanionError, ProviderAccountInfo, ProviderConfig, ProviderKind, ProviderQuotaWindow, Result,
};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION};
use serde::Deserialize;
use url::Url;

const ACCOUNT_CHECK_URL: &str = "https://chatgpt.com/backend-api/wham/accounts/check";
const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

#[derive(Debug, Clone, Default)]
struct AccountProfile {
    name: Option<String>,
    structure: Option<String>,
    account_id: Option<String>,
    plan_type: Option<String>,
    subscription_active_until: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UsageResponse {
    #[serde(rename = "plan_type")]
    plan_type: Option<String>,
    #[serde(rename = "rate_limit")]
    rate_limit: Option<RateLimitInfo>,
    #[serde(rename = "code_review_rate_limit")]
    _code_review_rate_limit: Option<RateLimitInfo>,
}

#[derive(Debug, Deserialize)]
struct RateLimitInfo {
    allowed: Option<bool>,
    #[serde(rename = "limit_reached")]
    limit_reached: Option<bool>,
    #[serde(rename = "primary_window")]
    primary_window: Option<WindowInfo>,
    #[serde(rename = "secondary_window")]
    secondary_window: Option<WindowInfo>,
}

#[derive(Debug, Deserialize)]
struct WindowInfo {
    #[serde(rename = "used_percent")]
    used_percent: Option<f64>,
    #[serde(rename = "limit_window_seconds")]
    limit_window_seconds: Option<i64>,
    #[serde(rename = "reset_after_seconds")]
    reset_after_seconds: Option<i64>,
    #[serde(rename = "reset_at")]
    reset_at: Option<i64>,
}

pub async fn refresh_official_codex_account(
    provider: &ProviderConfig,
) -> Result<ProviderAccountInfo> {
    if provider.kind != ProviderKind::OfficialCodex {
        return Err(CompanionError::InvalidConfig(format!(
            "provider {} 不是 Codex 官方账号",
            provider.id
        )));
    }

    let auth = ensure_codex_auth_snapshot(provider).await?;
    let client = reqwest::Client::new();
    let account_id = provider
        .account
        .as_ref()
        .and_then(|account| account.account_id.clone())
        .or_else(|| auth.account_id.clone());
    let mut account = provider.account.clone().unwrap_or_default();
    if account.email.is_none() {
        account.email = auth.email.clone();
    }
    if account.display_name.is_none() {
        account.display_name = auth.name.clone().or_else(|| auth.email.clone());
    }
    if account.account_id.is_none() {
        account.account_id = account_id.clone();
    }
    if account.subscription_type.is_none() {
        account.subscription_type = auth
            .plan_type
            .clone()
            .map(|value| value.to_ascii_uppercase());
    }

    if let Ok(profile) =
        fetch_account_profile(&client, &auth.access_token, account_id.as_deref()).await
    {
        if let Some(account_id) = profile.account_id {
            account.account_id = Some(account_id);
        }
        if let Some(name) = profile.name {
            account.team_name = Some(name.clone());
            if account.display_name.is_none() {
                account.display_name = Some(name);
            }
        }
        if let Some(structure) = profile.structure {
            account.subscription_status = Some(structure);
        }
        if let Some(plan_type) = profile.plan_type {
            account.subscription_type = Some(plan_type.to_ascii_uppercase());
        }
        if let Some(subscription_active_until) = profile.subscription_active_until {
            account.valid_until = Some(subscription_active_until);
        }
    }

    let usage = fetch_usage(&client, &auth.access_token, account.account_id.as_deref()).await?;
    apply_usage_to_account(&mut account, usage);
    account.last_refresh_at = Some(Utc::now().to_rfc3339());
    Ok(account)
}

pub async fn refresh_api_key_usage(provider: &ProviderConfig) -> Result<ProviderAccountInfo> {
    let token = resolve_auth_token(provider).ok_or_else(|| {
        CompanionError::InvalidConfig(format!("provider {} 缺少 API key", provider.id))
    })?;
    let client = reqwest::Client::new();
    let mut account = provider.account.clone().unwrap_or_default();
    account.display_name = account.display_name.or_else(|| Some(provider.name.clone()));
    account.subscription_type = account
        .subscription_type
        .or_else(|| Some("API Key".to_string()));
    account.subscription_status = Some("连接正常".to_string());

    let mut last_error = None;
    for usage_url in api_usage_endpoints(&provider.base_url) {
        match fetch_api_usage(&client, &usage_url, &token).await {
            Ok(value) => {
                apply_api_key_usage_to_account(&mut account, &value);
                account.last_refresh_at = Some(Utc::now().to_rfc3339());
                return Ok(account);
            }
            Err(error) => last_error = Some(error),
        }
    }
    account.quota_label = account.quota_label.or_else(|| {
        Some(match last_error {
            Some(error) => format!("余量接口不可用：{error}"),
            None => "该 provider 未声明余量接口".to_string(),
        })
    });
    account.last_refresh_at = Some(Utc::now().to_rfc3339());
    Ok(account)
}

async fn fetch_api_usage(
    client: &reqwest::Client,
    usage_url: &str,
    token: &str,
) -> std::result::Result<serde_json::Value, String> {
    let response = client
        .get(usage_url)
        .bearer_auth(token)
        .header(ACCEPT, "application/json")
        .send()
        .await
        .map_err(|source| format!("请求失败: {source}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|source| format!("读取响应失败: {source}"))?;
    if !status.is_success() {
        return Err(format!("{status} [body_len:{}]", body.len()));
    }
    serde_json::from_str::<serde_json::Value>(&body)
        .map_err(|source| format!("解析 JSON 失败: {source}"))
}

async fn fetch_account_profile(
    client: &reqwest::Client,
    access_token: &str,
    account_id: Option<&str>,
) -> std::result::Result<AccountProfile, String> {
    let response = client
        .get(ACCOUNT_CHECK_URL)
        .headers(codex_headers(access_token, account_id)?)
        .send()
        .await
        .map_err(|error| format!("请求账号信息失败: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| format!("读取账号信息响应失败: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "账号信息接口返回 {status}，body_len={}",
            body.len()
        ));
    }
    let value = serde_json::from_str::<serde_json::Value>(&body)
        .map_err(|error| format!("账号信息 JSON 解析失败: {error}"))?;
    Ok(parse_account_profile(&value, account_id))
}

async fn fetch_usage(
    client: &reqwest::Client,
    access_token: &str,
    account_id: Option<&str>,
) -> Result<UsageResponse> {
    let response = client
        .get(USAGE_URL)
        .headers(codex_headers(access_token, account_id).map_err(CompanionError::InvalidConfig)?)
        .send()
        .await
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("请求 Codex 额度失败: {source}"))
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|source| {
        CompanionError::InvalidConfig(format!("读取 Codex 额度响应失败: {source}"))
    })?;
    if !status.is_success() {
        let code = extract_error_code(&body)
            .map(|code| format!(" [error_code:{code}]"))
            .unwrap_or_default();
        return Err(CompanionError::InvalidConfig(format!(
            "Codex 额度接口返回 {status}{code} [body_len:{}]",
            body.len()
        )));
    }
    serde_json::from_str::<UsageResponse>(&body).map_err(|source| {
        CompanionError::InvalidConfig(format!("解析 Codex 额度 JSON 失败: {source}"))
    })
}

fn codex_headers(
    access_token: &str,
    account_id: Option<&str>,
) -> std::result::Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {access_token}"))
            .map_err(|error| format!("构建 Authorization 头失败: {error}"))?,
    );
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    if let Some(account_id) = account_id.and_then(normalize_optional) {
        headers.insert(
            "ChatGPT-Account-Id",
            HeaderValue::from_str(&account_id)
                .map_err(|error| format!("构建 ChatGPT-Account-Id 头失败: {error}"))?,
        );
    }
    Ok(headers)
}

fn parse_account_profile(value: &serde_json::Value, expected_id: Option<&str>) -> AccountProfile {
    let records = collect_account_records(value);
    if records.is_empty() {
        return AccountProfile::default();
    }
    let selected = expected_id
        .and_then(|expected| {
            records.iter().find(|record| {
                pick_first_string(
                    &[*record],
                    &[
                        &["id"],
                        &["account_id"],
                        &["accountId"],
                        &["chatgpt_account_id"],
                        &["workspace_id"],
                    ],
                )
                .as_deref()
                    == Some(expected)
            })
        })
        .or_else(|| records.first())
        .copied()
        .unwrap_or(value);

    AccountProfile {
        name: pick_first_string(
            &[selected],
            &[
                &["name"],
                &["display_name"],
                &["displayName"],
                &["account_name"],
                &["organization_name"],
                &["workspace_name"],
                &["title"],
            ],
        ),
        structure: pick_first_string(
            &[selected],
            &[
                &["structure"],
                &["account_structure"],
                &["accountStructure"],
                &["kind"],
                &["type"],
                &["account_type"],
            ],
        ),
        account_id: pick_first_string(
            &[selected],
            &[
                &["id"],
                &["account_id"],
                &["accountId"],
                &["chatgpt_account_id"],
                &["workspace_id"],
            ],
        ),
        plan_type: pick_first_string(
            &[selected],
            &[
                &["plan_type"],
                &["planType"],
                &["auth_file_plan_type"],
                &["chatgpt_plan_type"],
                &["subscription", "plan_type"],
                &["entitlement", "plan_type"],
            ],
        ),
        subscription_active_until: pick_first_string(
            &[selected],
            &[
                &["subscription_active_until"],
                &["subscriptionActiveUntil"],
                &["chatgpt_subscription_active_until"],
                &["subscription", "active_until"],
                &["subscription", "activeUntil"],
                &["entitlement", "subscription_active_until"],
                &["entitlement", "expires_at"],
            ],
        ),
    }
}

fn collect_account_records(value: &serde_json::Value) -> Vec<&serde_json::Value> {
    let mut records = Vec::new();
    for path in [
        &["accounts"][..],
        &["data", "accounts"][..],
        &["account_items"][..],
        &["items"][..],
    ] {
        if let Some(array) = get_path(value, path).and_then(serde_json::Value::as_array) {
            records.extend(array.iter().filter(|item| item.is_object()));
        }
    }
    if records.is_empty() {
        if let Some(object) = value.get("accounts").and_then(serde_json::Value::as_object) {
            records.extend(object.values().filter(|item| item.is_object()));
        }
    }
    if records.is_empty() && value.is_object() {
        records.push(value);
    }
    records
}

fn apply_usage_to_account(account: &mut ProviderAccountInfo, usage: UsageResponse) {
    if let Some(plan_type) = usage.plan_type.as_ref() {
        account.subscription_type = Some(plan_type.to_ascii_uppercase());
    }
    let limit_reached = usage
        .rate_limit
        .as_ref()
        .and_then(|rate| rate.limit_reached)
        .unwrap_or(false);
    let allowed = usage
        .rate_limit
        .as_ref()
        .and_then(|rate| rate.allowed)
        .unwrap_or(!limit_reached);
    account.subscription_status = Some(if limit_reached || !allowed {
        "额度耗尽".to_string()
    } else {
        "可用".to_string()
    });

    let windows = usage_windows(&usage);
    if !windows.is_empty() {
        let lowest = windows
            .iter()
            .min_by(|left, right| {
                left.remaining_percent
                    .partial_cmp(&right.remaining_percent)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned();
        account.quota_label = Some(
            windows
                .iter()
                .map(|window| format!("{} {}%", window.label, window.remaining_percent.round()))
                .collect::<Vec<_>>()
                .join(" / "),
        );
        if let Some(window) = lowest {
            account.quota_percent = Some(window.remaining_percent);
            account.quota_reset_at = window.reset_at;
        }
        account.quota_windows = windows;
    }
}

fn apply_api_key_usage_to_account(account: &mut ProviderAccountInfo, value: &serde_json::Value) {
    let usage = usage_record(value);
    let total = pick_first_number(
        usage,
        &[
            &["total_granted"],
            &["total"],
            &["total_quota"],
            &["quota"],
            &["quota_total"],
            &["total_usd_granted"],
            &["limit"],
        ],
    );
    let used = pick_first_number(
        usage,
        &[
            &["total_used"],
            &["used"],
            &["used_quota"],
            &["quota_used"],
            &["total_usd_used"],
            &["usage"],
        ],
    );
    let available = pick_first_number(
        usage,
        &[
            &["total_available"],
            &["available"],
            &["remaining"],
            &["remaining_quota"],
            &["quota_remaining"],
            &["total_usd_available"],
            &["limit_remaining"],
            &["limitRemaining"],
        ],
    );
    let unlimited = pick_first_bool(
        usage,
        &[
            &["unlimited_quota"],
            &["unlimited"],
            &["is_unlimited"],
            &["unlimitedQuota"],
        ],
    )
    .unwrap_or(false);
    let summary = pick_first_string(
        &[usage],
        &[
            &["summary_display"],
            &["summaryDisplay"],
            &["display"],
            &["description"],
        ],
    );
    let expires_at = pick_first_timestamp(
        usage,
        &[
            &["expires_at"],
            &["expiresAt"],
            &["expire_at"],
            &["expireAt"],
        ],
    );

    account.usage_total = total;
    account.usage_used = used;
    account.usage_available = available;
    account.valid_until = expires_at.clone().or_else(|| account.valid_until.clone());
    account.quota_reset_at = expires_at
        .clone()
        .or_else(|| account.quota_reset_at.clone());

    let remaining_percent = if unlimited {
        Some(100.0)
    } else {
        available.zip(total).and_then(|(available, total)| {
            if total > 0.0 {
                Some((available / total * 100.0).clamp(0.0, 100.0))
            } else {
                None
            }
        })
    };
    if let Some(percent) = remaining_percent {
        account.quota_percent = Some(percent);
        account.quota_windows = vec![ProviderQuotaWindow {
            label: "API".to_string(),
            remaining_percent: percent,
            reset_at: expires_at,
            window_minutes: None,
        }];
    }
    account.quota_label = summary.or_else(|| match (available, total) {
        (Some(available), Some(total)) if total > 0.0 => Some(format!(
            "{} / {}",
            compact_number(available),
            compact_number(total)
        )),
        (Some(available), _) => Some(format!("剩余 {}", compact_number(available))),
        _ if unlimited => Some("不限量".to_string()),
        _ => account.quota_label.clone(),
    });
    account.subscription_status = Some(if unlimited || remaining_percent.unwrap_or(0.0) > 0.0 {
        "可用".to_string()
    } else {
        "额度耗尽".to_string()
    });
}

fn usage_windows(usage: &UsageResponse) -> Vec<ProviderQuotaWindow> {
    let Some(rate_limit) = usage.rate_limit.as_ref() else {
        return Vec::new();
    };
    [
        window_summary("5h", rate_limit.primary_window.as_ref()),
        window_summary("Week", rate_limit.secondary_window.as_ref()),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn window_summary(
    fallback_label: &str,
    window: Option<&WindowInfo>,
) -> Option<ProviderQuotaWindow> {
    let window = window?;
    let used = window.used_percent.unwrap_or(0.0).clamp(0.0, 100.0);
    let window_minutes = window.limit_window_seconds.and_then(|seconds| {
        if seconds > 0 {
            Some((seconds + 59) / 60)
        } else {
            None
        }
    });
    Some(ProviderQuotaWindow {
        label: window_label(window_minutes, fallback_label),
        remaining_percent: 100.0 - used,
        reset_at: reset_at_iso(window),
        window_minutes,
    })
}

fn window_label(window_minutes: Option<i64>, fallback: &str) -> String {
    let Some(minutes) = window_minutes else {
        return fallback.to_string();
    };
    if minutes >= 10_080 {
        "Week".to_string()
    } else if minutes >= 60 && minutes % 60 == 0 {
        format!("{}h", minutes / 60)
    } else {
        format!("{minutes}m")
    }
}

fn reset_at_iso(window: &WindowInfo) -> Option<String> {
    let timestamp = window.reset_at.or_else(|| {
        window.reset_after_seconds.and_then(|seconds| {
            if seconds >= 0 {
                Some(Utc::now().timestamp() + seconds)
            } else {
                None
            }
        })
    })?;
    DateTime::<Utc>::from_timestamp(timestamp, 0).map(|date| date.to_rfc3339())
}

fn extract_error_code(body: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    pick_first_string(
        &[&value],
        &[
            &["detail", "code"],
            &["error", "code"],
            &["code"],
            &["error_code"],
        ],
    )
}

fn api_usage_endpoints(base_url: &str) -> Vec<String> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let host = Url::parse(trimmed)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
        .unwrap_or_default();
    let mut endpoints = Vec::new();
    if host.contains("openrouter.ai") {
        endpoints.push("https://openrouter.ai/api/v1/auth/key".to_string());
    }
    endpoints.push(new_api_usage_url(trimmed));
    endpoints.push(api_root_url(trimmed, "/api/v1/user/token"));
    endpoints.push(api_root_url(trimmed, "/api/user/self"));
    dedupe_strings(endpoints)
}

fn new_api_usage_url(base_url: &str) -> String {
    api_root_url(base_url, "/api/usage/token")
}

fn api_root_url(base_url: &str, path: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    let root = trimmed
        .strip_suffix("/v1")
        .or_else(|| trimmed.strip_suffix("/v1/"))
        .unwrap_or(trimmed);
    if root.ends_with("/api") && path.starts_with("/api/") {
        return format!(
            "{}{}",
            root.trim_end_matches('/'),
            path.strip_prefix("/api").unwrap_or(path)
        );
    }
    format!("{}{}", root.trim_end_matches('/'), path)
}

fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    let mut output = Vec::new();
    for value in values {
        if !output.contains(&value) {
            output.push(value);
        }
    }
    output
}

fn usage_record(value: &serde_json::Value) -> &serde_json::Value {
    value
        .get("usage")
        .filter(|usage| usage.is_object())
        .or_else(|| {
            value
                .get("data")
                .and_then(|data| data.get("usage"))
                .filter(|usage| usage.is_object())
        })
        .or_else(|| value.get("data").filter(|data| data.is_object()))
        .or_else(|| {
            value
                .get("profile")
                .and_then(|profile| profile.get("usage"))
                .filter(|usage| usage.is_object())
        })
        .unwrap_or(value)
}

fn pick_first_number(value: &serde_json::Value, paths: &[&[&str]]) -> Option<f64> {
    for path in paths {
        let Some(item) = get_path(value, path) else {
            continue;
        };
        if let Some(number) = item.as_f64() {
            return Some(number);
        }
        if let Some(number) = item
            .as_str()
            .and_then(|text| text.trim().parse::<f64>().ok())
        {
            return Some(number);
        }
    }
    None
}

fn pick_first_bool(value: &serde_json::Value, paths: &[&[&str]]) -> Option<bool> {
    for path in paths {
        let Some(item) = get_path(value, path) else {
            continue;
        };
        if let Some(value) = item.as_bool() {
            return Some(value);
        }
        if let Some(value) = item.as_str() {
            match value.trim().to_ascii_lowercase().as_str() {
                "true" | "1" | "yes" => return Some(true),
                "false" | "0" | "no" => return Some(false),
                _ => {}
            }
        }
    }
    None
}

fn pick_first_timestamp(value: &serde_json::Value, paths: &[&[&str]]) -> Option<String> {
    for path in paths {
        let Some(item) = get_path(value, path) else {
            continue;
        };
        if let Some(text) = item.as_str().and_then(normalize_optional) {
            return Some(text);
        }
        if let Some(number) = item.as_i64() {
            if number <= 0 {
                continue;
            }
            if let Some(date) = DateTime::<Utc>::from_timestamp(number, 0) {
                return Some(date.to_rfc3339());
            }
        }
    }
    None
}

fn compact_number(value: f64) -> String {
    if (value.fract()).abs() < f64::EPSILON {
        format!("{}", value as i64)
    } else {
        format!("{value:.2}")
    }
}

fn normalize_optional(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn pick_first_string(value: &[&serde_json::Value], paths: &[&[&str]]) -> Option<String> {
    for source in value {
        for path in paths {
            if let Some(text) = get_path(source, path)
                .and_then(serde_json::Value::as_str)
                .and_then(normalize_optional)
            {
                return Some(text);
            }
        }
    }
    None
}

fn get_path<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a serde_json::Value> {
    let mut cursor = value;
    for key in path {
        cursor = cursor.get(*key)?;
    }
    Some(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usage_windows_like_cockpit() {
        let usage = serde_json::from_value::<UsageResponse>(serde_json::json!({
            "plan_type": "team",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": {
                    "used_percent": 30,
                    "limit_window_seconds": 18000,
                    "reset_at": 1780800000
                },
                "secondary_window": {
                    "used_percent": 23,
                    "limit_window_seconds": 604800,
                    "reset_after_seconds": 3600
                }
            }
        }))
        .expect("usage");
        let mut account = ProviderAccountInfo::default();
        apply_usage_to_account(&mut account, usage);
        assert_eq!(account.subscription_type.as_deref(), Some("TEAM"));
        assert_eq!(account.subscription_status.as_deref(), Some("可用"));
        assert_eq!(account.quota_windows.len(), 2);
        assert_eq!(account.quota_windows[0].label, "5h");
        assert_eq!(account.quota_windows[0].remaining_percent, 70.0);
        assert_eq!(account.quota_windows[1].label, "Week");
        assert_eq!(account.quota_windows[1].remaining_percent, 77.0);
        assert_eq!(account.quota_percent, Some(70.0));
        assert_eq!(account.quota_label.as_deref(), Some("5h 70% / Week 77%"));
    }

    #[test]
    fn parses_account_check_profile() {
        let value = serde_json::json!({
            "accounts": [{
                "id": "acct-1",
                "name": "035611",
                "structure": "team",
                "plan_type": "team",
                "subscription_active_until": "2026-07-06T13:27:00Z"
            }]
        });
        let profile = parse_account_profile(&value, Some("acct-1"));
        assert_eq!(profile.account_id.as_deref(), Some("acct-1"));
        assert_eq!(profile.name.as_deref(), Some("035611"));
        assert_eq!(profile.structure.as_deref(), Some("team"));
        assert_eq!(profile.plan_type.as_deref(), Some("team"));
        assert_eq!(
            profile.subscription_active_until.as_deref(),
            Some("2026-07-06T13:27:00Z")
        );
    }

    #[test]
    fn parses_new_api_usage() {
        let value = serde_json::json!({
            "usage": {
                "total_granted": 1000,
                "total_used": 250,
                "total_available": 750,
                "expires_at": 1780800000,
                "summary_display": "750 / 1000"
            }
        });
        let mut account = ProviderAccountInfo::default();
        apply_api_key_usage_to_account(&mut account, &value);
        assert_eq!(account.usage_total, Some(1000.0));
        assert_eq!(account.usage_used, Some(250.0));
        assert_eq!(account.usage_available, Some(750.0));
        assert_eq!(account.quota_percent, Some(75.0));
        assert_eq!(account.quota_label.as_deref(), Some("750 / 1000"));
        assert_eq!(account.quota_windows.len(), 1);
    }

    #[test]
    fn parses_openrouter_key_usage() {
        let value = serde_json::json!({
            "data": {
                "usage": 25,
                "limit": 100,
                "limit_remaining": 75
            }
        });
        let mut account = ProviderAccountInfo::default();
        apply_api_key_usage_to_account(&mut account, &value);
        assert_eq!(account.usage_total, Some(100.0));
        assert_eq!(account.usage_used, Some(25.0));
        assert_eq!(account.usage_available, Some(75.0));
        assert_eq!(account.quota_percent, Some(75.0));
    }

    #[test]
    fn generates_provider_specific_usage_endpoints() {
        let endpoints = api_usage_endpoints("https://openrouter.ai/api/v1");
        assert_eq!(
            endpoints.first().map(String::as_str),
            Some("https://openrouter.ai/api/v1/auth/key")
        );
        assert!(endpoints.contains(&"https://openrouter.ai/api/usage/token".to_string()));

        let endpoints = api_usage_endpoints("https://new-api.example.com/v1");
        assert_eq!(
            endpoints.first().map(String::as_str),
            Some("https://new-api.example.com/api/usage/token")
        );
    }
}
