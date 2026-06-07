use crate::runtime::CompanionDaemon;
use codex_companion_core::{ProviderGroup, Result};
use codex_companion_provider::{set_group_order, upsert_group, use_group, GroupUpsert};

impl CompanionDaemon {
    pub fn upsert_group(&self, input: GroupUpsert) -> Result<ProviderGroup> {
        upsert_group(&self.store, input)
    }

    pub fn use_group(&self, id: &str) -> Result<ProviderGroup> {
        use_group(&self.store, id)
    }

    pub fn set_group_order(&self, id: &str, provider_order: Vec<String>) -> Result<ProviderGroup> {
        set_group_order(&self.store, id, provider_order)
    }
}
