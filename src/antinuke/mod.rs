use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use rand::{rngs::StdRng, Rng, SeedableRng};


#[cfg(test)]
use self::db_mock as db;
use crate::AppContext;
#[cfg(not(test))]
use crate::db;
use serenity::all::Http;

#[cfg(test)]
pub mod db_mock {
    use anyhow::Result;
    use once_cell::sync::Lazy;
    use serde_json::Value;
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    pub static SNAPSHOTS: Lazy<Mutex<Vec<Value>>> = Lazy::new(|| Mutex::new(Vec::new()));
    pub static PROTECTED: Lazy<Mutex<HashMap<u64, HashSet<u64>>>> =
        Lazy::new(|| Mutex::new(HashMap::new()));

    pub async fn create_incident(
        _db: &crate::db::Db,
        _guild_id: u64,
        _reason: &str,
    ) -> Result<i64> {
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
    pub async fn set_protected_channels(
        _db: &crate::db::Db,
        guild_id: u64,
        channels: &[u64],
    ) -> Result<()> {
        PROTECTED
            .lock()
            .unwrap()
            .insert(guild_id, channels.iter().cloned().collect());
        Ok(())
    }

    pub async fn fetch_protected_channels(
        _db: &crate::db::Db,
    ) -> Result<HashMap<u64, HashSet<u64>>> {
        Ok(PROTECTED.lock().unwrap().clone())
    }


     pub async fn list_incidents(_db: &crate::db::Db, _guild_id: u64) -> Result<Vec<(i64, String)>> {
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
    http: Arc<Http>,
    thresholds: HashMap<EventType, u32>,
    guild_thresholds: HashMap<u64, HashMap<EventType, u32>>,
    reset_after: Duration,
    events: Mutex<HashMap<(u64, EventType), Counter>>, // (guild_id, event)
    protected_channels: Mutex<HashMap<u64, HashSet<u64>>>,
    maintenance: Mutex<HashMap<u64, Instant>>,
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
        let http = Arc::new(Http::new(&ctx.settings.discord.token));
        let this = Arc::new(Self {
            ctx,
            http,
            thresholds,
            guild_thresholds,
            reset_after,
            events: Mutex::new(HashMap::new()),
        protected_channels: Mutex::new(HashMap::new()),
            maintenance: Mutex::new(HashMap::new()),
        });

        let an = Arc::clone(&this);
        tokio::spawn(async move {
            if let Ok(map) = db::fetch_protected_channels(&an.ctx.db).await {
                tracing::info!("loaded protected channels");
                *an.protected_channels.lock().await = map;
            }
            let mut interval = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
            loop {
                interval.tick().await;
                let mut rng = StdRng::from_entropy();
                an.rotate_with_rng(&mut rng).await;
            }
        });

        this
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
        #[cfg(not(test))]
        let snapshot = {
            let api = snapshot::SerenityApi { http: &self.http };
            snapshot::take_snapshot(&api, guild_id).await?
        };
        #[cfg(test)]
        let snapshot = snapshot::GuildSnapshot {
            roles: vec![],
            channels: vec![],
        };
        let incident_id = db::create_incident(&self.ctx.db, guild_id, reason).await?;
        let json = serde_json::to_value(&snapshot)?;
        db::insert_snapshot(&self.ctx.db, incident_id, &json).await?;
        db::insert_action(&self.ctx.db, incident_id, "cut", None).await?;
        Ok(())
    }

    pub async fn rotate_with_rng<R: Rng + ?Sized>(&self, rng: &mut R) {
        let guilds: Vec<u64> = {
            let map = self.protected_channels.lock().await;
            if map.is_empty() {
                self.guild_thresholds.keys().cloned().collect()
            } else {
                map.keys().cloned().collect()
            }
        };
        for gid in guilds {
            let channel_id = rng.r#gen::<u64>();
            {
                let mut map = self.protected_channels.lock().await;
                map.insert(gid, HashSet::from([channel_id]));
            }
            if let Err(e) = db::set_protected_channels(&self.ctx.db, gid, &[channel_id]).await {
                tracing::warn!(error=?e, guild_id=gid, "set_protected_channels failed");
            } else {
                tracing::info!(guild_id=gid, channel_id, "protected channel rotated");
            }
        }
    }

    /// Notify about channel deletion; trigger cut immediately for protected channels.
    pub async fn notify_channel_delete(&self, guild_id: u64, channel_id: u64) -> Result<()> {
        if self.is_maintenance(guild_id).await {
            return Ok(());
        }
        let protected = self.protected_channels.lock().await;
        if protected
            .get(&guild_id)
            .map(|s| s.contains(&channel_id))
            .unwrap_or(false)
        {
            drop(protected);
            self.cut(guild_id, "protected channel deleted").await?;
            return Ok(());
        }
        drop(protected);
        self.notify(guild_id, EventType::ChannelDelete).await
    }

    /// Start maintenance mode for guild.
    pub async fn start_maintenance(&self, guild_id: u64) {
        let mut map = self.maintenance.lock().await;
        map.insert(guild_id, Instant::now());
    }

    /// Stop maintenance mode for guild.
    pub async fn stop_maintenance(&self, guild_id: u64) {
        let mut map = self.maintenance.lock().await;
        map.remove(&guild_id);
    }

    /// Check if guild is in maintenance mode.
    pub async fn is_maintenance(&self, guild_id: u64) -> bool {
        let map = self.maintenance.lock().await;
        map.contains_key(&guild_id)
    }

    /// Expose current protected channels for testing.
    pub async fn get_protected(&self, guild_id: u64) -> HashSet<u64> {
        let map = self.protected_channels.lock().await;
        map.get(&guild_id).cloned().unwrap_or_default()
    }

    /// Testing helper to insert guild into protected map.
    #[cfg(test)]
    pub async fn insert_guild(&self, guild_id: u64) {
        let mut map = self.protected_channels.lock().await;
        map.insert(guild_id, HashSet::new());
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
    use crate::config::{
        AntinukeConfig, App, ChatGuardConfig, Database, Discord, Logging, Settings,
    };
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
        for id in 1..threshold {
            an.notify_channel_delete(guild, id as u64).await.unwrap();
        }
        an.notify_channel_delete(guild, 999).await.unwrap();
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