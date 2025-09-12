use anyhow::{Result, ensure};

use crate::{AppContext, db};

/// Approve an incident. In a real implementation this would verify moderator
/// permissions using the ACL system; here we simply require a non-zero
/// moderator id and record the action in the database.
pub async fn approve(ctx: &AppContext, incident_id: i64, moderator_id: u64) -> Result<()> {
    ensure!(moderator_id != 0, "missing moderator");
    let acl = ctx.command_acl();
    ensure!(
        acl.has_permission(moderator_id, "antinuke.approve").await,
        "missing permission"
    );
    tracing::info!(incident_id, moderator_id, "incident approved");
    db::insert_action(&ctx.db, incident_id, "approve", Some(moderator_id)).await?;
    Ok(())
}