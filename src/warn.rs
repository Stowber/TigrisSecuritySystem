// src/warn.rs

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use once_cell::sync::Lazy;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use sqlx::{Pool, Postgres, Row};
use chrono::Utc;
use serenity::all::*;

use crate::{
    AppContext,
    registry::env_channels, // kanał logów LOGS_BAN_KICK_MUTE
};

/* =========================
   Konfiguracja & typy
   ========================= */

const SYSTEM_NAME: &str = "Tigris Warn System";
const _SERVER_NAME: &str = "Unfaithful";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarnConfig {
    pub decay_days: i32,      // po ilu dniach punkty „wygasają”
    pub timeout_pts: i32,     // suma punktów => timeout
    pub timeout_hours: i32,   // ile godzin timeout
    pub kick_pts: i32,        // suma punktów => kick
    pub ban_pts: i32,         // suma punktów => ban
}
impl Default for WarnConfig {
    fn default() -> Self {
        Self {
            decay_days: 30,
            timeout_pts: 3,
            timeout_hours: 12,
            kick_pts: 6,
            ban_pts: 9,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarnCase {
    pub id: i64,
    pub guild_id: u64,
    pub user_id: u64,
    pub moderator_id: u64,
    pub points: i32,
    pub reason: String,
    pub evidence: Option<String>,
    /// Unix seconds (pochodzi z EXTRACT(EPOCH FROM created_at))
    pub created_at: i64,
}

/* =========================
   Cache per-guild
   ========================= */

static CFG: Lazy<DashMap<u64, WarnConfig>> = Lazy::new(DashMap::new);

/* =========================
   Public API
   ========================= */

pub struct Warns;

impl Warns {
    /// Awaryjne tworzenie tabel oraz brakujących kolumn (idempotentnie).
    pub async fn ensure_tables(db: &Pool<Postgres>) -> Result<()> {
        // 1) Schemat
        sqlx::query(r#"CREATE SCHEMA IF NOT EXISTS tss;"#)
            .execute(db).await?;

        // 2) Tabela spraw – jeśli kiedyś była bez soft-delete, to niżej dołożymy kolumny
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tss.warn_cases (
              id            BIGSERIAL PRIMARY KEY,
              guild_id      BIGINT    NOT NULL,
              user_id       BIGINT    NOT NULL,
              moderator_id  BIGINT    NOT NULL,
              points        INTEGER   NOT NULL,
              reason        TEXT      NOT NULL,
              evidence      TEXT,
              created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
            );
            "#
        ).execute(db).await?;

        // 2a) Dołóż brakujące kolumny soft-delete (bez zmiany starej migracji)
        sqlx::query(
            r#"
            ALTER TABLE tss.warn_cases
                ADD COLUMN IF NOT EXISTS deleted_at    TIMESTAMPTZ NULL,
                ADD COLUMN IF NOT EXISTS deleted_by    BIGINT      NULL,
                ADD COLUMN IF NOT EXISTS delete_reason TEXT        NULL;
            "#
        ).execute(db).await?;

        // 2b) Indeksy (idempotentnie)
        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_warn_cases_guild_user
              ON tss.warn_cases (guild_id, user_id);

            CREATE INDEX IF NOT EXISTS idx_warn_cases_guild_created
              ON tss.warn_cases (guild_id, created_at DESC);
            "#
        ).execute(db).await?;

        // 3) Konfiguracja – prosta tabela z JSON-em cfg
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tss.warn_config (
              guild_id BIGINT PRIMARY KEY,
              cfg JSONB NOT NULL DEFAULT '{}'::jsonb
            );
            "#
        ).execute(db).await?;

        Ok(())
    }

    pub async fn register_commands(ctx: &Context, gid: GuildId) -> Result<()> {
        // /warn
        gid.create_command(&ctx.http,
            CreateCommand::new("warn")
                .description("Nadaj ostrzeżenie (punkty)")
                .add_option(CreateCommandOption::new(CommandOptionType::User, "user", "Kogo ostrzec").required(true))
                .add_option(CreateCommandOption::new(CommandOptionType::String, "reason", "Powód").required(true))
                .add_option(CreateCommandOption::new(CommandOptionType::Integer, "points", "Ile punktów (domyślnie 1)"))
                .add_option(CreateCommandOption::new(CommandOptionType::String, "evidence", "Dowód/URL (opcjonalnie)"))
                .default_member_permissions(Permissions::MODERATE_MEMBERS)
        ).await?;

        // /warns
        gid.create_command(&ctx.http,
            CreateCommand::new("warns")
                .description("Pokaż ostrzeżenia użytkownika")
                .add_option(CreateCommandOption::new(CommandOptionType::User, "user", "Użytkownik").required(true))
        ).await?;

        // /warn-remove
        gid.create_command(&ctx.http,
            CreateCommand::new("warn-remove")
                .description("Usuń (un-warn) po ID sprawy")
                .add_option(CreateCommandOption::new(CommandOptionType::Integer, "case_id", "ID ostrzeżenia").required(true))
                .add_option(CreateCommandOption::new(CommandOptionType::String, "reason", "Powód usunięcia").required(true))
                .default_member_permissions(Permissions::MODERATE_MEMBERS)
        ).await?;

        // /warn-config set …
        gid.create_command(&ctx.http,
            CreateCommand::new("warn-config")
                .description("Konfiguracja warnów (progi/wygaśnięcie)")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::SubCommand, "set", "Ustaw parametry")
                        .add_sub_option(CreateCommandOption::new(CommandOptionType::Integer, "decay_days", "Wygasanie punktów (dni)"))
                        .add_sub_option(CreateCommandOption::new(CommandOptionType::Integer, "timeout_pts", "Punkty do timeoutu"))
                        .add_sub_option(CreateCommandOption::new(CommandOptionType::Integer, "timeout_hours", "Długość timeoutu (h)"))
                        .add_sub_option(CreateCommandOption::new(CommandOptionType::Integer, "kick_pts", "Punkty do kicka"))
                        .add_sub_option(CreateCommandOption::new(CommandOptionType::Integer, "ban_pts", "Punkty do bana"))
                )
                .default_member_permissions(Permissions::ADMINISTRATOR)
        ).await?;

        Ok(())
    }

    pub async fn on_interaction(ctx: &Context, app: &AppContext, interaction: Interaction) {
        if let Some(cmd) = interaction.clone().command() {
            match cmd.data.name.as_str() {
                "warn"         => { if let Err(e) = handle_warn(ctx, app, &cmd).await        { tracing::warn!(?e, "warn failed"); } }
                "warns"        => { if let Err(e) = handle_warns(ctx, app, &cmd).await       { tracing::warn!(?e, "warns failed"); } }
                "warn-remove"  => { if let Err(e) = handle_warn_remove(ctx, app, &cmd).await { tracing::warn!(?e, "warn-remove failed"); } }
                "warn-config"  => { if let Err(e) = handle_warn_config(ctx, app, &cmd).await { tracing::warn!(?e, "warn-config failed"); } }
                _ => {}
            }
        }
    }
}

/* =========================
   Slash handlers
   ========================= */

async fn handle_warn(ctx: &Context, app: &AppContext, cmd: &CommandInteraction) -> Result<()> {
    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true))
    ).await?;

    let Some(gid) = cmd.guild_id else { return edit_ephemeral(ctx, cmd, "Użyj na serwerze.").await; };

    // ACL
    if !moderate_permission(ctx, gid, cmd.user.id).await {
        return edit_ephemeral(ctx, cmd, "⛔ Brak uprawnień.").await;
    }

    // args
    let mut user: Option<UserId> = None;
    let mut reason: Option<String> = None;
    let mut points: i32 = 1;
    let mut evidence: Option<String> = None;

    for o in &cmd.data.options {
        match (&o.name[..], &o.value) {
            ("user",     CommandDataOptionValue::User(u))     => user = Some(*u),
            ("reason",   CommandDataOptionValue::String(s))   => reason = Some(s.clone()),
            ("points",   CommandDataOptionValue::Integer(n))  => points = *n as i32,
            ("evidence", CommandDataOptionValue::String(s))   => evidence = Some(s.clone()),
            _ => {}
        }
    }

    let Some(uid) = user else { return edit_ephemeral(ctx, cmd, "Musisz wskazać użytkownika.").await; };
    let reason_text = reason.unwrap_or_else(|| "Brak powodu".into());
    if uid.get() == ctx.cache.current_user().id.get() || uid == cmd.user.id {
        return edit_ephemeral(ctx, cmd, "Nie można wystawić ostrzeżenia temu użytkownikowi.").await;
    }

    // zapis sprawy (created_at: DEFAULT now())
    let case_id = insert_warn(&app.db, gid.get(), uid.get(), cmd.user.id.get(), points, &reason_text, evidence.as_deref()).await?;

    // policz punkty (po wygaszeniu) i ewentualnie eskaluj
    let cfg = load_cfg(&app.db, gid.get()).await;
    let sum_active = sum_active_points(&app.db, gid.get(), uid.get(), cfg.decay_days).await?;

    // DM
    let _ = dm_warn(ctx, uid, points, &reason_text, evidence.as_deref()).await;

    // ewentualne auto-akcje
    let action_taken = escalate_if_needed(ctx, gid, uid, &cfg, sum_active, &reason_text).await;

    // log
    if let Some(log_ch) = log_channel(&app) {
        let embed = log_embed_warn(ctx, gid, cmd.user.id, uid, points, &reason_text, evidence.as_deref(), sum_active, &cfg).await;
        let _ = ChannelId::new(log_ch).send_message(&ctx.http, CreateMessage::new().embed(embed)).await;
    }

    // potwierdzenie
    let conf = confirm_embed_warn(ctx, gid, cmd.user.id, uid, case_id, points, &reason_text, evidence.as_deref(), sum_active, &cfg, action_taken.as_deref()).await;
    cmd.edit_response(&ctx.http, EditInteractionResponse::new().embeds(vec![conf])).await?;
    Ok(())
}

async fn handle_warns(ctx: &Context, app: &AppContext, cmd: &CommandInteraction) -> Result<()> {
    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true))
    ).await?;

    let Some(gid) = cmd.guild_id else { return edit_ephemeral(ctx, cmd, "Użyj na serwerze.").await; };
    let mut user: Option<UserId> = None;
    for o in &cmd.data.options {
        if o.name == "user" {
            if let CommandDataOptionValue::User(u) = &o.value { user = Some(*u); }
        }
    }
    let Some(uid) = user else { return edit_ephemeral(ctx, cmd, "Wskaż użytkownika.").await; };

    let cfg = load_cfg(&app.db, gid.get()).await;
    let list = list_active_warns(&app.db, gid.get(), uid.get(), cfg.decay_days, 10).await?;
    let total = sum_active_points(&app.db, gid.get(), uid.get(), cfg.decay_days).await?;

    let mut e = CreateEmbed::new()
        .title("📒 Ostrzeżenia użytkownika")
        .colour(Colour::new(0x3498DB))
        .field("Użytkownik", format!("<@{}>", uid.get()), true)
        .field("Suma aktywnych punktów", format!("**{}**", total), true)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    if list.is_empty() {
        e = e.description("_Brak aktywnych ostrzeżeń (po wygaszeniu)._");
    } else {
        let mut lines = Vec::new();
        for c in list {
            lines.push(format!("`#{}` • **{}p** • <t:{}:R>\n{}", c.id, c.points, c.created_at, truncate(&c.reason, 180)));
        }
        e = e.description(lines.join("\n\n"));
    }

    cmd.edit_response(&ctx.http, EditInteractionResponse::new().embeds(vec![e])).await?;
    Ok(())
}

async fn handle_warn_remove(ctx: &Context, app: &AppContext, cmd: &CommandInteraction) -> Result<()> {
    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true))
    ).await?;

    let Some(gid) = cmd.guild_id else { return edit_ephemeral(ctx, cmd, "Użyj na serwerze.").await; };
    if !moderate_permission(ctx, gid, cmd.user.id).await {
        return edit_ephemeral(ctx, cmd, "⛔ Brak uprawnień.").await;
    }

    let mut case_id: Option<i64> = None;
    let mut reason: Option<String> = None;
    for o in &cmd.data.options {
        match (&o.name[..], &o.value) {
            ("case_id", CommandDataOptionValue::Integer(n)) => case_id = Some(*n as i64),
            ("reason",  CommandDataOptionValue::String(s))   => reason = Some(s.clone()),
            _ => {}
        }
    }
    let Some(cid) = case_id else { return edit_ephemeral(ctx, cmd, "Podaj `case_id`.").await; };
    let del_rs = reason.unwrap_or_else(|| "moderator remove".into());

    if let Some((uid, _pts)) = soft_delete_warn(&app.db, gid.get(), cid, cmd.user.id.get(), &del_rs).await? {
        // potwierdzenie
        let e = CreateEmbed::new()
            .title("✅ Ostrzeżenie usunięte")
            .colour(Colour::new(0x2ECC71))
            .field("Case", format!("#{}", cid), true)
            .field("Użytkownik", format!("<@{}>", uid), true)
            .field("Powód usunięcia", format!("```{}```", truncate(&del_rs, 900)), false)
            .footer(CreateEmbedFooter::new(SYSTEM_NAME));
        cmd.edit_response(&ctx.http, EditInteractionResponse::new().embeds(vec![e])).await?;

        // log
        if let Some(log_ch) = log_channel(&app) {
            let embed = log_embed_unwarn(ctx, gid, cmd.user.id, UserId::new(uid), cid, &del_rs).await;
            let _ = ChannelId::new(log_ch).send_message(&ctx.http, CreateMessage::new().embed(embed)).await;
        }
    } else {
        edit_ephemeral(ctx, cmd, "Nie znaleziono sprawy lub już usunięta.").await?;
    }
    Ok(())
}

async fn handle_warn_config(ctx: &Context, app: &AppContext, cmd: &CommandInteraction) -> Result<()> {
    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true))
    ).await?;
    let Some(gid) = cmd.guild_id else { return edit_ephemeral(ctx, cmd, "Użyj na serwerze.").await; };
    if !admin_permission(ctx, gid, cmd.user.id).await {
        return edit_ephemeral(ctx, cmd, "⛔ Tylko administrator.").await;
    }

    // odczyt aktualnego
    let mut cfg = load_cfg_from_cache_or_default(gid.get());

    // subcommand set
    if let Some(sc) = cmd.data.options.first() {
        if sc.name == "set" {
            if let CommandDataOptionValue::SubCommand(params) = &sc.value {
                for p in params {
                    match (&p.name[..], &p.value) {
                        ("decay_days",    CommandDataOptionValue::Integer(n)) => cfg.decay_days    = *n as i32,
                        ("timeout_pts",   CommandDataOptionValue::Integer(n)) => cfg.timeout_pts   = *n as i32,
                        ("timeout_hours", CommandDataOptionValue::Integer(n)) => cfg.timeout_hours = *n as i32,
                        ("kick_pts",      CommandDataOptionValue::Integer(n)) => cfg.kick_pts      = *n as i32,
                        ("ban_pts",       CommandDataOptionValue::Integer(n)) => cfg.ban_pts       = *n as i32,
                        _ => {}
                    }
                }
            }
        }
    }

    // zapis do DB + odświeżenie cache
    save_cfg(&app.db, gid.get(), &cfg).await?;
    CFG.insert(gid.get(), cfg.clone());

    // embed potwierdzający (ephemeral)
    let e = CreateEmbed::new()
        .title("⚙️ Zapisano konfigurację warnów")
        .colour(Colour::new(0x95A5A6))
        .description(format!(
            "- Wygasanie: **{} dni**\n- Timeout: **{}p** na **{}h**\n- Kick: **{}p**\n- Ban: **{}p**",
            cfg.decay_days, cfg.timeout_pts, cfg.timeout_hours, cfg.kick_pts, cfg.ban_pts
        ))
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));
    cmd.edit_response(&ctx.http, EditInteractionResponse::new().embeds(vec![e])).await?;

    // log do kanału
    if let Some(log_ch) = log_channel(&app) {
        let embed = log_embed_config_change(gid, cmd.user.id, &cfg);
        let _ = ChannelId::new(log_ch)
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }

    Ok(())
}

/* =========================
   Embeds / DM / Logs
   ========================= */

async fn dm_warn(ctx: &Context, uid: UserId, pts: i32, reason: &str, evidence: Option<&str>) -> Result<()> {
    let user = uid.to_user(&ctx.http).await?;
    let mut e = CreateEmbed::new()
        .title("Ostrzeżenie")
        .colour(Colour::new(0xE67E22))
        .description("Otrzymujesz ostrzeżenie od zespołu moderacji.")
        .field("Punkty", format!("**{}**", pts), true)
        .field("Powód", format!("```{}```", truncate(reason, 900)), false)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));
    if let Some(ev) = evidence { e = e.field("Dowód", ev.to_string(), false); }
    if let Some(avatar) = user.avatar_url() { e = e.thumbnail(avatar); }

    let dm = user.create_dm_channel(&ctx.http).await?;
    let _ = dm.send_message(&ctx.http, CreateMessage::new().embed(e)).await;
    Ok(())
}

async fn log_embed_warn(
    ctx: &Context,
    _gid: GuildId,
    mod_id: UserId,
    user_id: UserId,
    pts: i32,
    reason: &str,
    evidence: Option<&str>,
    sum_after: i32,
    cfg: &WarnConfig,
) -> CreateEmbed {
    let now = now_unix();
    let mut e = CreateEmbed::new()
        .title("⚠️ Nadano ostrzeżenie")
        .colour(Colour::new(0xE67E22))
        .field("Użytkownik", format!("<@{}> (`{}`)", user_id.get(), user_id.get()), true)
        .field("Moderator", format!("<@{}> (`{}`)", mod_id.get(), mod_id.get()), true)
        .field("Punkty", format!("**{}**  → suma: **{}**", pts, sum_after), true)
        .field("Kiedy", format!("<t:{now}:F> • <t:{now}:R>"), true)
        .field("Powód", format!("```{}```", truncate(reason, 1400)), false)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    if let Some(ev) = evidence { e = e.field("Dowód", ev.to_string(), false); }

    if let Ok(user) = user_id.to_user(&ctx.http).await {
        if let Some(avatar) = user.avatar_url() { e = e.thumbnail(avatar); }
    }

    // sugestia progów
    e = e.description(format!(
        "_Progi:_ timeout **{}p/{}h**, kick **{}p**, ban **{}p** (wygasanie: **{} dni**).",
        cfg.timeout_pts, cfg.timeout_hours, cfg.kick_pts, cfg.ban_pts, cfg.decay_days
    ));

    e
}

async fn confirm_embed_warn(
    ctx: &Context,
    _gid: GuildId,
    _mod_id: UserId,
    uid: UserId,
    case_id: i64,
    pts: i32,
    reason: &str,
    evidence: Option<&str>,
    sum_after: i32,
    cfg: &WarnConfig,
    action_taken: Option<&str>,
) -> CreateEmbed {
    let mut e = CreateEmbed::new()
        .title("✅ Ostrzeżenie nadane")
        .colour(Colour::new(0x2ECC71))
        .field("Case", format!("#{}", case_id), true)
        .field("Użytkownik", format!("<@{}>", uid.get()), true)
        .field("Punkty", format!("**{}**  → suma aktywnych: **{}**", pts, sum_after), true)
        .field("Powód", format!("```{}```", truncate(reason, 900)), false)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    if let Some(ev) = evidence { e = e.field("Dowód", ev.to_string(), false); }
    if let Some(act) = action_taken { e = e.field("Eskalacja", format!("**{}**", act), false); }

    if let Ok(user) = uid.to_user(&ctx.http).await {
        if let Some(avatar) = user.avatar_url() { e = e.thumbnail(avatar); }
    }

    e = e.description(format!(
        "_Progi:_ timeout **{}p/{}h**, kick **{}p**, ban **{}p** (wygasanie: **{} dni**).",
        cfg.timeout_pts, cfg.timeout_hours, cfg.kick_pts, cfg.ban_pts, cfg.decay_days
    ));

    e
}

/// Log: zdjęcie warna (soft-delete)
async fn log_embed_unwarn(
    ctx: &Context,
    gid: GuildId,
    mod_id: UserId,
    user_id: UserId,
    case_id: i64,
    reason: &str,
) -> CreateEmbed {
    let now = now_unix();
    let mut e = CreateEmbed::new()
        .title("🧹 Usunięto ostrzeżenie (un-warn)")
        .colour(Colour::new(0x27AE60))
        .field("Gildia", format!("{}", gid.get()), true)
        .field("Case", format!("#{}", case_id), true)
        .field("Użytkownik", format!("<@{}> (`{}`)", user_id.get(), user_id.get()), true)
        .field("Moderator", format!("<@{}> (`{}`)", mod_id.get(), mod_id.get()), true)
        .field("Kiedy", format!("<t:{now}:F> • <t:{now}:R>"), true)
        .field("Powód usunięcia", format!("```{}```", truncate(reason, 1000)), false)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    if let Ok(user) = user_id.to_user(&ctx.http).await {
        if let Some(avatar) = user.avatar_url() { e = e.thumbnail(avatar); }
    }
    e
}

/// Log: zmiana konfiguracji warnów
fn log_embed_config_change(gid: GuildId, admin_id: UserId, cfg: &WarnConfig) -> CreateEmbed {
    let now = now_unix();
    CreateEmbed::new()
        .title("🛠️ Zmieniono konfigurację warnów")
        .colour(Colour::new(0x95A5A6))
        .field("Gildia", format!("{}", gid.get()), true)
        .field("Admin", format!("<@{}> (`{}`)", admin_id.get(), admin_id.get()), true)
        .field("Kiedy", format!("<t:{now}:F> • <t:{now}:R>"), true)
        .field("Nowe wartości", format!(
            "- Wygasanie: **{} dni**\n- Timeout: **{}p** na **{}h**\n- Kick: **{}p**\n- Ban: **{}p**",
            cfg.decay_days, cfg.timeout_pts, cfg.timeout_hours, cfg.kick_pts, cfg.ban_pts
        ), false)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME))
}

/* =========================
   Eskalacja
   ========================= */

async fn escalate_if_needed(
    ctx: &Context,
    gid: GuildId,
    uid: UserId,
    cfg: &WarnConfig,
    total_points: i32,
    reason: &str,
) -> Option<String> {
    // priorytet: ban > kick > timeout
    if total_points >= cfg.ban_pts {
        let _ = gid.ban_with_reason(&ctx.http, uid, 0, &format!("[{}] Eskalacja: ban (warns)", SYSTEM_NAME)).await;
        return Some(format!("**BAN** (próg {}p) – powód: {}", cfg.ban_pts, reason));
    }
    if total_points >= cfg.kick_pts {
        let _ = gid.kick_with_reason(&ctx.http, uid, &format!("[{}] Eskalacja: kick (warns)", SYSTEM_NAME)).await;
        return Some(format!("**KICK** (próg {}p)", cfg.kick_pts));
    }
    if total_points >= cfg.timeout_pts {
        // timeout (communication disabled)
        if let Ok(mut member) = gid.member(&ctx.http, uid).await {
            let until = Utc::now() + chrono::Duration::hours(cfg.timeout_hours as i64);
            let _ = member.disable_communication_until_datetime(&ctx.http, until.into()).await;
            return Some(format!("**TIMEOUT** {}h (próg {}p)", cfg.timeout_hours, cfg.timeout_pts));
        }
    }
    None
}

/* =========================
   DB + Config + Utils
   ========================= */

async fn insert_warn(
    db: &Pool<Postgres>,
    gid: u64,
    uid: u64,
    mod_id: u64,
    pts: i32,
    reason: &str,
    evidence: Option<&str>
) -> Result<i64> {
    let id: i64 = sqlx::query_scalar(
        r#"
        INSERT INTO tss.warn_cases
            (guild_id, user_id, moderator_id, points, reason, evidence)
        VALUES ($1,$2,$3,$4,$5,$6)
        RETURNING id
        "#
    )
    .bind(gid as i64)
    .bind(uid as i64)
    .bind(mod_id as i64)
    .bind(pts)
    .bind(reason)
    .bind(evidence)
    .fetch_one(db)
    .await?;
    Ok(id)
}

async fn list_active_warns(
    db: &Pool<Postgres>,
    gid: u64,
    uid: u64,
    decay_days: i32,
    limit: i64
) -> Result<Vec<WarnCase>> {
    let cutoff_unix: i64 = now_unix() - (decay_days as i64 * 86_400);
    let rows = sqlx::query(
        r#"
        SELECT
            id,
            guild_id,
            user_id,
            moderator_id,
            points,
            reason,
            evidence,
            EXTRACT(EPOCH FROM created_at)::BIGINT AS created_at_unix
        FROM tss.warn_cases
        WHERE guild_id = $1
          AND user_id = $2
          AND deleted_at IS NULL
          AND created_at >= to_timestamp($3)
        ORDER BY created_at DESC
        LIMIT $4
        "#
    )
    .bind(gid as i64)
    .bind(uid as i64)
    .bind(cutoff_unix)
    .bind(limit)
    .fetch_all(db)
    .await?;

    let list = rows.into_iter().map(|r| WarnCase {
        id:            r.try_get::<i64, _>("id").unwrap(),
        guild_id:      r.try_get::<i64, _>("guild_id").unwrap() as u64,
        user_id:       r.try_get::<i64, _>("user_id").unwrap() as u64,
        moderator_id:  r.try_get::<i64, _>("moderator_id").unwrap() as u64,
        points:        r.try_get::<i32, _>("points").unwrap(),
        reason:        r.try_get::<String, _>("reason").unwrap(),
        evidence:      r.try_get::<Option<String>, _>("evidence").unwrap(),
        created_at:    r.try_get::<i64, _>("created_at_unix").unwrap(),
    }).collect();

    Ok(list)
}

async fn sum_active_points(
    db: &Pool<Postgres>,
    gid: u64,
    uid: u64,
    decay_days: i32
) -> Result<i32> {
    let cutoff_unix: i64 = now_unix() - (decay_days as i64 * 86_400);
    let row = sqlx::query(
        r#"
        SELECT COALESCE(SUM(points), 0) AS s
        FROM tss.warn_cases
        WHERE guild_id = $1
          AND user_id = $2
          AND deleted_at IS NULL
          AND created_at >= to_timestamp($3)
        "#
    )
    .bind(gid as i64)
    .bind(uid as i64)
    .bind(cutoff_unix)
    .fetch_one(db)
    .await?;
    let s: i64 = row.try_get("s").unwrap_or(0);
    Ok(s as i32)
}

async fn soft_delete_warn(
    db: &Pool<Postgres>,
    gid: u64,
    case_id: i64,
    by: u64,
    why: &str
) -> Result<Option<(u64, i32)>> {
    let row = sqlx::query(
        r#"
        UPDATE tss.warn_cases
           SET deleted_at = now(),
               deleted_by = $1,
               delete_reason = $2
         WHERE id = $3
           AND guild_id = $4
           AND deleted_at IS NULL
        RETURNING user_id, points
        "#
    )
    .bind(by as i64)
    .bind(why)
    .bind(case_id)
    .bind(gid as i64)
    .fetch_optional(db)
    .await?;

    if let Some(r) = row {
        let uid: i64 = r.try_get("user_id")?;
        let pts: i32 = r.try_get("points")?;
        Ok(Some((uid as u64, pts)))
    } else {
        Ok(None)
    }
}

fn load_cfg_from_cache_or_default(gid: u64) -> WarnConfig {
    if let Some(c) = CFG.get(&gid) { return c.clone(); }
    let def = WarnConfig::default();
    CFG.insert(gid, def.clone());
    def
}

async fn load_cfg(db: &Pool<Postgres>, gid: u64) -> WarnConfig {
    if let Some(c) = CFG.get(&gid) { return c.clone(); }
    let row = sqlx::query("SELECT cfg FROM tss.warn_config WHERE guild_id=$1")
        .bind(gid as i64)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    if let Some(r) = row {
        let val: serde_json::Value = r.try_get("cfg").unwrap();
        if let Ok(cfg) = serde_json::from_value::<WarnConfig>(val) {
            CFG.insert(gid, cfg.clone());
            return cfg;
        }
    }
    let def = WarnConfig::default();
    CFG.insert(gid, def.clone());
    let _ = save_cfg(db, gid, &def).await;
    def
}

async fn save_cfg(db: &Pool<Postgres>, gid: u64, cfg: &WarnConfig) -> Result<()> {
    let v = serde_json::to_value(cfg)?;
    sqlx::query(
        r#"
        INSERT INTO tss.warn_config (guild_id, cfg)
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

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

async fn edit_ephemeral(ctx: &Context, cmd: &CommandInteraction, msg: &str) -> Result<()> {
    cmd.edit_response(&ctx.http, EditInteractionResponse::new().content(msg)).await?;
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut t = s[..max.saturating_sub(1)].to_string();
        t.push('…');
        t
    }
}

fn log_channel(app: &AppContext) -> Option<u64> {
    let env = app.env();
    let id = env_channels::logs::ban_kick_mute_id(&env);
    if id == 0 { None } else { Some(id) }
}

async fn moderate_permission(ctx: &Context, gid: GuildId, uid: UserId) -> bool {
    if let Ok(m) = gid.member(&ctx.http, uid).await {
        if let Ok(p) = m.permissions(&ctx.cache) {
            return p.moderate_members() || p.kick_members() || p.administrator();
        }
    }
    false
}
async fn admin_permission(ctx: &Context, gid: GuildId, uid: UserId) -> bool {
    if let Ok(m) = gid.member(&ctx.http, uid).await {
        if let Ok(p) = m.permissions(&ctx.cache) {
            return p.administrator();
        }
    }
    false
}
