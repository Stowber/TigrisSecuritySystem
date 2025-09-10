use anyhow::Result;

use crate::AppContext;

use super::{approve, restore, snapshot};

/// Register slash commands with Discord. Real implementation would use the
/// Discord HTTP API – here we simply expose stubs.
pub async fn register_commands() {
     // no-op placeholder
}

/// Handle `/antinuke approve <incident_id>`.
pub async fn cmd_approve(app: &AppContext, incident_id: i64, moderator_id: u64) -> Result<()> {
    approve::approve(app, incident_id, moderator_id).await
}

/// Handle `/antinuke restore <incident_id>` by taking a snapshot and applying
/// it back. This is a placeholder that does not interact with Discord.
pub async fn cmd_restore(app: &AppContext, guild_id: u64, incident_id: i64) -> Result<()> {
    let snap = snapshot::take_snapshot(guild_id).await?;
    restore::apply_snapshot(app, guild_id, incident_id, &snap).await
}

/// Report basic status of the monitoring service.
pub async fn cmd_status(app: &AppContext, guild_id: u64) -> Result<Vec<(i64, String)>> {
    app.antinuke().incidents(guild_id).await
}