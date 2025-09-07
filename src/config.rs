use anyhow::Result;
use serde::{Deserialize, Serialize};
use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Settings {
    pub env: String,
    pub app: App,
    pub discord: Discord,
    pub database: Database,
    pub logging: Logging,
    pub chatguard: ChatGuardConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct App {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Discord {
    pub token: String,
    pub app_id: Option<String>,
    pub intents: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Database {
    pub url: String,
    pub max_connections: Option<u32>,
    pub statement_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Logging {
    pub json: Option<bool>,
    pub level: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ChatGuardConfig {
    #[serde(default)]
    pub racial_slurs: Vec<String>,
}

impl Settings {
    pub fn load() -> Result<Self> {
        // Które środowisko?
        let env = std::env::var("TSS_ENV").unwrap_or_else(|_| "development".to_string());

        // Załaduj .env.<env> i .env (jeśli są)
        let _ = dotenvy::from_filename(format!(".env.{}", env));
        let _ = dotenvy::dotenv();

        // Domyślne wartości
        #[derive(Deserialize, Serialize)]
        struct Defaults {
            env: String,
            app: App,
            discord: Discord,
            database: Database,
            logging: Logging,
            chatguard: ChatGuardConfig,
        }

        let defaults = Defaults {
            env: env.clone(),
            app: App {
                name: "Tigrissystem Security".into(),
            },
            discord: Discord {
                token: "".into(),
                app_id: None,
                intents: vec![
                    "GUILDS".into(),
                    "GUILD_MEMBERS".into(),
                    "GUILD_MESSAGES".into(),
                    "MESSAGE_CONTENT".into(),
                    "GUILD_MESSAGE_REACTIONS".into(),
                    "GUILD_VOICE_STATES".into(),
                ],
            },
            database: Database {
                url: "postgres://tss:tss@localhost:5432/tss".into(),
                max_connections: Some(10),
                statement_timeout_ms: Some(5_000),
            },
            logging: Logging {
                json: Some(false),
                level: Some("info".into()),
            },
            chatguard: ChatGuardConfig {
                racial_slurs: vec![],
            },
        };

        // Warstwy: domyślne -> plik TOML -> zmienne środowiskowe TSS_*
        let figment = Figment::from(Serialized::defaults(defaults))
            .merge(Toml::file(format!("config/{}.toml", env)))
            // TSS_DATABASE_URL => database.url itd.
            .merge(Env::prefixed("TSS_").split("_"));

        let mut s: Settings = figment.extract()?;
        s.env = env;

        // Uzupełnij brakujące domyślne
        if s.database.max_connections.is_none() {
            s.database.max_connections = Some(10);
        }

        Ok(s)
    }
}
