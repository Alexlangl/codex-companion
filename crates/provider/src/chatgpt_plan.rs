//! Independent public-client registration. Codex credentials are never reused
//! as plan credentials; only a validated identity can bind the two accounts.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use codex_companion_core::{
    atomic_write_private_file, http_client_builder, ConfigStore, ProviderAccountInfo,
    ProviderConfig, ProviderKind,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

pub const RESOURCE: &str = "https://api.openai.com/v1";
const ISSUER: &str = "https://auth.openai.com";
const DIRECT_SCOPE: &str = "chatgpt.tokens.use.direct";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanLogin {
    pub client_id: Option<String>,
    pub subject: Option<String>,
    pub nonce: String,
    pub host_id: String,
    pub source_provider_id: Option<String>,
    pub source_subject: Option<String>,
    pub source_email: Option<String>,
    pub source_account_id: Option<String>,
    #[serde(default)]
    pub source_concurrency_key: Option<String>,
    pub provider_id: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Credentials {
    client_id: String,
    subject: String,
    access_token: String,
    refresh_token: String,
    id_token: String,
    scopes: Vec<String>,
    expires_at: i64,
    #[serde(default)]
    earliest_refresh_at: Option<i64>,
    login: PlanLogin,
}

pub fn is_chatgpt_plan(provider: &ProviderConfig) -> bool {
    provider
        .account
        .as_ref()
        .and_then(|a| a.auth_mode.as_deref())
        == Some("chatgpt_plan")
}

fn has_plan_scope(scopes: &[String]) -> bool {
    [DIRECT_SCOPE, "resource.invoke"]
        .iter()
        .all(|required| scopes.iter().any(|scope| scope == required))
}

pub fn catalog_fingerprint(provider: &ProviderConfig) -> Option<String> {
    let creds = read(provider).ok()?;
    let digest = Sha256::digest(format!("{}:{}", creds.client_id, creds.subject));
    Some(format!("{:x}", digest))
}

pub fn bound_concurrency_key(provider: &ProviderConfig) -> Option<String> {
    let credentials = read(provider).ok()?;
    credentials.login.source_concurrency_key.or_else(|| {
        let user = credentials.login.source_subject?;
        let account = credentials.login.source_account_id?;
        Some(format!("oauth:{}:{user}:{account}", user.len()))
    })
}

pub fn bound_source(provider: &ProviderConfig) -> Option<String> {
    if !is_chatgpt_plan(provider) {
        return None;
    }
    read(provider).ok()?.login.source_provider_id
}

pub async fn revoke_plan(provider: &ProviderConfig) -> Result<(), String> {
    let path = auth_path(provider)?;
    let lock_path = path.clone();
    let _lock = tokio::task::spawn_blocking(move || crate::lock_private_auth_file(&lock_path))
        .await
        .map_err(|_| "无法锁定 ChatGPT 授权")?
        .map_err(|_| "无法锁定 ChatGPT 授权")?;
    let mut credentials = read(provider)?;
    let http = client()?;
    let discovery = response_json(
        http.get("https://auth.openai.com/.well-known/openid-configuration")
            .send()
            .await
            .map_err(|_| "无法读取撤销授权地址；本机账号已停用，可稍后重试")?,
    )
    .await?;
    let endpoint = discovery["revocation_endpoint"]
        .as_str()
        .filter(|url| trusted_auth_url(url))
        .ok_or("OpenAI 未提供可信的撤销地址；本机账号已停用，请在 ChatGPT 中撤销授权")?;
    let response = http
        .post(endpoint)
        .form(&[
            ("token", credentials.refresh_token.as_str()),
            ("token_type_hint", "refresh_token"),
            ("client_id", credentials.client_id.as_str()),
        ])
        .send()
        .await
        .map_err(|_| "无法确认服务端撤销；本机账号已停用，请稍后重试或在 ChatGPT 中撤销授权")?;
    if !response.status().is_success() {
        return Err("服务端未确认撤销；本机账号已停用，请稍后重试或在 ChatGPT 中撤销授权".into());
    }
    // Retain the issued registration for reauthorization, never a usable token.
    credentials.access_token.clear();
    credentials.refresh_token.clear();
    credentials.id_token.clear();
    credentials.scopes.clear();
    credentials.expires_at = 0;
    save(&path, &credentials)
}

fn trusted_auth_url(raw: &str) -> bool {
    url::Url::parse(raw).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("auth.openai.com")
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
    })
}

fn save(path: &std::path::Path, credentials: &Credentials) -> Result<(), String> {
    atomic_write_private_file(
        path,
        &serde_json::to_vec(credentials).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn host_urn() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "urn:uuid:{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

fn random() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn auth_path(provider: &ProviderConfig) -> Result<PathBuf, String> {
    provider
        .auth_ref
        .as_deref()
        .and_then(|r| r.strip_prefix("file:"))
        .map(PathBuf::from)
        .ok_or_else(|| "ChatGPT 授权凭据必须保存在本机文件".into())
}

fn read(provider: &ProviderConfig) -> Result<Credentials, String> {
    let bytes =
        std::fs::read(auth_path(provider)?).map_err(|_| "无法读取 ChatGPT 授权凭据".to_string())?;
    serde_json::from_slice(&bytes).map_err(|_| "ChatGPT 授权凭据格式无效，请重新授权".into())
}

pub fn prepare_plan_login(
    store: &ConfigStore,
    selected: Option<&str>,
) -> Result<PlanLogin, String> {
    let config = store.load().map_err(|e| e.to_string())?;
    if let Some(id) = selected {
        let provider = config.providers.get(id).ok_or("所选账号不存在")?;
        if is_chatgpt_plan(provider) {
            let creds = read(provider)?;
            let mut login = creds.login;
            login.client_id = Some(creds.client_id);
            login.subject = Some(creds.subject);
            login.nonce = random();
            login.provider_id = Some(id.to_string());
            return Ok(login);
        }
    }
    let path = store.data_dir().join("oauth").join("plan-host-id");
    let host_id =
        crate::with_private_auth_file_lock(&path, || match std::fs::read_to_string(&path) {
            Ok(id) if id.starts_with("urn:uuid:") => Ok(id),
            _ => {
                let id = host_urn();
                atomic_write_private_file(&path, id.as_bytes())?;
                Ok(id)
            }
        })
        .map_err(|e| e.to_string())?;
    let source = selected.and_then(|id| config.providers.get(id));
    if source.is_some_and(|p| p.kind != ProviderKind::OfficialCodex) {
        return Err("只能将 ChatGPT 授权绑定到官方账号".into());
    }
    let account = source.and_then(|p| p.account.as_ref());
    if source.is_some()
        && (account.and_then(|a| a.user_id.as_deref()).is_none()
            || account
                .and_then(|a| a.email.as_deref())
                .is_none_or(|email| email.trim().is_empty()))
    {
        return Err("请先刷新官方账号资料，再绑定 ChatGPT 授权".into());
    }
    Ok(PlanLogin {
        client_id: None,
        subject: None,
        nonce: random(),
        host_id,
        source_provider_id: selected.map(str::to_owned),
        source_subject: account.and_then(|a| a.user_id.clone()),
        source_email: account.and_then(|a| a.email.clone()),
        source_account_id: account.and_then(|a| a.account_id.clone()),
        source_concurrency_key: source.map(crate::account_identity_key),
        provider_id: None,
    })
}

pub fn plan_authorize_url(
    login: &PlanLogin,
    redirect: &str,
    verifier: &str,
    state: &str,
) -> String {
    let mut url = url::Url::parse("https://auth.openai.com/api/accounts/authorize").unwrap();
    url.query_pairs_mut().extend_pairs([
        (
            "client_id",
            login.client_id.as_deref().unwrap_or("dynamic_agent_client"),
        ),
        ("ext_agent_host_id", login.host_id.as_str()),
        ("response_type", "code"),
        ("redirect_uri", redirect),
        (
            "scope",
            "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
        ),
        ("resource", RESOURCE),
        ("state", state),
        ("nonce", login.nonce.as_str()),
        ("code_challenge_method", "S256"),
        (
            "code_challenge",
            &URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
        ),
    ]);
    if login.client_id.is_none() {
        url.query_pairs_mut()
            .append_pair("agent_name_hint", "Codex Companion");
    }
    url.into()
}

pub fn plan_callback_client(login: &PlanLogin, returned: Option<&str>) -> Result<String, String> {
    let issued = returned
        .or(login.client_id.as_deref())
        .ok_or("注册回调缺少 client_id，请重新授权")?;
    if issued == "dynamic_agent_client"
        || !issued.starts_with("oaiapp_")
        || issued.len() > 256
        || !issued
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        || login
            .client_id
            .as_deref()
            .is_some_and(|expected| expected != issued)
    {
        return Err("ChatGPT 授权 client_id 不匹配".into());
    }
    Ok(issued.to_string())
}

fn client() -> Result<reqwest::Client, String> {
    http_client_builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "无法创建 ChatGPT 授权客户端".into())
}

async fn response_json(response: reqwest::Response) -> Result<Value, String> {
    let status = response.status();
    let bytes = crate::http::read_response_bytes_limited(response, 1024 * 1024)
        .await
        .map_err(|_| "ChatGPT 授权响应读取失败".to_string())?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| format!("ChatGPT 授权响应格式无效（{status}）"))?;
    if !status.is_success() {
        let code = value["error"]
            .as_str()
            .or_else(|| value["error"]["code"].as_str())
            .unwrap_or("authorization_failed");
        let safe = code
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(80)
            .collect::<String>();
        return Err(format!("ChatGPT 授权失败（{status}, {safe}）"));
    }
    Ok(value)
}

#[derive(Deserialize)]
struct Identity {
    sub: String,
    iss: String,
    aud: Value,
    exp: i64,
    nonce: Option<String>,
    nbf: Option<i64>,
    iat: Option<i64>,
    azp: Option<String>,
    email: Option<String>,
}

fn verify_identity(
    token: &str,
    client_id: &str,
    nonce: Option<&str>,
    keys: &Value,
) -> Result<Identity, String> {
    let invalid = || "ChatGPT 身份签名或声明校验失败".to_string();
    if token.len() > 65536 {
        return Err(invalid());
    }
    let parts: Vec<_> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(invalid());
    }
    let decode = |s: &str| URL_SAFE_NO_PAD.decode(s).map_err(|_| invalid());
    let head: Value = serde_json::from_slice(&decode(parts[0])?).map_err(|_| invalid())?;
    if head["alg"] != "RS256" || head.get("crit").is_some() {
        return Err(invalid());
    }
    let key = keys["keys"]
        .as_array()
        .and_then(|keys| {
            keys.iter().find(|k| {
                k["kid"] == head["kid"]
                    && k["kid"].is_string()
                    && k["kty"] == "RSA"
                    && k.get("use").is_none_or(|v| v == "sig")
                    && k.get("alg").is_none_or(|v| v == "RS256")
            })
        })
        .ok_or_else(invalid)?;
    let n = decode(key["n"].as_str().ok_or_else(invalid)?)?;
    let e = decode(key["e"].as_str().ok_or_else(invalid)?)?;
    ring::signature::RsaPublicKeyComponents { n: &n, e: &e }
        .verify(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &decode(parts[2])?,
        )
        .map_err(|_| invalid())?;
    let identity: Identity = serde_json::from_slice(&decode(parts[1])?).map_err(|_| invalid())?;
    validate_claims(&identity, client_id, nonce)?;
    Ok(identity)
}

fn validate_claims(
    identity: &Identity,
    client_id: &str,
    nonce: Option<&str>,
) -> Result<(), String> {
    let invalid = || "ChatGPT 身份签名或声明校验失败".to_string();
    let now = chrono::Utc::now().timestamp();
    let audience = identity.aud.as_str() == Some(client_id)
        || identity
            .aud
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == client_id));
    if identity.iss != ISSUER
        || !audience
        || (identity.aud.as_array().is_some_and(|a| a.len() > 1)
            && identity.azp.as_deref() != Some(client_id))
        || identity.exp <= now
        || identity.sub.trim().is_empty()
        || identity.sub.len() > 512
        || identity.nbf.is_some_and(|n| n > now + 30)
        || identity.iat.is_some_and(|n| n > now + 60)
        || identity.azp.as_deref().is_some_and(|v| v != client_id)
        || nonce.is_some_and(|n| identity.nonce.as_deref() != Some(n))
    {
        return Err(invalid());
    }
    Ok(())
}

async fn validate_identity(
    token: &str,
    client_id: &str,
    nonce: Option<&str>,
) -> Result<Identity, String> {
    let client = client()?;
    let discovery = response_json(
        client
            .get("https://auth.openai.com/.well-known/openid-configuration")
            .send()
            .await
            .map_err(|_| "无法读取 OpenAI 身份发现配置".to_string())?,
    )
    .await?;
    if discovery["issuer"] != ISSUER {
        return Err("OpenAI 身份 issuer 无效".into());
    }
    let jwks = discovery["jwks_uri"]
        .as_str()
        .ok_or("OpenAI JWKS 地址缺失")?;
    if !trusted_auth_url(jwks) {
        return Err("OpenAI JWKS 地址不可信".into());
    }
    let keys = response_json(
        client
            .get(jwks)
            .send()
            .await
            .map_err(|_| "无法读取 OpenAI 签名密钥".to_string())?,
    )
    .await?;
    verify_identity(token, client_id, nonce, &keys)
}

fn token_credentials(
    value: Value,
    login: PlanLogin,
    client_id: String,
    old: Option<&Credentials>,
) -> Result<Credentials, String> {
    let get = |key: &str| {
        value[key]
            .as_str()
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    let scopes = get("scope")
        .map(|v| v.split_whitespace().map(str::to_owned).collect())
        .or_else(|| old.map(|o| o.scopes.clone()))
        .ok_or("ChatGPT 授权未返回权限范围")?;
    Ok(Credentials {
        client_id,
        subject: login.subject.clone().unwrap_or_default(),
        access_token: get("access_token").ok_or("ChatGPT 授权缺少 access_token")?,
        refresh_token: get("refresh_token")
            .or_else(|| old.map(|o| o.refresh_token.clone()))
            .ok_or("ChatGPT 授权缺少 refresh_token")?,
        id_token: get("id_token")
            .or_else(|| old.map(|o| o.id_token.clone()))
            .ok_or("ChatGPT 授权缺少 id_token")?,
        expires_at: chrono::Utc::now()
            .timestamp()
            .checked_add(
                value["expires_in"]
                    .as_i64()
                    .filter(|n| *n > 0)
                    .ok_or("ChatGPT 授权过期时间无效")?,
            )
            .ok_or("ChatGPT 授权过期时间溢出")?,
        earliest_refresh_at: value["earliest_refresh_at"].as_i64(),
        scopes,
        login,
    })
}

pub async fn complete_plan_login(
    store: &ConfigStore,
    login: &PlanLogin,
    client_id: &str,
    code: &str,
    verifier: &str,
    redirect: &str,
) -> Result<crate::ProviderImportOutcome, String> {
    let client_id = plan_callback_client(login, Some(client_id))?;
    let value = response_json(
        client()?
            .post("https://auth.openai.com/api/accounts/oauth/token")
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", client_id.as_str()),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect),
                ("resource", RESOURCE),
            ])
            .send()
            .await
            .map_err(|_| "ChatGPT 授权令牌交换失败，请重新授权".to_string())?,
    )
    .await?;
    let mut credentials = token_credentials(value, login.clone(), client_id.clone(), None)?;
    let identity = validate_identity(&credentials.id_token, &client_id, Some(&login.nonce)).await?;
    if login.subject.as_deref().is_some_and(|s| s != identity.sub)
        || login.source_email.as_deref().is_some_and(|email| {
            !identity
                .email
                .as_deref()
                .is_some_and(|actual| actual.trim().eq_ignore_ascii_case(email.trim()))
        })
    {
        return Err("授权返回的 ChatGPT 账号与所选账号不一致，原账号凭据未更改".into());
    }
    credentials.subject = identity.sub.clone();
    credentials.login.subject = Some(identity.sub.clone());
    credentials.login.client_id = Some(client_id.clone());
    let id = login.provider_id.clone().unwrap_or_else(|| {
        format!(
            "chatgpt-plan-{}",
            &format!(
                "{:x}",
                Sha256::digest(format!("{client_id}:{}", identity.sub))
            )[..16]
        )
    });
    let path = store
        .data_dir()
        .join("auth")
        .join("accounts")
        .join(format!("{id}.json"));
    let enabled = has_plan_scope(&credentials.scopes);
    let provider = ProviderConfig {
        id: id.clone(),
        name: format!("ChatGPT · {}", identity.email.as_deref().unwrap_or("Plan")),
        kind: ProviderKind::OpenAiCompatible,
        base_url: RESOURCE.into(),
        websocket_url: Some("wss://api.openai.com/v1/responses".into()),
        auth_ref: Some(format!("file:{}", path.display())),
        direct_auth_ref: None,
        model_map: BTreeMap::new(),
        priority: 0,
        enabled,
        refresh_interval_seconds: 300,
        account: Some(ProviderAccountInfo {
            auth_mode: Some("chatgpt_plan".into()),
            user_id: Some(identity.sub.clone()),
            email: identity.email,
            ..Default::default()
        }),
    };
    let created = !store
        .load()
        .map_err(|e| e.to_string())?
        .providers
        .contains_key(&id);
    crate::persist_with_private_auth_file(
        &path,
        &serde_json::to_string(&credentials).map_err(|e| e.to_string())?,
        || {
            store.update(|config| {
                if let Some(source_id) = &login.source_provider_id {
                    let source = config.providers.get(source_id).ok_or_else(|| {
                        codex_companion_core::CompanionError::InvalidConfig(
                            "绑定的官方账号已删除".into(),
                        )
                    })?;
                    if source.account.as_ref().and_then(|a| a.user_id.as_deref())
                        != login.source_subject.as_deref()
                        || source
                            .account
                            .as_ref()
                            .and_then(|a| a.account_id.as_deref())
                            != login.source_account_id.as_deref()
                    {
                        return Err(codex_companion_core::CompanionError::InvalidConfig(
                            "绑定的官方账号已改变".into(),
                        ));
                    }
                    for group in config.groups.values_mut() {
                        if !group.provider_order.contains(&id) {
                            if let Some(index) =
                                group.provider_order.iter().position(|p| p == source_id)
                            {
                                group.provider_order.insert(index, id.clone());
                            }
                        }
                    }
                }
                config.providers.insert(id.clone(), provider.clone());
                config.health.insert(id.clone(), Default::default());
                Ok(())
            })
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(crate::ProviderImportOutcome {
        provider,
        import_kind: "chatgpt_plan".into(),
        account_id: identity.sub,
        auth_path: path,
        created,
        message: if enabled {
            "ChatGPT 授权已保存，可通过本地代理使用"
        } else {
            "已保存身份登录，但未获准使用 ChatGPT 套餐；请重新授权并允许套餐使用"
        }
        .into(),
    })
}

pub async fn ensure_plan_access(provider: &ProviderConfig) -> Result<String, String> {
    let path = auth_path(provider)?;
    let lock_path = path.clone();
    let _lock = tokio::task::spawn_blocking(move || {
        crate::lock_private_auth_file(&lock_path).map_err(|_| "无法锁定 ChatGPT 授权".to_string())
    })
    .await
    .map_err(|_| "ChatGPT 授权刷新任务失败")??;
    let mut credentials = read(provider)?;
    if !has_plan_scope(&credentials.scopes) {
        return Err("ChatGPT 套餐使用未获授权，请重新登录".into());
    }
    let now = chrono::Utc::now().timestamp();
    if credentials.expires_at <= now + 60
        && credentials.earliest_refresh_at.is_none_or(|at| at <= now)
    {
        if credentials.refresh_token.is_empty() {
            return Err("ChatGPT 授权已撤销，请重新登录".into());
        }
        let refreshed = response_json(
            client()?
                .post("https://auth.openai.com/api/accounts/oauth/token")
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", credentials.client_id.as_str()),
                    ("refresh_token", credentials.refresh_token.as_str()),
                    ("resource", RESOURCE),
                ])
                .send()
                .await
                .map_err(|_| "ChatGPT 授权刷新连接失败".to_string())?,
        )
        .await;
        let value = match refreshed {
            Ok(value) => value,
            Err(error) => {
                if error.contains("invalid_grant") || error.contains("invalid_client") {
                    credentials.access_token.clear();
                    credentials.refresh_token.clear();
                    credentials.scopes.clear();
                    credentials.expires_at = 0;
                    save(&path, &credentials)?;
                }
                return Err(error);
            }
        };
        let next = token_credentials(
            value,
            credentials.login.clone(),
            credentials.client_id.clone(),
            Some(&credentials),
        )?;
        if next.id_token != credentials.id_token {
            let identity = validate_identity(&next.id_token, &next.client_id, None).await?;
            if identity.sub != credentials.subject {
                return Err("ChatGPT 刷新返回了其他账号，已停止使用".into());
            }
        }
        save(&path, &next)?;
        if !has_plan_scope(&next.scopes) {
            return Err("ChatGPT 套餐授权已撤销，请重新登录".into());
        }
        credentials = next;
    }
    if credentials.expires_at <= now {
        return Err("ChatGPT 授权已过期，请重新登录".into());
    }
    Ok(credentials.access_token)
}

pub fn plan_binding_valid(store: &ConfigStore, provider: &ProviderConfig) -> bool {
    let Ok(creds) = read(provider) else {
        return false;
    };
    let Some(source_id) = creds.login.source_provider_id.as_ref() else {
        return true;
    };
    store
        .load()
        .ok()
        .and_then(|config| config.providers.get(source_id).cloned())
        .is_some_and(|source| {
            source.enabled
                && source.kind == ProviderKind::OfficialCodex
                && source.account.as_ref().and_then(|a| a.user_id.as_deref())
                    == creds.login.source_subject.as_deref()
                && source
                    .account
                    .as_ref()
                    .and_then(|a| a.account_id.as_deref())
                    == creds.login.source_account_id.as_deref()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn registration_and_callback_keep_client_and_identity_separate() {
        let temp = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(temp.path().join("config.json"));
        let mut login = prepare_plan_login(&store, None).unwrap();
        let again = prepare_plan_login(&store, None).unwrap();
        assert_eq!(login.host_id, again.host_id);
        assert!(login.host_id.starts_with("urn:uuid:"));
        assert_eq!(login.host_id.len(), 45);
        assert_ne!(login.nonce, again.nonce);
        let url = url::Url::parse(&plan_authorize_url(
            &login,
            "http://127.0.0.1:1455/auth/callback",
            "verifier",
            "state",
        ))
        .unwrap();
        let params: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(params["client_id"], "dynamic_agent_client");
        assert_eq!(params["agent_name_hint"], "Codex Companion");
        assert_eq!(params["resource"], RESOURCE);
        assert!(plan_callback_client(&login, None).is_err());
        assert!(plan_callback_client(&login, Some("dynamic_agent_client")).is_err());
        login.client_id = Some("oaiapp_fixture".into());
        assert_eq!(
            plan_callback_client(&login, None).unwrap(),
            "oaiapp_fixture"
        );
        assert!(plan_callback_client(&login, Some("oaiapp_other")).is_err());
    }
    #[test]
    fn unsigned_identity_cannot_be_saved() {
        let token = format!(
            "{}.{}.AA",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"none","kid":"fixture"}"#),
            URL_SAFE_NO_PAD.encode(r#"{"sub":"fixture"}"#)
        );
        assert!(
            verify_identity(&token, "oaiapp_fixture", Some("nonce"), &json!({"keys":[]})).is_err()
        );
    }
    #[test]
    fn identity_claims_require_issuer_audience_freshness_and_nonce() {
        let original = json!({"sub":"fixture-user","iss":ISSUER,"aud":"oaiapp_fixture","exp":chrono::Utc::now().timestamp()+600,"nonce":"nonce-fixture"});
        let validate = |value: Value| {
            validate_claims(
                &serde_json::from_value(value).unwrap(),
                "oaiapp_fixture",
                Some("nonce-fixture"),
            )
        };
        assert!(validate(original.clone()).is_ok());
        for (key, value) in [
            ("iss", json!("https://other.invalid")),
            ("aud", json!("oaiapp_other")),
            ("exp", json!(0)),
            ("nonce", json!("wrong")),
            ("sub", json!("")),
            ("aud", json!(["oaiapp_fixture", "other"])),
        ] {
            let mut changed = original.clone();
            changed[key] = value;
            assert!(validate(changed).is_err(), "{key}");
        }
        let mut authorized = original.clone();
        authorized["aud"] = json!(["oaiapp_fixture", "other"]);
        authorized["azp"] = json!("oaiapp_fixture");
        assert!(validate(authorized).is_ok());
        for url in [
            "https://auth.openai.com:444/token",
            "https://other.invalid/token",
            "https://auth.openai.com@other.invalid/token",
            "http://auth.openai.com/token",
        ] {
            assert!(!trusted_auth_url(url));
        }
    }

    #[test]
    fn missing_plan_scope_cannot_use_a_saved_identity_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(dir.path().join("config.json"));
        let login = prepare_plan_login(&store, None).unwrap();
        let credentials = token_credentials(json!({"access_token":"access-fixture","refresh_token":"refresh-fixture","id_token":"id-fixture","scope":"openid profile email","expires_in":600}),login,"oaiapp_fixture".into(),None).unwrap();
        assert!(!has_plan_scope(&credentials.scopes));
        let next = token_credentials(json!({"access_token":"next-fixture","refresh_token":"rotated-fixture","scope":"openid resource.invoke chatgpt.tokens.use.direct","expires_in":600}),credentials.login.clone(),credentials.client_id.clone(),Some(&credentials)).unwrap();
        assert!(has_plan_scope(&next.scopes));
        assert_eq!(next.refresh_token, "rotated-fixture");
        assert_eq!(next.id_token, "id-fixture");
    }
}
