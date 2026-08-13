use crate::runtime::CompanionDaemon;
use codex_companion_core::{
    ApiClient, ApiClientCreate, ApiClientSecret, ApiClientUpdate, ApiRequestLog,
    ApiServiceSelfTest, ApiServiceSnapshot, CompanionError, HealthStatusKind, RelayConfig,
    RelaySettingsUpdate, Result,
};
use codex_companion_relay::{clear_event_logs, ApiServiceStore};
use std::collections::BTreeMap;
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
        let cleared_requests = self.api_service_store()?.clear_request_logs()?;
        clear_event_logs(&self.store.data_dir())
            .map_err(|source| CompanionError::io(self.store.data_dir().join("relay"), source))?;
        Ok(cleared_requests)
    }

    pub fn session_provider_preferences(
        &self,
        session_ids: &[String],
    ) -> Result<BTreeMap<String, String>> {
        if session_ids.len() > 100 {
            return Err(CompanionError::InvalidConfig(
                "单次最多查询 100 个会话的 Provider 首选项".into(),
            ));
        }
        self.api_service_store()?
            .list_session_provider_preferences(session_ids)
    }

    pub fn set_session_provider_preference(
        &self,
        session_id: &str,
        provider_id: &str,
    ) -> Result<String> {
        let provider_id = provider_id.trim();
        let config = self.store.load()?;
        let group = config
            .groups
            .get(&config.relay.active_group_id)
            .ok_or_else(|| {
                CompanionError::InvalidConfig(format!(
                    "当前分组不存在: {}",
                    config.relay.active_group_id
                ))
            })?;
        if !group
            .provider_order
            .iter()
            .any(|candidate| candidate == provider_id)
        {
            return Err(CompanionError::InvalidConfig(format!(
                "Provider {provider_id} 不在当前分组 {} 中",
                group.name
            )));
        }
        if !config
            .providers
            .get(provider_id)
            .is_some_and(|provider| provider.enabled)
        {
            return Err(CompanionError::InvalidConfig(format!(
                "Provider {provider_id} 不存在或已停用"
            )));
        }
        self.api_service_store()?
            .set_session_provider_preference(session_id, provider_id)?;
        Ok(provider_id.to_string())
    }

    pub fn clear_session_provider_preference(&self, session_id: &str) -> Result<bool> {
        self.api_service_store()?
            .clear_session_provider_preference(session_id)
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
    use codex_companion_core::{ConfigStore, ProviderConfig, ProviderKind};
    use codex_companion_relay::RequestLogStart;
    use std::collections::BTreeMap;

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

    #[test]
    fn clearing_api_logs_also_clears_relay_events() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        let daemon = CompanionDaemon::new(store.clone());
        let api_service = daemon.api_service_store().expect("API service");
        api_service
            .record_request_start(RequestLogStart {
                request_id: "request-1",
                method: "POST",
                path: "/v1/responses",
                model: Some("gpt-test"),
                reasoning_effort: None,
                service_tier: None,
                client_id: None,
            })
            .expect("request log");
        let events_dir = store.data_dir().join("relay");
        std::fs::create_dir_all(&events_dir).expect("events directory");
        std::fs::write(events_dir.join("events.jsonl"), "event\n").expect("event log");
        std::fs::write(events_dir.join("events.previous.jsonl"), "previous\n")
            .expect("previous event log");

        assert_eq!(daemon.clear_api_request_logs().expect("clear logs"), 1);
        assert!(daemon
            .api_request_logs(100)
            .expect("request logs")
            .is_empty());
        assert!(daemon.relay_events().is_empty());
    }

    #[test]
    fn session_preference_requires_an_enabled_provider_in_the_active_group() {
        let temp = tempfile::tempdir().expect("temp");
        let store = ConfigStore::new(temp.path().join("config.json"));
        store
            .update(|config| {
                config.relay.active_group_id = "default".to_string();
                config
                    .providers
                    .insert("enabled".to_string(), test_provider("enabled", true));
                config
                    .providers
                    .insert("disabled".to_string(), test_provider("disabled", false));
                let group = config.groups.get_mut("default").expect("default group");
                group.provider_order = vec!["enabled".to_string(), "disabled".to_string()];
                Ok(())
            })
            .expect("config");
        let daemon = CompanionDaemon::new(store);

        assert_eq!(
            daemon
                .set_session_provider_preference("session-a", "enabled")
                .expect("preference"),
            "enabled"
        );
        assert!(daemon
            .set_session_provider_preference("session-a", "disabled")
            .is_err());
        assert!(daemon
            .set_session_provider_preference("session-a", "outside")
            .is_err());
        assert_eq!(
            daemon
                .session_provider_preferences(&["session-a".to_string()])
                .expect("preferences")
                .get("session-a")
                .map(String::as_str),
            Some("enabled")
        );
        assert!(daemon
            .clear_session_provider_preference("session-a")
            .expect("clear"));
    }

    fn test_provider(id: &str, enabled: bool) -> ProviderConfig {
        ProviderConfig {
            id: id.to_string(),
            name: id.to_string(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: format!("https://{id}.example.com/v1"),
            websocket_url: None,
            auth_ref: None,
            direct_auth_ref: None,
            model_map: BTreeMap::new(),
            priority: 0,
            enabled,
            refresh_interval_seconds: 60,
            account: None,
        }
    }
}
