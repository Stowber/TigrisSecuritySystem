// src/lib.rs

pub mod admcheck;
pub mod admin_points;
pub mod altguard; // ← udostępniamy moduł AltGuard
pub mod ban;
pub mod antinuke;
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
pub mod watchlist;
pub mod techlog;

// opcjonalny skrót: use crate::env_roles;
pub use crate::registry::env_roles;
pub mod commands_sync;
pub mod idguard;
pub mod verify;
pub mod command_acl;

use anyhow::Result;
use once_cell::sync::OnceCell;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use config::Settings;
use db::Db;
use permissions::Role;

// (opcjonalnie) gotowiec z właściwymi intents do użycia w discord::run_bot
use serenity::all::GatewayIntents;

/// Globalny kontekst aplikacji.
/// Tu trzymamy uchwyt do DB, konfigurację i… gotowe serwisy (AltGuard, IdGuard).
#[derive(Debug, Clone)]
pub struct AppContext {
    pub settings: Settings,
    pub db: Db,
    altguard: OnceCell<Arc<altguard::AltGuard>>,
    idguard: OnceCell<Arc<idguard::IdGuard>>,
    antinuke: OnceCell<Arc<antinuke::Antinuke>>,
    pub user_roles: Arc<Mutex<HashMap<u64, Vec<Role>>>>,
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
            antinuke: OnceCell::new(),
            user_roles: Arc::new(Mutex::new(HashMap::new())),
        });

        // 4) AltGuard
        let ag = altguard::AltGuard::new(ctx.clone());
        let _ = ctx.altguard.set(ag); // set() można wołać tylko raz
        chatguard::init(&ctx.settings.chatguard)?;

        // 5) IdGuard
        let idg = idguard::IdGuard::new(ctx.clone());
        let _ = ctx.idguard.set(idg);

        // 6) Antinuke service
        let an = antinuke::Antinuke::new(ctx.clone());
        let _ = ctx.antinuke.set(an.clone());

        // spawn simple HTTP API
        let api_port = ctx.settings.antinuke.api_port.unwrap_or(50055);
        let api_service = an.clone();
        tokio::spawn(async move {
             let addr = ([0, 0, 0, 0], api_port).into();
            if let Err(e) = antinuke::api::serve(addr, api_service).await {
                if let Some(io) = e.downcast_ref::<std::io::Error>() {
                    if io.kind() == std::io::ErrorKind::AddrInUse {
                        tracing::error!("Antinuke API port {api_port} already in use");
                        return;
                    }
                }
                tracing::error!(error=?e, "Antinuke API server failed");
            }
        });

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

     /// Getter for Antinuke service
    pub fn antinuke(&self) -> Arc<antinuke::Antinuke> {
        self.antinuke
            .get()
            .expect("Antinuke not initialized")
            .clone()
    }

    /// Środowisko: "production" | "development".
    /// Czytamy z ENV `TSS_ENV`; brak → "development".
    #[inline]
    pub fn env(&self) -> String {
        std::env::var("TSS_ENV").unwrap_or_else(|_| "development".to_string())
    }
}
impl AppContext {
/// Tworzy uproszczony kontekst do testów bez połączeń zewnętrznych.
    pub fn new_testing(settings: Settings, db: Db) -> Arc<Self> {
        Arc::new(Self {
            settings,
            db,
            altguard: OnceCell::new(),
            idguard: OnceCell::new(),
            antinuke: OnceCell::new(),
            user_roles: Arc::new(Mutex::new(HashMap::new())),
        })
    }


/// Initialize Antinuke service in tests without unsafe code.
    pub fn with_antinuke(self: &Arc<Self>) -> Arc<antinuke::Antinuke> {
        let an = antinuke::Antinuke::new(self.clone());
        let _ = self.antinuke.set(an.clone());
        an
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
        | GatewayIntents::GUILD_VOICE_STATES
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
