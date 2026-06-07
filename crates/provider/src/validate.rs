use codex_companion_core::{CompanionError, Result};

pub fn validate_id(id: &str) -> Result<()> {
    let trimmed = id.trim();
    if trimmed.is_empty()
        || !trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(CompanionError::InvalidConfig(format!(
            "invalid provider/group id: {id}"
        )));
    }
    Ok(())
}

pub fn validate_base_url(base_url: &str) -> Result<()> {
    let trimmed = base_url.trim();
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(CompanionError::InvalidConfig(format!(
            "base_url must start with http:// or https://: {base_url}"
        )));
    }
    Ok(())
}
