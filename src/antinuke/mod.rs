use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;

use crate::AppContext;

pub mod api;
pub mod approve;
pub mod commands;
pub mod restore;
pub mod snapshot;

/// Simple antinuke service placeholder.
#[derive(Debug)]
pub struct Antinuke {
    ctx: Arc<AppContext>,
    events: Mutex<HashMap<u64, u32>>, // guild_id -> event count
}

impl Antinuke {
    pub fn new(ctx: Arc<AppContext>) -> Arc<Self> {
        Arc::new(Self {
            ctx,
            events: Mutex::new(HashMap::new()),
        })
    }

    /// Trigger protective action for a guild.
    pub async fn cut(&self, guild_id: u64, reason: &str) -> Result<()> {
        tracing::warn!(%guild_id, %reason, "antinuke cut triggered");
        Ok(())
    }

    /// Notify about channel deletion; simplistic threshold of 5.
    pub async fn notify_channel_delete(&self, guild_id: u64) {
        let mut events = self.events.lock().await;
        let count = events.entry(guild_id).or_insert(0);
        *count += 1;
        if *count > 5 {
            let _ = self.cut(guild_id, "channel_delete threshold").await;
            *count = 0;
        }
    }

    /// Notify about role deletion.
    pub async fn notify_role_delete(&self, guild_id: u64) {
        let _ = self.cut(guild_id, "role_delete").await;
    }

    /// Notify about ban events.
    pub async fn notify_ban(&self, guild_id: u64) {
        let _ = self.cut(guild_id, "ban").await;
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
    async fn cut_runs() {
        let ctx = ctx();
        let an = Antinuke::new(ctx);
        an.cut(1, "test").await.unwrap();
    }
}