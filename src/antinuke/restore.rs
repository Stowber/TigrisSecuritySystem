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
    for r in &existing_roles {
        match snapshot.roles.iter().find(|sr| sr.id == r.id) {
            Some(desired) => {
                if r != desired {
                    api.update_role(guild_id, desired).await?;
                }
            }
            None => {
                api.delete_role(guild_id, r.id).await?;
            }
        }
    }
    for role in &snapshot.roles {
        if !existing_roles.iter().any(|r| r.id == role.id) {
            api.create_role(guild_id, role).await?;
        }
    }
    let existing_channels = api.fetch_channels(guild_id).await?;
     for c in &existing_channels {
        match snapshot.channels.iter().find(|sc| sc.id == c.id) {
            Some(desired) => {
                if c != desired {
                    api.update_channel(guild_id, desired).await?;
                }
            }
            None => {
                api.delete_channel(guild_id, c.id).await?;
            }
        }
    }
    for channel in &snapshot.channels {
        if !existing_channels.iter().any(|c| c.id == channel.id) {
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

    async fn update_role(&self, _guild_id: u64, role: &RoleSnapshot) -> Result<()> {
            if let Some(r) = self.roles.lock().await.iter_mut().find(|r| r.id == role.id) {
                *r = role.clone();
            }
            Ok(())
        }
        async fn update_channel(&self, _guild_id: u64, channel: &ChannelSnapshot) -> Result<()> {
            if let Some(c) = self
                .channels
                .lock()
                .await
                .iter_mut()
                .find(|c| c.id == channel.id)
            {
                *c = channel.clone();
            }
            Ok(())
        }
        async fn delete_role(&self, _guild_id: u64, role_id: u64) -> Result<()> {
            self.roles.lock().await.retain(|r| r.id != role_id);
            Ok(())
        }
        async fn delete_channel(&self, _guild_id: u64, channel_id: u64) -> Result<()> {
            self.channels.lock().await.retain(|c| c.id != channel_id);
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
    async fn adds_missing_objects() {
        let ctx = ctx();
        let api = MockApi::default();
        let snap = GuildSnapshot {
            roles: vec![RoleSnapshot {
                id: 1,
                name: "r".into(),
                position: 1,
                permissions: 0,
            }],
            channels: vec![ChannelSnapshot {
                id: 1,
                name: "c".into(),
                kind: "text".into(),
                position: 1,
                parent_id: None,
            }],
        };
        let _ = apply_snapshot(&api, &ctx, 1, 1, &snap).await;
        assert_eq!(api.roles.lock().await, snap.roles);
        assert_eq!(api.channels.lock().await, snap.channels);
    }

    #[tokio::test]
    async fn updates_existing_objects() {
        let ctx = ctx();
        let api = MockApi {
            roles: Mutex::new(vec![RoleSnapshot {
                id: 1,
                name: "old".into(),
                position: 1,
                permissions: 0,
            }]),
            channels: Mutex::new(vec![ChannelSnapshot {
                id: 1,
                name: "old".into(),
                kind: "text".into(),
                position: 1,
                parent_id: None,
            }]),
        };
        let snap = GuildSnapshot {
            roles: vec![RoleSnapshot {
                id: 1,
                name: "new".into(),
                position: 2,
                permissions: 1,
            }],
            channels: vec![ChannelSnapshot {
                id: 1,
                name: "new".into(),
                kind: "text".into(),
                position: 2,
                parent_id: None,
            }],
        };
        let _ = apply_snapshot(&api, &ctx, 1, 1, &snap).await;
        assert_eq!(api.roles.lock().await[0].name, "new");
        assert_eq!(api.roles.lock().await[0].position, 2);
        assert_eq!(api.channels.lock().await[0].name, "new");
        assert_eq!(api.channels.lock().await[0].position, 2);
    }

    #[tokio::test]
    async fn removes_extra_objects() {
        let ctx = ctx();
        let api = MockApi {
            roles: Mutex::new(vec![RoleSnapshot {
                id: 1,
                name: "old".into(),
                position: 1,
                permissions: 0,
            }]),
            channels: Mutex::new(vec![ChannelSnapshot {
                id: 1,
                name: "old".into(),
                kind: "text".into(),
                position: 1,
                parent_id: None,
            }]),
        };
        let snap = GuildSnapshot {
            roles: vec![],
            channels: vec![],
        };
        let _ = apply_snapshot(&api, &ctx, 1, 1, &snap).await;
        assert!(api.roles.lock().await.is_empty());
        assert!(api.channels.lock().await.is_empty());
    }
}