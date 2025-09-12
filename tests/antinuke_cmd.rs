use std::sync::Arc;

use serde_json::json;
use sqlx::postgres::{PgPool, PgPoolOptions};
use tigris_security::{
    AppContext,
    antinuke::{
        commands::{cmd_approve, cmd_restore, cmd_status},
    },
    config::{App, ChatGuardConfig, Database, Discord, Logging, Settings},
    db,
};

fn base_settings() -> Settings {
    Settings {
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
            url: String::new(),
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
    }
}

fn ctx_from_pool(pool: PgPool) -> Arc<AppContext> {
    let settings = base_settings();
    let ctx = AppContext::new_testing(settings, pool);
    ctx.with_antinuke();
    ctx
}

fn failing_ctx() -> Arc<AppContext> {
    let mut settings = base_settings();
    settings.database.url = "postgres://localhost:1/test?connect_timeout=1".into();
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_lazy(&settings.database.url)
        .unwrap();
    let ctx = AppContext::new_testing(settings, pool);
    ctx.with_antinuke();
    ctx
}

#[sqlx::test(migrations = "./migrations")]
async fn status_empty(pool: PgPool) {
    let ctx = ctx_from_pool(pool);
    let incidents = cmd_status(&ctx, 1).await.unwrap();
    assert!(incidents.is_empty());
}

#[sqlx::test(migrations = "./migrations")]
async fn status_non_empty(pool: PgPool) {
    let ctx = ctx_from_pool(pool.clone());
    let id = db::create_incident(&ctx.db, 1, "test").await.unwrap();
    let incidents = cmd_status(&ctx, 1).await.unwrap();
    assert_eq!(incidents, vec![(id, "test".into())]);
}

#[sqlx::test(migrations = "./migrations")]
async fn incident_inserts_missing_guild(pool: PgPool) {
    let ctx = ctx_from_pool(pool.clone());
    let id = db::create_incident(&ctx.db, 1, "reason").await.unwrap();
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM tss.antinuke_guilds WHERE guild_id = $1)",
    )
    .bind(1i64)
    .fetch_one(&ctx.db)
    .await
    .unwrap();
    assert!(exists);
    let incidents = cmd_status(&ctx, 1).await.unwrap();
    assert_eq!(incidents, vec![(id, "reason".into())]);
}

#[sqlx::test(migrations = "./migrations")]
async fn approve_writes_action(pool: PgPool) {
    let ctx = ctx_from_pool(pool.clone());
    let incident = db::create_incident(&ctx.db, 1, "reason").await.unwrap();
    cmd_approve(&ctx, incident, 42).await.unwrap();
    let (actor_id, kind): (Option<i64>, String) =
        sqlx::query_as("SELECT actor_id, kind FROM tss.antinuke_actions WHERE incident_id = $1")
            .bind(incident)
            .fetch_one(&ctx.db)
            .await
            .unwrap();
    assert_eq!(actor_id, Some(42));
    assert_eq!(kind, "approve");
}

#[tokio::test]
async fn approve_db_error() {
    let ctx = failing_ctx();
    let res = cmd_approve(&ctx, 1, 42).await;
    assert!(res.is_err());
}

#[sqlx::test(migrations = "./migrations")]
async fn restore_writes_action(pool: PgPool) {
    let ctx = ctx_from_pool(pool.clone());
    let incident = db::create_incident(&ctx.db, 1, "reason").await.unwrap();
    db::insert_snapshot(&ctx.db, incident, &json!({"roles": [], "channels": []}))
        .await
        .unwrap();
    cmd_restore(&ctx, 1, incident).await.unwrap();
    let kind: String =
        sqlx::query_scalar("SELECT kind FROM tss.antinuke_actions WHERE incident_id = $1")
            .bind(incident)
            .fetch_one(&ctx.db)
            .await
            .unwrap();
    assert_eq!(kind, "restore");
}

#[tokio::test]
async fn restore_db_error() {
    let ctx = failing_ctx();
    let res = cmd_restore(&ctx, 1, 1).await;
    assert!(res.is_err());
}