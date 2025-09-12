use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{db, AppContext};
#[cfg(not(test))]
use crate::db;
#[cfg(test)]
use self::db_mock as db;

#[cfg(test)]
mod db_mock {
    use super::*;
    use anyhow::Result;
    use once_cell::sync::Lazy;
    use serde_json::Value;
    use std::sync::Mutex;

    pub static SNAPSHOTS: Lazy<Mutex<Vec<Value>>> = Lazy::new(|| Mutex::new(Vec::new()));

    pub async fn create_incident(_db: &crate::db::Db, _guild_id: u64, _reason: &str) -> Result<i64> {
        Ok(1)
    }

    pub async fn insert_snapshot(
        _db: &crate::db::Db,
        _incident_id: i64,
        data: &Value,
    ) -> Result<()> {
        SNAPSHOTS.lock().unwrap().push(data.clone());
        Ok(())
    }

    pub async fn insert_action(
        _db: &crate::db::Db,
        _incident_id: i64,
        _kind: &str,
        _actor_id: Option<u64>,
    ) -> Result<()> {
        Ok(())
    }

    pub async fn list_incidents(
        _db: &crate::db::Db,
        _guild_id: u64,
    ) -> Result<Vec<(i64, String)>> {
        Ok(vec![])
    }
}

pub mod api;
pub mod approve;
pub mod commands;
pub mod restore;
pub mod snapshot;

/// Types of destructive actions monitored by the antinuke service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    ChannelDelete,
    RoleDelete,
    Ban,
    Webhook,
}

#[derive(Debug)]
struct Counter {
    count: u32,
    last_reset: Instant,
}

/// Antinuke service responsible for tracking destructive events and triggering
/// protective actions when configured thresholds are exceeded.
/// 
#[derive(Debug)]
pub struct Antinuke {
    ctx: Arc<AppContext>,
    thresholds: HashMap<EventType, u32>,
    guild_thresholds: HashMap<u64, HashMap<EventType, u32>>,
    reset_after: Duration,
    events: Mutex<HashMap<(u64, EventType), Counter>>, // (guild_id, event)
}

impl Antinuke {
    pub fn new(ctx: Arc<AppContext>) -> Arc<Self> {
        let default_threshold = ctx.settings.antinuke.threshold.unwrap_or(5);
        let reset_after = Duration::from_secs(ctx.settings.antinuke.reset_seconds.unwrap_or(60));
        let mut thresholds = HashMap::from([
            (EventType::ChannelDelete, default_threshold),
            (EventType::RoleDelete, default_threshold),
            (EventType::Ban, default_threshold),
            (EventType::Webhook, default_threshold),
        ]);
        thresholds.extend(ctx.settings.antinuke.thresholds.clone());
        let guild_thresholds = ctx.settings.antinuke.guild_thresholds.clone();
        Arc::new(Self {
            ctx,
            thresholds,
            guild_thresholds,
            reset_after,
            events: Mutex::new(HashMap::new()),
        })
    }

    /// Access underlying [AppContext].
    pub fn ctx(&self) -> &AppContext {
        &self.ctx
    }

    /// Record destructive action and escalate if threshold exceeded.
    pub async fn notify(&self, guild_id: u64, kind: EventType) -> Result<()> {
        let mut map = self.events.lock().await;
        let counter = map.entry((guild_id, kind)).or_insert(Counter {
            count: 0,
            last_reset: Instant::now(),
        });

        if counter.last_reset.elapsed() > self.reset_after {
            counter.count = 0;
            counter.last_reset = Instant::now();
        }

        counter.count += 1;
        let threshold = self
            .guild_thresholds
            .get(&guild_id)
            .and_then(|m| m.get(&kind))
            .copied()
            .or_else(|| self.thresholds.get(&kind).copied())
            .unwrap_or(5);
        if counter.count >= threshold {
            let reason = format!("{:?} threshold {}", kind, threshold);
            self.cut(guild_id, &reason).await?;
            counter.count = 0;
            counter.last_reset = Instant::now();
        }
        Ok(())
    }

    /// Trigger protective action for a guild and persist incident to DB.
    pub async fn cut(&self, guild_id: u64, reason: &str) -> Result<()> {
        tracing::warn!(%guild_id, %reason, "antinuke cut triggered");
        let snapshot = snapshot::take_snapshot(guild_id).await?;
        let incident_id = db::create_incident(&self.ctx.db, guild_id, reason).await?;
        let json = serde_json::to_value(&snapshot)?;
        db::insert_snapshot(&self.ctx.db, incident_id, &json).await?;
        db::insert_action(&self.ctx.db, incident_id, "cut", None).await?;
        Ok(())
    }

    /// Notify about channel deletion; simplistic threshold of 5.
    pub async fn notify_channel_delete(&self, guild_id: u64) -> Result<()> {
        self.notify(guild_id, EventType::ChannelDelete).await
    }

    /// Notify about role deletion.
    pub async fn notify_ban(&self, guild_id: u64) -> Result<()> {
        self.notify(guild_id, EventType::Ban).await
    }

    /// Notify about webhook updates.
    pub async fn notify_webhook(&self, guild_id: u64) -> Result<()> {
        self.notify(guild_id, EventType::Webhook).await
    }

    /// Notify about ban events.
    pub async fn notify_role_delete(&self, guild_id: u64) -> Result<()> {
        self.notify(guild_id, EventType::RoleDelete).await
    }

    /// List incidents for guild for API responses.
    pub async fn incidents(&self, guild_id: u64) -> Result<Vec<(i64, String)>> {
        db::list_incidents(&self.ctx.db, guild_id).await
    }

    /// Record manual approval action.
    pub async fn record_action(
        &self,
        incident_id: i64,
        kind: &str,
        actor_id: Option<u64>,
    ) -> Result<()> {
        db::insert_action(&self.ctx.db, incident_id, kind, actor_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{App, ChatGuardConfig, Database, Discord, Logging, Settings};
    use sqlx::postgres::PgPoolOptions;
    use std::collections::HashMap;

     fn ctx_with_config(antinuke: AntinukeConfig) -> Arc<AppContext> {
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
            antinuke,
        };
        let db = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy(&settings.database.url)
            .unwrap();
        AppContext::new_testing(settings, db)
    }
    fn ctx() -> Arc<AppContext> {
        ctx_with_config(Default::default())
    }

    #[tokio::test]
    async fn cut_runs() {
        let ctx = ctx();
        let an = Antinuke::new(ctx);
        an.cut(1, "test").await.unwrap();
    }
#[tokio::test]
    async fn notify_threshold() {
        let ctx = ctx();
        let an = Antinuke::new(ctx);
        let guild = 1;
        let threshold = an.thresholds[&EventType::ChannelDelete];
        for _ in 1..threshold {
            an.notify_channel_delete(guild).await.unwrap();
        }
        an.notify_channel_delete(guild).await.unwrap();
        let map = an.events.lock().await;
        let counter = map.get(&(guild, EventType::ChannelDelete)).unwrap();
        assert_eq!(counter.count, 0);
    }

    #[tokio::test]
    async fn notify_webhook_threshold() {
        let ctx = ctx();
        let an = Antinuke::new(ctx);
        let guild = 2;
        let threshold = an.thresholds[&EventType::Webhook];
        for i in 1..threshold {
            an.notify_webhook(guild).await.unwrap();
            let map = an.events.lock().await;
            let counter = map.get(&(guild, EventType::Webhook)).unwrap();
            assert_eq!(counter.count, i);
        }
        an.notify_webhook(guild).await.unwrap();
        let map = an.events.lock().await;
        let counter = map.get(&(guild, EventType::Webhook)).unwrap();
        assert_eq!(counter.count, 0);
    }
    #[tokio::test]
    async fn cut_saves_snapshot() {
        let ctx = ctx();
        let an = Antinuke::new(ctx);
        an.cut(123, "save").await.unwrap();
        let snaps = super::db_mock::SNAPSHOTS.lock().unwrap();
        assert_eq!(snaps.len(), 1);
        let snap: snapshot::GuildSnapshot = serde_json::from_value(snaps[0].clone()).unwrap();
        assert!(snap.roles.is_empty() && snap.channels.is_empty());
    }
 #[tokio::test]
    async fn guild_override_threshold() {
        let mut cfg = AntinukeConfig::default();
        let mut overrides = HashMap::new();
        overrides.insert(EventType::Ban, 1);
        cfg.guild_thresholds.insert(42, overrides);
        let ctx = ctx_with_config(cfg);
        let an = Antinuke::new(ctx);
        an.notify_ban(42).await.unwrap_err();
        let map = an.events.lock().await;
        let counter = map.get(&(42, EventType::Ban)).unwrap();
        assert_eq!(counter.count, 0);
    }
}