use anyhow::Result;

use super::snapshot::{DiscordApi, GuildSnapshot};
use crate::{AppContext, db};

/// Apply snapshot to guild (placeholder implementation).
pub async fn apply_snapshot(
    api: &impl DiscordApi,
    ctx: &AppContext,
    guild_id: u64,
    incident_id: i64,
    snapshot: &GuildSnapshot,
) -> Result<()> {
    tracing::info!(%guild_id, "restoring snapshot");
    let existing_roles = api.fetch_roles(guild_id).await?;
    for role in &snapshot.roles {
        if !existing_roles.iter().any(|r| r.id == role.id) {
            tracing::debug!(%guild_id, role_id = role.id, "creating role");
            api.create_role(guild_id, role).await?;
        }
    }
    let existing_channels = api.fetch_channels(guild_id).await?;
    for channel in &snapshot.channels {
        if !existing_channels.iter().any(|c| c.id == channel.id) {
            tracing::debug!(%guild_id, channel_id = channel.id, "creating channel");
            api.create_channel(guild_id, channel).await?;
        }
    }
    db::insert_action(&ctx.db, incident_id, "restore", None).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::snapshot::{ChannelSnapshot, RoleSnapshot};
    use crate::config::{App, ChatGuardConfig, Database, Discord, Logging, Settings};
    use serenity::async_trait;
    use sqlx::postgres::PgPoolOptions;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[derive(Default)]
    struct MockApi {
        roles: Mutex<Vec<RoleSnapshot>>,
        channels: Mutex<Vec<ChannelSnapshot>>,
    }

    #[async_trait]
    impl DiscordApi for MockApi {
        async fn fetch_roles(&self, _guild_id: u64) -> Result<Vec<RoleSnapshot>> {
            Ok(self.roles.lock().await.clone())
        }
        async fn fetch_channels(&self, _guild_id: u64) -> Result<Vec<ChannelSnapshot>> {
            Ok(self.channels.lock().await.clone())
        }
        async fn create_role(&self, _guild_id: u64, role: &RoleSnapshot) -> Result<()> {
            self.roles.lock().await.push(role.clone());
            Ok(())
        }
        async fn create_channel(&self, _guild_id: u64, channel: &ChannelSnapshot) -> Result<()> {
            self.channels.lock().await.push(channel.clone());
            Ok(())
        }
    }

    fn ctx() -> Arc<AppContext> {
        let settings = Settings {
            env: "test".into(),
            app: App {
                name: "test".into(),
            },
            discord: Discord {
                token: String::new(),
                app_id: None,
                intents: vec![],
            },
            database: Database {
                url: "postgres://localhost:1/test?connect_timeout=1".into(),
                max_connections: Some(1),
                statement_timeout_ms: Some(5_000),
            },
            logging: Logging {
                json: Some(false),
                level: Some("info".into()),
            },
            chatguard: ChatGuardConfig {
                racial_slurs: vec![],
            },
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
        let api = MockApi::default();
        let snap = GuildSnapshot {
            roles: vec![],
            channels: vec![],
        };
        apply_snapshot(&api, &ctx, 1, 1, &snap).await.unwrap_err();
    }
}