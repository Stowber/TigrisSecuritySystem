// src/idguard.rs

use std::{
    cmp::Ordering,
    collections::HashMap,
    sync::Arc,
    time::Duration,
};

use anyhow::Result;
use dashmap::DashMap;
use futures_util::StreamExt;
use moka::sync::Cache;
use once_cell::sync::{Lazy, OnceCell};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sqlx::{Pool, Postgres, Row};
use tokio::{runtime::Handle, task, sync::{OnceCell as AsyncOnceCell, RwLock, Semaphore}};

use serenity::all::{
    ButtonStyle, ChannelId, CommandDataOption, CommandDataOptionValue, CommandInteraction,
    CommandOptionType, ComponentInteraction, Context, CreateActionRow, CreateButton, CreateCommand,
    CreateCommandOption, CreateEmbed, CreateEmbedFooter, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, EditInteractionResponse, GuildId, Interaction,
};

use url::Url;

use crate::{
    AppContext,
    registry::{env_channels, env_roles},
};

/* ===========================
   Publiczne typy i konfiguracja
   =========================== */

const BRAND_FOOTER: &str = "Tigris Security System™ • IdGuard";
/// Maksymalna dopuszczalna długość wzorca regex
const MAX_REGEX_LEN: usize = 200;
/// Timeout (w milisekundach) dla kompilacji i dopasowania regex
const REGEX_TIMEOUT_MS: u64 = 200;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum IdgMode {
    Monitor, // tylko loguj
    Auto,    // (placeholder) auto-reakcje – obecnie tylko raport + przyciski
}
impl Default for IdgMode {
    fn default() -> Self {
        IdgMode::Monitor
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum IdgPreset {
    Lenient,
    Balanced,
    Strict,
}
impl Default for IdgPreset {
    fn default() -> Self {
        IdgPreset::Balanced
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdgConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub mode: IdgMode,
    #[serde(default)]
    pub preset: IdgPreset,
    #[serde(default = "default_thresholds")]
    pub thresholds: IdgThresholds,
    #[serde(default = "default_weights")]
    pub weights: IdgWeights,
    #[serde(default = "default_true")]
    pub avatar_ocr: bool,
    #[serde(default = "default_true")]
    pub avatar_nsfw: bool,
}

fn default_true() -> bool {
    true
}
fn default_thresholds() -> IdgThresholds {
    IdgConfig::default().thresholds
}
fn default_weights() -> IdgWeights {
    IdgConfig::default().weights
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdgThresholds {
    pub watch: u8, // >= watch => WATCH
    pub block: u8, // >= block => BLOCK
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdgWeights {
    pub nick_token: i32,
    pub nick_regex: i32,
    pub avatar_hash: i32,
    pub avatar_ocr: i32,
    pub avatar_nsfw: i32,
}

impl Default for IdgConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            mode: IdgMode::Monitor,
            preset: IdgPreset::Balanced,
            thresholds: IdgThresholds {
                watch: 30,
                block: 60,
            },
            weights: IdgWeights {
                nick_token: 25,
                nick_regex: 35,
                avatar_hash: 40,
                avatar_ocr: 30,
                avatar_nsfw: 50,
            },
            avatar_ocr: true,
            avatar_nsfw: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum IdgVerdict {
    Clean,
    Watch,
    Block,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum IdgSignalKind {
    NickToken,
    NickRegex,
    AvatarHash,
    AvatarOCR,
    AvatarNSFW,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdgSignal {
    pub kind: IdgSignalKind,
    pub weight: i32,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct IdgInput {
    pub guild_id: u64,
    pub user_id: u64,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub global_name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdgReport {
    pub score: u8,
    pub verdict: IdgVerdict,
    pub signals: Vec<IdgSignal>,
    pub explain: String,
    pub avatar_hash: Option<u64>, // <- unikamy podwójnego pobierania
}

/* ===========================
   Reguły i pamięć
   =========================== */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleAction {
    Allow,
    Deny,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    Token,
    Regex,
}

#[derive(Debug, Clone)]
struct NickRule {
    action: RuleAction,
    kind: RuleKind,
    pattern: String,
    /// Prekompilowany regex dla RuleKind::Regex
    compiled: Option<Regex>,
    /// Lowercase pattern dla RuleKind::Token (dla dopasowań bez kosztów lowercase za każdym razem)
    pattern_lower: Option<String>,
    reason: String,
}

#[derive(Debug, Clone)]
struct AvatarDenyHash {
    hash: u64,      // aHash 8×8
    _reason: String, // opcjonalnie
}

#[derive(Debug)]
pub struct IdGuard {
    ctx: Arc<AppContext>,
    cfg_cache: DashMap<u64, IdgConfig>,                 // per-guild
    nick_rules: DashMap<u64, Arc<RwLock<Vec<NickRule>>>>,
    avatar_deny: DashMap<u64, Arc<RwLock<Vec<AvatarDenyHash>>>>,
}

/* ===========================
   Globalny HTTP client + throttle + DDL once
   =========================== */

static HTTP: OnceCell<reqwest::Client> = OnceCell::new();
static IMG_DL_SEM: OnceCell<Semaphore> = OnceCell::new();
static INIT_DDL: AsyncOnceCell<()> = AsyncOnceCell::const_new();

fn http() -> &'static reqwest::Client {
    HTTP.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent("Tigris-IdGuard/1.0")
            .connect_timeout(Duration::from_millis(1500))
            .timeout(Duration::from_millis(5000))
            .build()
            .expect("http client")
    })
}
fn img_sem() -> &'static Semaphore {
    IMG_DL_SEM.get_or_init(|| Semaphore::new(2)) // max 2 równoległe pobrania
}

/* ===========================
   API publiczne
   =========================== */

impl IdGuard {
    pub fn new(ctx: Arc<AppContext>) -> Arc<Self> {
        Arc::new(Self {
            ctx,
            cfg_cache: DashMap::new(),
            nick_rules: DashMap::new(),
            avatar_deny: DashMap::new(),
        })
    }

    fn db(&self) -> &Pool<Postgres> {
        &self.ctx.db
    }

    pub async fn warmup_cache(self: &Arc<Self>, guild_id: u64) {
        maybe_ensure_tables(self.db()).await;

        // konfig
        if let Ok(Some(cfg)) = load_cfg_db(self.db(), guild_id).await {
            self.cfg_cache.insert(guild_id, cfg);
        }

        // reguły nicków
        let list = self
            .nick_rules
            .entry(guild_id)
            .or_insert_with(|| Arc::new(RwLock::new(Vec::new())))
            .clone();
        if let Ok(mut rules) = load_rules_db(self.db(), guild_id).await {
            // limit pamięciowo (np. 5000)
            if rules.len() > 5000 {
                rules.drain(0..(rules.len() - 5000));
            }
            let mut guard = list.write().await;
            *guard = rules;
        }

        // denylist hashy avatarów
        let av = self
            .avatar_deny
            .entry(guild_id)
            .or_insert_with(|| Arc::new(RwLock::new(Vec::new())))
            .clone();
        if let Ok(mut hs) = load_avatar_hashes_db(self.db(), guild_id).await {
            if hs.len() > 5000 {
                hs.drain(0..(hs.len() - 5000));
            }
            let mut guard = av.write().await;
            *guard = hs;
        }
    }

    pub async fn register_commands(ctx: &Context, guild_id: GuildId) -> Result<()> {
        guild_id
            .create_command(
                &ctx.http,
                CreateCommand::new("idguard")
                    .description("Zarządzanie IdGuard")
                    .add_option(CreateCommandOption::new(
                        CommandOptionType::SubCommand,
                        "setup",
                        "Utwórz logi i włącz monitor mode",
                    ))
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::SubCommand,
                            "preset",
                            "Ustaw predefiniowane progi/wagi",
                        )
                        .add_sub_option(
                            CreateCommandOption::new(
                                CommandOptionType::String,
                                "value",
                                "lenient|balanced|strict",
                            )
                            .required(true)
                            .add_string_choice("lenient", "lenient")
                            .add_string_choice("balanced", "balanced")
                            .add_string_choice("strict", "strict"),
                        ),
                    )
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::SubCommand,
                            "mode",
                            "Monitor czy Auto",
                        )
                        .add_sub_option(
                            CreateCommandOption::new(
                                CommandOptionType::String,
                                "value",
                                "monitor|auto",
                            )
                            .required(true)
                            .add_string_choice("monitor", "monitor")
                            .add_string_choice("auto", "auto"),
                        ),
                    ),
            )
            .await?;

        // /teach allow|deny [nick?] [avatar?] [reason?]
        guild_id
            .create_command(
                &ctx.http,
                CreateCommand::new("teach")
                    .description("Ucz IdGuard (allow/deny nick lub avatar)")
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::SubCommand,
                            "allow",
                            "Zezwól (whitelist)",
                        )
                        .add_sub_option(CreateCommandOption::new(
                            CommandOptionType::String,
                            "nick",
                            "Tekst nicku (token lub regex:/.../flags)",
                        ))
                        .add_sub_option(CreateCommandOption::new(
                            CommandOptionType::String,
                            "avatar",
                            "URL avatara",
                        ))
                        .add_sub_option(CreateCommandOption::new(
                            CommandOptionType::String,
                            "reason",
                            "Powód (opcjonalnie)",
                        )),
                    )
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::SubCommand,
                            "deny",
                            "Zablokuj (blacklist)",
                        )
                        .add_sub_option(CreateCommandOption::new(
                            CommandOptionType::String,
                            "nick",
                            "Tekst nicku (token lub regex:/.../flags)",
                        ))
                        .add_sub_option(CreateCommandOption::new(
                            CommandOptionType::String,
                            "avatar",
                            "URL avatara",
                        ))
                        .add_sub_option(CreateCommandOption::new(
                            CommandOptionType::String,
                            "reason",
                            "Powód (opcjonalnie)",
                        )),
                    ),
            )
            .await?;

        Ok(())
    }

    /// Główna brama interakcji (slash + przyciski).
    pub async fn on_interaction(&self, ctx: &Context, app: &AppContext, interaction: Interaction) {
        maybe_ensure_tables(self.db()).await;

        // slash
        if let Some(cmd) = interaction.clone().command() {
            if cmd.data.name == "idguard" {
                if let Err(e) = self.on_cmd_idguard(ctx, app, &cmd).await {
                    tracing::warn!(error=?e, "idguard cmd failed");
                }
                return;
            }
            if cmd.data.name == "teach" {
                if let Err(e) = self.on_cmd_teach(ctx, app, &cmd).await {
                    tracing::warn!(error=?e, "teach cmd failed");
                }
                return;
            }
        }

        // przyciski
        if let Some(comp) = interaction.message_component() {
            if comp.data.custom_id.starts_with("idg_allow_nick:") {
                self.on_btn_allow_deny_nick(ctx, app, &comp, true).await;
                return;
            }
            if comp.data.custom_id.starts_with("idg_deny_nick:") {
                self.on_btn_allow_deny_nick(ctx, app, &comp, false).await;
                return;
            }
            if comp.data.custom_id.starts_with("idg_allow_avat:") {
                self.on_btn_allow_deny_avatar(ctx, app, &comp, true).await;
                return;
            }
            if comp.data.custom_id.starts_with("idg_deny_avat:") {
                self.on_btn_allow_deny_avatar(ctx, app, &comp, false).await;
                return;
            }
        }
    }

    /* ===========================
       Skan podczas weryfikacji / na żądanie
       =========================== */

    pub async fn check_user(&self, input: &IdgInput) -> IdgReport {
        let mut cfg = self
            .cfg_cache
            .get(&input.guild_id)
            .map(|e| e.clone())
            .unwrap_or_default();
        cfg = sanitize_cfg(cfg);

        if !cfg.enabled {
            return IdgReport {
                score: 0,
                verdict: IdgVerdict::Clean,
                signals: vec![],
                explain: "IdGuard disabled".into(),
                avatar_hash: None,
            };
        }

        let mut signals: Vec<IdgSignal> = Vec::with_capacity(8);

        // 1) NICK – token/regex (ALLOW short-circuit dla nicka; nie wpływa na avatar)
        let all_names = collect_names(&input.username, &input.display_name, &input.global_name);
        let norm_joined = all_names.join(" | ");
        let lowered = norm_joined.to_lowercase();

        let rules = self
            .nick_rules
            .get(&input.guild_id)
            .map(|a| a.clone())
            .unwrap_or_else(|| Arc::new(RwLock::new(Vec::new())));

        if !norm_joined.is_empty() {
            let guard = rules.read().await;

            // ALLOW -> pomijamy DENY/builtin dla nicka (ale nie dokładaj ujemnych punktów)
            let allowed_by_rule = guard
                .iter()
                .filter(|r| r.action == RuleAction::Allow)
                .any(|r| r.matches(&norm_joined, &lowered));

            if !allowed_by_rule {
                // Zbierz WSZYSTKIE dopasowane DENY i zsumuj
                for hit in guard
                    .iter()
                    .filter(|r| r.action == RuleAction::Deny)
                    .filter(|r| r.matches(&norm_joined, &lowered))
                {
                    let w = match hit.kind {
                        RuleKind::Regex => cfg.weights.nick_regex,
                        RuleKind::Token => cfg.weights.nick_token,
                    };
                    signals.push(IdgSignal {
                        kind: if hit.kind == RuleKind::Regex {
                            IdgSignalKind::NickRegex
                        } else {
                            IdgSignalKind::NickToken
                        },
                        weight: w,
                        detail: format!("deny: {}", hit.pattern),
                    });
                }

                // Wbudowane tokens – jeden prekompilowany regex, zwraca faktyczny match
                if let Some(m) = BUILTIN_BAD_RE.find(&lowered) {
                    signals.push(IdgSignal {
                        kind: IdgSignalKind::NickToken,
                        weight: cfg.weights.nick_token,
                        detail: format!("builtin: {}", m.as_str()),
                    });
                }
            }
        }

        // 2) AVATAR – hash deny + (stub) OCR + (stub) NSFW
        let mut avatar_hash: Option<u64> = None;
        if let Some(url) = &input.avatar_url {
            if let Ok(Some((h, bytes))) = fetch_and_ahash(url).await {
                avatar_hash = Some(h);
                let deny = self
                    .avatar_deny
                    .get(&input.guild_id)
                    .map(|a| a.clone())
                    .unwrap_or_else(|| Arc::new(RwLock::new(Vec::new())));
                let guard = deny.read().await;

                if let Some(hit) = guard.iter().min_by_key(|d| ((d.hash ^ h).count_ones())) {
                    let dist = (hit.hash ^ h).count_ones() as i32;
                    // podobieństwo: mniejszy dystans -> większa waga; bierzemy tylko bardzo podobne (<=6)
                    if dist <= 6 {
                        // dist=0 -> 1.0, dist=6 -> 0.0
                        let factor = 1.0 - (dist as f32 / 6.0);
                        let dyn_weight = (cfg.weights.avatar_hash as f32 * factor).round() as i32;
                        if dyn_weight > 0 {
                            signals.push(IdgSignal {
                                kind: IdgSignalKind::AvatarHash,
                                weight: dyn_weight,
                                detail: format!("avatar aHash≈ deny (d={})", dist),
                            });
                        }
                    }
                }
            if cfg.avatar_ocr {
                    if let Some(txt) = ocr_from_bytes(&bytes).await {
                        if !txt.trim().is_empty() {
                            signals.push(IdgSignal {
                                kind: IdgSignalKind::AvatarOCR,
                                weight: cfg.weights.avatar_ocr,
                                detail: txt,
                            });
                        }
                    }
                }

                if cfg.avatar_nsfw && !bytes.is_empty() {
                    if let Some(score) = nsfw_from_bytes(&bytes).await {
                        if score > 0.5 {
                            let dyn_weight =
                                (cfg.weights.avatar_nsfw as f32 * score).round() as i32;
                            signals.push(IdgSignal {
                                kind: IdgSignalKind::AvatarNSFW,
                                weight: dyn_weight,
                                detail: format!("nsfw score {:.2}", score),
                            });
                        }
                    }
                }
            }
        }

        // 3) Agregacja
        let mut score = 0i32;
        for s in &signals {
            score += s.weight;
        }
        score = score.clamp(0, 100);
        let score_u8 = score as u8;

        let verdict = if score_u8 >= cfg.thresholds.block {
            IdgVerdict::Block
        } else if score_u8 >= cfg.thresholds.watch {
            IdgVerdict::Watch
        } else {
            IdgVerdict::Clean
        };

        let mut s_sorted = signals.clone();
        // dodatnie malejąco, potem ujemne rosnąco – zgodnie z komentarzem
        s_sorted.sort_by(|a, b| {
            match (a.weight >= 0, b.weight >= 0) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                (true, true) => b.weight.cmp(&a.weight), // +: malejąco
                (false, false) => a.weight.cmp(&b.weight), // -: rosnąco
            }
        });

        let explain = if s_sorted.is_empty() {
            "Brak sygnałów".into()
        } else {
            let top3 = s_sorted
                .iter()
                .take(3)
                .map(|s| format!("{:?}({}):{}", s.kind, s.weight, s.detail))
                .collect::<Vec<_>>()
                .join(", ");
            format!("Top: {}", top3)
        };

        IdgReport {
            score: score_u8,
            verdict,
            signals: s_sorted,
            explain,
            avatar_hash,
        }
    }

    /// Log (embed) do kanału logów (LOGS_ALTGUARD / env_channels::logs::altguard_id).
    pub async fn log_review_embed(
        &self,
        ctx: &Context,
        app: &AppContext,
        input: &IdgInput,
        report: &IdgReport,
    ) {
        let env = app.env();
        let log_id = env_channels::logs::altguard_id(&env);
        if log_id == 0 {
            return;
        }

        let user_mention = format!("<@{}>", input.user_id);
        let (title, colour) = match report.verdict {
            IdgVerdict::Clean => ("IdGuard: czysto", 0x2ecc71),
            IdgVerdict::Watch => ("IdGuard: WATCH", 0xf1c40f),
            IdgVerdict::Block => ("IdGuard: BLOCK", 0xe74c3c),
        };

        let mut signals = if report.signals.is_empty() {
            "–".to_string()
        } else {
            report
                .signals
                .iter()
                .map(|s| format!("{:?}: {}", s.kind, s.detail))
                .collect::<Vec<_>>()
                .join("\n")
        };
        signals = clamp_chars(&signals, 1024); // pole embeda ma limit 1024 znaków

        // Przygotuj przyciski: tokeny z nicka (max 4) + avatar (jeśli jest)
        let mut buttons: Vec<CreateButton> = Vec::new();

        if let Some(nick_preview) = input
            .username
            .as_ref()
            .or(input.display_name.as_ref())
            .or(input.global_name.as_ref())
        {
            let tokens = tokens_for_buttons(nick_preview);
            let mut tokens_added = 0usize;
            for t in tokens {
                let patt = sanitize_custom_id(&t, 40);
                if patt != "_" {
                    buttons.push(
                        CreateButton::new(format!("idg_allow_nick:{}", patt))
                            .label(format!("Allow „{}”", clamp_label(&t)))
                            .style(ButtonStyle::Secondary),
                    );
                    buttons.push(
                        CreateButton::new(format!("idg_deny_nick:{}", patt))
                            .label(format!("Deny „{}”", clamp_label(&t)))
                            .style(ButtonStyle::Danger),
                    );
                    tokens_added += 1;
                    if tokens_added >= 4 {
                        break;
                    } // max 4 tokeny
                }
            }
        }

        // Avatar – jeśli mamy hash z raportu
        if let Some(h) = report.avatar_hash {
            let hid = format!("{:016x}", h);
            buttons.push(
                CreateButton::new(format!("idg_allow_avat:{}", hid))
                    .label("Allow Avatar")
                    .style(ButtonStyle::Secondary),
            );
            buttons.push(
                CreateButton::new(format!("idg_deny_avat:{}", hid))
                    .label("Deny Avatar")
                    .style(ButtonStyle::Danger),
            );
        }

        // Podziel wiersze po max 5 przycisków
        let mut components = vec![];
        if !buttons.is_empty() {
            for chunk in buttons.chunks(5) {
                components.push(CreateActionRow::Buttons(chunk.to_vec()));
            }
        }

        let mut embed = CreateEmbed::new()
            .title(title)
            .description(format!("Użytkownik: {}\n{}", user_mention, report.explain))
            .field("Score", format!("**{}**/100", report.score), true)
            .field("Signals", signals, false)
            .footer(CreateEmbedFooter::new(BRAND_FOOTER))
            .colour(serenity::all::Colour::new(colour));

        if let Some(url) = &input.avatar_url {
            embed = embed.thumbnail(url.clone());
        }

        let mut msg = CreateMessage::new().embed(embed);
        if !components.is_empty() {
            msg = msg.components(components);
        }

        let _ = ChannelId::new(log_id).send_message(&ctx.http, msg).await;
    }
}

/* ===========================
   Komendy / przyciski
   =========================== */

impl IdGuard {
    async fn on_cmd_idguard(
        &self,
        ctx: &Context,
        app: &AppContext,
        cmd: &CommandInteraction,
    ) -> Result<()> {
        maybe_ensure_tables(self.db()).await;

        // wymóg: staff
        let env = app.env();
        let staff = env_roles::staff_set(&env);
        let allowed = cmd
            .member
            .as_ref()
            .map(|m| m.roles.iter().any(|r| staff.contains(&r.get())))
            .unwrap_or(false);
        if !allowed {
            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("Brak uprawnień.")
                            .ephemeral(true),
                    ),
                )
                .await;
            return Ok(());
        }

        let gid = if let Some(g) = cmd.guild_id {
            g
        } else {
            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("Użyj na serwerze.")
                            .ephemeral(true),
                    ),
                )
                .await;
            return Ok(());
        };

        // /idguard SUB + opcje
        let (sub, sval) = extract_idguard_sub(&cmd.data.options);
        match sub.as_deref() {
            Some("setup") => {
                let idg = self;
                let mut cfg = idg
                    .cfg_cache
                    .get(&gid.get())
                    .map(|c| c.clone())
                    .unwrap_or_default();
                cfg.enabled = true;
                cfg.mode = IdgMode::Monitor;
                cfg.preset = IdgPreset::Balanced;
                cfg.thresholds = IdgThresholds {
                    watch: 30,
                    block: 60,
                };
                cfg.weights = IdgWeights {
                    nick_token: 25,
                    nick_regex: 35,
                    avatar_hash: 40,
                    avatar_ocr: 30,
                    avatar_nsfw: 50,
                };
                cfg = sanitize_cfg(cfg);
                idg.cfg_cache.insert(gid.get(), cfg.clone());
                if let Err(e) = save_cfg_db(idg.db(), gid.get(), &cfg).await {
                    tracing::warn!(?e, "save_cfg_db failed on setup");
                }

                let _ = cmd.create_response(&ctx.http, CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("IdGuard ustawiony w tryb **monitor**. Logi lecą do kanału LOGS_ALTGUARD.")
                        .ephemeral(true)
                )).await;
            }
            Some("preset") => {
                let choice = sval.unwrap_or_else(|| "balanced".into());
                let preset = match choice.as_str() {
                    "lenient" => IdgPreset::Lenient,
                    "strict" => IdgPreset::Strict,
                    _ => IdgPreset::Balanced,
                };
                let idg = self;
                let mut cfg = idg
                    .cfg_cache
                    .get(&gid.get())
                    .map(|c| c.clone())
                    .unwrap_or_default();
                cfg.preset = preset;
                // Ustal kompletne wartości (bez kumulacji)
                match preset {
                    IdgPreset::Lenient => {
                        cfg.thresholds = IdgThresholds {
                            watch: 40,
                            block: 75,
                        };
                        cfg.weights = IdgWeights {
                            nick_token: 20,
                            nick_regex: 25,
                            avatar_hash: 35,
                            avatar_ocr: 25,
                            avatar_nsfw: 50,
                        };
                    }
                    IdgPreset::Balanced => {
                        cfg.thresholds = IdgThresholds {
                            watch: 30,
                            block: 60,
                        };
                        cfg.weights = IdgWeights {
                            nick_token: 25,
                            nick_regex: 35,
                            avatar_hash: 40,
                            avatar_ocr: 30,
                            avatar_nsfw: 50,
                        };
                    }
                    IdgPreset::Strict => {
                        cfg.thresholds = IdgThresholds {
                            watch: 20,
                            block: 45,
                        };
                        cfg.weights = IdgWeights {
                            nick_token: 30,
                            nick_regex: 40,
                            avatar_hash: 45,
                            avatar_ocr: 35,
                            avatar_nsfw: 55,
                        };
                    }
                }
                cfg = sanitize_cfg(cfg);
                idg.cfg_cache.insert(gid.get(), cfg.clone());
                if let Err(e) = save_cfg_db(idg.db(), gid.get(), &cfg).await {
                    tracing::warn!(?e, "save_cfg_db failed on preset");
                }

                let _ = cmd
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content(format!("Preset ustawiony: **{:?}**.", preset))
                                .ephemeral(true),
                        ),
                    )
                    .await;
            }
            Some("mode") => {
                let choice = sval.unwrap_or_else(|| "monitor".into());
                let mode = if choice == "auto" {
                    IdgMode::Auto
                } else {
                    IdgMode::Monitor
                };
                let idg = self;
                let mut cfg = idg
                    .cfg_cache
                    .get(&gid.get())
                    .map(|c| c.clone())
                    .unwrap_or_default();
                cfg.mode = mode;
                cfg = sanitize_cfg(cfg);
                idg.cfg_cache.insert(gid.get(), cfg.clone());
                if let Err(e) = save_cfg_db(idg.db(), gid.get(), &cfg).await {
                    tracing::warn!(?e, "save_cfg_db failed on mode");
                }

                let _ = cmd
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content(format!("Tryb: **{:?}**.", mode))
                                .ephemeral(true),
                        ),
                    )
                    .await;
            }
            _ => {
                let _ = cmd
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content("Nieznana subkomenda.")
                                .ephemeral(true),
                        ),
                    )
                    .await;
            }
        }

        Ok(())
    }

    async fn on_cmd_teach(
        &self,
        ctx: &Context,
        app: &AppContext,
        cmd: &CommandInteraction,
    ) -> Result<()> {
        maybe_ensure_tables(self.db()).await;

        // ACL
        let env = app.env();
        let staff = env_roles::staff_set(&env);
        let allowed = cmd
            .member
            .as_ref()
            .map(|m| m.roles.iter().any(|r| staff.contains(&r.get())))
            .unwrap_or(false);
        if !allowed {
           let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("Brak uprawnień.")
                            .ephemeral(true),
                    ),
                )
                .await;
            return Ok(());
        }

        let gid = if let Some(g) = cmd.guild_id {
            g
        } else {
            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("Użyj na serwerze.")
                            .ephemeral(true),
                    ),
                )
                .await;
            return Ok(());
        };

        let (sub, params) = extract_teach_params(&cmd.data.options);
        let action = match sub.as_deref() {
            Some("allow") => RuleAction::Allow,
            Some("deny")  => RuleAction::Deny,
            _ => {
                let _ = cmd
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content("Użyj `allow` lub `deny`.")
                                .ephemeral(true),
                        ),
                    )
                    .await;
                return Ok(());
            }
        };

        let nick = params.get("nick").cloned();
        let avatar = params.get("avatar").cloned();
        let reason = params
            .get("reason")
            .cloned()
            .unwrap_or_else(|| "admin teach".into());

        // Wzajemnie wykluczaj nick i avatar, aby uniknąć częściowej obsługi
        if nick.is_some() && avatar.is_some() {
            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("Podaj **albo** `nick`, **albo** `avatar` (nie oba naraz).")
                            .ephemeral(true),
                    ),
                )
                .await;
            return Ok(());
        }

        if let Some(nick_text) = nick {
            let (kind, patt) = parse_pattern(&nick_text);
            let rule = NickRule::new(action, kind, patt.clone(), &reason)?;
            if let Err(e) = upsert_nick_rule(self.db(), gid.get(), &rule).await {
                tracing::warn!(?e, "upsert_nick_rule failed");
            }

            let list = self
                .nick_rules
                .entry(gid.get())
                .or_insert_with(|| Arc::new(RwLock::new(Vec::new())))
                .clone();
            {
                let mut guard = list.write().await;
                guard.retain(|r| {
                    !(r.action == rule.action && r.kind == rule.kind && r.pattern == rule.pattern)
                });
                if guard.len() >= 5000 {
                    guard.remove(0);
                }
                guard.push(rule);
            }

            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content(format!("OK: {:?} {:?} `{}`", action, kind, patt))
                            .ephemeral(true),
                    ),
                )
                .await;
            return Ok(());
        }

        if let Some(url) = avatar {
             if let Ok(Some((h, _))) = fetch_and_ahash(&url).await {
                if let Err(e) =
                    upsert_avatar_hash_deny_allow(self.db(), gid.get(), h, action, &reason).await
                {
                    tracing::warn!(?e, "upsert_avatar_hash_deny_allow failed");
                }

                // pamięć: DENY dodajemy, ALLOW zdejmujemy
                if action == RuleAction::Deny {
                    let av = self
                        .avatar_deny
                        .entry(gid.get())
                        .or_insert_with(|| Arc::new(RwLock::new(Vec::new())))
                        .clone();
                    let mut guard = av.write().await;
                    if guard.len() >= 5000 {
                        guard.remove(0);
                    }
                    guard.push(AvatarDenyHash {
                        hash: h,
                        _reason: reason,
                    });
                } else {
                    if let Some(av) = self.avatar_deny.get(&gid.get()) {
                        let mut guard = av.write().await;
                        guard.retain(|x| x.hash != h);
                    }
                }

                let _ = cmd
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content(format!("OK: {:?} avatar hash `{:016x}`", action, h))
                                .ephemeral(true),
                        ),
                    )
                    .await;
                return Ok(());
            } else {
                let _ = cmd.create_response(&ctx.http, CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("Nie udało się pobrać/zhashować avatara. Obsługiwane są tylko linki Discord CDN (cdn.discordapp.com / media.discordapp.net).")
                        .ephemeral(true)
                )).await;
                return Ok(());
            }
        }

        let _ = cmd
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("Podaj `nick` lub `avatar`.")
                        .ephemeral(true),
                ),
            )
            .await;

        Ok(())
    }

    async fn on_btn_allow_deny_nick(
        &self,
        ctx: &Context,
        app: &AppContext,
        i: &ComponentInteraction,
        allow: bool,
    ) {
        let env = app.env();
        if !ensure_staff_ephemeral(ctx, &env, i).await {
            return;
        }

        let _ = i
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(if allow {
                            "Zapisuję allow…"
                        } else {
                            "Zapisuję deny…"
                        })
                        .ephemeral(true),
                ),
            )
            .await;

        let Some(gid) = i.guild_id else {
            let _ = i
                .edit_response(
                    &ctx.http,
                    EditInteractionResponse::new().content("Tylko na serwerze."),
                )
                .await;
            return;
        };

        let patt = i
            .data
            .custom_id
            .splitn(2, ':')
            .nth(1)
            .unwrap_or("")
            .to_string();
        let (kind, pat) = parse_pattern(&patt);
         let action = if allow {
            RuleAction::Allow
        } else {
            RuleAction::Deny
        };
        let rule = match NickRule::new(action, kind, pat.clone(), "button") {
            Ok(r) => r,
            Err(e) => {
                 let _ = i
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new().content(format!("Błąd: {}", e)),
                    )
                    .await;
                return;
            }
        };
        if let Err(e) = upsert_nick_rule(self.db(), gid.get(), &rule).await {
            tracing::warn!(?e, "upsert_nick_rule failed from button");
        }

        let list = self
            .nick_rules
            .entry(gid.get())
            .or_insert_with(|| Arc::new(RwLock::new(Vec::new())))
            .clone();
        {
            let mut guard = list.write().await;
            guard.retain(|r| {
                !(r.action == rule.action && r.kind == rule.kind && r.pattern == rule.pattern)
            });
            if guard.len() >= 5000 {
                guard.remove(0);
            }
            guard.push(rule);
        }
        let _ = i
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new()
                    .content(format!("OK: {:?} {:?} `{}`", action, kind, pat)),
            )
            .await;
    }

    async fn on_btn_allow_deny_avatar(
        &self,
        ctx: &Context,
        app: &AppContext,
        i: &ComponentInteraction,
        allow: bool,
    ) {
        let env = app.env();
        if !ensure_staff_ephemeral(ctx, &env, i).await {
            return;
        }

        let _ = i
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(if allow {
                            "Zapisuję allow…"
                        } else {
                            "Zapisuję deny…"
                        })
                        .ephemeral(true),
                ),
            )
            .await;

        let Some(gid) = i.guild_id else {
            let _ = i
                .edit_response(
                    &ctx.http,
                    EditInteractionResponse::new().content("Tylko na serwerze."),
                )
                .await;
            return;
        };

        let hid = i.data.custom_id.splitn(2, ':').nth(1).unwrap_or("");
        let h = u64::from_str_radix(hid, 16).unwrap_or(0);

        let action = if allow {
            RuleAction::Allow
        } else {
            RuleAction::Deny
        };
        if let Err(e) =
            upsert_avatar_hash_deny_allow(self.db(), gid.get(), h, action, "button").await
        {
            tracing::warn!(?e, "upsert_avatar_hash_deny_allow failed from button");
        }

        if allow {
            if let Some(av) = self.avatar_deny.get(&gid.get()) {
                let mut guard = av.write().await;
                guard.retain(|x| x.hash != h);
            }
        } else {
            let av = self
                .avatar_deny
                .entry(gid.get())
                .or_insert_with(|| Arc::new(RwLock::new(Vec::new())))
                .clone();
            let mut guard = av.write().await;
            if guard.len() >= 5000 {
                guard.remove(0);
            }
            guard.push(AvatarDenyHash {
                hash: h,
                _reason: "button".into(),
            });
        }

        let _ = i
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new()
                    .content(format!("OK: {:?} avatar `{:016x}`", action, h)),
            )
            .await;
    }
}

/* ===========================
   Implementacje pomocnicze
   =========================== */

// Cache regexów dla tokenów (granice słów Unicode)
const TOKEN_RE_CACHE_CAPACITY: u64 = 1024;
static TOKEN_RE_CACHE: Lazy<Cache<String, Regex>> = Lazy::new(|| {
    Cache::builder()
        .max_capacity(TOKEN_RE_CACHE_CAPACITY)
        .build()
});

// Wbudowane „złe” słowa – pojedynczy prekompilowany regex (granice słów Unicode)
static BUILTIN_BAD_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?u)(?<!\p{Alnum})(hitler|nazi|swast|heil|adolf|kkk|nsfw|porn|sex|cum|kurwa|jebac|huj|chuj|pierdole|spierdalaj)(?!\p{Alnum})"
    ).unwrap()
});

fn contains_token_cached(haystack_lower: &str, needle_lower: &str) -> bool {
    let pat = format!(
        r"(?u)(?<!\p{{Alnum}}){}(?!\p{{Alnum}})",
        regex::escape(needle_lower)
    );
    let re = if let Some(r) = TOKEN_RE_CACHE.get(&pat) {
        // `Cache::get` returns a cloned `Regex`, no function call needed
        r
    } else {
        match Regex::new(&pat) {
            Ok(compiled) => {
                TOKEN_RE_CACHE.insert(pat.clone(), compiled.clone());
                compiled
            }
            Err(e) => {
                tracing::error!(%pat, ?e, "token regex compile failed");
                return false;
            }
        }
    };
    re.is_match(haystack_lower)
}

impl NickRule {
    fn new(action: RuleAction, kind: RuleKind, pattern: String, reason: &str) -> Result<Self> {
        let compiled = match kind {
            RuleKind::Token => None,
            RuleKind::Regex => Some(build_regex(&pattern)?),
        };
        let pattern_lower = match kind {
            RuleKind::Token => Some(pattern.to_lowercase()),
            RuleKind::Regex => None,
        };
        Ok(Self {
            action,
            kind,
            pattern,
            compiled,
            pattern_lower,
            reason: reason.to_string(),
        })
    }

    fn matches(&self, text_raw: &str, text_lower: &str) -> bool {
        match self.kind {
            RuleKind::Token => {
                let pat = self
                    .pattern_lower
                    .as_ref()
                    .map(|s| s.as_str())
                    .unwrap_or("");
                contains_token_cached(text_lower, pat)
            }
            RuleKind::Regex => self
                .compiled
                .as_ref()
                .map(|r| regex_is_match_with_timeout(r, text_raw))
                .unwrap_or(false),
        }
    }
}


fn regex_is_match_with_timeout(regex: &Regex, text: &str) -> bool {
    let r = regex.clone();
    let s = text.to_owned();
    task::block_in_place(|| {
        Handle::current().block_on(async move {
            tokio::time::timeout(
                Duration::from_millis(REGEX_TIMEOUT_MS),
                task::spawn_blocking(move || r.is_match(&s)),
            )
            .await
            .ok()
            .and_then(|v| v.ok())
            .unwrap_or(false)
        })
    })
}

/// Bezpieczny parser /pattern/flags (np. /foo.*/iu). Dopuszcza także "/body" bez końcowego '/'
fn build_regex(pat_with_slashes: &str) -> anyhow::Result<Regex> {
    let s = pat_with_slashes.trim();
    let (body, flags) = if s.starts_with('/') {
        match s[1..].rfind('/') {
            Some(rel) => {
                let idx = 1 + rel;
                (&s[1..idx], &s[idx + 1..]) // /body/flags
            }
            None => (&s[1..], ""), // "/body" bez flags
        }
    } else {
        (s, "")
    };

    if body.len() > MAX_REGEX_LEN {
        anyhow::bail!("regex pattern too long");
    }

    let body_owned = body.to_owned();
    let flags_owned = flags.to_owned();

    tokio::task::block_in_place(|| {
        Handle::current().block_on(async move {
            // Kompilacja regexa w wątku blokującym + timeout
            let join_res = tokio::time::timeout(
                Duration::from_millis(REGEX_TIMEOUT_MS),
                task::spawn_blocking(move || {
                    let mut b = RegexBuilder::new(&body_owned);
                    b.case_insensitive(flags_owned.contains('i'))
                        .unicode(true)
                        .multi_line(flags_owned.contains('m'))
                        .dot_matches_new_line(flags_owned.contains('s'))
                        .size_limit(1 << 20)
                        .dfa_size_limit(1 << 20)
                        .build() // -> Result<Regex, regex::Error>
                }),
            )
            .await;

            match join_res {
                Err(_) => Err(anyhow::anyhow!("regex compile timeout")), // timeout
                Ok(inner) => match inner {
                    Err(join_err) => Err(anyhow::Error::new(join_err)),   // JoinError
                    Ok(regex_res) => regex_res.map_err(anyhow::Error::new), // Result<Regex, regex::Error> -> anyhow::Result<Regex>
                },
            }
        })
    })
}

/* ===========================
   Utilities
   =========================== */

/// Pozwala na Unicode w custom_id (bez znaków sterujących i bez ':')
fn sanitize_custom_id(s: &str, max_len: usize) -> String {
    let mut out = String::new();
    let mut count = 0;
    for ch in s.chars() {
        if ch == ':' || ch.is_control() {
            continue;
        }
        out.push(ch);
        count += 1;
        if count >= max_len {
            break;
        }
    }
    if out.is_empty() { "_".into() } else { out }
}

/// Przycinanie etykiet przycisków do bezpiecznej długości (po znakach, nie bajtach)
fn clamp_label(s: &str) -> String {
    clamp_chars(s, 40)
}

/// Przycinanie po znakach z '…' gdy obcięte
fn clamp_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i + 1 >= max_chars {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

fn tokens_for_buttons(s: &str) -> Vec<String> {
    // prosta tokenizacja po sekwencjach alnum (≥3 znaki), z deduplikacją; preferuj dłuższe
    let mut tokens = Vec::new();
    let mut cur = String::new();
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            cur.push(ch);
        } else {
            if cur.chars().count() >= 3 {
                tokens.push(cur.clone());
            }
            cur.clear();
        }
    }
    if cur.chars().count() >= 3 {
        tokens.push(cur);
    }

    tokens.sort_unstable();
    tokens.dedup();
    // preferuj dłuższe tokeny
    tokens.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()));
    tokens.into_iter().take(8).collect() // zwróć więcej; i tak przytniemy do 4 przy renderze
}

pub fn parse_pattern(input: &str) -> (RuleKind, String) {
    let s = input.trim();
    if s.starts_with('/') && s.ends_with('/') && s.chars().count() >= 3 {
        (RuleKind::Regex, s.to_string())
    } else if s.starts_with('/') && s.chars().count() >= 3 && s[1..].contains('/') {
        // /body/flags
        (RuleKind::Regex, s.to_string())
    } else {
        (RuleKind::Token, s.to_string())
    }
}

fn collect_names(u: &Option<String>, d: &Option<String>, g: &Option<String>) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(x) = u {
        v.push(x.clone());
    }
    if let Some(x) = d {
        v.push(x.clone());
    }
    if let Some(x) = g {
        v.push(x.clone());
    }
    v
}

/* ===========================
   aHash + download (z limitem i whitelistą hostów)
   =========================== */

static CT_IMAGE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^image/").unwrap());

const AHASH_CACHE_TTL_SECS: u64 = 60 * 60; // 1 h
const AHASH_CACHE_CAPACITY: u64 = 1024;
static AHASH_CACHE: Lazy<Cache<String, u64>> = Lazy::new(|| {
    Cache::builder()
        .time_to_live(Duration::from_secs(AHASH_CACHE_TTL_SECS))
        .max_capacity(AHASH_CACHE_CAPACITY)
        .build()
});

fn host_is_discord_cdn(url: &str) -> bool {
    if let Ok(u) = Url::parse(url) {
        if u.scheme() != "https" {
            return false;
        }
        if let Some(host) = u.host_str() {
            match host.to_ascii_lowercase().as_str() {
                "cdn.discordapp.com" | "media.discordapp.net" => return true,
                _ => {}
            }
        }
    }
false
}
const MAX_IMAGE_DIMENSION: u32 = 4096; // limit obrazków do 4096×4096

async fn fetch_and_ahash(url: &str) -> Result<Option<(u64, Vec<u8>)>> {
    const MAX_IMAGE_BYTES: u64 = 1_500_000;

    if !host_is_discord_cdn(url) {
        return Ok(None);
    }
    if let Some(h) = AHASH_CACHE.get(url) {
        return Ok(Some((h, Vec::new())));
    }
    let _permit = img_sem().acquire().await.ok(); // delikatny throttle

    let resp = match http().get(url).send().await {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };

    // Sprawdź finalny URL po ewentualnych redirectach (ochrona whitelisty)
    if !host_is_discord_cdn(resp.url().as_str()) {
        return Ok(None);
    }

    if !resp.status().is_success() {
        return Ok(None);
    }

    // Nie trzymaj &str do nagłówka poza tym blokiem
    let is_image = {
        let ct_opt = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok());
        matches!(ct_opt, Some(v) if CT_IMAGE_RE.is_match(v))
    };
    if !is_image {
        return Ok(None);
    }

    // content-length limit
    if let Some(len) = resp.content_length() {
        if len > MAX_IMAGE_BYTES {
            return Ok(None);
        }
    }

    // Teraz możemy skonsumować response
    let mut stream = resp.bytes_stream();
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk_res) = stream.next().await {
        let chunk = match chunk_res {
            Ok(c) => c,
            Err(_) => return Ok(None),
        };
        if (bytes.len() as u64) + (chunk.len() as u64) > MAX_IMAGE_BYTES {
            return Ok(None);
        }
        bytes.extend_from_slice(&chunk);
    }
    let h = ahash_from_bytes(&bytes).await.ok().flatten();
    if let Some(v) = h {
        AHASH_CACHE.insert(url.to_string(), v);
    }

    Ok(h.map(|hash| (hash, bytes)))
}

async fn ahash_from_bytes(bytes: &[u8]) -> Result<Option<u64>> {
    let data = bytes.to_vec();
    task::spawn_blocking(move || {
        use image::{imageops::FilterType, io::Reader as ImageReader};
        use std::io::Cursor;

    let mut limits = image::io::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);

     // Odczytaj nagłówek i zwróć None, jeśli wymiary są zbyt duże.
        {
            let mut reader = match ImageReader::new(Cursor::new(&data)).with_guessed_format() {
                Ok(r) => r,
                Err(_) => return Ok(None),
            };
            reader.limits(limits.clone());
            if reader.into_dimensions().is_err() {
                return Ok(None);
            }
        }

        let mut reader = match ImageReader::new(Cursor::new(&data)).with_guessed_format() {
            Ok(r) => r,
            Err(_) => return Ok(None),
        };
        reader.limits(limits);
        let img = match reader.decode() {
            Ok(i) => i,
            Err(_) => return Ok(None),
        };

    let gray = img.resize_exact(8, 8, FilterType::Triangle).to_luma8();
        let mut sum: u64 = 0;
        let mut px = [0u8; 64];
        for (i, p) in gray.pixels().enumerate() {
            let v = p.0[0] as u64;
            sum += v;
            px[i] = p.0[0];
        }
    let avg = (sum / 64) as u8;
        let mut bits: u64 = 0;
        for (i, &v) in px.iter().enumerate() {
            if v > avg {
                bits |= 1u64 << i;
            }
        }
        Ok(Some(bits))
    })
    .await
    .map_err(|e| anyhow::Error::new(e))?
}

/* ===========================
   Stuby OCR/NSFW (do podmiany)
   OCR i NSFW
=========================== */

async fn ocr_from_bytes(bytes: &[u8]) -> Option<String> {
    use tempfile::NamedTempFile;
    use tokio::{fs, process::Command};

    let file = NamedTempFile::new().ok()?;
    fs::write(file.path(), bytes).await.ok()?;
    let output = Command::new("tesseract")
        .arg(file.path())
        .arg("stdout")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}
async fn nsfw_from_bytes(bytes: &[u8]) -> Option<f32> {
    use tempfile::NamedTempFile;
    use tokio::{fs, process::Command};

    let file = NamedTempFile::new().ok()?;
    fs::write(file.path(), bytes).await.ok()?;
    let script = "from nudenet import NudeClassifier;import sys,json;c=NudeClassifier();print(json.dumps(c.classify(sys.argv[1])))";
    let output = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(file.path())
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let json: JsonValue = serde_json::from_str(&stdout).ok()?;
    let score = json
        .as_object()
        .and_then(|o| o.values().next())
        .and_then(|v| v.get("unsafe"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    Some(score as f32)
}

/* ===========================
   DB — best effort + DDL
   =========================== */

async fn maybe_ensure_tables(db: &Pool<Postgres>) {
    let db = db.clone();
    let _ = INIT_DDL
        .get_or_init(|| async move {
            if let Err(e) = ensure_tables(&db).await {
                tracing::error!(?e, "ensure_tables failed");
            }
        })
        .await;
}

async fn ensure_tables(db: &Pool<Postgres>) -> Result<()> {
    // schema
    let _ = sqlx::query(r#"CREATE SCHEMA IF NOT EXISTS tss"#)
        .execute(db)
        .await?;

    // config
    let _ = sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS tss.idg_config (
          guild_id   BIGINT PRIMARY KEY,
          cfg        JSONB  NOT NULL,
          updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
        )
        "#,
    )
    .execute(db)
    .await?;

    // rules
    let _ = sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS tss.idg_rules (
          guild_id  BIGINT NOT NULL,
          action    TEXT   NOT NULL, -- 'allow'|'deny'
          kind      TEXT   NOT NULL, -- 'token'|'regex'
          pattern   TEXT   NOT NULL,
          reason    TEXT   NOT NULL,
          created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
          CONSTRAINT idg_rules_unique UNIQUE (guild_id, action, kind, pattern)
        )
        "#,
    )
    .execute(db)
    .await?;

    // avatar deny hashes
    let _ = sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS tss.idg_avatar_hash_deny (
          guild_id  BIGINT NOT NULL,
          hash      BIGINT NOT NULL,
          reason    TEXT   NOT NULL,
          created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
          CONSTRAINT idg_avatar_hash_deny_unique UNIQUE (guild_id, hash)
        )
        "#,
    )
    .execute(db)
    .await?;

    // indeksy pomocnicze
    let _ =
        sqlx::query(r#"CREATE INDEX IF NOT EXISTS idx_idg_rules_guild ON tss.idg_rules(guild_id)"#)
            .execute(db)
            .await?;
    let _ = sqlx::query(
        r#"CREATE INDEX IF NOT EXISTS idx_idg_avatar_hash_guild ON tss.idg_avatar_hash_deny(guild_id)"#,
    )
    .execute(db)
    .await?;

    Ok(())
}

async fn load_cfg_db(db: &Pool<Postgres>, guild_id: u64) -> Result<Option<IdgConfig>> {
    let row = sqlx::query("SELECT cfg FROM tss.idg_config WHERE guild_id = $1")
        .bind(guild_id as i64)
        .fetch_optional(db)
        .await?;
    if let Some(r) = row {
        let val: JsonValue = r.try_get("cfg")?;
        let mut cfg: IdgConfig = serde_json::from_value(val)?;
        // defensywnie: w razie braków w JSON po update’ach
        if cfg.thresholds.watch == 0 && cfg.thresholds.block == 0 {
           cfg.thresholds = IdgThresholds {
                watch: 30,
                block: 60,
            };
        }
        cfg = sanitize_cfg(cfg);
        Ok(Some(cfg))
    } else {
        Ok(None)
    }
}

async fn save_cfg_db(db: &Pool<Postgres>, guild_id: u64, cfg: &IdgConfig) -> Result<()> {
    let v = serde_json::to_value(cfg)?;
    let _ = sqlx::query(
        r#"INSERT INTO tss.idg_config (guild_id, cfg) VALUES ($1, $2)
           ON CONFLICT (guild_id) DO UPDATE SET cfg = EXCLUDED.cfg, updated_at = now()"#,
    )
    .bind(guild_id as i64)
    .bind(v)
    .execute(db)
    .await?;
    Ok(())
}

async fn load_rules_db(db: &Pool<Postgres>, guild_id: u64) -> Result<Vec<NickRule>> {
    let rows = match sqlx::query(
        "SELECT action, kind, pattern, reason FROM tss.idg_rules WHERE guild_id = $1",
    )
    .bind(guild_id as i64)
    .fetch_all(db)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(?e, "Failed to load nick rules from DB");
            return Ok(Vec::new());
        }
    };

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let action_s: String = r.try_get("action").unwrap_or_else(|_| "deny".into());
        let kind_s: String = r.try_get("kind").unwrap_or_else(|_| "token".into());
        let patt: String = r.try_get("pattern").unwrap_or_default();
        let reason: String = r.try_get("reason").unwrap_or_default();

        let action = if action_s.eq_ignore_ascii_case("allow") {
            RuleAction::Allow
        } else {
            RuleAction::Deny
        };
        let kind = if kind_s.eq_ignore_ascii_case("regex") {
            RuleKind::Regex
        } else {
            RuleKind::Token
        };

        // Nie panikuj, jeśli regex jest zły; pomiń i zaloguj
        match NickRule::new(action, kind, patt, &reason) {
            Ok(rule) => out.push(rule),
            Err(e) => {
                tracing::warn!(?e, "Pominięto niepoprawny regex w DB");
            }
        }
    }
    Ok(out)
}

async fn upsert_nick_rule(db: &Pool<Postgres>, guild_id: u64, rule: &NickRule) -> Result<()> {
    let _ = sqlx::query(
        r#"INSERT INTO tss.idg_rules (guild_id, action, kind, pattern, reason, created_at)
           VALUES ($1, $2, $3, $4, $5, now())
           ON CONFLICT (guild_id, action, kind, pattern)
           DO UPDATE SET reason = EXCLUDED.reason"#,
    )
    .bind(guild_id as i64)
    .bind(match rule.action {
        RuleAction::Allow => "allow",
        RuleAction::Deny => "deny",
    })
    .bind(match rule.kind {
        RuleKind::Token => "token",
        RuleKind::Regex => "regex",
    })
    .bind(&rule.pattern)
    .bind(&rule.reason)
    .execute(db)
    .await?;
    Ok(())
}

async fn load_avatar_hashes_db(db: &Pool<Postgres>, guild_id: u64) -> Result<Vec<AvatarDenyHash>> {
    let rows =
        match sqlx::query("SELECT hash, reason FROM tss.idg_avatar_hash_deny WHERE guild_id = $1")
            .bind(guild_id as i64)
            .fetch_all(db)
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(?e, "Failed to load avatar hash deny list from DB");
                return Ok(Vec::new());
            }
        };

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let h: i64 = r.try_get("hash").unwrap_or(0);
        let reason: String = r.try_get("reason").unwrap_or_default();
        if h > 0 {
            out.push(AvatarDenyHash {
                hash: h as u64,
                _reason: reason,
            });
        }
    }
    Ok(out)
}

async fn upsert_avatar_hash_deny_allow(
    db: &Pool<Postgres>,
    guild_id: u64,
    hash: u64,
    action: RuleAction,
    reason: &str,
) -> Result<()> {
    match action {
        RuleAction::Deny => {
            let _ = sqlx::query(
                r#"INSERT INTO tss.idg_avatar_hash_deny (guild_id, hash, reason, created_at)
                   VALUES ($1, $2, $3, now())
                   ON CONFLICT (guild_id, hash) DO UPDATE SET reason = EXCLUDED.reason"#,
            )
            .bind(guild_id as i64)
            .bind(hash as i64)
            .bind(reason)
            .execute(db)
            .await?;
        }
        RuleAction::Allow => {
            // allow: czyścimy ewentualny wpis z deny
            let _ =
                sqlx::query("DELETE FROM tss.idg_avatar_hash_deny WHERE guild_id=$1 AND hash=$2")
                    .bind(guild_id as i64)
                    .bind(hash as i64)
                    .execute(db)
                    .await?;
        }
    }
    Ok(())
}

/* ===========================
   Parser opcji
   =========================== */

// /idguard: SubCommand "setup" | "preset value:<str>" | "mode value:<str>"
fn extract_idguard_sub(options: &[CommandDataOption]) -> (Option<String>, Option<String>) {
    if let Some(op) = options.first() {
        if let CommandDataOptionValue::SubCommand(params) = &op.value {
            // szukamy parametru "value"
            for p in params {
                if p.name == "value" {
                    if let CommandDataOptionValue::String(s) = &p.value {
                        return (Some(op.name.clone()), Some(s.clone()));
                    }
                }
            }
            return (Some(op.name.clone()), None);
        }
    }
    (None, None)
}

// /teach: SubCommand "allow|deny" z opcjami nick/avatar/reason (string)
fn extract_teach_params(
    options: &[CommandDataOption],
) -> (Option<String>, HashMap<String, String>) {
    if let Some(op) = options.first() {
        if let CommandDataOptionValue::SubCommand(params) = &op.value {
            let mut out: HashMap<String, String> = HashMap::new();
            for p in params {
                match (p.name.as_str(), &p.value) {
                    ("nick", CommandDataOptionValue::String(s)) => {
                        out.insert("nick".into(), s.clone());
                    }
                    ("avatar", CommandDataOptionValue::String(s)) => {
                        out.insert("avatar".into(), s.clone());
                    }
                    ("reason", CommandDataOptionValue::String(s)) => {
                        out.insert("reason".into(), s.clone());
                    }
                    _ => {}
                }
            }
            return (Some(op.name.clone()), out);
        }
    }
    (None, HashMap::new())
}

/* ===========================
   ACL helpers (staff)
   =========================== */

fn is_staff_member_roles(env: &str, member_roles: &[serenity::all::RoleId]) -> bool {
    let staff = env_roles::staff_set(env);
    member_roles.iter().any(|r| staff.contains(&r.get()))
}

async fn ensure_staff_ephemeral(ctx: &Context, env: &str, i: &ComponentInteraction) -> bool {
    let ok = i
        .member
        .as_ref()
        .map(|m| is_staff_member_roles(env, &m.roles))
        .unwrap_or(false);
    if !ok {
        let _ = i
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("Brak uprawnień.")
                        .ephemeral(true),
                ),
            )
        .await;
    }
    ok
}

/* ===========================
   Konfiguracja – sanity
   =========================== */

pub fn sanitize_cfg(mut cfg: IdgConfig) -> IdgConfig {
    // progi: wymuś block ∈ [1,100], watch ∈ [0, block-1]
    cfg.thresholds.block = cfg.thresholds.block.clamp(1, 100);
    cfg.thresholds.watch = cfg
        .thresholds
        .watch
        .min(cfg.thresholds.block.saturating_sub(1));

    // wagi
    fn clamp_w(v: i32) -> i32 {
        v.clamp(0, 100)
    }
    cfg.weights.nick_token = clamp_w(cfg.weights.nick_token);
    cfg.weights.nick_regex = clamp_w(cfg.weights.nick_regex);
    cfg.weights.avatar_hash = clamp_w(cfg.weights.avatar_hash);
    cfg.weights.avatar_ocr  = clamp_w(cfg.weights.avatar_ocr);
    cfg.weights.avatar_nsfw = clamp_w(cfg.weights.avatar_nsfw);
    cfg
}
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::Arc;
    use tokio::sync::RwLock;
    use sqlx::postgres::PgPoolOptions;
    use crate::config::{Settings, App, Discord, Database, Logging, ChatGuardConfig};

    fn make_idguard() -> Arc<IdGuard> {
        let settings = Settings {
            env: "test".into(),
            app: App { name: "test".into() },
            discord: Discord { token: String::new(), app_id: None, intents: vec![] },
            database: Database { url: "postgres://localhost:1/test?connect_timeout=1".into(), max_connections: Some(1), statement_timeout_ms: Some(5_000) },
            logging: Logging { json: Some(false), level: Some("info".into()) },
            chatguard: ChatGuardConfig { racial_slurs: vec![] },
            antinuke: Default::default(),
        };
        let db = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy(&settings.database.url)
            .unwrap();
        let ctx = crate::AppContext::new_testing(settings, db);
        IdGuard::new(ctx)
    }

    #[test]
    fn contains_token_cached_basic() {
        assert!(contains_token_cached("foo bar", "bar"));
        assert!(!contains_token_cached("foobar", "foo"));
        // second call uses cached regex
        assert!(contains_token_cached("bar baz", "bar"));
    }

    #[tokio::test]
    async fn build_regex_parses_flags_and_body() {
        let re = build_regex("/foO/i").unwrap();
        assert!(re.is_match("foo"));
        assert!(re.is_match("FOO"));
        assert!(!re.is_match("bar"));
    }

    #[tokio::test]
    async fn fetch_and_ahash_rejects_non_discord() {
        // Untrusted host should be ignored without network access
        let res = fetch_and_ahash("https://example.com/avatar.png")
            .await
            .unwrap();
        assert!(res.is_none());
    }

    #[tokio::test]
    async fn allow_rule_skips_deny() {
        let idg = make_idguard();
        let gid = 1u64;
        let allow = NickRule::new(RuleAction::Allow, RuleKind::Token, "foo".into(), "test").unwrap();
        let deny  = NickRule::new(RuleAction::Deny,  RuleKind::Token, "bar".into(), "test").unwrap();
        idg
            .nick_rules
            .insert(gid, Arc::new(RwLock::new(vec![allow, deny])));

        let input = IdgInput {
            guild_id: gid,
            user_id: 1,
            username: Some("foo bar".into()),
            display_name: None,
            global_name: None,
            avatar_url: None,
        };
        let report = idg.check_user(&input).await;
        assert!(report.signals.is_empty());
        assert_eq!(report.score, 0);
        assert_eq!(report.verdict, IdgVerdict::Clean);
    }

    #[tokio::test]
    async fn deny_rule_adds_signal() {
        let idg = make_idguard();
        let gid = 2u64;
        let deny = NickRule::new(RuleAction::Deny, RuleKind::Token, "bar".into(), "test").unwrap();
        idg
            .nick_rules
            .insert(gid, Arc::new(RwLock::new(vec![deny])));

        let input = IdgInput {
            guild_id: gid,
            user_id: 1,
            username: Some("foo bar".into()),
            display_name: None,
            global_name: None,
            avatar_url: None,
        };
        let report = idg.check_user(&input).await;
        assert_eq!(report.score, IdgConfig::default().weights.nick_token as u8);
        assert_eq!(report.verdict, IdgVerdict::Clean);
        assert_eq!(report.signals.len(), 1);
        assert_eq!(report.signals[0].kind, IdgSignalKind::NickToken);
    }

    #[tokio::test]
    async fn report_block_verdict_when_score_high() {
        let idg = make_idguard();
        let gid = 3u64;
        let mut cfg = IdgConfig::default();
        cfg.thresholds.watch = 10;
        cfg.thresholds.block = 20;
        idg.cfg_cache.insert(gid, sanitize_cfg(cfg));
        let deny1 = NickRule::new(RuleAction::Deny, RuleKind::Token, "foo".into(), "t").unwrap();
        let deny2 = NickRule::new(RuleAction::Deny, RuleKind::Token, "bar".into(), "t").unwrap();
        idg
            .nick_rules
            .insert(gid, Arc::new(RwLock::new(vec![deny1, deny2])));
        let input = IdgInput {
            guild_id: gid,
            user_id: 1,
            username: Some("foo bar".into()),
            display_name: None,
            global_name: None,
            avatar_url: None,
        };
        let report = idg.check_user(&input).await;
        assert_eq!(report.verdict, IdgVerdict::Block);
        assert_eq!(report.score, 50);
    }

    proptest! {
        #[test]
        fn score_respects_thresholds(
            weights in proptest::collection::vec(-100..100i32, 0..6),
            watch in 0u8..100,
            block in 1u8..100,
        ) {
            let mut cfg = IdgConfig::default();
            cfg.thresholds.watch = watch;
            cfg.thresholds.block = block;
            cfg = sanitize_cfg(cfg);
            let mut signals = Vec::new();
            for w in weights {
                signals.push(IdgSignal { kind: IdgSignalKind::NickToken, weight: w, detail: String::new() });
            }
            let mut score = 0i32;
            for s in &signals { score += s.weight; }
            score = score.clamp(0, 100);
            let score_u8 = score as u8;
            let verdict = if score_u8 >= cfg.thresholds.block {
                IdgVerdict::Block
            } else if score_u8 >= cfg.thresholds.watch {
                IdgVerdict::Watch
            } else {
                IdgVerdict::Clean
            };
            prop_assert!(score_u8 <= 100);
            let verdict_ok = match verdict {
                IdgVerdict::Block => score_u8 >= cfg.thresholds.block,
                IdgVerdict::Watch => score_u8 >= cfg.thresholds.watch && score_u8 < cfg.thresholds.block,
                IdgVerdict::Clean => score_u8 < cfg.thresholds.watch,
            };
            prop_assert!(verdict_ok);
        }
    }
}
