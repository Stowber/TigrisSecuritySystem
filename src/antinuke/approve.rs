use anyhow::Result;

/// Placeholder approval logic.
pub async fn approve(_incident_id: i64, _moderator_id: u64) -> Result<()> {
    tracing::info!(
        incident_id = _incident_id,
        moderator_id = _moderator_id,
        "incident approved"
    );
    Ok(())
}