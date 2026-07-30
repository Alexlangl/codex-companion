use crate::runtime::CompanionDaemon;
use codex_companion_core::{
    ApiClient, ApiClientCreate, ApiClientSecret, ApiClientUpdate, ApiRequestLog,
    ApiServiceSelfTest, ApiServiceSnapshot, CompanionError, HealthStatusKind, RelayConfig,
    RelaySettingsUpdate, Result,
};
use codex_companion_relay::ApiServiceStore;
use std::net::SocketAddr;
use std::time::Instant;

impl CompanionDaemon {
    pub fn api_service_snapshot(&self) -> Result<ApiServiceSnapshot> {
        let service = self.api_service_store()?;
        let mut snapshot = service.snapshot(100)?;
        let config = self.store.load()?;
        snapshot.affinity_bindings =
            service.affinity_binding_count(config.relay.session_affinity_ttl_seconds)?;
        snapshot.pool_health.total = config.providers.len();
        for provider in config
            .providers
            .values()
            .filter(|provider| provider.enabled)
        {
            snapshot.pool_health.enabled += 1;
            match config.health.get(&provider.id).map(|health| &health.status) {
                Some(HealthStatusKind::Healthy) => snapshot.pool_health.healthy += 1,
                Some(HealthStatusKind::Cooldown | HealthStatusKind::RateLimited) => {
                    snapshot.pool_health.cooldown += 1;
                }
                Some(HealthStatusKind::Unknown) | None => {}
                Some(_) => snapshot.pool_health.degraded += 1,
            }
        }
        Ok(snapshot)
    }

    pub fn api_request_logs(&self, limit: usize) -> Result<Vec<ApiRequestLog>> {
        self.api_service_store()?.list_requests(limit)
    }

    pub fn create_api_client(&self, input: ApiClientCreate) -> Result<ApiClientSecret> {
        self.api_service_store()?.create_client(input)
    }

    pub fn update_api_client(&self, input: ApiClientUpdate) -> Result<ApiClient> {
        self.api_service_store()?.update_client(input)
    }

    pub fn rotate_api_client_key(&self, id: &str) -> Result<ApiClientSecret> {
        self.api_service_store()?.rotate_client_key(id)
    }

    pub fn delete_api_client(&self, id: &str) -> Result<bool> {
        self.api_service_store()?.delete_client(id)
    }

    pub fn clear_api_request_logs(&self) -> Result<usize> {
        self.api_service_store()?.clear_request_logs()
    }

    pub fn update_relay_settings(&self, input: RelaySettingsUpdate) -> Result<RelayConfig> {
        validate_relay_settings(&input)?;
        if input.require_api_key
            && !self
                .api_service_store()?
                .list_clients()?
                .iter()
                .any(|client| client.enabled)
        {
            return Err(CompanionError::InvalidConfig(
                "启用强制密钥前，至少需要一个已启用的 API client".into(),
            ));
        }
        let relay = self.store.update(|config| {
            validate_relay_auth_scope(&config.relay, input.require_api_key)?;
            config.relay.require_api_key = input.require_api_key;
            config.relay.retry_budget = input.retry_budget;
            config.relay.model_cooldown_seconds = input.model_cooldown_seconds;
            config.relay.session_affinity_ttl_seconds = input.session_affinity_ttl_seconds;
            config.relay.request_log_retention_days = input.request_log_retention_days;
            Ok(config.relay.clone())
        })?;
        let _ = self
            .api_service_store()?
            .prune_request_logs(relay.request_log_retention_days);
        Ok(relay)
    }

    pub async fn api_service_self_test(&self) -> ApiServiceSelfTest {
        let started_at = Instant::now();
        let config = match self.store.load() {
            Ok(config) => config,
            Err(error) => {
                return self_test_failure(
                    String::new(),
                    started_at,
                    false,
                    false,
                    format!("读取配置失败: {error}"),
                )
            }
        };
        let base_url = config.relay.base_url();
        let database_ok = self
            .api_service_store()
            .and_then(|service| service.snapshot(1).map(|_| ()))
            .is_ok();
        let response = reqwest::Client::new()
            .get(&base_url)
            .timeout(std::time::Duration::from_secs(3))
            .send()
            .await;
        let listener_ok = response.as_ref().is_ok_and(|response| {
            response.status().is_success()
                || (response.status() == reqwest::StatusCode::UNAUTHORIZED
                    && response
                        .headers()
                        .contains_key("x-codex-companion-request-id"))
        });
        let ok = database_ok && listener_ok;
        let message = if ok {
            "配置数据库与本地 HTTP 监听均可用；未消耗上游账号额度".to_string()
        } else {
            match response {
                Ok(response) => format!("本地监听返回异常状态: {}", response.status()),
                Err(error) => format!("无法连接本地监听: {error}"),
            }
        };
        ApiServiceSelfTest {
            ok,
            base_url,
            latency_ms: elapsed_ms(started_at),
            database_ok,
            listener_ok,
            message,
        }
    }

    fn api_service_store(&self) -> Result<ApiServiceStore> {
        let service = ApiServiceStore::from_config_store(&self.store);
        service.initialize()?;
        Ok(service)
    }
}

fn validate_relay_settings(input: &RelaySettingsUpdate) -> Result<()> {
    if input.retry_budget > 20 {
        return Err(CompanionError::InvalidConfig("重试预算不能超过 20".into()));
    }
    if !(5..=86_400).contains(&input.model_cooldown_seconds) {
        return Err(CompanionError::InvalidConfig(
            "模型冷却时间必须在 5 到 86400 秒之间".into(),
        ));
    }
    if !(60..=86_400).contains(&input.session_affinity_ttl_seconds) {
        return Err(CompanionError::InvalidConfig(
            "会话亲和时间必须在 60 到 86400 秒之间".into(),
        ));
    }
    if !(1..=3650).contains(&input.request_log_retention_days) {
        return Err(CompanionError::InvalidConfig(
            "请求日志保留时间必须在 1 到 3650 天之间".into(),
        ));
    }
    Ok(())
}

fn validate_relay_auth_scope(relay: &RelayConfig, require_api_key: bool) -> Result<()> {
    let addr = relay.bind_addr().parse::<SocketAddr>().map_err(|error| {
        CompanionError::InvalidConfig(format!(
            "无效的本地代理监听地址 {}: {error}",
            relay.bind_addr()
        ))
    })?;
    if !addr.ip().is_loopback() && !require_api_key {
        return Err(CompanionError::InvalidConfig(
            "非 loopback 监听必须启用 API client 密钥".into(),
        ));
    }
    Ok(())
}

fn self_test_failure(
    base_url: String,
    started_at: Instant,
    database_ok: bool,
    listener_ok: bool,
    message: String,
) -> ApiServiceSelfTest {
    ApiServiceSelfTest {
        ok: false,
        base_url,
        latency_ms: elapsed_ms(started_at),
        database_ok,
        listener_ok,
        message,
    }
}

fn elapsed_ms(started_at: Instant) -> u64 {
    started_at
        .elapsed()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_dangerous_or_unbounded_settings() {
        let valid = RelaySettingsUpdate {
            require_api_key: false,
            retry_budget: 3,
            model_cooldown_seconds: 300,
            session_affinity_ttl_seconds: 3600,
            request_log_retention_days: 30,
        };
        assert!(validate_relay_settings(&valid).is_ok());
        assert!(validate_relay_settings(&RelaySettingsUpdate {
            retry_budget: 21,
            ..valid.clone()
        })
        .is_err());
        assert!(validate_relay_settings(&RelaySettingsUpdate {
            session_affinity_ttl_seconds: 1,
            ..valid
        })
        .is_err());
    }

    #[test]
    fn non_loopback_relay_cannot_disable_client_keys() {
        let relay = RelayConfig {
            host: "0.0.0.0".to_string(),
            ..RelayConfig::default()
        };

        assert!(validate_relay_auth_scope(&relay, false).is_err());
        assert!(validate_relay_auth_scope(&relay, true).is_ok());
        assert!(validate_relay_auth_scope(&RelayConfig::default(), false).is_ok());
    }
}
