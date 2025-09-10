use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::Mutex;

use crate::{AppContext, db};

pub mod api;
pub mod approve;
pub mod commands;
pub mod restore;
pub mod snapshot;

/// Types of destructive actions monitored by the antinuke service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    reset_after: Duration,
    events: Mutex<HashMap<(u64, EventType), Counter>>, // (guild_id, event)
}

impl Antinuke {
    pub fn new(ctx: Arc<AppContext>) -> Arc<Self> {
         let default_threshold = ctx.settings.antinuke.threshold.unwrap_or(5);
        let reset_after = Duration::from_secs(ctx.settings.antinuke.reset_seconds.unwrap_or(60));
        let thresholds = HashMap::from([
            (EventType::ChannelDelete, default_threshold),
            (EventType::RoleDelete, default_threshold),
            (EventType::Ban, default_threshold),
            (EventType::Webhook, default_threshold),
        ]);
        Arc::new(Self {
            ctx,
            thresholds,
            reset_after,
            events: Mutex::new(HashMap::new()),
        })
    }

    /// Access underlying [AppContext].
    pub fn ctx(&self) -> &AppContext {
        &self.ctx
    }

    /// Record destructive action and escalate if threshold exceeded.
    pub async fn notify(&self, guild_id: u64, kind: EventType) {
        let mut map = self.events.lock().await;
        let counter = map
            .entry((guild_id, kind))
            .or_insert(Counter { count: 0, last_reset: Instant::now() });

        if counter.last_reset.elapsed() > self.reset_after {
            counter.count = 0;
            counter.last_reset = Instant::now();
        }

        counter.count += 1;
        let threshold = *self.thresholds.get(&kind).unwrap_or(&5);
        if counter.count >= threshold {
            let reason = format!("{:?} threshold {}", kind, threshold);
            let _ = self.cut(guild_id, &reason).await;
            counter.count = 0;
            counter.last_reset = Instant::now();
        }
    }

    /// Trigger protective action for a guild and persist incident to DB.
    pub async fn cut(&self, guild_id: u64, reason: &str) -> Result<()> {
        tracing::warn!(%guild_id, %reason, "antinuke cut triggered");
        let incident_id = db::create_incident(&self.ctx.db, guild_id, reason).await?;
        db::insert_action(&self.ctx.db, incident_id, "cut", None).await?;
        Ok(())
    }

    /// Notify about channel deletion; simplistic threshold of 5.
    pub async fn notify_channel_delete(&self, guild_id: u64) {
        self.notify(guild_id, EventType::RoleDelete).await;
    }

    /// Notify about role deletion.
    pub async fn notify_role_delete(&self, guild_id: u64) {
        let _ = self.cut(guild_id, "role_delete").await;
    }

    /// Notify about ban events.
    pub async fn notify_ban(&self, guild_id: u64) {
        self.notify(guild_id, EventType::Ban).await;
    }

    /// List incidents for guild for API responses.
    pub async fn incidents(&self, guild_id: u64) -> Result<Vec<(i64, String)>> {
        db::list_incidents(&self.ctx.db, guild_id).await
    }

    /// Record manual approval action.
    pub async fn record_action(&self, incident_id: i64, kind: &str, actor_id: Option<u64>) -> Result<()> {
        db::insert_action(&self.ctx.db, incident_id, kind, actor_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{App, ChatGuardConfig, Database, Discord, Logging, Settings};
    use sqlx::postgres::PgPoolOptions;

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
    async fn cut_runs() {
        let ctx = ctx();
        let an = Antinuke::new(ctx);
        an.cut(1, "test").await.unwrap_err();
    }
#[tokio::test]
    async fn notify_threshold() {
        let ctx = ctx();
        let an = Antinuke::new(ctx);
        for _ in 0..an.thresholds[&EventType::ChannelDelete] {
            an.notify_channel_delete(1).await;
        }
    }
}