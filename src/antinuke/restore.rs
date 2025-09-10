use anyhow::Result;

use crate::{AppContext, db};
use super::snapshot::GuildSnapshot;

/// Apply snapshot to guild (placeholder implementation).
pub async fn apply_snapshot(
    ctx: &AppContext,
    guild_id: u64,
    incident_id: i64,
    snapshot: &GuildSnapshot,
) -> Result<()> {
    tracing::info!(%guild_id, "restoring snapshot");
    for role in &snapshot.roles {
        tracing::debug!(%guild_id, role_id = role.id, "restore role");
    }
    for channel in &snapshot.channels {
        tracing::debug!(%guild_id, channel_id = channel.id, "restore channel");
    }
    db::insert_action(&ctx.db, incident_id, "restore", None).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{App, ChatGuardConfig, Database, Discord, Logging, Settings};
    use sqlx::postgres::PgPoolOptions;
    use std::sync::Arc;

    fn ctx() -> Arc<AppContext> {
        let settings = Settings {
            env: "test".into(),
            app: App { name: "test".into() },
            discord: Discord { token: String::new(), app_id: None, intents: vec![] },
            database: Database {
                url: "postgres://localhost:1/test?connect_timeout=1".into(),
                max_connections: Some(1),
                statement_timeout_ms: Some(5_000),
            },
            logging: Logging { json: Some(false), level: Some("info".into()) },
            chatguard: ChatGuardConfig { racial_slurs: vec![] },
            antinuke: Default::default(),
        };
        let db = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy(&settings.database.url)
            .unwrap();
        AppContext::new_testing(settings, db)
    }

    #[tokio::test]
    async fn restore_noop() {
        let ctx = ctx();
        let snap = GuildSnapshot { roles: vec![], channels: vec![] };
        apply_snapshot(&ctx, 1, 1, &snap).await.unwrap_err();
    }
}