// src/mute.rs

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use dashmap::DashMap;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serenity::all::*;
use sqlx::{Pool, Postgres, Row};
use chrono::{Utc, Duration};

use crate::{AppContext, registry::env_channels};
use crate::admcheck::has_permission;

const SYSTEM_NAME: &str = "Tigris Mute System";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MuteConfig {
    /// Rola „Muted” – jeśli ustawiona, używamy jej; w innym wypadku próbujemy timeout.
    pub role_id: Option<u64>,
    /// Domyślny czas w minutach (używany gdy /mute bez „czas”)
    pub default_minutes: i32,
}
impl Default for MuteConfig {
    fn default() -> Self {
        Self { role_id: None, default_minutes: 30 }
    }
}

#[derive(Debug, Clone)]
pub struct MuteCase {
    pub id: i64,
    pub guild_id: u64,
    pub user_id: u64,
    pub moderator_id: u64,
    pub reason: String,
    pub evidence: Option<String>,
    pub created_at_unix: i64,
    pub until_unix: Option<i64>,
    pub method: String,        // "role" | "timeout"
    pub role_id: Option<u64>,  // zapis, jeśli użyliśmy roli
}

static CFG: Lazy<DashMap<u64, MuteConfig>> = Lazy::new(DashMap::new);

pub struct Mute;

impl Mute {
    /* ===================== DB bootstrap ===================== */

    pub async fn ensure_tables(db: &Pool<Postgres>) -> Result<()> {
        // 0) Schemat
        sqlx::query(r#"CREATE SCHEMA IF NOT EXISTS tss;"#)
            .execute(db).await?;

        // 1) Konfiguracja
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tss.mute_config (
              guild_id BIGINT PRIMARY KEY,
              cfg JSONB NOT NULL DEFAULT '{}'::jsonb
            );
            ALTER TABLE tss.mute_config
              ADD COLUMN IF NOT EXISTS cfg JSONB NOT NULL DEFAULT '{}'::jsonb;
            "#
        ).execute(db).await?;

        // 2) Tabela historii mute (pełna definicja)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tss.mute_cases (
              id            BIGSERIAL PRIMARY KEY,
              guild_id      BIGINT       NOT NULL,
              user_id       BIGINT       NOT NULL,
              moderator_id  BIGINT       NOT NULL,
              reason        TEXT         NOT NULL,
              evidence      TEXT         NULL,
              created_at    TIMESTAMPTZ  NOT NULL DEFAULT now(),
              until         TIMESTAMPTZ  NULL,
              unmuted_at    TIMESTAMPTZ  NULL,
              unmuted_by    BIGINT       NULL,
              unmute_reason TEXT         NULL,
              method        TEXT         NOT NULL DEFAULT 'role',
              role_id       BIGINT       NULL
            );
            "#
        ).execute(db).await?;

        // 2a) Kolumny (idempotentnie – gdyby tabela istniała w starszej wersji)
        sqlx::query(
            r#"
            ALTER TABLE tss.mute_cases
              ADD COLUMN IF NOT EXISTS until         TIMESTAMPTZ NULL,
              ADD COLUMN IF NOT EXISTS unmuted_at    TIMESTAMPTZ NULL,
              ADD COLUMN IF NOT EXISTS unmuted_by    BIGINT      NULL,
              ADD COLUMN IF NOT EXISTS unmute_reason TEXT        NULL,
              ADD COLUMN IF NOT EXISTS method        TEXT        NOT NULL DEFAULT 'role',
              ADD COLUMN IF NOT EXISTS role_id       BIGINT      NULL;
            "#
        ).execute(db).await?;

        // 3) Indeksy
        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_mute_cases_gid_uid_created
              ON tss.mute_cases (guild_id, user_id, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_mute_cases_gid_until
              ON tss.mute_cases (guild_id, until);
            "#
        ).execute(db).await?;

        Ok(())
    }

    /// Szybka kontrola – jeśli nie ma tabeli, stwórz ją.
    async fn ensure_runtime(db: &Pool<Postgres>) -> Result<()> {
        let exists: Option<String> = sqlx::query_scalar(
            "SELECT to_regclass('tss.mute_cases')::text"
        ).fetch_one(db).await.unwrap_or(None);
        if exists.is_none() {
            // brak tabeli – odpal pełną migrację
            Self::ensure_tables(db).await?;
        }
        Ok(())
    }

    /* ===================== Commands ===================== */

    pub async fn register_commands(ctx: &Context, gid: GuildId) -> Result<()> {
        // /mute — wymagane parametry przed opcjonalnymi
        gid.create_command(&ctx.http,
            CreateCommand::new("mute")
                .description("Wycisz użytkownika (rola Muted lub timeout).")
                .default_member_permissions(Permissions::MODERATE_MEMBERS)
                .add_option(CreateCommandOption::new(
                        CommandOptionType::User, "user", "Kogo wyciszyć"
                    ).required(true))
                .add_option(CreateCommandOption::new(
                        CommandOptionType::String, "reason", "Powód"
                    ).required(true))
                .add_option(CreateCommandOption::new(
                        CommandOptionType::String, "duration", "Czas: 15m, 2h, 1d, 0=bezterminowo"
                    ))
                .add_option(CreateCommandOption::new(
                        CommandOptionType::String, "evidence", "Dowód/URL (opcjonalnie)"
                    ))
        ).await?;

        // /unmute
        gid.create_command(&ctx.http,
            CreateCommand::new("unmute")
                .description("Zdejmij wyciszenie (rola/timeout).")
                .default_member_permissions(Permissions::MODERATE_MEMBERS)
                .add_option(CreateCommandOption::new(
                        CommandOptionType::User, "user", "Kogo odciszyć"
                    ).required(true))
                .add_option(CreateCommandOption::new(
                        CommandOptionType::String, "reason", "Powód (opcjonalnie)"
                    ))
        ).await?;

        // /mute-config
        gid.create_command(&ctx.http,
            CreateCommand::new("mute-config")
                .description("Konfiguracja systemu mute")
                .default_member_permissions(Permissions::ADMINISTRATOR)
                .add_option(
                    CreateCommandOption::new(CommandOptionType::SubCommand, "set", "Ustaw parametry")
                        .add_sub_option(CreateCommandOption::new(
                                CommandOptionType::Integer, "default_minutes", "Domyślny czas (min)"
                            ))
                        .add_sub_option(CreateCommandOption::new(
                                CommandOptionType::String, "role_id", "ID roli Muted (opcjonalnie)"
                            ))
                )
        ).await?;

        Ok(())
    }

    pub async fn on_interaction(ctx: &Context, app: &AppContext, interaction: Interaction) {
        if let Some(cmd) = interaction.command() {
            match cmd.data.name.as_str() {
                "mute"        => { if let Err(e) = handle_mute(ctx, app, &cmd).await        { tracing::warn!(target: "tigris_security::mute", ?e, "mute failed"); } }
                "unmute"      => { if let Err(e) = handle_unmute(ctx, app, &cmd).await      { tracing::warn!(target: "tigris_security::mute", ?e, "unmute failed"); } }
                "mute-config" => { if let Err(e) = handle_mute_config(ctx, app, &cmd).await { tracing::warn!(target: "tigris_security::mute", ?e, "mute-config failed"); } }
                _ => {}
            }
        }
    }
}

/* ========================= Handlers ========================= */

async fn handle_mute(ctx: &Context, app: &AppContext, cmd: &CommandInteraction) -> Result<()> {
    // Dogoń migracje (również gdy startowa ensure_tables nie zadziałała)
    Mute::ensure_runtime(&app.db).await.ok();

    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true))
    ).await?;
    let Some(gid) = cmd.guild_id else { return edit(ctx, cmd, "Użyj na serwerze.").await; };

    if !has_permission(ctx, gid, cmd.user.id, crate::permissions::Permission::Mute).await {
        return edit(ctx, cmd, "⛔ Brak uprawnień.").await;
    }

    let mut user: Option<UserId> = None;
    let mut duration_s: Option<String> = None;
    let mut reason: Option<String> = None;
    let mut evidence: Option<String> = None;

    for o in &cmd.data.options {
        match (&o.name[..], &o.value) {
            ("user",     CommandDataOptionValue::User(u))   => user = Some(*u),
            ("duration", CommandDataOptionValue::String(s)) => duration_s = Some(s.clone()),
            ("reason",   CommandDataOptionValue::String(s)) => reason = Some(s.clone()),
            ("evidence", CommandDataOptionValue::String(s)) => evidence = Some(s.clone()),
            _ => {}
        }
    }
    let Some(uid) = user else { return edit(ctx, cmd, "Musisz wskazać użytkownika.").await; };
    let reason = reason.unwrap_or_else(|| "Brak powodu".into());

    // Czas
    let cfg = load_cfg(&app.db, gid.get()).await;
    let minutes = match duration_s.as_deref() {
        Some(s) => parse_duration_minutes(s).unwrap_or(cfg.default_minutes as i64),
        None => cfg.default_minutes as i64,
    };
    let until_opt = if minutes > 0 { Some(Utc::now() + Duration::minutes(minutes)) } else { None };

    // Zastosuj mute
    let (method, used_role) = if let Some(role_id) = cfg.role_id {
        // metoda: ROLA
        if let Ok(member) = gid.member(&ctx.http, uid).await {
            let _ = member.add_role(&ctx.http, RoleId::new(role_id)).await;
        }
        ("role".to_string(), Some(role_id))
    } else {
        // metoda: TIMEOUT (jeśli mamy czas > 0), inaczej brak akcji
        if let Some(until) = until_opt {
            if let Ok(mut member) = gid.member(&ctx.http, uid).await {
                let _ = member.disable_communication_until_datetime(&ctx.http, until.into()).await;
            }
            ("timeout".to_string(), None)
        } else {
            return edit(ctx, cmd, "Konfiguracja nie ma roli Muted, a czas = 0. Ustaw rolę w /mute-config lub podaj czas.").await;
        }
    };

    // Zapisz sprawę
    let until_unix: Option<i64> = until_opt.map(|dt| dt.timestamp());

    let case_id: i64 = sqlx::query_scalar(
        r#"
        INSERT INTO tss.mute_cases
            (guild_id, user_id, moderator_id, reason, evidence, until, method, role_id)
        VALUES
            ($1,$2,$3,$4,$5, CASE WHEN $6 IS NULL THEN NULL ELSE to_timestamp($6) END, $7, $8)
        RETURNING id
        "#
    )
    .bind(gid.get() as i64)
    .bind(uid.get() as i64)
    .bind(cmd.user.id.get() as i64)
    .bind(&reason)
    .bind(evidence.as_deref())
    .bind(until_unix) // Option<i64>
    .bind(&method)
    .bind(used_role.map(|v| v as i64))
    .fetch_one(&app.db)
    .await?;

    // Log
    if let Some(log_ch) = log_channel(app) {
        let e = embed_muted(ctx, gid, cmd.user.id, uid, &reason, evidence.as_deref(), minutes, &method, used_role).await;
        let _ = ChannelId::new(log_ch).send_message(&ctx.http, CreateMessage::new().embed(e)).await;
    }

    // Potwierdzenie
    let txt = if minutes > 0 {
        format!("✅ Uciszono <@{}> na **{}** (case `#{}`)", uid.get(), human_minutes(minutes), case_id)
    } else {
        format!("✅ Uciszono <@{}> **bezterminowo** (case `#{}`)", uid.get(), case_id)
    };
    edit(ctx, cmd, &txt).await
}

async fn handle_unmute(ctx: &Context, app: &AppContext, cmd: &CommandInteraction) -> Result<()> {
    // Dogoń migracje
    Mute::ensure_runtime(&app.db).await.ok();

    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true))
    ).await?;
    let Some(gid) = cmd.guild_id else { return edit(ctx, cmd, "Użyj na serwerze.").await; };

    if !moderate_permission(ctx, gid, cmd.user.id).await {
        return edit(ctx, cmd, "⛔ Brak uprawnień.").await;
    }

    // args
    let mut user: Option<UserId> = None;
    let mut reason: Option<String> = None;
    for o in &cmd.data.options {
        match (&o.name[..], &o.value) {
            ("user",   CommandDataOptionValue::User(u))   => user = Some(*u),
            ("reason", CommandDataOptionValue::String(s)) => reason = Some(s.clone()),
            _ => {}
        }
    }
    let Some(uid) = user else { return edit(ctx, cmd, "Wskaż użytkownika.").await; };
    let reason = reason.unwrap_or_else(|| "manual unmute".into());

    // Zdejmij timeout + rolę (ostrożnie)
    if let Ok(mut member) = gid.member(&ctx.http, uid).await {
        let _ = member.enable_communication(&ctx.http).await; // kasuje timeout
        // jeśli mamy skonfigurowaną rolę – zdejmij
        let cfg = load_cfg(&app.db, gid.get()).await;
        if let Some(rid) = cfg.role_id {
            let _ = member.remove_role(&ctx.http, RoleId::new(rid)).await;
        }
    }

    // Zapis w DB – zamknij najnowszy otwarty case
    let _ = sqlx::query(
        r#"
        WITH last AS (
            SELECT id
            FROM tss.mute_cases
            WHERE guild_id = $3
              AND user_id  = $4
              AND unmuted_at IS NULL
            ORDER BY created_at DESC
            LIMIT 1
        )
        UPDATE tss.mute_cases mc
           SET unmuted_at = now(),
               unmuted_by = $1,
               unmute_reason = $2
         WHERE mc.id IN (SELECT id FROM last)
        "#
    )
    .bind(cmd.user.id.get() as i64)
    .bind(&reason)
    .bind(gid.get() as i64)
    .bind(uid.get() as i64)
    .execute(&app.db)
    .await?;

    // Log
    if let Some(log_ch) = log_channel(app) {
        let e = embed_unmuted(ctx, gid, cmd.user.id, uid, &reason).await;
        let _ = ChannelId::new(log_ch).send_message(&ctx.http, CreateMessage::new().embed(e)).await;
    }

    edit(ctx, cmd, &format!("✅ Zdjęto wyciszenie z <@{}>.", uid.get())).await
}

async fn handle_mute_config(ctx: &Context, app: &AppContext, cmd: &CommandInteraction) -> Result<()> {
    // Dogoń migracje
    Mute::ensure_runtime(&app.db).await.ok();

    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true))
    ).await?;
    let Some(gid) = cmd.guild_id else { return edit(ctx, cmd, "Użyj na serwerze.").await; };

    if !admin_permission(ctx, gid, cmd.user.id).await {
        return edit(ctx, cmd, "⛔ Tylko administrator.").await;
    }

    let mut cfg = load_cfg_from_cache_or_default(gid.get());
    if let Some(sc) = cmd.data.options.first() {
        if sc.name == "set" {
            if let CommandDataOptionValue::SubCommand(params) = &sc.value {
                for p in params {
                    match (&p.name[..], &p.value) {
                        ("default_minutes", CommandDataOptionValue::Integer(n)) => cfg.default_minutes = *n as i32,
                        ("role_id",          CommandDataOptionValue::String(s)) => {
                            let trimmed = s.trim();
                            if trimmed.is_empty() { cfg.role_id = None; }
                            else if let Ok(id) = trimmed.parse::<u64>() { cfg.role_id = Some(id); }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    save_cfg(&app.db, gid.get(), &cfg).await?;
    CFG.insert(gid.get(), cfg.clone());

    // Log: zmiana konfiguracji
    if let Some(log_ch) = log_channel(app) {
        let e = CreateEmbed::new()
            .title("🛠️ Zmieniono konfigurację Mute")
            .colour(Colour::new(0x95A5A6))
            .description(format!(
                "- Rola: **{}**\n- Domyślny czas: **{}**",
                cfg.role_id.map(|v| v.to_string()).unwrap_or_else(|| "brak (timeout)".into()),
                human_minutes(cfg.default_minutes as i64),
            ))
            .field("Administrator", format!("<@{}>", cmd.user.id.get()), true)
            .footer(CreateEmbedFooter::new(SYSTEM_NAME));
        let _ = ChannelId::new(log_ch).send_message(&ctx.http, CreateMessage::new().embed(e)).await;
    }

    edit(ctx, cmd, "✅ Zapisano konfigurację Mute.").await
}

/* ========================= Embeds ========================= */

async fn embed_muted(
    ctx: &Context,
    _gid: GuildId,
    mod_id: UserId,
    uid: UserId,
    reason: &str,
    evidence: Option<&str>,
    minutes: i64,
    method: &str,
    role_id: Option<u64>,
) -> CreateEmbed {
    let now = now_unix();
    let mut e = CreateEmbed::new()
        .title("🔇 Wyciszono użytkownika")
        .colour(Colour::new(0xE67E22))
        .field("Użytkownik", format!("<@{}> (`{}`)", uid.get(), uid.get()), true)
        .field("Moderator", format!("<@{}> (`{}`)", mod_id.get(), mod_id.get()), true)
        .field("Metoda", format!("`{}`{}", method, role_id.map(|r| format!(" (role_id:{r})")).unwrap_or_default()), true)
        .field("Kiedy", format!("<t:{now}:F> • <t:{now}:R>"), true)
        .field("Powód", format!("```{}```", truncate(reason, 900)), false)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    if minutes > 0 { e = e.field("Czas", human_minutes(minutes), true); }
    if let Some(ev) = evidence { e = e.field("Dowód", ev.to_string(), false); }

    if let Ok(user) = uid.to_user(&ctx.http).await {
        if let Some(avatar) = user.avatar_url() { e = e.thumbnail(avatar); }
    }
    e
}

async fn embed_unmuted(
    ctx: &Context,
    _gid: GuildId,
    mod_id: UserId,
    uid: UserId,
    reason: &str,
) -> CreateEmbed {
    let now = now_unix();
    let mut e = CreateEmbed::new()
        .title("🔊 Zdjęto wyciszenie")
        .colour(Colour::new(0x2ECC71))
        .field("Użytkownik", format!("<@{}> (`{}`)", uid.get(), uid.get()), true)
        .field("Moderator", format!("<@{}> (`{}`)", mod_id.get(), mod_id.get()), true)
        .field("Kiedy", format!("<t:{now}:F> • <t:{now}:R>"), true)
        .field("Powód", format!("```{}```", truncate(reason, 900)), false)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    if let Ok(user) = uid.to_user(&ctx.http).await {
        if let Some(avatar) = user.avatar_url() { e = e.thumbnail(avatar); }
    }
    e
}

/* ========================= DB + CFG + utils ========================= */

fn load_cfg_from_cache_or_default(gid: u64) -> MuteConfig {
    if let Some(c) = CFG.get(&gid) { return c.clone(); }
    let def = MuteConfig::default();
    CFG.insert(gid, def.clone());
    def
}

async fn load_cfg(db: &Pool<Postgres>, gid: u64) -> MuteConfig {
    if let Some(c) = CFG.get(&gid) { return c.clone(); }
    let row = sqlx::query("SELECT cfg FROM tss.mute_config WHERE guild_id=$1")
        .bind(gid as i64)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

    if let Some(r) = row {
        let val: serde_json::Value = r.try_get("cfg").unwrap();
        if let Ok(cfg) = serde_json::from_value::<MuteConfig>(val) {
            CFG.insert(gid, cfg.clone());
            return cfg;
        }
    }

    // fallback
    let def = MuteConfig::default();
    CFG.insert(gid, def.clone());
    let _ = save_cfg(db, gid, &def).await;
    def
}

async fn save_cfg(db: &Pool<Postgres>, gid: u64, cfg: &MuteConfig) -> Result<()> {
    let v = serde_json::to_value(cfg)?;
    sqlx::query(
        r#"
        INSERT INTO tss.mute_config (guild_id, cfg)
        VALUES ($1, $2)
        ON CONFLICT (guild_id) DO UPDATE SET cfg = EXCLUDED.cfg
        "#
    )
    .bind(gid as i64)
    .bind(v)
    .execute(db)
    .await?;
    Ok(())
}

fn parse_duration_minutes(s: &str) -> Option<i64> {
    let t = s.trim().to_lowercase();
    if t == "0" { return Some(0); }
    // jeśli czyste minuty
    if let Ok(n) = t.parse::<i64>() { return Some(n.max(0)); }

    let (num_part, unit) = t.split_at(t.len().saturating_sub(1));
    let n = num_part.parse::<i64>().ok()?;
    match unit {
        "m" => Some(n),
        "h" => Some(n.saturating_mul(60)),
        "d" => Some(n.saturating_mul(60 * 24)),
        _ => None,
    }
}

fn human_minutes(mins: i64) -> String {
    if mins < 60 { format!("{}m", mins) }
    else if mins % 60 == 0 { format!("{}h", mins / 60) }
    else { format!("{}h {}m", mins / 60, mins % 60) }
}

fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

async fn edit(ctx: &Context, cmd: &CommandInteraction, msg: &str) -> Result<()> {
    cmd.edit_response(&ctx.http, EditInteractionResponse::new().content(msg)).await?;
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() }
    else { let mut t = s[..max.saturating_sub(1)].to_string(); t.push('…'); t }
}

fn log_channel(app: &AppContext) -> Option<u64> {
    let env = app.env();
    let id = env_channels::logs::ban_kick_mute_id(&env);
    if id == 0 { None } else { Some(id) }
}

async fn moderate_permission(ctx: &Context, gid: GuildId, uid: UserId) -> bool {
    if let Ok(m) = gid.member(&ctx.http, uid).await {
        #[allow(deprecated)]
        if let Ok(p) = m.permissions(&ctx.cache) {
            return p.moderate_members() || p.kick_members() || p.administrator();
        }
    }
    false
}
async fn admin_permission(ctx: &Context, gid: GuildId, uid: UserId) -> bool {
    if let Ok(m) = gid.member(&ctx.http, uid).await {
        #[allow(deprecated)]
        if let Ok(p) = m.permissions(&ctx.cache) {
            return p.administrator();
        }
    }
    false
}
