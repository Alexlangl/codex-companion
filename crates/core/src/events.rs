use crate::types::RelayEvent;
use chrono::Utc;

pub fn now_event(
    kind: impl Into<String>,
    provider_id: Option<String>,
    message: impl Into<String>,
) -> RelayEvent {
    RelayEvent {
        timestamp: Utc::now(),
        kind: kind.into(),
        provider_id,
        message: message.into(),
    }
}
