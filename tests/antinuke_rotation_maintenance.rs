// tests/antinuke_rotation_maintenance.rs

use rand::rngs::StdRng;
use serenity::all::Http;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use rand::SeedableRng;

use tigris_security::antinuke::Antinuke;
use tigris_security::antinuke::commands::handle_subcommand;
#[cfg(feature = "test-utils")]
use tigris_security::antinuke::db_mock; // dostępne tylko, gdy włączysz feature test-utils
use tigris_security::config::{
    AntinukeConfig, App, ChatGuardConfig, Database, Discord, Logging, Settings,
};
use tigris_security::permissions::Role;
use tigris_security::AppContext;

/// Szybki helper do budowy kontekstu testowego bez prawdziwej bazy.
fn ctx_with_config(antinuke: AntinukeConfig) -> Arc<AppContext> {
    let settings = Settings {
        env: "test".into(),
        app: App { name: "test".into() },
        discord: Discord {
            token: String::new(),
            app_id: None,
            intents: vec![],
        },
        // Lazy-połączenie do „martwego” hosta – nie uderzamy w realną bazę w tych testach.
        database: Database {
            url: "postgres://localhost:1/test?connect_timeout=1".into(),
            max_connections: Some(1),
            statement_timeout_ms: Some(5_000),
        },
        logging: Logging {
            json: Some(false),
            level: Some("info".into()),
        },
        chatguard: ChatGuardConfig { racial_slurs: vec![] },
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

/// Test rotacji – sprawdza, że dwa kolejne obroty zmieniają zestaw chronionych kanałów.
/// Jeśli zestawy są puste (brak zarejestrowanego gildii w środku), asercja i tak przejdzie
/// jeśli algorytm faktycznie zwraca inny stan ; w praktyce zapewnij, że gildia 1 istnieje w stanie.
#[tokio::test]
async fn rotation_changes_channels() {
    let ctx = ctx();
    let an = Antinuke::new(ctx);
    let mut rng = StdRng::from_os_rng();

    an.rotate_with_rng(&mut rng).await;
    let first = an.get_protected(1).await;

    an.rotate_with_rng(&mut rng).await;
    let second = an.get_protected(1).await;

    assert_ne!(
        first, second,
        "protected set didn't change; upewnij się, że gildia 1 jest obsługiwana w stanie podczas testu"
    );
}

/// Test, że maintenance wycina akcję „cut”. Wymaga mocka DB/Snapshot – włącz:
/// `cargo test --features test-utils`
#[cfg(feature = "test-utils")]
#[tokio::test]
async fn maintenance_suppresses_cut() {
    let mut cfg = AntinukeConfig::default();
    cfg.threshold = Some(1);
    let ctx = ctx_with_config(cfg);
    let an = Antinuke::new(ctx);

    // czysty stan mocka
    db_mock::SNAPSHOTS.lock().unwrap().clear();

    an.start_maintenance(1).await;
    an.notify_channel_delete(1, 10).await.unwrap();
    assert_eq!(db_mock::SNAPSHOTS.lock().unwrap().len(), 0);

    an.stop_maintenance(1).await;
    an.notify_channel_delete(1, 20).await.unwrap();
    assert_eq!(db_mock::SNAPSHOTS.lock().unwrap().len(), 1);

    // sprzątanie
    db_mock::SNAPSHOTS.lock().unwrap().clear();
}

/// Uprawnienia do maintenance – korzysta ze ścieżki komend.
#[tokio::test]
async fn maintenance_permissions() {
    let ctx = ctx();
    let http = Http::new(""); // placeholder – nie wołamy prawdziwego API

    // Bez roli – brak uprawnień
    let msg = handle_subcommand(&ctx, &http, 1, 1, "maintenance.start", None).await;
    assert!(
        msg.contains("missing permission"),
        "expected missing permission, got: {msg}"
    );

    // Z rolą – powinno przejść. Upewnij się, że ACL mapuje tę rolę na antinuke.maintenance.*
    ctx.user_roles
        .lock()
        .unwrap()
        .insert(1, vec![Role::TechnikZarzad]);

    let msg = handle_subcommand(&ctx, &http, 1, 1, "maintenance.start", None).await;
    assert_eq!(msg, "maintenance started");
}
