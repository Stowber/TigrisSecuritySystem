use std::sync::Arc;

use serenity::async_trait;
use tokio::sync::Mutex;

use tigris_security::{
    AppContext,
    antinuke::{restore, snapshot},
    config::{App, ChatGuardConfig, Database, Discord, Logging, Settings},
};

use snapshot::{ChannelSnapshot, DiscordApi, RoleSnapshot};

#[derive(Default)]
struct MockApi {
    roles: Mutex<Vec<RoleSnapshot>>,
    channels: Mutex<Vec<ChannelSnapshot>>,
}

#[async_trait]
impl DiscordApi for MockApi {
    async fn fetch_roles(&self, _guild_id: u64) -> anyhow::Result<Vec<RoleSnapshot>> {
        Ok(self.roles.lock().await.clone())
    }
    async fn fetch_channels(&self, _guild_id: u64) -> anyhow::Result<Vec<ChannelSnapshot>> {
        Ok(self.channels.lock().await.clone())
    }
    async fn create_role(&self, _guild_id: u64, role: &RoleSnapshot) -> anyhow::Result<()> {
        self.roles.lock().await.push(role.clone());
        Ok(())
    }
    async fn create_channel(
        &self,
        _guild_id: u64,
        channel: &ChannelSnapshot,
    ) -> anyhow::Result<()> {
        self.channels.lock().await.push(channel.clone());
        Ok(())
    }

 async fn update_role(&self, _guild_id: u64, role: &RoleSnapshot) -> anyhow::Result<()> {
        if let Some(r) = self
            .roles
            .lock()
            .await
            .iter_mut()
            .find(|r| r.id == role.id)
        {
            *r = role.clone();
        }
        Ok(())
    }
    async fn update_channel(
        &self,
        _guild_id: u64,
        channel: &ChannelSnapshot,
    ) -> anyhow::Result<()> {
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
    async fn delete_role(&self, _guild_id: u64, role_id: u64) -> anyhow::Result<()> {
        self.roles.lock().await.retain(|r| r.id != role_id);
        Ok(())
    }
    async fn delete_channel(&self, _guild_id: u64, channel_id: u64) -> anyhow::Result<()> {
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
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_lazy(&settings.database.url)
        .unwrap();
    AppContext::new_testing(settings, db)
}

#[tokio::test]
async fn snapshot_and_restore_mock() {
    let ctx = ctx();
    let api = MockApi::default();

    // seed initial state
    api.roles.lock().await.push(RoleSnapshot {
        id: 1,
        name: "r".into(),
        position: 1,
        permissions: 7,
    });
    api.channels.lock().await.push(ChannelSnapshot {
        id: 10,
        name: "c".into(),
        kind: "text".into(),
        position: 1,
        parent_id: None,
    });

    let snap = snapshot::take_snapshot(&api, 1).await.unwrap();

    // simulate wipe
    api.roles.lock().await.clear();
    api.channels.lock().await.clear();

    // restoration (will fail on DB, but should recreate state)
    let _ = restore::apply_snapshot(&api, &ctx, 1, 1, &snap).await;

    assert_eq!(api.roles.lock().await.clone(), snap.roles);
    assert_eq!(api.channels.lock().await.clone(), snap.channels);
}