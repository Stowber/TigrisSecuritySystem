use rand::{rngs::StdRng, SeedableRng};
use tigris_security::antinuke::{Antinuke};
use tigris_security::antinuke::commands::handle_subcommand;
use tigris_security::antinuke::db_mock;
use tigris_security::config::{AntinukeConfig, App, ChatGuardConfig, Database, Discord, Logging, Settings};
use tigris_security::AppContext;
use tigris_security::permissions::Role;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;

fn ctx_with_config(antinuke: AntinukeConfig) -> Arc<AppContext> {
    let settings = Settings {
        env: "test".into(),
        app: App { name: "test".into() },
        discord: Discord { token: String::new(), app_id: None, intents: vec![] },
        database: Database { url: "postgres://localhost:1/test?connect_timeout=1".into(), max_connections: Some(1), statement_timeout_ms: Some(5_000) },
        logging: Logging { json: Some(false), level: Some("info".into()) },
        chatguard: ChatGuardConfig { racial_slurs: vec![] },
        antinuke,
    };
    let db = PgPoolOptions::new().max_connections(1).connect_lazy(&settings.database.url).unwrap();
    AppContext::new_testing(settings, db)
}
fn ctx() -> Arc<AppContext> { ctx_with_config(Default::default()) }

#[tokio::test]
async fn rotation_changes_channels() {
    let ctx = ctx();
    let an = Antinuke::new(ctx);
    an.insert_guild(1).await;
    let mut rng = StdRng::seed_from_u64(1);
    an.rotate_with_rng(&mut rng).await;
    let first = an.get_protected(1).await;
    an.rotate_with_rng(&mut rng).await;
    let second = an.get_protected(1).await;
    assert_ne!(first, second);
}

#[tokio::test]
async fn maintenance_suppresses_cut() {
    let mut cfg = AntinukeConfig::default();
    cfg.threshold = Some(1);
    let ctx = ctx_with_config(cfg);
    let an = Antinuke::new(ctx);
    db_mock::SNAPSHOTS.lock().unwrap().clear();
    an.start_maintenance(1).await;
    an.notify_channel_delete(1, 10).await.unwrap();
    assert_eq!(db_mock::SNAPSHOTS.lock().unwrap().len(), 0);
    an.stop_maintenance(1).await;
    an.notify_channel_delete(1, 20).await.unwrap();
    assert_eq!(db_mock::SNAPSHOTS.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn maintenance_permissions() {
    let ctx = ctx();
    let msg = handle_subcommand(&ctx, 1, 1, "maintenance.start", None).await;
    assert!(msg.contains("missing permission"));
    ctx.user_roles.lock().unwrap().insert(1, vec![Role::TechnikZarzad]);
    let msg = handle_subcommand(&ctx, 1, 1, "maintenance.start", None).await;
    assert_eq!(msg, "maintenance started");
}