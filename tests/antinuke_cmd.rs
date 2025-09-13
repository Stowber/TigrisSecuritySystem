// tests/antinuke_cmd.rs

use serenity::all::Http;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;

use tigris_security::antinuke::commands::handle_subcommand;
use tigris_security::config::{App, ChatGuardConfig, Database, Discord, Logging, Settings};
use tigris_security::permissions::Role;
use tigris_security::AppContext;

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
        // Używamy lazy-połączenia do "martwego" hosta, żeby testy nie wymagały realnej bazy.
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
    }
}

fn ctx() -> Arc<AppContext> {
    let settings = base_settings();
    let db = PgPoolOptions::new()
        .max_connections(1)
        .connect_lazy(&settings.database.url)
        .unwrap();
    AppContext::new_testing(settings, db)
}

#[tokio::test]
async fn approve_permission_error() {
    let ctx = ctx();
    let http = Http::new("");

    let msg = handle_subcommand(&ctx, &http, 1, 1, "approve", Some(1)).await;
    assert!(
        msg.contains("missing permission"),
        "expected missing permission, got: {msg}"
    );
}

#[tokio::test]
async fn approve_permission_ok_but_db_fails() {
    let ctx = ctx();
    let http = Http::new("");
    // Dajemy rolę admina, by przejść ACL; operacja sama padnie na DB/API i powinna zwrócić błąd domenowy.
    ctx.user_roles
        .lock()
        .unwrap()
        .insert(1, vec![Role::TechnikZarzad]);

    let msg = handle_subcommand(&ctx, &http, 1, 1, "approve", Some(1)).await;
    assert!(
        msg.starts_with("approve failed:"),
        "expected domain error prefix, got: {msg}"
    );
}

#[tokio::test]
async fn restore_permission_ok_but_http_or_db_fails() {
    let ctx = ctx();
    let http = Http::new("");
    ctx.user_roles
        .lock()
        .unwrap()
        .insert(1, vec![Role::TechnikZarzad]);

    let msg = handle_subcommand(&ctx, &http, 1, 1, "restore", Some(1)).await;
    assert!(
        msg.starts_with("restore failed:"),
        "expected restore failure prefix, got: {msg}"
    );
}

#[tokio::test]
async fn test_triggers_cut() {
    let ctx = ctx();
    let http = Http::new("");
    ctx.user_roles
        .lock()
        .unwrap()
        .insert(1, vec![Role::TechnikZarzad]);

    let msg = handle_subcommand(&ctx, &http, 1, 1, "test", None).await;
    assert_eq!(msg, "test incident triggered");
}

#[tokio::test]
async fn maintenance_permissions() {
    let ctx = ctx();
    let http = Http::new("");

    // Bez roli – brak uprawnień
    let msg = handle_subcommand(&ctx, &http, 1, 1, "maintenance.start", None).await;
    assert!(
        msg.contains("missing permission"),
        "expected missing permission, got: {msg}"
    );

    // Z adminem – powinno przejść
    ctx.user_roles
        .lock()
        .unwrap()
        .insert(1, vec![Role::TechnikZarzad]);

    let msg = handle_subcommand(&ctx, &http, 1, 1, "maintenance.start", None).await;
    assert_eq!(msg, "maintenance started");
}

#[tokio::test]
async fn unknown_subcommand_is_reported() {
    let ctx = ctx();
    let http = Http::new("");
    ctx.user_roles
        .lock()
        .unwrap()
        .insert(1, vec![Role::TechnikZarzad]);

    let msg = handle_subcommand(&ctx, &http, 1, 1, "doesnotexist", None).await;
    assert!(
        msg.starts_with("unknown subcommand:"),
        "expected unknown subcommand error, got: {msg}"
    );
}
