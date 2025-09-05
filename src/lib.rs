// src/lib.rs

pub mod admcheck;
pub mod admin_points;
pub mod altguard; // ← udostępniamy moduł AltGuard
pub mod ban;
pub mod chatguard;
pub mod config;
pub mod db;
pub mod discord;
pub mod fotosystem;
pub mod kick;
pub mod logging;
pub mod mdel;
pub mod mute;
pub mod new_channels;
pub mod permissions;
pub mod registry; // ← rejestr ról/kanałów PROD/DEV
pub mod stats_channels;
pub mod userinfo;
pub mod warn;
mod welcome;
pub mod levels;

// opcjonalny skrót: use crate::env_roles;
pub use crate::registry::env_roles;
pub mod commands_sync;
pub mod idguard;
pub mod verify;
pub mod command_acl;

use anyhow::Result;
use once_cell::sync::OnceCell;
use std::sync::Arc;

use config::Settings;
use db::Db;

// (opcjonalnie) gotowiec z właściwymi intents do użycia w discord::run_bot
use serenity::all::GatewayIntents;

/// Globalny kontekst aplikacji.
/// Tu trzymamy uchwyt do DB, konfigurację i… gotowe serwisy (AltGuard, IdGuard).
#[derive(Clone)]
pub struct AppContext {
    pub settings: Settings,
    pub db: Db,
    altguard: OnceCell<Arc<altguard::AltGuard>>,
    idguard: OnceCell<Arc<idguard::IdGuard>>,
}

impl AppContext {
    /// Bootstrap całej aplikacji:
    /// - logi
    /// - połączenie z DB + migracje
    /// - stworzenie i wstrzyknięcie AltGuard oraz IdGuard do OnceCell
    pub async fn bootstrap(settings: Settings) -> Result<Arc<Self>> {
        // 1) logi
        logging::init(&settings);

        // 2) DB
        let db = db::connect(&settings.database.url, settings.database.max_connections).await?;
        db::migrate(&db).await?;

        // 3) kontekst (na razie z pustymi OnceCell)
        let ctx = Arc::new(Self {
            settings,
            db,
            altguard: OnceCell::new(),
            idguard: OnceCell::new(),
        });

        // 4) AltGuard
        let ag = altguard::AltGuard::new(ctx.clone());
        let _ = ctx.altguard.set(ag); // set() można wołać tylko raz

        // 5) IdGuard
        let idg = idguard::IdGuard::new(ctx.clone());
        let _ = ctx.idguard.set(idg);

        Ok(ctx)
    }

    /// Wygodny getter: daj mi AltGuarda (Arc).
    pub fn altguard(&self) -> Arc<altguard::AltGuard> {
        self.altguard
            .get()
            .expect("AltGuard not initialized")
            .clone()
    }

    /// Wygodny getter: daj mi IdGuarda (Arc).
    pub fn idguard(&self) -> Arc<idguard::IdGuard> {
        self.idguard.get().expect("IdGuard not initialized").clone()
    }

    /// Środowisko: "production" | "development".
    /// Czytamy z ENV `TSS_ENV`; brak → "development".
    #[inline]
    pub fn env(&self) -> String {
        std::env::var("TSS_ENV").unwrap_or_else(|_| "development".to_string())
    }
}

/// Gotowy zestaw intents do użycia w kliencie Discord:
/// - GUILDS, GUILD_MESSAGES, MESSAGE_CONTENT (konieczne do filtrowania treści),
/// - GUILD_MEMBERS (role – potrzebne do sprawdzania staffu).
pub fn default_gateway_intents() -> GatewayIntents {
    GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILD_MEMBERS
}

/// Start klienta Discorda (Gateway + slash commands).
pub async fn run(ctx: Arc<AppContext>) -> Result<()> {
    // PRZYKŁAD: gdy znasz guild_id (np. w handlerze GUILD_CREATE):
    // let ag = ctx.altguard();
    // ag.warmup_cache(GUILD_ID).await;

    // Uwaga: w discord::run_bot użyj default_gateway_intents()
    // przy tworzeniu Clienta.
    discord::run_bot(ctx).await
}
