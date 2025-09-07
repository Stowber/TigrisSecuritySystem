use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use dashmap::DashMap;
use once_cell::sync::Lazy;
use serenity::all::*;
use tokio::task::JoinHandle;

use crate::AppContext;
use crate::admcheck::has_permission;
use crate::altguard::{
    AltGuard, TEST_MAX_IMAGE_BYTES, TestMessageFP, test_fetch_and_ahash_inner,
    test_is_trusted_discord_cdn, test_weight_behavior_pattern,
};
use crate::config::{App, ChatGuardConfig, Database, Discord, Logging, Settings};
use crate::idguard::{IdgConfig, IdgThresholds, IdgWeights, RuleKind, parse_pattern, sanitize_cfg};

pub struct TestCmd;

static TASKS: Lazy<DashMap<String, JoinHandle<()>>> = Lazy::new(|| DashMap::new());

impl TestCmd {
    pub async fn register_commands(ctx: &Context, guild_id: GuildId) -> Result<()> {
        guild_id
            .create_command(
                &ctx.http,
                CreateCommand::new("test")
                    .description("Komendy testowe")
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::SubCommandGroup,
                            "altguard",
                            "Testy AltGuard",
                        )
                        .add_sub_option(CreateCommandOption::new(
                            CommandOptionType::SubCommand,
                            "start",
                            "Start testów AltGuard",
                        ))
                        .add_sub_option(CreateCommandOption::new(
                            CommandOptionType::SubCommand,
                            "stop",
                            "Stop testów AltGuard",
                        )),
                    )
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::SubCommandGroup,
                            "idguard",
                            "Testy IdGuard",
                        )
                        .add_sub_option(CreateCommandOption::new(
                            CommandOptionType::SubCommand,
                            "start",
                            "Start testów IdGuard",
                        ))
                        .add_sub_option(CreateCommandOption::new(
                            CommandOptionType::SubCommand,
                            "stop",
                            "Stop testów IdGuard",
                        )),
                    ),
            )
            .await?;
        Ok(())
    }

    pub async fn on_interaction(ctx: &Context, _app: &AppContext, interaction: Interaction) {
        if let Some(cmd) = interaction.clone().command() {
            if cmd.data.name != "test" {
                return;
            }
            let Some(gid) = cmd.guild_id else {
                return;
            };
            if !has_permission(ctx, gid, cmd.user.id, crate::permissions::Permission::Test).await {
                let _ = cmd
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content("⛔ Brak uprawnień.")
                                .ephemeral(true),
                        ),
                    )
                    .await;
                return;
            }

            let Some(group) = cmd.data.options.first() else {
                return;
            };
            let Some(sub) = group.options.first() else {
                return;
            };
            let key = format!("{}:{}", gid.get(), group.name);
            match (group.name.as_str(), sub.name.as_str()) {
                ("altguard", "start") => {
                    if TASKS.contains_key(&key) {
                        let _ = cmd
                            .create_response(
                                &ctx.http,
                                CreateInteractionResponse::Message(
                                    CreateInteractionResponseMessage::new()
                                        .content("Test AltGuard już działa.")
                                        .ephemeral(true),
                                ),
                            )
                            .await;
                        return;
                    }
                    let _ = cmd
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new()
                                    .content("Startuję test AltGuard…")
                                    .ephemeral(true),
                            ),
                        )
                        .await;
                    let ctx2 = ctx.clone();
                    let channel = cmd.channel_id;
                    let key_clone = key.clone();
                    let handle = tokio::spawn(async move {
                        let res = run_altguard_tests(&ctx2, channel).await;
                        TASKS.remove(&key_clone);
                        match res {
                            Ok(_) => {
                                let _ = channel
                                    .send_message(
                                        &ctx2.http,
                                        CreateMessage::new().content("AltGuard: Ok"),
                                    )
                                    .await;
                            }
                            Err(e) => {
                                let _ = channel
                                    .send_message(
                                        &ctx2.http,
                                        CreateMessage::new()
                                            .content(format!("AltGuard: Fail ({e})")),
                                    )
                                    .await;
                            }
                        }
                    });
                    TASKS.insert(key, handle);
                }
                ("altguard", "stop") => {
                    if let Some((_, h)) = TASKS.remove(&key) {
                        h.abort();
                    }
                    let _ = cmd
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new()
                                    .content("AltGuard test zatrzymany.")
                                    .ephemeral(true),
                            ),
                        )
                        .await;
                }
                ("idguard", "start") => {
                    if TASKS.contains_key(&key) {
                        let _ = cmd
                            .create_response(
                                &ctx.http,
                                CreateInteractionResponse::Message(
                                    CreateInteractionResponseMessage::new()
                                        .content("Test IdGuard już działa.")
                                        .ephemeral(true),
                                ),
                            )
                            .await;
                        return;
                    }
                    let _ = cmd
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new()
                                    .content("Startuję test IdGuard…")
                                    .ephemeral(true),
                            ),
                        )
                        .await;
                    let ctx2 = ctx.clone();
                    let channel = cmd.channel_id;
                    let key_clone = key.clone();
                    let handle = tokio::spawn(async move {
                        let res = run_idguard_tests(&ctx2, channel).await;
                        TASKS.remove(&key_clone);
                        match res {
                            Ok(_) => {
                                let _ = channel
                                    .send_message(
                                        &ctx2.http,
                                        CreateMessage::new().content("IdGuard: Ok"),
                                    )
                                    .await;
                            }
                            Err(e) => {
                                let _ = channel
                                    .send_message(
                                        &ctx2.http,
                                        CreateMessage::new()
                                            .content(format!("IdGuard: Fail ({e})")),
                                    )
                                    .await;
                            }
                        }
                    });
                    TASKS.insert(key, handle);
                }
                ("idguard", "stop") => {
                    if let Some((_, h)) = TASKS.remove(&key) {
                        h.abort();
                    }
                    let _ = cmd
                        .create_response(
                            &ctx.http,
                            CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new()
                                    .content("IdGuard test zatrzymany.")
                                    .ephemeral(true),
                            ),
                        )
                        .await;
                }
                _ => {}
            }
        }
    }
}

fn confusable_variants(name: &str) -> Vec<String> {
    let mapping = [('e', 'е'), ('t', 'т'), ('s', 'ѕ'), ('u', 'υ')];
    let base: Vec<char> = name.chars().collect();
    let mut variants = Vec::new();
    for &(latin, conf) in &mapping {
        for (i, ch) in base.iter().enumerate() {
            if *ch == latin {
                let mut new_chars = base.clone();
                new_chars[i] = conf;
                variants.push(new_chars.iter().collect());
            }
        }
    }
    variants
}

async fn run_altguard_tests(ctx: &Context, channel: ChannelId) -> Result<()> {
    channel
        .send_message(
            &ctx.http,
            CreateMessage::new().content("AltGuard: confusable names"),
        )
        .await?;
    let ag = {
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
        };
        let db = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy(&settings.database.url)
            .unwrap();
        let ctx = AppContext::new_testing(settings, db);
        AltGuard::new(ctx)
    };
    ag.push_punished_name(1, "testuser").await;
    for variant in confusable_variants("testuser") {
        let weight = ag
            .test_similarity_to_punished(1, &[variant.clone()])
            .await?
            .unwrap_or(0);
        if weight <= 0 {
            return Err(anyhow!("{variant} not flagged"));
        }
    }
    channel
        .send_message(
            &ctx.http,
            CreateMessage::new().content("confusable names ok"),
        )
        .await?;

    channel
        .send_message(
            &ctx.http,
            CreateMessage::new().content("AltGuard: avatar url"),
        )
        .await?;
    if test_is_trusted_discord_cdn("http://evil.com/avatar.png")
        || !test_is_trusted_discord_cdn("https://cdn.discordapp.com/avatars/0.png")
    {
        return Err(anyhow!("avatar url check"));
    }
    channel
        .send_message(&ctx.http, CreateMessage::new().content("avatar url ok"))
        .await?;

    channel
        .send_message(
            &ctx.http,
            CreateMessage::new().content("AltGuard: large avatar"),
        )
        .await?;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let body = vec![0u8; TEST_MAX_IMAGE_BYTES + 1];
        let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
        let _ = socket.write_all(header.as_bytes()).await;
        let _ = socket.write_all(&body).await;
    });
    let url = format!("http://{}/big.png", addr);
    let hash = test_fetch_and_ahash_inner(&url).await?;
    server.await?;
    if hash.is_some() {
        return Err(anyhow!("large avatar not rejected"));
    }
    channel
        .send_message(&ctx.http, CreateMessage::new().content("large avatar ok"))
        .await?;

    channel
        .send_message(
            &ctx.http,
            CreateMessage::new().content("AltGuard: behavior pattern"),
        )
        .await?;
    let join_at = Instant::now();
    let mut msgs = Vec::new();
    for i in 0..5 {
        msgs.push(TestMessageFP {
            at: join_at + Duration::from_secs(60 + i),
            has_link: i == 0,
            mentions: 5,
            len: 3,
            sig: 1,
            repeated_special: true,
            entropy: 1.0,
        });
    }
    let weight = test_weight_behavior_pattern(&msgs, join_at, 15);
    if weight <= 0 {
        return Err(anyhow!("behavior pattern"));
    }
    channel
        .send_message(
            &ctx.http,
            CreateMessage::new().content("behavior pattern ok"),
        )
        .await?;
    Ok(())
}

async fn run_idguard_tests(ctx: &Context, channel: ChannelId) -> Result<()> {
    channel
        .send_message(
            &ctx.http,
            CreateMessage::new().content("IdGuard: parse_pattern"),
        )
        .await?;
    if parse_pattern("foo") != (RuleKind::Token, "foo".to_string())
        || parse_pattern("/foo") != (RuleKind::Token, "/foo".to_string())
        || parse_pattern("/foo/") != (RuleKind::Regex, "/foo/".to_string())
        || parse_pattern("/foo/i") != (RuleKind::Regex, "/foo/i".to_string())
    {
        return Err(anyhow!("parse_pattern"));
    }
    channel
        .send_message(&ctx.http, CreateMessage::new().content("parse_pattern ok"))
        .await?;

    channel
        .send_message(
            &ctx.http,
            CreateMessage::new().content("IdGuard: thresholds"),
        )
        .await?;
    let cfg = IdgConfig {
        thresholds: IdgThresholds {
            watch: 200,
            block: 0,
        },
        ..Default::default()
    };
    let cfg = sanitize_cfg(cfg);
    if cfg.thresholds.block != 1 || cfg.thresholds.watch != 0 {
        return Err(anyhow!("thresholds clamp"));
    }
    let cfg = IdgConfig {
        thresholds: IdgThresholds {
            watch: 250,
            block: 150,
        },
        ..Default::default()
    };
    let cfg = sanitize_cfg(cfg);
    if cfg.thresholds.block != 100 || cfg.thresholds.watch != 99 {
        return Err(anyhow!("thresholds clamp2"));
    }
    channel
        .send_message(&ctx.http, CreateMessage::new().content("thresholds ok"))
        .await?;

    channel
        .send_message(&ctx.http, CreateMessage::new().content("IdGuard: weights"))
        .await?;
    let cfg = IdgConfig {
        weights: IdgWeights {
            nick_token: -5,
            nick_regex: 150,
            avatar_hash: 50,
            avatar_ocr: 101,
            avatar_nsfw: -1,
        },
        ..Default::default()
    };
    let cfg = sanitize_cfg(cfg);
    if cfg.weights.nick_token != 0
        || cfg.weights.nick_regex != 100
        || cfg.weights.avatar_hash != 50
        || cfg.weights.avatar_ocr != 100
        || cfg.weights.avatar_nsfw != 0
    {
        return Err(anyhow!("weights clamp"));
    }
    channel
        .send_message(&ctx.http, CreateMessage::new().content("weights ok"))
        .await?;
    Ok(())
}