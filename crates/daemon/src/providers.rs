use crate::runtime::CompanionDaemon;
use codex_companion_core::{ProviderConfig, ProviderHealth, ProviderKind, Result};
use codex_companion_provider::{
    add_provider, import_api_key_provider, import_local_codex_provider, import_provider_json,
    import_provider_json_many, list_providers, refresh_provider_status, remove_provider,
    test_provider, ProviderImportOutcome, ProviderUpsert,
};
use std::path::PathBuf;

impl CompanionDaemon {
    pub fn add_provider(&self, input: ProviderUpsert) -> Result<ProviderConfig> {
        add_provider(&self.store, input)
    }

    pub fn import_provider_json(
        &self,
        json_text: &str,
        provider_id: Option<String>,
        provider_name: Option<String>,
    ) -> Result<ProviderImportOutcome> {
        import_provider_json(&self.store, json_text, provider_id, provider_name)
    }

    pub fn import_provider_json_many(
        &self,
        json_text: &str,
        provider_id: Option<String>,
        provider_name: Option<String>,
    ) -> Result<Vec<ProviderImportOutcome>> {
        import_provider_json_many(&self.store, json_text, provider_id, provider_name)
    }

    pub fn import_api_key_provider(
        &self,
        provider_name: String,
        kind: ProviderKind,
        base_url: String,
        api_key: String,
        env_var: Option<String>,
        model: Option<String>,
        refresh_interval_seconds: Option<u64>,
    ) -> Result<ProviderImportOutcome> {
        import_api_key_provider(
            &self.store,
            provider_name,
            kind,
            base_url,
            api_key,
            env_var,
            model,
            refresh_interval_seconds,
        )
    }

    pub fn import_local_codex_provider(
        &self,
        codex_dir: Option<PathBuf>,
    ) -> Result<ProviderImportOutcome> {
        import_local_codex_provider(&self.store, codex_dir)
    }

    pub fn remove_provider(&self, id: &str) -> Result<bool> {
        remove_provider(&self.store, id)
    }

    pub fn list_providers(&self) -> Result<Vec<ProviderConfig>> {
        list_providers(&self.store)
    }

    pub async fn test_provider(&self, id: &str) -> std::result::Result<(), String> {
        let config = self
            .store
            .load()
            .map_err(|error| format!("failed to load config: {error}"))?;
        let provider = config
            .providers
            .get(id)
            .ok_or_else(|| format!("unknown provider: {id}"))?;
        test_provider(provider).await
    }

    pub async fn refresh_provider(&self, id: &str) -> Result<ProviderHealth> {
        refresh_provider_status(&self.store, id).await
    }

    pub async fn refresh_all_providers(&self) -> Result<Vec<ProviderHealth>> {
        let ids = self
            .store
            .load()?
            .providers
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut output = Vec::new();
        for id in ids {
            output.push(refresh_provider_status(&self.store, &id).await?);
        }
        Ok(output)
    }
}
