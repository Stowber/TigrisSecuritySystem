use anyhow::Result;

use super::snapshot::GuildSnapshot;

/// Apply snapshot to guild (placeholder implementation).
pub async fn apply_snapshot(_guild_id: u64, _snapshot: &GuildSnapshot) -> Result<()> {
    tracing::info!(guild_id = %_guild_id, "restoring snapshot");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn restore_noop() {
        let snap = GuildSnapshot {
            roles: vec![],
            channels: vec![],
        };
        apply_snapshot(1, &snap).await.unwrap();
    }
}