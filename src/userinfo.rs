use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serenity::all::*;
use sqlx::{Pool, Postgres, Row};
use serde_json::Value as Json;

use crate::AppContext;

const SYSTEM_NAME: &str = "Tigris User Inspector";

pub struct UserInfo;

impl UserInfo {
    /* ===================== Komendy ===================== */

    pub async fn register_commands(ctx: &Context, gid: GuildId) -> Result<()> {
        // /user – wymagane „user”, opcjonalne „public”
        gid.create_command(&ctx.http,
            CreateCommand::new("user")
                .description("Pokaż szczegółowe informacje o użytkowniku (profil, serwer, moderacja).")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::User, "user", "Którego użytkownika sprawdzić")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Boolean, "public", "Odpowiedź publiczna (domyślnie prywatna)")
                )
                .default_member_permissions(Permissions::MODERATE_MEMBERS)
        ).await?;
        Ok(())
    }

    pub async fn on_interaction(ctx: &Context, app: &AppContext, interaction: Interaction) {
        if let Some(cmd) = interaction.command() {
            if cmd.data.name.as_str() == "user" {
                if let Err(e) = handle_user(ctx, app, &cmd).await {
                    tracing::warn!(?e, "user command failed");
                }
            }
        }
    }
}

/* ========================= Handler ========================= */

async fn handle_user(ctx: &Context, app: &AppContext, cmd: &CommandInteraction) -> Result<()> {
    let Some(gid) = cmd.guild_id else {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new().content("Użyj na serwerze.")
            )
        ).await?;
        return Ok(());
    };

    // Zbierz opcje przed defer, żeby ustawić ephemeral zależnie od "public"
    let mut target: Option<UserId> = None;
    let mut want_public = false;
    for o in &cmd.data.options {
        match (&o.name[..], &o.value) {
            ("user",   CommandDataOptionValue::User(u))    => target = Some(*u),
            ("public", CommandDataOptionValue::Boolean(b)) => want_public = *b,
            _ => {}
        }
    }
    let Some(uid) = target else {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new().content("Wskaż użytkownika.")
            )
        ).await?;
        return Ok(());
    };

    // Defer
    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Defer(
            CreateInteractionResponseMessage::new().ephemeral(!want_public)
        )
    ).await?;

    // Dane
    let user = uid.to_user(&ctx.http).await?;
    let member_opt = gid.member(&ctx.http, uid).await.ok();
    let member = member_opt.as_ref();

    /* ====== Profil ====== */
    let avatar = user.avatar_url();
    let banner = user.banner_url();
    let created_unix = to_unix(user.id.created_at());

    let mut e_profile = CreateEmbed::new()
        .title("👤 Użytkownik")
        .colour(Colour::new(0x95A5A6))
        .field("ID", format!("`{}`", uid.get()), true)
        .field("Mention", format!("<@{}>", uid.get()), true)
        .field("Utworzono", format!("<t:{0}:F> • <t:{0}:R>", created_unix), true)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    if let Some(global) = &user.global_name {
        e_profile = e_profile.field("Global name", format!("`{}`", truncate(global, 100)), true);
    }
    e_profile = e_profile.field("Username", format!("`{}`", truncate(&user.name, 100)), true);

    if let Some(ava) = avatar.clone() {
        e_profile = e_profile.thumbnail(ava);
    }
    if let Some(ban) = banner {
        e_profile = e_profile.image(ban);
    }

    /* ====== Serwer ====== */
    let mut e_guild = CreateEmbed::new()
        .title("🏠 Informacje serwerowe")
        .colour(Colour::new(0x3498DB))
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    if let Some(m) = member {
        if let Some(joined) = m.joined_at {
            let ts = to_unix(joined);
            e_guild = e_guild.field("Dołączył", format!("<t:{0}:F> • <t:{0}:R>", ts), true);
        }
        if let Some(nick) = &m.nick {
            e_guild = e_guild.field("Pseudonim", format!("`{}`", truncate(nick, 100)), true);
        }

        // Role
        let roles = &m.roles;
        let roles_txt = if roles.is_empty() {
            "_brak_".to_string()
        } else {
            // Do ~900 znaków, mentions: <@&ID>
            let mentions: Vec<String> = roles.iter().map(|r| format!("<@&{}>", r.get())).collect();
            let mut acc = String::new();
            for (i, part) in mentions.iter().enumerate() {
                if acc.len() + part.len() + 1 > 900 { acc.push_str(" …"); break; }
                if i > 0 { acc.push(' '); }
                acc.push_str(part);
            }
            acc
        };
        e_guild = e_guild.field(format!("Role ({})", roles.len()), roles_txt, false);

        // Uprawnienia (na poziomie gildii — jak w reszcie projektu)
        if let Ok(p) = m.permissions(&ctx.cache) {
            let mut flags = Vec::new();
            if p.administrator()      { flags.push("administrator"); }
            if p.manage_guild()       { flags.push("manage_guild"); }
            if p.manage_channels()    { flags.push("manage_channels"); }
            if p.manage_roles()       { flags.push("manage_roles"); }
            if p.manage_messages()    { flags.push("manage_messages"); }
            if p.kick_members()       { flags.push("kick_members"); }
            if p.ban_members()        { flags.push("ban_members"); }
            if p.moderate_members()   { flags.push("moderate_members"); }
            let perms_txt = if flags.is_empty() { "_brak istotnych flag_".into() } else { flags.join(", ") };
            e_guild = e_guild.field("Uprawnienia (gildia)", perms_txt, false);
        }

        // Timeout?
        if let Some(until) = m.communication_disabled_until {
            let ts = to_unix(until);
            if ts > now_unix() {
                e_guild = e_guild.field("Timeout do", format!("<t:{0}:F> • <t:{0}:R>", ts), true);
            }
        }
        // Boost?
        if let Some(ps) = m.premium_since {
            let ts = to_unix(ps);
            e_guild = e_guild.field("Boostuje od", format!("<t:{0}:F> • <t:{0}:R>", ts), true);
        }
    } else {
        e_guild = e_guild.description("_Użytkownik nie jest członkiem tej gildii._");
    }

    /* ====== Moderacja (warn/mute) ====== */
    let mut e_mod = CreateEmbed::new()
        .title("🛡️ Moderacja")
        .colour(Colour::new(0xE67E22))
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    // WARN – konfiguracja wygasania
    let decay_days: i64 = get_warn_decay_days(&app.db, gid.get()).await.unwrap_or(30) as i64;

    // WARN – total & active points + ostatnie 5
    let total_warns: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM tss.warn_cases WHERE guild_id=$1 AND user_id=$2 AND deleted_at IS NULL"#,
    )
    .bind(gid.get() as i64)
    .bind(uid.get() as i64)
    .fetch_one(&app.db)
    .await
    .unwrap_or(0);

    let active_points: i64 = sqlx::query_scalar(
        r#"
        SELECT COALESCE(SUM(points),0)
        FROM tss.warn_cases
        WHERE guild_id=$1 AND user_id=$2 AND deleted_at IS NULL AND created_at >= now() - ($3 || ' days')::interval
        "#,
    )
    .bind(gid.get() as i64)
    .bind(uid.get() as i64)
    .bind(decay_days)
    .fetch_one(&app.db)
    .await
    .unwrap_or(0);

    let recent_warns = sqlx::query(
        r#"
        SELECT id, points, reason, EXTRACT(EPOCH FROM created_at)::BIGINT AS ts
        FROM tss.warn_cases
        WHERE guild_id=$1 AND user_id=$2 AND deleted_at IS NULL
        ORDER BY created_at DESC
        LIMIT 5
        "#,
    )
    .bind(gid.get() as i64)
    .bind(uid.get() as i64)
    .fetch_all(&app.db)
    .await
    .unwrap_or_default();

    let mut warn_lines = Vec::new();
    for r in recent_warns {
        let id: i64 = r.try_get("id").unwrap_or(0);
        let pts: i32 = r.try_get("points").unwrap_or(0);
        let rsn: String = r.try_get("reason").unwrap_or_default();
        let ts: i64 = r.try_get("ts").unwrap_or(now_unix());
        warn_lines.push(format!("`#{}` • **{}p** • <t:{ts}:R>\n{}", id, pts, truncate(&rsn, 140)));
    }
    if warn_lines.is_empty() {
        warn_lines.push("_brak aktywnych wpisów_".into());
    }
    e_mod = e_mod
        .field("Warny (suma)", format!("**{}**", total_warns), true)
        .field("Aktywne punkty", format!("**{}** _(okres: {} dni)_", active_points, decay_days), true)
        .field("Ostatnie", warn_lines.join("\n\n"), false);

    // MUTE – aktualny stan + ostatnie 3
    let mute_role = get_mute_role(&app.db, gid.get()).await.ok().flatten();
    let mut currently_muted = false;

    if let Some(m) = member {
        if let Some(until) = m.communication_disabled_until {
            let ts = to_unix(until);
            if ts > now_unix() {
                currently_muted = true;
            }
        }
        if !currently_muted {
            if let Some(rid) = mute_role {
                if m.roles.iter().any(|r| r.get() == rid) {
                    currently_muted = true;
                }
            }
        }
    }

    let recent_mutes = sqlx::query(
        r#"
        SELECT id, reason, method, EXTRACT(EPOCH FROM created_at)::BIGINT AS ts,
               EXTRACT(EPOCH FROM until)::BIGINT AS until_ts,
               EXTRACT(EPOCH FROM unmuted_at)::BIGINT AS unmuted_ts
        FROM tss.mute_cases
        WHERE guild_id=$1 AND user_id=$2
        ORDER BY created_at DESC
        LIMIT 3
        "#,
    )
    .bind(gid.get() as i64)
    .bind(uid.get() as i64)
    .fetch_all(&app.db)
    .await
    .unwrap_or_default();

    let mut mute_lines = Vec::new();
    for r in recent_mutes {
        let id: i64 = r.try_get("id").unwrap_or(0);
        let method: String = r.try_get("method").unwrap_or_else(|_| "role".into());
        let rsn: String = r.try_get("reason").unwrap_or_default();
        let ts: i64 = r.try_get("ts").unwrap_or(now_unix());
        let until_ts: Option<i64> = r.try_get::<Option<i64>, _>("until_ts").unwrap_or(None);
        let unmuted_ts: Option<i64> = r.try_get::<Option<i64>, _>("unmuted_ts").unwrap_or(None);

        let when = format!("<t:{ts}:R>");
        let dur = if let Some(u) = until_ts {
            if u > 0 { format!(" • do <t:{u}:R>") } else { String::new() }
        } else { String::new() };
        let closed = if let Some(u) = unmuted_ts {
            if u > 0 { format!(" • unmute <t:{u}:R>") } else { String::new() }
        } else { String::new() };
        mute_lines.push(format!("`#{}` • `{}` • {}{}{}\n{}", id, method, when, dur, closed, truncate(&rsn, 140)));
    }
    if mute_lines.is_empty() {
        mute_lines.push("_brak wpisów_".into());
    }

    e_mod = e_mod
        .field("Aktualny stan", if currently_muted { "**🔇 wyciszony**" } else { "brak wyciszenia" }, true)
        .field("Rola Muted", mute_role.map(|r| format!("<@&{}>", r)).unwrap_or_else(|| "_brak (timeout)_".into()), true)
        .field("Ostatnie wyciszenia", mute_lines.join("\n\n"), false);

    // Odpowiedź
    let embeds = vec![e_profile, e_guild, e_mod];
    cmd.edit_response(&ctx.http, EditInteractionResponse::new().embeds(embeds)).await?;
    Ok(())
}

/* ========================= Helpers ========================= */

async fn get_warn_decay_days(db: &Pool<Postgres>, gid: u64) -> Result<i32> {
    let row = sqlx::query("SELECT cfg FROM tss.warn_config WHERE guild_id=$1")
        .bind(gid as i64)
        .fetch_optional(db)
        .await?;
    if let Some(r) = row {
        let v: Json = r.try_get("cfg")?;
        if let Some(n) = v.get("decay_days").and_then(|x| x.as_i64()) {
            return Ok(n as i32);
        }
    }
    Ok(30)
}

async fn get_mute_role(db: &Pool<Postgres>, gid: u64) -> Result<Option<u64>> {
    let row = sqlx::query("SELECT cfg FROM tss.mute_config WHERE guild_id=$1")
        .bind(gid as i64)
        .fetch_optional(db)
        .await?;
    if let Some(r) = row {
        let v: Json = r.try_get("cfg")?;
        if let Some(id) = v.get("role_id").and_then(|x| x.as_u64()) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max { s.to_string() } else {
        let mut t = s[..max.saturating_sub(1)].to_string();
        t.push('…');
        t
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn to_unix(ts: Timestamp) -> i64 {
    ts.unix_timestamp()
}
