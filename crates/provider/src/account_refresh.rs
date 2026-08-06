use crate::auth::resolve_auth_token;
use crate::codex_oauth::ensure_codex_auth_snapshot;
use crate::types::ProviderUsageQueryTestInput;
use chrono::{DateTime, Local, Utc};
use codex_companion_core::{
    CompanionError, ProviderAccountInfo, ProviderConfig, ProviderKind, ProviderQuotaWindow,
    ProviderUsageQueryTemplate, Result,
};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, REFERER, USER_AGENT};
use rquickjs::{Context, Function, Runtime};
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;
use std::path::Path;
use url::{Host, Url};

const ACCOUNT_CHECK_URL: &str = "https://chatgpt.com/backend-api/accounts/check/v4-2023-04-27";
const LEGACY_ACCOUNT_CHECK_URL: &str = "https://chatgpt.com/backend-api/wham/accounts/check";
const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const CHATGPT_WEB_REFERER: &str = "https://chatgpt.com/";
const CHATGPT_WEB_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36";
const DEFAULT_USAGE_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const DEFAULT_USAGE_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
    #[serde(
        rename = "additional_rate_limits",
        default,
        deserialize_with = "deserialize_null_default"
    )]
    additional_rate_limits: Vec<AdditionalRateLimit>,
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Deserialize)]
struct AdditionalRateLimit {
    #[serde(rename = "limit_name")]
    limit_name: Option<String>,
    #[serde(rename = "metered_feature")]
    metered_feature: Option<String>,
    #[serde(rename = "rate_limit")]
    rate_limit: Option<RateLimitInfo>,
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
    let client = reqwest::Client::builder()
        .timeout(DEFAULT_USAGE_HTTP_TIMEOUT)
        .connect_timeout(DEFAULT_USAGE_CONNECT_TIMEOUT)
        .build()
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("创建 Codex 额度客户端失败: {source}"))
        })?;
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

pub async fn refresh_api_key_usage(
    provider: &ProviderConfig,
    data_dir: &Path,
) -> Result<ProviderAccountInfo> {
    let mut account = provider.account.clone().unwrap_or_default();
    account.display_name = account.display_name.or_else(|| Some(provider.name.clone()));
    account.subscription_type = account
        .subscription_type
        .or_else(|| Some("API Key".to_string()));
    account.subscription_status = Some("连接正常".to_string());

    if let Some(query) = account.usage_query.clone() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(
                query.timeout_seconds.clamp(2, 30),
            ))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|source| {
                CompanionError::InvalidConfig(format!("创建余量查询客户端失败: {source}"))
            })?;
        let mut credentials = load_usage_query_credentials(data_dir, &provider.id)?;
        if credentials.api_key.as_deref().is_none_or(str::is_empty) {
            credentials.api_key = resolve_auth_token(provider);
        }
        let value = fetch_configured_usage(&client, &query, &credentials).await?;
        apply_configured_usage_to_account(&mut account, &value)?;
        account.last_refresh_at = Some(Utc::now().to_rfc3339());
        return Ok(account);
    }

    let token = resolve_auth_token(provider).ok_or_else(|| {
        CompanionError::InvalidConfig(format!("provider {} 缺少 API key", provider.id))
    })?;
    let client = reqwest::Client::builder()
        .timeout(DEFAULT_USAGE_HTTP_TIMEOUT)
        .connect_timeout(DEFAULT_USAGE_CONNECT_TIMEOUT)
        .build()
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("创建余量查询客户端失败: {source}"))
        })?;
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
    Err(CompanionError::InvalidConfig(match last_error {
        Some(error) => format!("余量接口不可用：{error}"),
        None => "该 provider 未声明余量接口".to_string(),
    }))
}

#[derive(Debug, Default, Deserialize)]
struct UsageQueryCredentials {
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
}

fn load_usage_query_credentials(
    data_dir: &Path,
    provider_id: &str,
) -> Result<UsageQueryCredentials> {
    let path = data_dir
        .join("auth")
        .join("usage-queries")
        .join(format!("{provider_id}.json"));
    let text =
        std::fs::read_to_string(&path).map_err(|source| CompanionError::io(&path, source))?;
    let credentials = serde_json::from_str::<UsageQueryCredentials>(&text).map_err(|source| {
        CompanionError::InvalidConfig(format!("余量查询凭据格式无效: {source}"))
    })?;
    Ok(credentials)
}

#[derive(Debug, Deserialize)]
struct UsageScriptRequest {
    url: String,
    method: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: Option<String>,
}

const USAGE_SCRIPT_MEMORY_LIMIT_BYTES: usize = 16 * 1024 * 1024;
const USAGE_SCRIPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const USAGE_RESPONSE_LIMIT_BYTES: usize = 1024 * 1024;

pub fn usage_query_preset(template: ProviderUsageQueryTemplate) -> String {
    match template {
        ProviderUsageQueryTemplate::General | ProviderUsageQueryTemplate::Custom => r#"({
  request: {
    url: "{{baseUrl}}/user/balance",
    method: "GET",
    headers: {
      "Authorization": "Bearer {{apiKey}}"
    }
  },
  extractor: function (response) {
    return {
      isValid: response.is_active !== false,
      remaining: response.balance,
      unit: response.unit || "USD"
    };
  }
})"#
        .to_string(),
        ProviderUsageQueryTemplate::NewApi => r#"({
  request: {
    url: "{{baseUrl}}/api/user/self",
    method: "GET",
    headers: {
      "Content-Type": "application/json",
      "Authorization": "Bearer {{accessToken}}",
      "New-Api-User": "{{userId}}"
    }
  },
  extractor: function (response) {
    if (response.success && response.data) {
      return {
        planName: response.data.group || "默认套餐",
        remaining: response.data.quota / 500000,
        used: response.data.used_quota / 500000,
        total: (response.data.quota + response.data.used_quota) / 500000,
        unit: "USD"
      };
    }
    return {
      isValid: false,
      invalidMessage: response.message || "查询失败"
    };
  }
})"#
        .to_string(),
        ProviderUsageQueryTemplate::OpenRouter => r#"({
  request: {
    url: "{{baseUrl}}/api/v1/credits",
    method: "GET",
    headers: {
      "Authorization": "Bearer {{apiKey}}"
    }
  },
  extractor: function (response) {
    var data = response.data || response;
    var total = data.total_credits;
    var used = data.total_usage || 0;
    return {
      remaining: total == null ? null : Math.max(total - used, 0),
      used: used,
      total: total,
      unit: "USD",
      planName: "OpenRouter"
    };
  }
})"#
        .to_string(),
    }
}

async fn fetch_configured_usage(
    client: &reqwest::Client,
    query: &codex_companion_core::ProviderUsageQuery,
    credentials: &UsageQueryCredentials,
) -> Result<serde_json::Value> {
    let script = if query.script.trim().is_empty() {
        usage_query_preset(query.template)
    } else {
        query.script.clone()
    };
    let request = evaluate_usage_request(&script)?;
    let request = replace_usage_request_variables(request, query, credentials);
    validate_usage_request_url(&request.url, &query.base_url, query.template)?;
    let method = request.method.parse::<reqwest::Method>().map_err(|_| {
        CompanionError::InvalidConfig(format!("余额查询不支持请求方法 {}", request.method))
    })?;
    if matches!(method, reqwest::Method::CONNECT | reqwest::Method::TRACE) {
        return Err(CompanionError::InvalidConfig(format!(
            "余额查询不支持请求方法 {method}"
        )));
    }
    let mut outgoing = client.request(method, &request.url);
    for (name, value) in request.headers {
        outgoing = outgoing.header(name, value);
    }
    if let Some(body) = request.body {
        outgoing = outgoing.body(body);
    }
    let mut response = outgoing.send().await.map_err(|source| {
        CompanionError::InvalidConfig(format!("余额查询网络请求失败: {source}"))
    })?;
    let status = response.status();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|source| {
        CompanionError::InvalidConfig(format!("读取余额查询响应失败: {source}"))
    })? {
        if body.len().saturating_add(chunk.len()) > USAGE_RESPONSE_LIMIT_BYTES {
            return Err(CompanionError::InvalidConfig(
                "余额查询响应超过 1 MiB 限制".to_string(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        return Err(CompanionError::InvalidConfig(format!(
            "余额查询接口返回 {status} [body_len:{}]",
            body.len()
        )));
    }
    let response_value = serde_json::from_slice::<serde_json::Value>(&body).map_err(|source| {
        CompanionError::InvalidConfig(format!("解析余额查询响应失败: {source}"))
    })?;
    evaluate_usage_extractor(&script, &response_value)
}

fn script_runtime() -> Result<Runtime> {
    let runtime = Runtime::new().map_err(|source| {
        CompanionError::InvalidConfig(format!("创建余额查询脚本运行时失败: {source}"))
    })?;
    runtime.set_memory_limit(USAGE_SCRIPT_MEMORY_LIMIT_BYTES);
    runtime.set_max_stack_size(256 * 1024);
    let deadline = std::time::Instant::now() + USAGE_SCRIPT_TIMEOUT;
    runtime.set_interrupt_handler(Some(Box::new(move || std::time::Instant::now() > deadline)));
    Ok(runtime)
}

fn evaluate_usage_request(script: &str) -> Result<UsageScriptRequest> {
    let runtime = script_runtime()?;
    let context = Context::full(&runtime).map_err(|source| {
        CompanionError::InvalidConfig(format!("创建余额查询脚本上下文失败: {source}"))
    })?;
    let json = context.with(|ctx| {
        let config: rquickjs::Object = ctx.eval(script).map_err(|source| {
            CompanionError::InvalidConfig(format!("解析余额查询脚本失败: {source}"))
        })?;
        let request: rquickjs::Object = config.get("request").map_err(|source| {
            CompanionError::InvalidConfig(format!("余额查询脚本缺少 request: {source}"))
        })?;
        ctx.json_stringify(request)
            .map_err(|source| {
                CompanionError::InvalidConfig(format!("序列化余额查询 request 失败: {source}"))
            })?
            .ok_or_else(|| CompanionError::InvalidConfig("余额查询 request 为空".to_string()))?
            .get::<String>()
            .map_err(|source| {
                CompanionError::InvalidConfig(format!("读取余额查询 request 失败: {source}"))
            })
    })?;
    serde_json::from_str(&json).map_err(|source| {
        CompanionError::InvalidConfig(format!("余额查询 request 格式无效: {source}"))
    })
}

fn evaluate_usage_extractor(
    script: &str,
    response: &serde_json::Value,
) -> Result<serde_json::Value> {
    let runtime = script_runtime()?;
    let context = Context::full(&runtime).map_err(|source| {
        CompanionError::InvalidConfig(format!("创建余额查询脚本上下文失败: {source}"))
    })?;
    let response_json = serde_json::to_string(response).map_err(|source| {
        CompanionError::InvalidConfig(format!("序列化余额查询响应失败: {source}"))
    })?;
    let result_json = context.with(|ctx| {
        let config: rquickjs::Object = ctx.eval(script).map_err(|source| {
            CompanionError::InvalidConfig(format!("解析余额查询脚本失败: {source}"))
        })?;
        let extractor: Function = config.get("extractor").map_err(|source| {
            CompanionError::InvalidConfig(format!("余额查询脚本缺少 extractor: {source}"))
        })?;
        let response = ctx.json_parse(response_json).map_err(|source| {
            CompanionError::InvalidConfig(format!("注入余额查询响应失败: {source}"))
        })?;
        let result: rquickjs::Value = extractor.call((response,)).map_err(|source| {
            CompanionError::InvalidConfig(format!("执行余额查询 extractor 失败: {source}"))
        })?;
        ctx.json_stringify(result)
            .map_err(|source| {
                CompanionError::InvalidConfig(format!("序列化余额查询结果失败: {source}"))
            })?
            .ok_or_else(|| CompanionError::InvalidConfig("余额查询结果为空".to_string()))?
            .get::<String>()
            .map_err(|source| {
                CompanionError::InvalidConfig(format!("读取余额查询结果失败: {source}"))
            })
    })?;
    serde_json::from_str(&result_json)
        .map_err(|source| CompanionError::InvalidConfig(format!("余额查询结果格式无效: {source}")))
}

fn replace_usage_request_variables(
    mut request: UsageScriptRequest,
    query: &codex_companion_core::ProviderUsageQuery,
    credentials: &UsageQueryCredentials,
) -> UsageScriptRequest {
    let effective_base_url = usage_query_effective_base_url(query);
    let replacements = [
        ("{{baseUrl}}", effective_base_url.as_str()),
        ("{{apiKey}}", credentials.api_key.as_deref().unwrap_or("")),
        (
            "{{accessToken}}",
            credentials.access_token.as_deref().unwrap_or(""),
        ),
        ("{{userId}}", credentials.user_id.as_deref().unwrap_or("")),
    ];
    let replace = |mut value: String| {
        for (placeholder, replacement) in replacements {
            value = value.replace(placeholder, replacement);
        }
        value
    };
    request.url = replace(request.url);
    request.method = replace(request.method);
    request.headers = request
        .headers
        .into_iter()
        .map(|(name, value)| (replace(name), replace(value)))
        .collect();
    request.body = request.body.map(replace);
    request
}

fn usage_query_effective_base_url(query: &codex_companion_core::ProviderUsageQuery) -> String {
    match query.template {
        ProviderUsageQueryTemplate::NewApi | ProviderUsageQueryTemplate::OpenRouter => {
            Url::parse(query.base_url.trim())
                .ok()
                .and_then(|url| {
                    let origin = url.origin().ascii_serialization();
                    (origin != "null").then_some(origin)
                })
                .unwrap_or_else(|| query.base_url.trim().trim_end_matches('/').to_string())
        }
        ProviderUsageQueryTemplate::General | ProviderUsageQueryTemplate::Custom => {
            query.base_url.trim().trim_end_matches('/').to_string()
        }
    }
}

fn validate_usage_request_url(
    request_url: &str,
    base_url: &str,
    template: ProviderUsageQueryTemplate,
) -> Result<()> {
    let request = Url::parse(request_url)
        .map_err(|source| CompanionError::InvalidConfig(format!("余额查询 URL 无效: {source}")))?;
    let base = Url::parse(base_url).map_err(|source| {
        CompanionError::InvalidConfig(format!("余额查询 Base URL 无效: {source}"))
    })?;
    let request_loopback = matches!(request.host(), Some(Host::Ipv4(ip)) if ip.is_loopback())
        || matches!(request.host(), Some(Host::Ipv6(ip)) if ip.is_loopback())
        || matches!(request.host(), Some(Host::Domain(host)) if host.eq_ignore_ascii_case("localhost"));
    if request.scheme() != "https" && !(request.scheme() == "http" && request_loopback) {
        return Err(CompanionError::InvalidConfig(
            "余额查询只允许 HTTPS，localhost 可使用 HTTP".to_string(),
        ));
    }
    let cross_origin = request.scheme() != base.scheme()
        || request.host_str() != base.host_str()
        || request.port_or_known_default() != base.port_or_known_default();
    if template != ProviderUsageQueryTemplate::Custom && cross_origin {
        return Err(CompanionError::InvalidConfig(
            "余额查询请求必须与查询 Base URL 同源".to_string(),
        ));
    }
    Ok(())
}

pub async fn test_configured_usage_query(
    store: &codex_companion_core::ConfigStore,
    input: ProviderUsageQueryTestInput,
) -> Result<ProviderAccountInfo> {
    let config = store.load()?;
    let provider = match input.provider_id.as_deref() {
        Some(provider_id) => {
            crate::validate::validate_id(provider_id)?;
            Some(config.providers.get(provider_id).cloned().ok_or_else(|| {
                CompanionError::InvalidConfig(format!("unknown provider: {provider_id}"))
            })?)
        }
        None => None,
    };
    let mut credentials = provider
        .as_ref()
        .and_then(|provider| load_usage_query_credentials(&store.data_dir(), &provider.id).ok())
        .unwrap_or_default();
    if let Some(api_key) = input
        .usage_query
        .api_key
        .as_deref()
        .and_then(normalize_optional)
        .or_else(|| {
            input
                .provider_api_key
                .as_deref()
                .and_then(normalize_optional)
        })
        .or_else(|| provider.as_ref().and_then(resolve_auth_token))
    {
        credentials.api_key = Some(api_key);
    }
    if let Some(access_token) = input
        .usage_query
        .access_token
        .as_deref()
        .and_then(normalize_optional)
    {
        credentials.access_token = Some(access_token);
    }
    if let Some(user_id) = input
        .usage_query
        .user_id
        .as_deref()
        .and_then(normalize_optional)
    {
        credentials.user_id = Some(user_id);
    }
    if matches!(
        input.usage_query.template,
        ProviderUsageQueryTemplate::NewApi
    ) && (credentials.access_token.is_none() || credentials.user_id.is_none())
    {
        return Err(CompanionError::InvalidConfig(
            "NewAPI 余量查询缺少访问令牌或用户 ID".to_string(),
        ));
    }
    let base_url = input
        .usage_query
        .base_url
        .as_deref()
        .and_then(normalize_optional)
        .unwrap_or_else(|| input.provider_base_url.trim().to_string());
    let query = codex_companion_core::ProviderUsageQuery {
        template: input.usage_query.template,
        base_url,
        script: input
            .usage_query
            .script
            .as_deref()
            .and_then(normalize_optional)
            .unwrap_or_else(|| usage_query_preset(input.usage_query.template)),
        timeout_seconds: input.usage_query.timeout_seconds.clamp(2, 30),
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(query.timeout_seconds))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|source| {
            CompanionError::InvalidConfig(format!("创建余额查询客户端失败: {source}"))
        })?;
    let result = fetch_configured_usage(&client, &query, &credentials).await?;
    let mut account = ProviderAccountInfo::default();
    apply_configured_usage_to_account(&mut account, &result)?;
    account.last_refresh_at = Some(Utc::now().to_rfc3339());
    Ok(account)
}

fn apply_configured_usage_to_account(
    account: &mut ProviderAccountInfo,
    value: &serde_json::Value,
) -> Result<()> {
    let valid = value
        .get("isValid")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    if !valid {
        let message = value
            .get("invalidMessage")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("查询失败");
        return Err(CompanionError::InvalidConfig(format!(
            "余额查询失败: {message}"
        )));
    }
    let remaining = value.get("remaining").and_then(serde_json::Value::as_f64);
    let used = value.get("used").and_then(serde_json::Value::as_f64);
    let total = value
        .get("total")
        .and_then(serde_json::Value::as_f64)
        .or_else(|| {
            remaining
                .zip(used)
                .map(|(remaining, used)| remaining + used)
        });
    if remaining.is_none() && used.is_none() && total.is_none() {
        return Err(CompanionError::InvalidConfig(
            "余额查询结果缺少 remaining、used 或 total".to_string(),
        ));
    }
    let unit = value
        .get("unit")
        .and_then(serde_json::Value::as_str)
        .filter(|unit| !unit.trim().is_empty())
        .unwrap_or("USD");
    account.subscription_type = value
        .get("planName")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| account.subscription_type.clone());
    account.subscription_status = Some(if remaining.unwrap_or(1.0) > 0.0 {
        "可用".to_string()
    } else {
        "额度耗尽".to_string()
    });
    account.quota_label = Some(format!("{unit} 余额"));
    account.quota_percent = None;
    account.quota_reset_at = None;
    account.quota_windows.clear();
    account.usage_available = remaining;
    account.usage_used = used;
    account.usage_total = total;
    Ok(())
}

pub fn provider_supports_api_key_usage(provider: &ProviderConfig) -> bool {
    if provider
        .account
        .as_ref()
        .and_then(|account| account.usage_query.as_ref())
        .is_some()
    {
        return true;
    }
    if provider.kind == ProviderKind::RelayProvider {
        return true;
    }
    if provider.kind != ProviderKind::OpenAiCompatible {
        return false;
    }
    let Ok(url) = Url::parse(provider.base_url.trim()) else {
        return false;
    };
    let host = url
        .host_str()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    !host.is_empty() && host != "api.openai.com"
}

async fn fetch_api_usage(
    client: &reqwest::Client,
    usage_url: &str,
    token: &str,
) -> std::result::Result<serde_json::Value, String> {
    let mut last_error = None;
    for attempt in 0..3 {
        match fetch_api_usage_once(client, usage_url, token).await {
            Ok(value) => return Ok(value),
            Err(error) => {
                let retryable = error.retryable;
                last_error = Some(error.message);
                if !retryable || attempt == 2 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(150 * (attempt + 1))).await;
            }
        }
    }
    Err(last_error.unwrap_or_else(|| "余量接口不可用".to_string()))
}

struct UsageFetchError {
    message: String,
    retryable: bool,
}

async fn fetch_api_usage_once(
    client: &reqwest::Client,
    usage_url: &str,
    token: &str,
) -> std::result::Result<serde_json::Value, UsageFetchError> {
    let response = client
        .get(usage_url)
        .bearer_auth(token)
        .header(ACCEPT, "application/json")
        .send()
        .await
        .map_err(|source| UsageFetchError {
            message: format!("请求失败: {source}"),
            retryable: true,
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|source| UsageFetchError {
        message: format!("读取响应失败: {source}"),
        retryable: true,
    })?;
    if !status.is_success() {
        return Err(UsageFetchError {
            message: format!("{status} [body_len:{}]", body.len()),
            retryable: status.as_u16() == 429 || status.is_server_error(),
        });
    }
    let value =
        serde_json::from_str::<serde_json::Value>(&body).map_err(|source| UsageFetchError {
            message: format!("解析 JSON 失败: {source}"),
            retryable: false,
        })?;
    if value
        .get("success")
        .and_then(serde_json::Value::as_bool)
        .is_some_and(|success| !success)
        || value
            .get("code")
            .and_then(serde_json::Value::as_bool)
            .is_some_and(|success| !success)
    {
        let message = value
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("余量接口返回失败");
        return Err(UsageFetchError {
            message: message.to_string(),
            retryable: false,
        });
    }
    Ok(value)
}

async fn fetch_account_profile(
    client: &reqwest::Client,
    access_token: &str,
    account_id: Option<&str>,
) -> std::result::Result<AccountProfile, String> {
    let mut last_error = None;
    for (url, target_path, include_timezone) in [
        (
            ACCOUNT_CHECK_URL,
            "/backend-api/accounts/check/v4-2023-04-27",
            true,
        ),
        (
            LEGACY_ACCOUNT_CHECK_URL,
            "/backend-api/wham/accounts/check",
            false,
        ),
    ] {
        match fetch_account_profile_once(
            client,
            access_token,
            account_id,
            url,
            target_path,
            include_timezone,
        )
        .await
        {
            Ok(profile) => return Ok(profile),
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.unwrap_or_else(|| "账号信息接口不可用".to_string()))
}

async fn fetch_account_profile_once(
    client: &reqwest::Client,
    access_token: &str,
    account_id: Option<&str>,
    url: &str,
    target_path: &str,
    include_timezone: bool,
) -> std::result::Result<AccountProfile, String> {
    let mut request =
        client
            .get(url)
            .headers(chatgpt_web_headers(access_token, account_id, target_path)?);
    if include_timezone {
        let timezone_offset_min = -(Local::now().offset().local_minus_utc() / 60);
        request = request.query(&[("timezone_offset_min", timezone_offset_min)]);
    }
    let response = request
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
    let headers = codex_headers(access_token, account_id).map_err(CompanionError::InvalidConfig)?;
    let mut last_error = None;
    for attempt in 0..3 {
        match fetch_usage_once(client, headers.clone()).await {
            Ok(usage) => return Ok(usage),
            Err((message, retryable)) => {
                last_error = Some(message);
                if !retryable || attempt == 2 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(150 * (attempt + 1))).await;
            }
        }
    }
    Err(CompanionError::InvalidConfig(
        last_error.unwrap_or_else(|| "Codex 额度接口不可用".to_string()),
    ))
}

async fn fetch_usage_once(
    client: &reqwest::Client,
    headers: HeaderMap,
) -> std::result::Result<UsageResponse, (String, bool)> {
    let response = client
        .get(USAGE_URL)
        .headers(headers)
        .send()
        .await
        .map_err(|source| (format!("请求 Codex 额度失败: {source}"), true))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|source| (format!("读取 Codex 额度响应失败: {source}"), true))?;
    if !status.is_success() {
        let code = extract_error_code(&body)
            .map(|code| format!(" [error_code:{code}]"))
            .unwrap_or_default();
        return Err((
            format!(
                "Codex 额度接口返回 {status}{code} [body_len:{}]",
                body.len()
            ),
            status.as_u16() == 429 || status.is_server_error(),
        ));
    }
    serde_json::from_str::<UsageResponse>(&body)
        .map_err(|source| (format!("解析 Codex 额度 JSON 失败: {source}"), false))
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

fn chatgpt_web_headers(
    access_token: &str,
    account_id: Option<&str>,
    target_path: &str,
) -> std::result::Result<HeaderMap, String> {
    let mut headers = codex_headers(access_token, account_id)?;
    headers.insert(REFERER, HeaderValue::from_static(CHATGPT_WEB_REFERER));
    headers.insert(USER_AGENT, HeaderValue::from_static(CHATGPT_WEB_USER_AGENT));
    headers.insert(
        "x-openai-target-path",
        HeaderValue::from_str(target_path)
            .map_err(|error| format!("构建 x-openai-target-path 头失败: {error}"))?,
    );
    headers.insert(
        "x-openai-target-route",
        HeaderValue::from_str(target_path)
            .map_err(|error| format!("构建 x-openai-target-route 头失败: {error}"))?,
    );
    Ok(headers)
}

fn parse_account_profile(value: &serde_json::Value, expected_id: Option<&str>) -> AccountProfile {
    let records = collect_account_records(value);
    if records.is_empty() {
        return AccountProfile::default();
    }
    let selected = expected_id
        .and_then(|expected| {
            records.iter().copied().find(|record| {
                pick_first_string(
                    &[*record],
                    &[
                        &["id"],
                        &["account_id"],
                        &["accountId"],
                        &["chatgpt_account_id"],
                        &["workspace_id"],
                        &["account", "id"],
                        &["account", "account_id"],
                        &["account", "accountId"],
                        &["account", "chatgpt_account_id"],
                        &["account", "workspace_id"],
                    ],
                )
                .as_deref()
                    == Some(expected)
            })
        })
        .or_else(|| {
            value
                .get("account_ordering")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| items.first())
                .and_then(serde_json::Value::as_str)
                .and_then(|key| value.get("accounts").and_then(|accounts| accounts.get(key)))
                .filter(|record| record.is_object())
        })
        .or_else(|| records.first().copied())
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
                &["account", "name"],
                &["account", "display_name"],
                &["account", "displayName"],
                &["account", "account_name"],
                &["account", "organization_name"],
                &["account", "workspace_name"],
                &["account", "title"],
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
                &["account", "structure"],
                &["account", "account_structure"],
                &["account", "accountStructure"],
                &["account", "kind"],
                &["account", "type"],
                &["account", "account_type"],
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
                &["account", "id"],
                &["account", "account_id"],
                &["account", "accountId"],
                &["account", "chatgpt_account_id"],
                &["account", "workspace_id"],
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
                &["entitlement", "subscription_plan"],
                &["account", "plan_type"],
                &["account", "planType"],
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
                &["account", "subscription_active_until"],
                &["account", "subscriptionActiveUntil"],
                &["account", "expires_at"],
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
        let primary_windows = primary_usage_windows(&usage);
        let summary_windows = if primary_windows.is_empty() {
            &windows
        } else {
            &primary_windows
        };
        let lowest = summary_windows
            .iter()
            .min_by(|left, right| {
                left.remaining_percent
                    .partial_cmp(&right.remaining_percent)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned();
        account.quota_label = Some(
            summary_windows
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
    if apply_public_key_usage_to_account(account, value) {
        return;
    }

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
            &["quota_limit"],
            &["quotaLimit"],
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
            &["quotaRemaining"],
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
            &["access_until"],
            &["accessUntil"],
        ],
    );

    account.usage_total = if unlimited { None } else { total };
    account.usage_used = used;
    account.usage_available = if unlimited { None } else { available };
    account.valid_until = expires_at.clone().or_else(|| account.valid_until.clone());
    account.quota_reset_at = if unlimited {
        expires_at.clone()
    } else {
        expires_at
            .clone()
            .or_else(|| account.quota_reset_at.clone())
    };

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
        account.quota_windows = if unlimited {
            Vec::new()
        } else {
            vec![ProviderQuotaWindow {
                label: "API".to_string(),
                remaining_percent: percent,
                reset_at: expires_at,
                window_minutes: None,
            }]
        };
    }
    account.quota_label = if unlimited {
        Some("不限量".to_string())
    } else {
        summary.or_else(|| match (available, total) {
            (Some(available), Some(total)) if total > 0.0 => Some(format!(
                "{} / {}",
                compact_number(available),
                compact_number(total)
            )),
            (Some(available), _) => Some(format!("剩余 {}", compact_number(available))),
            _ => account.quota_label.clone(),
        })
    };
    account.subscription_status = Some(if unlimited || remaining_percent.unwrap_or(0.0) > 0.0 {
        "可用".to_string()
    } else {
        "额度耗尽".to_string()
    });
}

fn apply_public_key_usage_to_account(
    account: &mut ProviderAccountInfo,
    value: &serde_json::Value,
) -> bool {
    let has_public_usage_shape = value.get("mode").is_some()
        || value.get("balance").is_some()
        || value.get("remaining").is_some()
        || value.get("quota").is_some()
        || value.get("quotaLimit").is_some()
        || value.get("quotaRemaining").is_some()
        || value.get("accessUntil").is_some()
        || value.get("rate_limits").is_some()
        || value.get("daily_usage").is_some()
        || value
            .get("usage")
            .and_then(|usage| usage.get("today"))
            .is_some();
    if !has_public_usage_shape {
        return false;
    }

    let mode = pick_first_string(&[value], &[&["mode"]]);
    let plan_name = pick_first_string(
        &[value],
        &[
            &["planName"],
            &["plan_name"],
            &["subscription", "planName"],
            &["subscription", "plan_name"],
            &["subscription", "name"],
        ],
    );
    let status = pick_first_string(&[value], &[&["status"]]);
    let balance = pick_first_number(
        value,
        &[&["balance"], &["wallet_balance"], &["walletBalance"]],
    );
    let remaining = pick_first_number(
        value,
        &[
            &["remaining"],
            &["quota", "remaining"],
            &["quota_remaining"],
            &["quotaRemaining"],
        ],
    );
    let quota_total = pick_first_number(
        value,
        &[
            &["quota", "limit"],
            &["quota", "total"],
            &["quota", "total_granted"],
            &["limit"],
            &["quota_limit"],
            &["quotaLimit"],
        ],
    );
    let quota_used = pick_first_number(
        value,
        &[&["quota", "used"], &["quota", "total_used"], &["used"]],
    );
    let quota_remaining = pick_first_number(
        value,
        &[
            &["quota", "remaining"],
            &["remaining"],
            &["quota_remaining"],
            &["quotaRemaining"],
        ],
    );
    let expires_at = pick_first_timestamp(
        value,
        &[
            &["expires_at"],
            &["expiresAt"],
            &["subscription", "expires_at"],
            &["subscription", "expiresAt"],
            &["access_until"],
            &["accessUntil"],
        ],
    );

    let usage_available = quota_remaining.or(remaining).or(balance);
    account.usage_available = usage_available;
    account.usage_total = quota_total;
    account.usage_used = quota_used;
    account.valid_until = expires_at.clone().or_else(|| account.valid_until.clone());
    account.quota_reset_at = expires_at
        .clone()
        .or_else(|| account.quota_reset_at.clone());

    if let Some(plan_name) = plan_name {
        account.subscription_type = Some(plan_name);
    } else if mode.as_deref() == Some("quota_limited") {
        account.subscription_type = Some("Quota".to_string());
    } else {
        account.subscription_type = account
            .subscription_type
            .clone()
            .or_else(|| Some("API Key".to_string()));
    }

    let remaining_percent = quota_remaining
        .zip(quota_total)
        .and_then(|(remaining, total)| {
            if total > 0.0 {
                Some((remaining / total * 100.0).clamp(0.0, 100.0))
            } else {
                None
            }
        });
    if let Some(percent) = remaining_percent {
        account.quota_percent = Some(percent);
        account.quota_windows = vec![ProviderQuotaWindow {
            label: "API".to_string(),
            remaining_percent: percent,
            reset_at: expires_at.clone(),
            window_minutes: None,
        }];
    } else {
        let windows = public_usage_windows(value);
        if !windows.is_empty() {
            if let Some(lowest) = windows.iter().min_by(|left, right| {
                left.remaining_percent
                    .partial_cmp(&right.remaining_percent)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }) {
                account.quota_percent = Some(lowest.remaining_percent);
                account.quota_reset_at = lowest.reset_at.clone();
            }
            account.quota_windows = windows;
        } else {
            account.quota_percent = None;
            account.quota_windows.clear();
        }
    }

    account.quota_label = if balance.is_some() && remaining_percent.is_none() {
        Some("账户余额".to_string())
    } else if quota_total.is_some() || remaining.is_some() || quota_remaining.is_some() {
        Some("剩余额度".to_string())
    } else {
        account.quota_label.clone()
    };
    account.subscription_status = Some(
        if status.as_deref() == Some("quota_exhausted")
            || usage_available.is_some_and(|available| available <= 0.0)
        {
            "额度耗尽".to_string()
        } else {
            "可用".to_string()
        },
    );
    true
}

fn public_usage_windows(value: &serde_json::Value) -> Vec<ProviderQuotaWindow> {
    let mut windows = Vec::new();
    if let Some(rate_limits) = value
        .get("rate_limits")
        .and_then(serde_json::Value::as_array)
    {
        for item in rate_limits {
            let label =
                pick_first_string(&[item], &[&["window"]]).unwrap_or_else(|| "Window".to_string());
            let Some(limit) = pick_first_number(item, &[&["limit"]]) else {
                continue;
            };
            if limit <= 0.0 {
                continue;
            }
            let used = pick_first_number(item, &[&["used"]]).unwrap_or(0.0);
            windows.push(ProviderQuotaWindow {
                label: public_window_label(&label),
                remaining_percent: ((limit - used).max(0.0) / limit * 100.0).clamp(0.0, 100.0),
                reset_at: pick_first_timestamp(item, &[&["reset_at"], &["resetAt"]]),
                window_minutes: public_window_minutes(&label),
            });
        }
    }

    if let Some(subscription) = value
        .get("subscription")
        .filter(|subscription| subscription.is_object())
    {
        for (label, usage_key, limit_key, reset_key) in [
            (
                "Day",
                "daily_usage_usd",
                "daily_limit_usd",
                "daily_window_resets_at",
            ),
            (
                "Week",
                "weekly_usage_usd",
                "weekly_limit_usd",
                "weekly_window_resets_at",
            ),
            (
                "Month",
                "monthly_usage_usd",
                "monthly_limit_usd",
                "monthly_window_resets_at",
            ),
        ] {
            let Some(limit) = pick_first_number(subscription, &[&[limit_key]]) else {
                continue;
            };
            if limit <= 0.0 {
                continue;
            }
            let used = pick_first_number(subscription, &[&[usage_key]]).unwrap_or(0.0);
            windows.push(ProviderQuotaWindow {
                label: label.to_string(),
                remaining_percent: ((limit - used).max(0.0) / limit * 100.0).clamp(0.0, 100.0),
                reset_at: pick_first_timestamp(subscription, &[&[reset_key]]),
                window_minutes: match label {
                    "Day" => Some(1_440),
                    "Week" => Some(10_080),
                    "Month" => Some(43_200),
                    _ => None,
                },
            });
        }
    }
    windows
}

fn public_window_label(value: &str) -> String {
    match value {
        "5h" => "5h".to_string(),
        "1d" => "Day".to_string(),
        "7d" => "Week".to_string(),
        other => other.to_string(),
    }
}

fn public_window_minutes(value: &str) -> Option<i64> {
    match value {
        "5h" => Some(300),
        "1d" => Some(1_440),
        "7d" => Some(10_080),
        _ => None,
    }
}

fn usage_windows(usage: &UsageResponse) -> Vec<ProviderQuotaWindow> {
    let mut windows = primary_usage_windows(usage);
    for additional in &usage.additional_rate_limits {
        let Some(rate_limit) = additional.rate_limit.as_ref() else {
            continue;
        };
        let name = additional
            .limit_name
            .as_deref()
            .or(additional.metered_feature.as_deref())
            .map(additional_limit_label)
            .unwrap_or_else(|| "Model".to_string());
        for window in [
            window_summary("5h", rate_limit.primary_window.as_ref()),
            window_summary("Week", rate_limit.secondary_window.as_ref()),
        ]
        .into_iter()
        .flatten()
        {
            windows.push(ProviderQuotaWindow {
                label: format!("{name} {}", window.label),
                ..window
            });
        }
    }
    windows
}

fn primary_usage_windows(usage: &UsageResponse) -> Vec<ProviderQuotaWindow> {
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

fn additional_limit_label(value: &str) -> String {
    value
        .replace(['_', '-'], " ")
        .split_whitespace()
        .map(|word| match word.to_ascii_lowercase().as_str() {
            "gpt" => "GPT".to_string(),
            "codex" => "Codex".to_string(),
            "spark" => "Spark".to_string(),
            _ => word.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
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
    if minutes >= 43_200 {
        "30d".to_string()
    } else if minutes >= 10_080 {
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
    endpoints.push(public_key_usage_url(trimmed));
    endpoints.push(api_root_url(trimmed, "/v1/usage?days=30"));
    endpoints.push(api_root_url(trimmed, "/api/usage/token/"));
    endpoints.push(new_api_usage_url(trimmed));
    endpoints.push(api_root_url(trimmed, "/api/v1/user/token"));
    endpoints.push(api_root_url(trimmed, "/api/user/self"));
    dedupe_strings(endpoints)
}

fn public_key_usage_url(base_url: &str) -> String {
    format!("{}/usage?days=30", base_url.trim().trim_end_matches('/'))
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
    fn parses_configured_new_api_balance_in_usd() {
        let value = serde_json::json!({
            "planName": "default",
            "remaining": 264.27,
            "used": 856.17,
            "total": 1120.44,
            "unit": "USD"
        });
        let mut account = ProviderAccountInfo::default();

        apply_configured_usage_to_account(&mut account, &value).expect("parse NewAPI balance");

        assert_eq!(account.subscription_type.as_deref(), Some("default"));
        assert_eq!(account.subscription_status.as_deref(), Some("可用"));
        assert_eq!(account.quota_label.as_deref(), Some("USD 余额"));
        assert_eq!(account.quota_percent, None);
        assert_eq!(account.usage_available, Some(264.27));
        assert_eq!(account.usage_used, Some(856.17));
        assert_eq!(account.usage_total, Some(1120.44));
    }

    #[test]
    fn normalizes_new_api_query_base_to_origin() {
        let query = codex_companion_core::ProviderUsageQuery {
            template: ProviderUsageQueryTemplate::NewApi,
            base_url: "https://api.example.com/v1/responses".to_string(),
            script: String::new(),
            timeout_seconds: 10,
        };
        assert_eq!(
            usage_query_effective_base_url(&query),
            "https://api.example.com"
        );
    }

    #[test]
    fn interrupts_non_terminating_usage_scripts() {
        let started = std::time::Instant::now();
        let result = evaluate_usage_request("while (true) {}");

        assert!(result.is_err());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(7),
            "usage script interrupt exceeded its deadline"
        );
    }

    #[test]
    fn rejects_cross_origin_usage_requests() {
        let error = validate_usage_request_url(
            "https://other.example.com/api/user/self",
            "https://api.example.com",
            ProviderUsageQueryTemplate::NewApi,
        )
        .expect_err("cross-origin request must fail");

        assert!(error.to_string().contains("必须与查询 Base URL 同源"));
    }

    #[test]
    fn custom_usage_query_allows_an_explicit_https_origin() {
        validate_usage_request_url(
            "https://balance.example.net/account",
            "https://api.example.com",
            ProviderUsageQueryTemplate::Custom,
        )
        .expect("custom HTTPS request may use another origin");
    }

    #[test]
    fn custom_usage_query_still_rejects_insecure_remote_http() {
        let error = validate_usage_request_url(
            "http://balance.example.net/account",
            "https://api.example.com",
            ProviderUsageQueryTemplate::Custom,
        )
        .expect_err("remote HTTP must fail");

        assert!(error.to_string().contains("只允许 HTTPS"));
    }

    #[tokio::test]
    async fn configured_usage_query_does_not_follow_redirects() {
        use axum::{
            http::{header::LOCATION, StatusCode},
            routing::get,
            Json, Router,
        };

        let app = Router::new()
            .route(
                "/start",
                get(|| async { (StatusCode::FOUND, [(LOCATION, "/final")]) }),
            )
            .route(
                "/final",
                get(|| async { Json(serde_json::json!({ "remaining": 100 })) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let query = codex_companion_core::ProviderUsageQuery {
            template: ProviderUsageQueryTemplate::Custom,
            base_url: format!("http://{address}"),
            script: r#"({
  request: { url: "{{baseUrl}}/start", method: "GET" },
  extractor: function (response) { return response; }
})"#
            .to_string(),
            timeout_seconds: 10,
        };
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client");

        let error = fetch_configured_usage(&client, &query, &UsageQueryCredentials::default())
            .await
            .expect_err("redirect must not be followed");

        assert!(error.to_string().contains("302 Found"));
    }

    #[tokio::test]
    async fn configured_usage_query_rejects_oversized_responses() {
        use axum::{routing::get, Json, Router};

        let app = Router::new().route(
            "/large",
            get(|| async {
                Json(serde_json::json!({
                    "payload": "x".repeat(USAGE_RESPONSE_LIMIT_BYTES)
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let query = codex_companion_core::ProviderUsageQuery {
            template: ProviderUsageQueryTemplate::Custom,
            base_url: format!("http://{address}"),
            script: r#"({
  request: { url: "{{baseUrl}}/large", method: "GET" },
  extractor: function (response) { return response; }
})"#
            .to_string(),
            timeout_seconds: 10,
        };

        let error = fetch_configured_usage(
            &reqwest::Client::new(),
            &query,
            &UsageQueryCredentials::default(),
        )
        .await
        .expect_err("oversized response must fail");

        assert!(error.to_string().contains("超过 1 MiB 限制"));
    }

    #[tokio::test]
    async fn configured_new_api_query_sends_user_credentials_to_self_endpoint() {
        use axum::{
            extract::Request, http::StatusCode, response::IntoResponse, routing::get, Json, Router,
        };

        let app = Router::new().route(
            "/api/user/self",
            get(|request: Request| async move {
                let headers = request.headers();
                let authorized = headers
                    .get(AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    == Some("Bearer access-test");
                let user_matches = headers
                    .get("New-Api-User")
                    .and_then(|value| value.to_str().ok())
                    == Some("user-test");
                if !authorized || !user_matches {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                Json(serde_json::json!({
                    "success": true,
                    "data": {
                        "group": "default",
                        "quota": 1_000_000,
                        "used_quota": 500_000
                    }
                }))
                .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let query = codex_companion_core::ProviderUsageQuery {
            template: ProviderUsageQueryTemplate::NewApi,
            base_url: format!("http://{address}/v1/responses"),
            script: usage_query_preset(ProviderUsageQueryTemplate::NewApi),
            timeout_seconds: 10,
        };
        let credentials = UsageQueryCredentials {
            access_token: Some("access-test".to_string()),
            user_id: Some("user-test".to_string()),
            ..UsageQueryCredentials::default()
        };

        let response = fetch_configured_usage(&reqwest::Client::new(), &query, &credentials)
            .await
            .expect("query succeeds");

        assert_eq!(response["remaining"], 2.0);
        assert_eq!(response["used"], 1.0);
    }

    #[tokio::test]
    async fn configured_openrouter_query_uses_credits_endpoint() {
        use axum::{
            extract::Request, http::StatusCode, response::IntoResponse, routing::get, Json, Router,
        };

        let app = Router::new().route(
            "/api/v1/credits",
            get(|request: Request| async move {
                let authorized = request
                    .headers()
                    .get(AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    == Some("Bearer sk-or-test");
                if !authorized {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                Json(serde_json::json!({
                    "data": {
                        "total_credits": 10.0,
                        "total_usage": 2.5
                    }
                }))
                .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        let query = codex_companion_core::ProviderUsageQuery {
            template: ProviderUsageQueryTemplate::OpenRouter,
            base_url: format!("http://{address}/api/v1"),
            script: usage_query_preset(ProviderUsageQueryTemplate::OpenRouter),
            timeout_seconds: 10,
        };
        let credentials = UsageQueryCredentials {
            api_key: Some("sk-or-test".to_string()),
            ..UsageQueryCredentials::default()
        };

        let response = fetch_configured_usage(&reqwest::Client::new(), &query, &credentials)
            .await
            .expect("query succeeds");

        assert_eq!(response["remaining"], 7.5);
        assert_eq!(response["used"], 2.5);
        assert_eq!(response["total"], 10.0);
        assert_eq!(response["planName"], "OpenRouter");
    }

    #[tokio::test]
    async fn configured_usage_test_rejects_unknown_provider_ids() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = codex_companion_core::ConfigStore::new(temp.path().join("config.json"));
        let credential_path = store.data_dir().join("auth/usage-queries/unknown.json");
        std::fs::create_dir_all(credential_path.parent().expect("parent")).expect("directory");
        std::fs::write(&credential_path, r#"{"api_key":"secret"}"#).expect("credentials");
        let input = ProviderUsageQueryTestInput {
            provider_id: Some("unknown".to_string()),
            provider_base_url: "https://api.example.com".to_string(),
            provider_api_key: None,
            usage_query: crate::types::ProviderUsageQueryUpdate {
                enabled: true,
                template: ProviderUsageQueryTemplate::General,
                base_url: None,
                script: Some(usage_query_preset(ProviderUsageQueryTemplate::General)),
                timeout_seconds: 10,
                api_key: None,
                access_token: None,
                user_id: None,
            },
        };

        let error = test_configured_usage_query(&store, input)
            .await
            .expect_err("unknown provider must fail");

        assert!(error.to_string().contains("unknown provider"));
        assert!(credential_path.exists());
    }

    #[test]
    fn parses_usage_windows_with_rate_limits() {
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
    fn parses_free_plan_and_model_specific_windows() {
        let usage = serde_json::from_value::<UsageResponse>(serde_json::json!({
            "plan_type": "free",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 25,
                    "limit_window_seconds": 2592000,
                    "reset_at": 1780800000
                }
            },
            "additional_rate_limits": [{
                "limit_name": "gpt-5.3-codex-spark",
                "metered_feature": "codex_spark",
                "rate_limit": {
                    "primary_window": {
                        "used_percent": 40,
                        "limit_window_seconds": 18000
                    },
                    "secondary_window": {
                        "used_percent": 60,
                        "limit_window_seconds": 604800
                    }
                }
            }]
        }))
        .expect("usage");

        let mut account = ProviderAccountInfo::default();
        apply_usage_to_account(&mut account, usage);
        let windows = account.quota_windows.clone();
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].label, "30d");
        assert_eq!(windows[0].remaining_percent, 75.0);
        assert_eq!(windows[1].label, "GPT 5.3 Codex Spark 5h");
        assert_eq!(windows[2].label, "GPT 5.3 Codex Spark Week");
        assert_eq!(account.quota_percent, Some(75.0));
        assert_eq!(account.quota_label.as_deref(), Some("30d 75%"));
    }

    #[test]
    fn parses_official_plan_variants_with_nullable_additional_limits() {
        for plan_type in ["team", "k12team", "plus", "pro", "free"] {
            let usage = serde_json::from_value::<UsageResponse>(serde_json::json!({
                "plan_type": plan_type,
                "rate_limit": null,
                "code_review_rate_limit": null,
                "additional_rate_limits": null
            }))
            .expect("usage");

            let mut account = ProviderAccountInfo::default();
            apply_usage_to_account(&mut account, usage);

            assert_eq!(
                account.subscription_type.as_deref(),
                Some(plan_type.to_ascii_uppercase().as_str())
            );
            assert_eq!(account.subscription_status.as_deref(), Some("可用"));
            assert!(account.quota_windows.is_empty());
        }
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
    fn parses_cockpit_account_and_entitlement_profile() {
        let value = serde_json::json!({
            "account_ordering": ["personal", "school"],
            "accounts": {
                "personal": {
                    "account": {
                        "account_id": "acct-free",
                        "name": "person@example.test",
                        "structure": "personal"
                    },
                    "entitlement": {
                        "subscription_plan": "free",
                        "expires_at": "2026-08-01T00:00:00Z"
                    }
                },
                "school": {
                    "account": {
                        "account_id": "acct-school",
                        "name": "K12 Workspace",
                        "structure": "workspace"
                    },
                    "entitlement": {
                        "subscription_plan": "k12team",
                        "expires_at": "2027-07-01T00:00:00Z"
                    }
                }
            }
        });

        let selected = parse_account_profile(&value, Some("acct-school"));
        assert_eq!(selected.account_id.as_deref(), Some("acct-school"));
        assert_eq!(selected.name.as_deref(), Some("K12 Workspace"));
        assert_eq!(selected.structure.as_deref(), Some("workspace"));
        assert_eq!(selected.plan_type.as_deref(), Some("k12team"));
        assert_eq!(
            selected.subscription_active_until.as_deref(),
            Some("2027-07-01T00:00:00Z")
        );

        let ordered = parse_account_profile(&value, None);
        assert_eq!(ordered.account_id.as_deref(), Some("acct-free"));
        assert_eq!(ordered.plan_type.as_deref(), Some("free"));
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
    fn unlimited_api_key_usage_does_not_expose_negative_remaining_quota() {
        let value = serde_json::json!({
            "code": true,
            "data": {
                "total_granted": 12_456_524,
                "total_used": 13_077_025,
                "total_available": -620_501,
                "unlimited_quota": true
            }
        });
        let mut account = ProviderAccountInfo::default();

        apply_api_key_usage_to_account(&mut account, &value);

        assert_eq!(account.subscription_status.as_deref(), Some("可用"));
        assert_eq!(account.quota_label.as_deref(), Some("不限量"));
        assert_eq!(account.quota_percent, Some(100.0));
        assert!(account.quota_windows.is_empty());
        assert_eq!(account.usage_total, None);
        assert_eq!(account.usage_used, Some(13_077_025.0));
        assert_eq!(account.usage_available, None);
    }

    #[test]
    fn parses_public_key_usage_with_wallet_balance() {
        let value = serde_json::json!({
            "mode": "wallet",
            "planName": "标准",
            "balance": 6.42,
            "remaining": 6.42,
            "usage": {
                "today": {
                    "requests": 234,
                    "total_tokens": 20910000,
                    "actual_cost": 1.23
                }
            }
        });
        let mut account = ProviderAccountInfo::default();
        apply_api_key_usage_to_account(&mut account, &value);
        assert_eq!(account.subscription_type.as_deref(), Some("标准"));
        assert_eq!(account.subscription_status.as_deref(), Some("可用"));
        assert_eq!(account.usage_available, Some(6.42));
        assert_eq!(account.quota_label.as_deref(), Some("账户余额"));
        assert_eq!(account.quota_percent, None);
        assert!(account.quota_windows.is_empty());
    }

    #[test]
    fn parses_public_key_quota_windows() {
        let value = serde_json::json!({
            "mode": "quota_limited",
            "status": "active",
            "quota": {
                "limit": 100.0,
                "used": 25.0,
                "remaining": 75.0
            },
            "rate_limits": [
                { "window": "5h", "limit": 50.0, "used": 10.0, "reset_at": 1780800000 },
                { "window": "7d", "limit": 100.0, "used": 30.0 }
            ]
        });
        let mut account = ProviderAccountInfo::default();
        apply_api_key_usage_to_account(&mut account, &value);
        assert_eq!(account.subscription_status.as_deref(), Some("可用"));
        assert_eq!(account.usage_total, Some(100.0));
        assert_eq!(account.usage_used, Some(25.0));
        assert_eq!(account.usage_available, Some(75.0));
        assert_eq!(account.quota_percent, Some(75.0));
        assert_eq!(account.quota_label.as_deref(), Some("剩余额度"));
    }

    #[test]
    fn parses_new_api_camel_case_quota_fields() {
        let value = serde_json::json!({
            "data": {
                "quotaLimit": 200.0,
                "quotaRemaining": 50.0,
                "accessUntil": 1_800_000_000
            }
        });
        let mut account = ProviderAccountInfo::default();
        apply_api_key_usage_to_account(&mut account, &value);

        assert_eq!(account.usage_total, Some(200.0));
        assert_eq!(account.usage_available, Some(50.0));
        assert_eq!(account.quota_percent, Some(25.0));
        assert!(account.valid_until.is_some());
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
            Some("https://new-api.example.com/v1/usage?days=30")
        );
        assert!(endpoints.contains(&"https://new-api.example.com/api/usage/token".to_string()));

        let endpoints = api_usage_endpoints("https://cn.pptoken.cc/v1");
        assert_eq!(
            endpoints.first().map(String::as_str),
            Some("https://cn.pptoken.cc/v1/usage?days=30")
        );
        assert!(endpoints.contains(&"https://cn.pptoken.cc/api/usage/token/".to_string()));
    }

    #[test]
    fn non_openai_api_key_providers_probe_usage() {
        let mut provider =
            provider_config("https://api.openai.com/v1", ProviderKind::OpenAiCompatible);
        assert!(!provider_supports_api_key_usage(&provider));

        provider.base_url = "https://openrouter.ai/api/v1".to_string();
        assert!(provider_supports_api_key_usage(&provider));

        provider.base_url = "https://relay.example.com/v1".to_string();
        assert!(provider_supports_api_key_usage(&provider));

        provider.kind = ProviderKind::RelayProvider;
        assert!(provider_supports_api_key_usage(&provider));
    }

    fn provider_config(base_url: &str, kind: ProviderKind) -> ProviderConfig {
        ProviderConfig {
            id: "p".to_string(),
            name: "Provider".to_string(),
            kind,
            base_url: base_url.to_string(),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map: std::collections::BTreeMap::new(),
            priority: 0,
            enabled: true,
            refresh_interval_seconds: 60,
            account: None,
        }
    }
}
