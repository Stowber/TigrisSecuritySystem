use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use once_cell::sync::Lazy;
use serenity::all::*;
use tokio::task::JoinHandle;
use sqlx::postgres::PgPoolOptions;

use crate::AppContext;
use crate::admcheck::has_permission;
use crate::altguard::{
    test_fetch_and_ahash_inner, test_is_trusted_discord_cdn, test_weight_behavior_pattern, AltGuard,
    TestMessageFP, TEST_MAX_IMAGE_BYTES,
};
use crate::config::{App, ChatGuardConfig, Database, Discord, Logging, Settings};
use crate::idguard::{parse_pattern, sanitize_cfg, IdgConfig, IdgThresholds, IdgWeights, RuleKind};

pub struct TestCmd;

static TASKS: Lazy<DashMap<String, JoinHandle<()>>> = Lazy::new(|| DashMap::new());

async fn respond_ephemeral(ctx: &Context, cmd: &CommandInteraction, msg: &str) {
    let _ = cmd
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(msg)
                    .ephemeral(true),
            ),
        )
        .await;
}

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
                respond_ephemeral(ctx, &cmd, "⛔ Brak uprawnień.").await;
                return;
            }

            let Some(group) = cmd.data.options.first() else {
                return;
            };

            // Odczyt subkomendy z grupy: value == SubCommandGroup(Vec<CommandDataOption>)
            let Some(sub) = (match &group.value {
                CommandDataOptionValue::SubCommandGroup(options) => options.first(),
                _ => None,
            }) else {
                return;
            };

            let key = format!("{}:{}", gid.get(), &group.name);
            match (group.name.as_str(), sub.name.as_str()) {
                ("altguard", "start") => {
                    if TASKS.contains_key(&key) {
                        respond_ephemeral(ctx, &cmd, "Test AltGuard już działa.").await;
                        return;
                    }
                    respond_ephemeral(ctx, &cmd, "Startuję test AltGuard…").await;
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
                    respond_ephemeral(ctx, &cmd, "AltGuard test zatrzymany.").await;
                }
                ("idguard", "start") => {
                    if TASKS.contains_key(&key) {
                        respond_ephemeral(ctx, &cmd, "Test IdGuard już działa.").await;
                        return;
                    }
                    respond_ephemeral(ctx, &cmd, "Startuję test IdGuard…").await;
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
                    respond_ephemeral(ctx, &cmd, "IdGuard test zatrzymany.").await;
                }
                _ => {}
            }
        }
    }
}

fn confusable_variants(name: &str) -> Vec<String> {
    let mapping = [
        ('e', 'е'),
        ('t', 'т'),
        ('s', 'ѕ'),
        ('u', 'υ'),
        ('r', 'г'),
        ('a', 'а'),
        ('o', 'о'),
        ('p', 'р'),
    ];
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
      let mut results: Vec<std::result::Result<(), String>> = Vec::new();

    channel
        .send_message(
            &ctx.http,
            CreateMessage::new().content("AltGuard: confusable names"),
        )
        .await?;
    let res = (|| async {
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
        let base = "testauserop";
        ag.push_punished_name(1, base).await;
        for variant in confusable_variants(base) {
            let weight = ag
                .test_similarity_to_punished(1, &[variant.clone()])
                .await?
                .unwrap_or(0);
            if weight <= 0 {
                return Err(anyhow!("{variant} not flagged"));
            }
        }
        Ok(())
    })()
    .await
    .map_err(|e| format!("confusable names: {e}"));
    if res.is_ok() {
        channel
            .send_message(
                &ctx.http,
                CreateMessage::new().content("confusable names ok"),
            )
            .await?;
    } else {
        channel
            .send_message(
                &ctx.http,
                CreateMessage::new().content(format!(
                    "confusable names failed: {}",
                    res.as_ref().unwrap_err()
                )),
            )
            .await?;
    }
        results.push(res);

    channel
        .send_message(
            &ctx.http,
            CreateMessage::new().content("AltGuard: avatar url"),
        )
        .await?;
    let res = (|| async {
        let bad_urls = [
            "http://evil.com/avatar.png",
            "https://cdn.discordapp.com.evil.com",
            "ftp://cdn.discordapp.com/avatar.png",
        ];
        if bad_urls.iter().any(|u| test_is_trusted_discord_cdn(u))
            || !test_is_trusted_discord_cdn("https://cdn.discordapp.com/avatars/0.png")
        {
            return Err(anyhow!("avatar url check"));
        }
        Ok(())
    })()
    .await
    .map_err(|e| format!("avatar url: {e}"));
    if res.is_ok() {
        channel
            .send_message(&ctx.http, CreateMessage::new().content("avatar url ok"))
            .await?;
    } else {
        channel
            .send_message(
                &ctx.http,
                CreateMessage::new()
                    .content(format!("avatar url failed: {}", res.as_ref().unwrap_err())),
            )
            .await?;
    }
    results.push(res);

    channel
        .send_message(
            &ctx.http,
            CreateMessage::new().content("AltGuard: large avatar"),
        )
        .await?;
    let res = (|| async {
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
        Ok(())
    })()
    .await
    .map_err(|e| format!("large avatar: {e}"));
    if res.is_ok() {
        channel
            .send_message(&ctx.http, CreateMessage::new().content("large avatar ok"))
            .await?;
    } else {
        channel
            .send_message(
                &ctx.http,
                CreateMessage::new().content(format!(
                    "large avatar failed: {}",
                    res.as_ref().unwrap_err()
                )),
            )
            .await?;
    }
     results.push(res);

    channel
        .send_message(
            &ctx.http,
            CreateMessage::new().content("AltGuard: behavior pattern"),
        )
        .await?;
    let res = (|| async {
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
        Ok(())
    })()
    .await
    .map_err(|e| format!("behavior pattern: {e}"));
    if res.is_ok() {
        channel
            .send_message(
                &ctx.http,
                CreateMessage::new().content("behavior pattern ok"),
            )
            .await?;
    } else {
        channel
            .send_message(
                &ctx.http,
                CreateMessage::new().content(format!(
                    "behavior pattern failed: {}",
                    res.as_ref().unwrap_err()
                )),
            )
            .await?;
    }
    results.push(res);

    let successes = results.iter().filter(|r| r.is_ok()).count();
    let failures = results.len() - successes;
    let errors: Vec<&String> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    let mut report = format!("AltGuard tests completed. Passed: {successes}, Failed: {failures}");
    if !errors.is_empty() {
        report.push_str("\nErrors:\n");
        report.push_str(&errors.join("\n"));
    }
    channel
         .send_message(&ctx.http, CreateMessage::new().content(report))
        .await?;
    if failures == 0 {
        Ok(())
    } else {
        Err(anyhow!("{failures} AltGuard tests failed"))
    }
}

async fn run_idguard_tests(ctx: &Context, channel: ChannelId) -> Result<()> {
    let mut results: Vec<std::result::Result<(), String>> = Vec::new();
    channel
        .send_message(
            &ctx.http,
            CreateMessage::new().content("IdGuard: parse_pattern"),
        )
        .await?;
    let res = (|| async {
        if parse_pattern("foo") != (RuleKind::Token, "foo".to_string())
            || parse_pattern("/foo") != (RuleKind::Token, "/foo".to_string())
            || parse_pattern("/foo/") != (RuleKind::Regex, "/foo/".to_string())
            || parse_pattern("/foo/i") != (RuleKind::Regex, "/foo/i".to_string())
        {
            return Err(anyhow!("parse_pattern"));
        }
        Ok(())
    })()
    .await
    .map_err(|e| format!("parse_pattern: {e}"));
    if res.is_ok() {
        channel
            .send_message(&ctx.http, CreateMessage::new().content("parse_pattern ok"))
            .await?;
    } else {
        channel
            .send_message(
                &ctx.http,
                CreateMessage::new().content(format!(
                    "parse_pattern failed: {}",
                    res.as_ref().unwrap_err()
                )),
            )
            .await?;
    }
     results.push(res);

    channel
        .send_message(
            &ctx.http,
            CreateMessage::new().content("IdGuard: thresholds"),
        )
        .await?;
    let res = (|| async {
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
        let cfg = IdgConfig {
            thresholds: IdgThresholds {
                watch: 90,
                block: 50,
            },
            ..Default::default()
        };
        let cfg = sanitize_cfg(cfg);
        if cfg.thresholds.block != 50 || cfg.thresholds.watch != 49 {
            return Err(anyhow!("thresholds clamp3"));
        }
        Ok(())
    })()
    .await
    .map_err(|e| format!("thresholds: {e}"));
    if res.is_ok() {
        channel
            .send_message(&ctx.http, CreateMessage::new().content("thresholds ok"))
            .await?;
    } else {
        channel
            .send_message(
                &ctx.http,
                CreateMessage::new()
                    .content(format!("thresholds failed: {}", res.as_ref().unwrap_err())),
            )
            .await?;
    }
    results.push(res);

    channel
        .send_message(&ctx.http, CreateMessage::new().content("IdGuard: weights"))
        .await?;
    let res = (|| async {
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
        Ok(())
    })()
    .await
    .map_err(|e| format!("weights: {e}"));
    if res.is_ok() {
        channel
            .send_message(&ctx.http, CreateMessage::new().content("weights ok"))
            .await?;
    } else {
        channel
            .send_message(
                &ctx.http,
                CreateMessage::new()
                    .content(format!("weights failed: {}", res.as_ref().unwrap_err())),
            )
            .await?;
    }
    results.push(res);

    channel
        .send_message(
            &ctx.http,
            CreateMessage::new().content("IdGuard: weights negative"),
        )
        .await?;
    let res = (|| async {
        let cfg = IdgConfig {
            weights: IdgWeights {
                nick_token: -10,
                nick_regex: -20,
                avatar_hash: -30,
                avatar_ocr: -40,
                avatar_nsfw: -50,
            },
            ..Default::default()
        };
        let cfg = sanitize_cfg(cfg);
        if cfg.weights.nick_token != 0
            || cfg.weights.nick_regex != 0
            || cfg.weights.avatar_hash != 0
            || cfg.weights.avatar_ocr != 0
            || cfg.weights.avatar_nsfw != 0
        {
            return Err(anyhow!("weights clamp neg"));
        }
        Ok(())
    })()
    .await
    .map_err(|e| format!("weights negative: {e}"));
    if res.is_ok() {
        channel
            .send_message(
                &ctx.http,
                CreateMessage::new().content("weights negative ok"),
            )
            .await?;
    } else {
        channel
            .send_message(
                &ctx.http,
                CreateMessage::new().content(format!(
                    "weights negative failed: {}",
                    res.as_ref().unwrap_err()
                )),
            )
            .await?;
    }
    results.push(res);

    channel
        .send_message(
            &ctx.http,
            CreateMessage::new().content("IdGuard: weights high"),
        )
        .await?;
    let res = (|| async {
        let cfg = IdgConfig {
            weights: IdgWeights {
                nick_token: 150,
                nick_regex: 200,
                avatar_hash: 250,
                avatar_ocr: 101,
                avatar_nsfw: 1000,
            },
            ..Default::default()
        };
        let cfg = sanitize_cfg(cfg);
        if cfg.weights.nick_token != 100
            || cfg.weights.nick_regex != 100
            || cfg.weights.avatar_hash != 100
            || cfg.weights.avatar_ocr != 100
            || cfg.weights.avatar_nsfw != 100
        {
            return Err(anyhow!("weights clamp high"));
        }
        Ok(())
    })()
    .await
    .map_err(|e| format!("weights high: {e}"));
    if res.is_ok() {
        channel
            .send_message(&ctx.http, CreateMessage::new().content("weights high ok"))
            .await?;
    } else {
        channel
            .send_message(
                &ctx.http,
                CreateMessage::new().content(format!(
                    "weights high failed: {}",
                    res.as_ref().unwrap_err()
                )),
            )
            .await?;
    }
    results.push(res);

    let successes = results.iter().filter(|r| r.is_ok()).count();
    let failures = results.len() - successes;
    let errors: Vec<&String> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    let mut report = format!("IdGuard tests completed. Passed: {successes}, Failed: {failures}");
    if !errors.is_empty() {
        report.push_str("\nErrors:\n");
        report.push_str(&errors.join("\n"));
    }
    channel
        .send_message(&ctx.http, CreateMessage::new().content(report))
        .await?;
    if failures == 0 {
        Ok(())
    } else {
        Err(anyhow!("{failures} IdGuard tests failed"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn stop_removes_key_from_tasks() {
        
        let key = "1:altguard".to_string();
        let handle = tokio::spawn(async {
            // Never resolves unless aborted.
            std::future::pending::<()>().await;
        });
        TASKS.insert(key.clone(), handle);

        // Simulate stop command.
        if let Some((_, h)) = TASKS.remove(&key) {
            h.abort();
        }

        assert!(!TASKS.contains_key(&key));
    }
}
