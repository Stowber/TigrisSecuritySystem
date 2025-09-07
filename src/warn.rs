use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serenity::all::*;
use sqlx::{Pool, Postgres, Row};
use crate::env_roles;
use crate::admin_points::AdminPoints;

use crate::{registry::env_channels, AppContext, watchlist::Watchlist};

const SYSTEM_NAME: &str = "Tigris Warn System";
const DECAY_DAYS: i64 = 30; // warn wygasa po tylu dniach

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarnCase {
    pub id: i64,
    pub guild_id: u64,
    pub user_id: u64,
    pub moderator_id: u64,
    pub reason: String,
    pub evidence: Option<String>,
    pub created_at: i64,
}

pub struct Warns;

impl Warns {
    pub async fn ensure_tables(db: &Pool<Postgres>) -> Result<()> {
        sqlx::query(r#"CREATE SCHEMA IF NOT EXISTS tss;"#)
            .execute(db)
            .await?;

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
            "#,
        )
        .execute(db)
        .await?;

        sqlx::query(
            r#"
            ALTER TABLE tss.warn_cases
                ADD COLUMN IF NOT EXISTS deleted_at    TIMESTAMPTZ NULL,
                ADD COLUMN IF NOT EXISTS deleted_by    BIGINT      NULL,
                ADD COLUMN IF NOT EXISTS delete_reason TEXT        NULL;
            "#,
        )
        .execute(db)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_warn_cases_guild_user
              ON tss.warn_cases (guild_id, user_id);

            CREATE INDEX IF NOT EXISTS idx_warn_cases_guild_created
              ON tss.warn_cases (guild_id, created_at DESC);
            "#,
        )
        .execute(db)
        .await?;

        Ok(())
    }

    pub async fn register_commands(ctx: &Context, gid: GuildId) -> Result<()> {
        gid.create_command(
            &ctx.http,
            CreateCommand::new("warn")
                .description("Nadaj ostrzeżenie")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::User, "user", "Kogo ostrzec")
                        .required(true),
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "reason", "Powód")
                        .required(true),
                )
                .add_option(CreateCommandOption::new(
                    CommandOptionType::String,
                    "evidence",
                    "Dowód/URL (opcjonalnie)",
                ))
                .default_member_permissions(Permissions::MODERATE_MEMBERS),
        )
        .await?;

        gid.create_command(
            &ctx.http,
            CreateCommand::new("warns")
                .description("Pokaż ostrzeżenia użytkownika")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::User, "user", "Użytkownik")
                        .required(true),
                ),
        )
        .await?;

        gid.create_command(
            &ctx.http,
            CreateCommand::new("warn-remove")
                .description("Usuń (un-warn) po ID sprawy")
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::Integer,
                        "case_id",
                        "ID ostrzeżenia",
                    )
                    .required(true),
                )
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::String,
                        "reason",
                        "Powód usunięcia",
                    )
                    .required(true),
                )
                .default_member_permissions(Permissions::MODERATE_MEMBERS),
        )
        .await?;

        Ok(())
    }

    pub async fn on_interaction(ctx: &Context, app: &AppContext, interaction: Interaction) {
        if let Some(cmd) = interaction.clone().command() {
            match cmd.data.name.as_str() {
                "warn" => {
                    if let Err(e) = handle_warn(ctx, app, &cmd).await {
                        tracing::warn!(?e, "warn failed");
                    }
                }
                "warns" => {
                    if let Err(e) = handle_warns(ctx, app, &cmd).await {
                        tracing::warn!(?e, "warns failed");
                    }
                }
                "warn-remove" => {
                    if let Err(e) = handle_warn_remove(ctx, app, &cmd).await {
                        tracing::warn!(?e, "warn-remove failed");
                    }
                }
                _ => {}
            }
        }
    }
}

// Slash handlers
async fn handle_warn(ctx: &Context, app: &AppContext, cmd: &CommandInteraction) -> Result<()> {
    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true)),
    )
    .await?;

    let Some(gid) = cmd.guild_id else {
        return edit_ephemeral(ctx, cmd, "Użyj na serwerze.").await;
    };

     if !has_permission(ctx, gid, cmd.user.id, crate::permissions::Permission::Warn).await {
        return edit_ephemeral(ctx, cmd, "⛔ Brak uprawnień.").await;
    }

    let mut user: Option<UserId> = None;
    let mut reason: Option<String> = None;
    let mut evidence: Option<String> = None;

    for o in &cmd.data.options {
        match (&o.name[..], &o.value) {
            ("user", CommandDataOptionValue::User(u)) => user = Some(*u),
            ("reason", CommandDataOptionValue::String(s)) => reason = Some(s.clone()),
            ("evidence", CommandDataOptionValue::String(s)) => evidence = Some(s.clone()),
            _ => {}
        }
    }

    let Some(uid) = user else {
        return edit_ephemeral(ctx, cmd, "Musisz wskazać użytkownika.").await;
    };
    let reason_text = reason.unwrap_or_else(|| "Brak powodu".into());
    if uid.get() == ctx.cache.current_user().id.get() || uid == cmd.user.id {
         return edit_ephemeral(
            ctx,
            cmd,
            "Nie można wystawić ostrzeżenia temu użytkownikowi.",
        )
        .await;
    }

    let env = std::env::var("TSS_ENV").unwrap_or_else(|_| "production".to_string());
    let Ok(guild) = gid.to_partial_guild(&ctx.http).await else {
        return edit_ephemeral(ctx, cmd, "⛔ Nie udało się odczytać danych serwera.").await;
    };
    let Ok(mod_m) = gid.member(&ctx.http, cmd.user.id).await else {
        return edit_ephemeral(ctx, cmd, "⛔ Nie udało się odczytać twoich ról.").await;
    };
    let Ok(tgt_m) = gid.member(&ctx.http, uid).await else {
        return edit_ephemeral(ctx, cmd, "⛔ Ten użytkownik nie jest na serwerze.").await;
    };

    let staff_roles = env_roles::staff_set(&env);
    let allowed_staff_warn = vec![
        env_roles::opiekun_id(&env),
        env_roles::technik_zarzad_id(&env),
        env_roles::co_owner_id(&env),
        env_roles::owner_id(&env),
    ];
    let target_is_staff =
        tgt_m.roles.iter().any(|rid| staff_roles.contains(&rid.get()));
    let mod_can_warn_staff =
        mod_m.roles.iter().any(|rid| allowed_staff_warn.contains(&rid.get()));
    if target_is_staff && !mod_can_warn_staff {
        return edit_ephemeral(ctx, cmd, "Nie możesz wystawić ostrzeżenia administracji.").await;
    }

    let top_pos = |m: &serenity::all::Member| {
        m.roles
            .iter()
            .filter_map(|rid| guild.roles.get(rid))
            .map(|r| r.position)
            .max()
            .unwrap_or(0)
    };
    if top_pos(&mod_m) <= top_pos(&tgt_m) {
        return edit_ephemeral(ctx, cmd, "Nie możesz wystawić ostrzeżenia temu użytkownikowi.").await;
    }

    let case_id = insert_warn(
        &app.db,
        gid.get(),
        uid.get(),
        cmd.user.id.get(),
        &reason_text,
        evidence.as_deref(),
    )
    .await?;

    // +1 punkt dla administratora za nadanie ostrzeżenia
    match AdminPoints::award_warn_given(&app.db, cmd.user.id.get()).await {
        Ok(total_after) => {
            tracing::info!(
                moderator_id = cmd.user.id.get(),
                delta = 1.0f32,
                total_after = %format!("{:.1}", total_after),
                "Warn given – points awarded",
            );
        }
        Err(e) => {
            tracing::warn!(error=?e, "AdminPoints.award_warn_given failed");
        }
    }


    let _ = dm_warn(ctx, uid, &reason_text, evidence.as_deref()).await;

    if let Some(log_ch) = log_channel(&app) {
        let embed = log_embed_warn(
            ctx,
            gid,
            cmd.user.id,
            uid,
            &reason_text,
            evidence.as_deref(),
        )
        .await;
        let _ = ChannelId::new(log_ch)
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }

    Watchlist::log_action(
        ctx,
        &app.db,
        gid.get(),
        uid.get(),
        Some(cmd.user.id.get()),
        &format!("Warn: {reason_text}"),
    )
    .await;

    let conf = confirm_embed_warn(ctx, uid, case_id, &reason_text, evidence.as_deref()).await;
    cmd.edit_response(&ctx.http, EditInteractionResponse::new().embeds(vec![conf]))
        .await?;
    Ok(())
}

async fn handle_warns(ctx: &Context, app: &AppContext, cmd: &CommandInteraction) -> Result<()> {
    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true)),
    )
    .await?;

    let Some(gid) = cmd.guild_id else {
        return edit_ephemeral(ctx, cmd, "Użyj na serwerze.").await;
    };
    if !has_permission(ctx, gid, cmd.user.id, crate::permissions::Permission::Warns).await {
        return edit_ephemeral(ctx, cmd, "⛔ Brak uprawnień.").await;
    }

    let mut user: Option<UserId> = None;
    for o in &cmd.data.options {
        if o.name == "user" {
            if let CommandDataOptionValue::User(u) = &o.value {
                user = Some(*u);
            }
        }
    }
    let Some(uid) = user else {
        return edit_ephemeral(ctx, cmd, "Wskaż użytkownika.").await;
    };

    let list = list_active_warns(&app.db, gid.get(), uid.get(), DECAY_DAYS, 10).await?;

    let mut e = CreateEmbed::new()
        .title("📒 Ostrzeżenia użytkownika")
        .colour(Colour::new(0x3498DB))
        .field("Użytkownik", format!("<@{}>", uid.get()), true)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    if list.is_empty() {
        e = e.description("_Brak aktywnych ostrzeżeń (po wygaszeniu)._ ");
    } else {
        let mut lines = Vec::new();
        for c in list {
            let expires_ts = c.created_at + (DECAY_DAYS * 86_400);
            let expires = DateTime::<Utc>::from_timestamp(expires_ts, 0)
                .unwrap()
                .format("%d/%m/%Y")
                .to_string();
            lines.push(format!(
                "`#{}` • wygasa: {}\n{}",
                c.id,
                expires,
                truncate(&c.reason, 180)
            ));
        }
        e = e.description(lines.join("\n\n"));
    }

    cmd.edit_response(&ctx.http, EditInteractionResponse::new().embeds(vec![e]))
        .await?;
    Ok(())
}

async fn handle_warn_remove(
    ctx: &Context,
    app: &AppContext,
    cmd: &CommandInteraction,
) -> Result<()> {
    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true)),
    )
    .await?;

    let Some(gid) = cmd.guild_id else {
        return edit_ephemeral(ctx, cmd, "Użyj na serwerze.").await;
    };
    if !has_permission(ctx, gid, cmd.user.id, crate::permissions::Permission::WarnRemove).await {
        return edit_ephemeral(ctx, cmd, "⛔ Brak uprawnień.").await;
    }

    let mut case_id: Option<i64> = None;
    let mut reason: Option<String> = None;
    for o in &cmd.data.options {
        match (&o.name[..], &o.value) {
            ("case_id", CommandDataOptionValue::Integer(n)) => case_id = Some(*n as i64),
            ("reason", CommandDataOptionValue::String(s)) => reason = Some(s.clone()),
            _ => {}
        }
    }
    let Some(cid) = case_id else {
        return edit_ephemeral(ctx, cmd, "Podaj `case_id`.").await;
    };
    let del_rs = reason.unwrap_or_else(|| "moderator remove".into());

    if let Some(uid) = soft_delete_warn(&app.db, gid.get(), cid, cmd.user.id.get(), &del_rs).await? {
        let user_id = UserId::new(uid);
        let _ = dm_unwarn(ctx, user_id, cmd.user.id, &del_rs).await;

        let e = CreateEmbed::new()
            .title("✅ Ostrzeżenie usunięte")
            .colour(Colour::new(0x2ECC71))
            .field("Case", format!("#{}", cid), true)
            .field("Użytkownik", format!("<@{}>", uid), true)
            .field(
                "Powód usunięcia",
                format!("```{}```", truncate(&del_rs, 900)),
                false,
            )
            .footer(CreateEmbedFooter::new(SYSTEM_NAME));
        cmd.edit_response(&ctx.http, EditInteractionResponse::new().embeds(vec![e]))
            .await?;

        if let Some(log_ch) = log_channel(&app) {
            let embed = log_embed_unwarn(ctx, gid, cmd.user.id, user_id, cid, &del_rs).await;
            let _ = ChannelId::new(log_ch)
                .send_message(&ctx.http, CreateMessage::new().embed(embed))
                .await;
        }
    } else {
        edit_ephemeral(ctx, cmd, "Nie znaleziono sprawy lub już usunięta.").await?;
    }
    Ok(())
}

// Embeds / DM / Logs

async fn dm_warn(ctx: &Context, uid: UserId, reason: &str, evidence: Option<&str>) -> Result<()> {
    let user = uid.to_user(&ctx.http).await?;
    let mut e = CreateEmbed::new()
        .title("Ostrzeżenie")
        .colour(Colour::new(0xE67E22))
        .description("Otrzymujesz ostrzeżenie od zespołu moderacji.")
        .field("Powód", format!("```{}```", truncate(reason, 900)), false)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));
    if let Some(ev) = evidence {
        e = e.field("Dowód", ev.to_string(), false);
    }
    if let Some(avatar) = user.avatar_url() {
        e = e.thumbnail(avatar);
    }

    let dm = user.create_dm_channel(&ctx.http).await?;
    let _ = dm
        .send_message(&ctx.http, CreateMessage::new().embed(e))
        .await;
    Ok(())
}

async fn dm_unwarn(ctx: &Context, uid: UserId, moderator: UserId, reason: &str) -> Result<()> {
    let user = uid.to_user(&ctx.http).await?;
    let mod_user = moderator.to_user(&ctx.http).await?;
    let mut e = CreateEmbed::new()
        .title("Ostrzeżenie usunięte")
        .colour(Colour::new(0x27AE60))
        .description(format!(
            "Twoje ostrzeżenie zostało zdjęte przez administratora <@{}>.",
            moderator.get()
        ))
        .field(
            "Powód usunięcia",
            format!("```{}```", truncate(reason, 900)),
            false,
        )
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));
    if let Some(avatar) = mod_user.avatar_url() {
        e = e.thumbnail(avatar);
    }

    let dm = user.create_dm_channel(&ctx.http).await?;
    let _ = dm
        .send_message(&ctx.http, CreateMessage::new().embed(e))
        .await;
    Ok(())
}

async fn log_embed_warn(
    ctx: &Context,
    _gid: GuildId,
    mod_id: UserId,
    user_id: UserId,
    reason: &str,
    evidence: Option<&str>,
) -> CreateEmbed {
    let now = now_unix();
    let mut e = CreateEmbed::new()
        .title("⚠️ Nadano ostrzeżenie")
        .colour(Colour::new(0xE67E22))
        .field(
            "Użytkownik",
            format!("<@{}> (`{}`)", user_id.get(), user_id.get()),
            true,
        )
        .field(
            "Moderator",
            format!("<@{}> (`{}`)", mod_id.get(), mod_id.get()),
            true,
        )
        .field("Kiedy", format!("<t:{now}:F> • <t:{now}:R>"), true)
        .field("Powód", format!("```{}```", truncate(reason, 1400)), false)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    if let Some(ev) = evidence {
        e = e.field("Dowód", ev.to_string(), false);
    }

    if let Ok(user) = user_id.to_user(&ctx.http).await {
        if let Some(avatar) = user.avatar_url() {
            e = e.thumbnail(avatar);
        }
    }
    e
}

async fn confirm_embed_warn(
    ctx: &Context,
    uid: UserId,
    case_id: i64,
    reason: &str,
    evidence: Option<&str>,
) -> CreateEmbed {
    let expires_ts = now_unix() + (DECAY_DAYS * 86_400);
    let expires = DateTime::<Utc>::from_timestamp(expires_ts, 0)
        .unwrap()
        .format("%d/%m/%Y")
        .to_string();
    let mut e = CreateEmbed::new()
        .title("✅ Ostrzeżenie nadane")
        .colour(Colour::new(0x2ECC71))
        .field("Case", format!("#{}", case_id), true)
        .field("Użytkownik", format!("<@{}>", uid.get()), true)
        .field("Wygasa", expires, true)
        .field("Powód", format!("```{}```", truncate(reason, 900)), false)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    if let Some(ev) = evidence {
        e = e.field("Dowód", ev.to_string(), false);
    }

    if let Ok(user) = uid.to_user(&ctx.http).await {
        if let Some(avatar) = user.avatar_url() {
            e = e.thumbnail(avatar);
        }
    }
    e
}

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
        .field(
            "Użytkownik",
            format!("<@{}> (`{}`)", user_id.get(), user_id.get()),
            true,
        )
        .field(
            "Moderator",
            format!("<@{}> (`{}`)", mod_id.get(), mod_id.get()),
            true,
        )
        .field("Kiedy", format!("<t:{now}:F> • <t:{now}:R>"), true)
        .field(
            "Powód usunięcia",
            format!("```{}```", truncate(reason, 1000)),
            false,
        )
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    if let Ok(user) = user_id.to_user(&ctx.http).await {
        if let Some(avatar) = user.avatar_url() {
            e = e.thumbnail(avatar);
        }
    }
    e
}

// DB + Utils

async fn insert_warn(
    db: &Pool<Postgres>,
    gid: u64,
    uid: u64,
    mod_id: u64,
    reason: &str,
    evidence: Option<&str>,
) -> Result<i64> {
    let id: i64 = sqlx::query_scalar(
        r#"
        INSERT INTO tss.warn_cases
            (guild_id, user_id, moderator_id, points, reason, evidence)
        VALUES ($1,$2,$3,1,$4,$5)
        RETURNING id
        "#,
    )
    .bind(gid as i64)
    .bind(uid as i64)
    .bind(mod_id as i64)
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
    decay_days: i64,
    limit: i64,
) -> Result<Vec<WarnCase>> {
    let cutoff_unix: i64 = now_unix() - (decay_days * 86_400);
    let rows = sqlx::query(
        r#"
        SELECT
            id,
            guild_id,
            user_id,
            moderator_id,
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
        "#,
    )
    .bind(gid as i64)
    .bind(uid as i64)
    .bind(cutoff_unix)
    .bind(limit)
    .fetch_all(db)
    .await?;

    let list = rows
        .into_iter()
        .map(|r| WarnCase {
            id: r.try_get::<i64, _>("id").unwrap(),
            guild_id: r.try_get::<i64, _>("guild_id").unwrap() as u64,
            user_id: r.try_get::<i64, _>("user_id").unwrap() as u64,
            moderator_id: r.try_get::<i64, _>("moderator_id").unwrap() as u64,
            reason: r.try_get::<String, _>("reason").unwrap(),
            evidence: r.try_get::<Option<String>, _>("evidence").unwrap(),
            created_at: r.try_get::<i64, _>("created_at_unix").unwrap(),
        })
        .collect();
    Ok(list)
}

async fn soft_delete_warn(
    db: &Pool<Postgres>,
    gid: u64,
    case_id: i64,
    by: u64,
    why: &str,
) -> Result<Option<u64>> {
    let row = sqlx::query(
        r#"
        UPDATE tss.warn_cases
           SET deleted_at = now(),
               deleted_by = $1,
               delete_reason = $2
         WHERE id = $3
           AND guild_id = $4
           AND deleted_at IS NULL
        RETURNING user_id
        "#,
    )
    .bind(by as i64)
    .bind(why)
    .bind(case_id)
    .bind(gid as i64)
    .fetch_optional(db)
    .await?;

    if let Some(r) = row {
        let uid: i64 = r.try_get("user_id")?;
        Ok(Some(uid as u64))
    } else {
        Ok(None)
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

async fn edit_ephemeral(ctx: &Context, cmd: &CommandInteraction, msg: &str) -> Result<()> {
    cmd.edit_response(&ctx.http, EditInteractionResponse::new().content(msg))
        .await?;
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
    if id == 0 {
        None
    } else {
        Some(id)
    }
}

async fn has_permission(
    ctx: &Context,
    gid: GuildId,
    uid: UserId,
    perm: crate::permissions::Permission,
) -> bool {
    if let Ok(member) = gid.member(&ctx.http, uid).await {
        use crate::permissions::{role_has_permission, Role};
        let env = std::env::var("TSS_ENV").unwrap_or_else(|_| "production".to_string());
        for r in &member.roles {
            let rid = r.get();
            let role = if rid == crate::registry::env_roles::owner_id(&env) {
                Role::Wlasciciel
            } else if rid == crate::registry::env_roles::co_owner_id(&env) {
                Role::WspolWlasciciel
            } else if rid == crate::registry::env_roles::technik_zarzad_id(&env) {
                Role::TechnikZarzad
            } else if rid == crate::registry::env_roles::opiekun_id(&env) {
                Role::Opiekun
            } else if rid == crate::registry::env_roles::admin_id(&env) {
                Role::Admin
            } else if rid == crate::registry::env_roles::moderator_id(&env) {
                Role::Moderator
            } else if rid == crate::registry::env_roles::test_moderator_id(&env) {
                Role::TestModerator
            } else {
                continue;
            };
            if role_has_permission(role, perm) {
                return true;
            }
        }
    }
    false
}
