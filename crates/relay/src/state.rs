use codex_companion_core::ConfigStore;

#[derive(Debug, Clone)]
pub(crate) struct RelayState {
    pub store: ConfigStore,
    pub client: reqwest::Client,
}
