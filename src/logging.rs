use crate::config::Settings;
use tracing_subscriber::{fmt, EnvFilter};
use tracing_subscriber::prelude::*;

/// Inicjalizacja logowania.
/// Uwaga: bez dodatkowych feature’ów używamy formatu tekstowego.
/// (Jeśli chcesz JSON, daj znać — dołożę wariant z featurem.)
pub fn init(settings: &Settings) {
    let level = settings
        .logging
        .level
        .clone()
        .unwrap_or_else(|| "info".to_string());

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    // Prosty formatter (tekst). Unikamy .json(), żeby nie wymagać extra feature w Cargo.toml
    let fmt_layer = fmt::layer();

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .init();
}
