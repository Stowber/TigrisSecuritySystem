// src/admcheck.rs

use std::collections::HashSet;
use anyhow::Result;
use serenity::all::*;
use sqlx::{Pool, Postgres};

use crate::AppContext;
use crate::registry::roles::core::{WLASCICIEL, WSPOL_WLASCICIEL, TECHNIK_ZARZAD, OPIEKUN};

pub struct AdmCheck;

impl AdmCheck {
    pub async fn register_commands(ctx: &Context, gid: GuildId) -> Result<()> {
        gid.create_command(
            &ctx.http,
            CreateCommand::new("admcheck")
                .description("Podgląd danych administratora (tylko: Właściciel / Współwłaściciel / Technik / Opiekun).")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::User, "user", "Kogo sprawdzić")
                        .required(true),
                )
                .default_member_permissions(Permissions::MODERATE_MEMBERS),
        )
        .await?;
        Ok(())
    }

    pub async fn on_interaction(ctx: &Context, app: &AppContext, interaction: Interaction) {
        let Some(cmd) = interaction.command() else { return; };
        if cmd.data.name.as_str() != "admcheck" { return; }
        if let Err(e) = handle_admcheck(ctx, app, &cmd).await {
            tracing::warn!(?e, "admcheck failed");
        }
    }
}

async fn handle_admcheck(ctx: &Context, app: &AppContext, cmd: &CommandInteraction) -> Result<()> {
    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true)),
    )
    .await?;

    let Some(gid) = cmd.guild_id else {
        return edit_ephemeral(ctx, cmd, "Użyj na serwerze.").await;
    };

    if !is_staff_whitelisted(ctx, gid, cmd.user.id).await {
        return edit_ephemeral(
            ctx,
            cmd,
            "⛔ Tylko właściciel / współwłaściciel / technik / opiekun administracji.",
        )
        .await;
    }

    // target
    let mut target: Option<UserId> = None;
    for o in &cmd.data.options {
        if o.name == "user" {
            if let CommandDataOptionValue::User(u) = o.value {
                target = Some(u);
            }
        }
    }
    let Some(uid) = target else { return edit_ephemeral(ctx, cmd, "Wskaż użytkownika.").await; };

    // pobieramy member bez trzymania CacheRef przez await
    let member = gid.member(&ctx.http, uid).await.ok();

    // joined at (z Member.joined_at)
    let joined_at_unix = member
        .as_ref()
        .and_then(|m| m.joined_at)
        .map(|d| d.unix_timestamp())
        .unwrap_or(0);

    // nazwy ról (po HTTP, aby uniknąć CacheRef across await)
    let role_list = if let Some(m) = &member {
        let mut names = Vec::new();
        if let Ok(all_roles) = gid.roles(&ctx.http).await {
            for rid in &m.roles {
                if let Some(r) = all_roles.get(rid) {
                    names.push(r.name.clone());
                }
            }
        }
        names.join(", ")
    } else {
        "-".into()
    };

    // ====== STATYSTYKI z DB (odporne na różne nazwy tabel) ======
    let (points, photos_verified) =
        fetch_points_and_photos(&app.db, gid.get(), uid.get()).await;

    let warns_given = count_warns_given(&app.db, gid.get(), uid.get()).await;
    let bans_given  = count_bans_given(&app.db, gid.get(), uid.get()).await;

    // ====== EMBED ======
    let mut e = CreateEmbed::new()
        .title("🧰 AdmCheck")
        .colour(Colour::new(0x3498DB))
        .field(
            "Użytkownik",
            format!("<@{}> ({})", uid.get(), uid.get()),
            true,
        )
        .field(
            "Rangi",
            if role_list.is_empty() { "-".into() } else { role_list },
            false,
        )
        .field(
            "Na serwerze od",
            if joined_at_unix > 0 {
                format!("<t:{joined_at_unix}:F> • <t:{joined_at_unix}:R>")
            } else {
                "—".into()
            },
            true,
        )
        .field("Zweryfikowane zdjęcia", photos_verified.to_string(), true)
        .field("Warny (nadane)", warns_given.to_string(), true)
        .field("Bany (nadane)", bans_given.to_string(), true)
        .field("Punkty admina", format!("{:.1}", points), true)
        .footer(CreateEmbedFooter::new("Tigris AdmCheck"));

    if let Ok(u) = uid.to_user(&ctx.http).await {
        if let Some(avatar) = u.avatar_url() {
            e = e.thumbnail(avatar);
        }
    }

    cmd.edit_response(&ctx.http, EditInteractionResponse::new().embeds(vec![e]))
        .await?;
    Ok(())
}

/* ───────────────────────── helpers: DB probing ───────────────────────── */

async fn pg_has_table(db: &Pool<Postgres>, full_name: &str) -> bool {
    sqlx::query_scalar::<_, Option<String>>("SELECT to_regclass($1)")
        .bind(full_name)
        .fetch_one(db)
        .await
        .ok()
        .flatten()
        .is_some()
}

async fn table_has_column(
    db: &Pool<Postgres>,
    schema: &str,
    table: &str,
    column: &str,
) -> bool {
    sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS (
          SELECT 1
          FROM information_schema.columns
          WHERE table_schema = $1 AND table_name = $2 AND column_name = $3
        )"#,
    )
    .bind(schema)
    .bind(table)
    .bind(column)
    .fetch_one(db)
    .await
    .unwrap_or(false)
}

/// Zwraca (punkty_admina, liczba_zweryfikowanych_zdjęć)
async fn fetch_points_and_photos(db: &Pool<Postgres>, gid: u64, uid: u64) -> (f64, i64) {
    // 1) aktualne punkty z jednej z możliwych tabel
    let mut points = 0.0f64;

    if pg_has_table(db, "tss.admin_points").await {
        if let Ok(Some(v)) = sqlx::query_scalar::<_, Option<f64>>(
            "SELECT score::float8 FROM tss.admin_points WHERE guild_id=$1 AND admin_id=$2 LIMIT 1",
        )
        .bind(gid as i64)
        .bind(uid as i64)
        .fetch_one(db)
        .await
        {
            points = v;
        }
    } else if pg_has_table(db, "tss.admin_score").await {
        if let Ok(Some(v)) = sqlx::query_scalar::<_, Option<f64>>(
            "SELECT score::float8 FROM tss.admin_score WHERE guild_id=$1 AND admin_id=$2 LIMIT 1",
        )
        .bind(gid as i64)
        .bind(uid as i64)
        .fetch_one(db)
        .await
        {
            points = v;
        }
    } else if pg_has_table(db, "tss.admin_points_totals").await {
        if let Ok(Some(v)) = sqlx::query_scalar::<_, Option<f64>>(
            "SELECT total::float8 FROM tss.admin_points_totals WHERE guild_id=$1 AND admin_id=$2 LIMIT 1",
        )
        .bind(gid as i64)
        .bind(uid as i64)
        .fetch_one(db)
        .await
        {
            points = v;
        }
    }

    // 2) liczba zaakceptowanych zdjęć – szukamy po różnych źródłach
    let mut photos = 0i64;

    if pg_has_table(db, "tss.admin_points_log").await {
        if let Ok(Some(c)) = sqlx::query_scalar::<_, Option<i64>>(
            r#"
            SELECT COUNT(*)::bigint
            FROM tss.admin_points_log
            WHERE guild_id=$1 AND admin_id=$2
              AND (
                    lower(event) = 'photo_accept'
                 OR lower(action) = 'photo_accept'
                 OR lower(kind)   = 'photo_accept'
                 OR lower(note) LIKE '%akceptacj%zdj%'
                 OR lower(note) LIKE '%photo%accept%'
              )
            "#,
        )
        .bind(gid as i64)
        .bind(uid as i64)
        .fetch_one(db)
        .await
        {
            photos = c;
        }
    } else if pg_has_table(db, "tss.admin_points_events").await {
        if let Ok(Some(c)) = sqlx::query_scalar::<_, Option<i64>>(
            r#"
            SELECT COUNT(*)::bigint
            FROM tss.admin_points_events
            WHERE guild_id=$1 AND admin_id=$2
              AND (lower(event)='photo_accept' OR lower(title) LIKE '%photo%')
            "#,
        )
        .bind(gid as i64)
        .bind(uid as i64)
        .fetch_one(db)
        .await
        {
            photos = c;
        }
    } else if pg_has_table(db, "tss.photo_verifications").await {
        if let Ok(Some(c)) = sqlx::query_scalar::<_, Option<i64>>(
            r#"
            SELECT COUNT(*)::bigint
            FROM tss.photo_verifications
            WHERE guild_id=$1 AND moderator_id=$2 AND status IN ('accepted','ok','approved')
            "#,
        )
        .bind(gid as i64)
        .bind(uid as i64)
        .fetch_one(db)
        .await
        {
            photos = c;
        }
    }

    (points, photos)
}

async fn count_warns_given(db: &Pool<Postgres>, gid: u64, uid: u64) -> i64 {
    // preferowana nowa tabela
    if pg_has_table(db, "tss.warn_cases").await {
        // dobierz poprawną nazwę kolumny moderatora i ewentualnie deleted_at
        let mod_col = if table_has_column(db, "tss", "warn_cases", "moderator_id").await {
            "moderator_id"
        } else if table_has_column(db, "tss", "warn_cases", "mod_id").await {
            "mod_id"
        } else {
            ""
        };

        if !mod_col.is_empty() {
            let has_deleted = table_has_column(db, "tss", "warn_cases", "deleted_at").await;
            let sql = if has_deleted {
                format!(
                    "SELECT COUNT(*)::bigint FROM tss.warn_cases \
                     WHERE guild_id=$1 AND {mod_col}=$2 AND deleted_at IS NULL",
                    mod_col = mod_col
                )
            } else {
                format!(
                    "SELECT COUNT(*)::bigint FROM tss.warn_cases \
                     WHERE guild_id=$1 AND {mod_col}=$2",
                    mod_col = mod_col
                )
            };
            return sqlx::query_scalar::<_, i64>(&sql)
                .bind(gid as i64)
                .bind(uid as i64)
                .fetch_one(db)
                .await
                .unwrap_or(0);
        }
    }

    // starsza tabela
    if pg_has_table(db, "tss.warns").await {
        let mod_col = if table_has_column(db, "tss", "warns", "moderator_id").await {
            "moderator_id"
        } else if table_has_column(db, "tss", "warns", "mod_id").await {
            "mod_id"
        } else {
            ""
        };

        if !mod_col.is_empty() {
            let has_deleted = table_has_column(db, "tss", "warns", "deleted_at").await;
            let sql = if has_deleted {
                format!(
                    "SELECT COUNT(*)::bigint FROM tss.warns \
                     WHERE guild_id=$1 AND {mod_col}=$2 AND deleted_at IS NULL",
                    mod_col = mod_col
                )
            } else {
                format!(
                    "SELECT COUNT(*)::bigint FROM tss.warns \
                     WHERE guild_id=$1 AND {mod_col}=$2",
                    mod_col = mod_col
                )
            };
            return sqlx::query_scalar::<_, i64>(&sql)
                .bind(gid as i64)
                .bind(uid as i64)
                .fetch_one(db)
                .await
                .unwrap_or(0);
        }
    }

    0
}

async fn count_bans_given(db: &Pool<Postgres>, gid: u64, uid: u64) -> i64 {
    if pg_has_table(db, "tss.ban_cases").await {
        let mod_col = if table_has_column(db, "tss", "ban_cases", "moderator_id").await {
            "moderator_id"
        } else if table_has_column(db, "tss", "ban_cases", "mod_id").await {
            "mod_id"
        } else { "" };

        if !mod_col.is_empty() {
            let has_deleted = table_has_column(db, "tss", "ban_cases", "deleted_at").await;
            let sql = if has_deleted {
                format!(
                    "SELECT COUNT(*)::bigint FROM tss.ban_cases \
                     WHERE guild_id=$1 AND {mod_col}=$2 AND deleted_at IS NULL",
                    mod_col = mod_col
                )
            } else {
                format!(
                    "SELECT COUNT(*)::bigint FROM tss.ban_cases \
                     WHERE guild_id=$1 AND {mod_col}=$2",
                    mod_col = mod_col
                )
            };

            return sqlx::query_scalar::<_, i64>(&sql)
                .bind(gid as i64)
                .bind(uid as i64)
                .fetch_one(db)
                .await
                .unwrap_or(0);
        }
    }

    if pg_has_table(db, "tss.bans").await {
        let mod_col = if table_has_column(db, "tss", "bans", "moderator_id").await {
            "moderator_id"
        } else if table_has_column(db, "tss", "bans", "mod_id").await {
            "mod_id"
        } else { "" };

        if !mod_col.is_empty() {
            let has_deleted = table_has_column(db, "tss", "bans", "deleted_at").await;
            let sql = if has_deleted {
                format!(
                    "SELECT COUNT(*)::bigint FROM tss.bans \
                     WHERE guild_id=$1 AND {mod_col}=$2 AND deleted_at IS NULL",
                    mod_col = mod_col
                )
            } else {
                format!(
                    "SELECT COUNT(*)::bigint FROM tss.bans \
                     WHERE guild_id=$1 AND {mod_col}=$2",
                    mod_col = mod_col
                )
            };

            return sqlx::query_scalar::<_, i64>(&sql)
                .bind(gid as i64)
                .bind(uid as i64)
                .fetch_one(db)
                .await
                .unwrap_or(0);
        }
    }

    0
}

/* ───────────────────────── permissions / misc ───────────────────────── */

async fn is_staff_whitelisted(ctx: &Context, gid: GuildId, uid: UserId) -> bool {
    // owner zawsze może
    if let Ok(g) = gid.to_partial_guild(&ctx.http).await {
        if g.owner_id == uid {
            return true;
        }
    }

    let member = match gid.member(&ctx.http, uid).await {
        Ok(m) => m,
        Err(_) => return false,
    };

    // role z registry (0 oznacza „nie ustawiono”)
    let mut wanted: HashSet<RoleId> = HashSet::new();
    for rid in [WLASCICIEL, WSPOL_WLASCICIEL, TECHNIK_ZARZAD, OPIEKUN] {
        if rid != 0 {
            wanted.insert(RoleId::new(rid));
        }
    }

    // fallback po nazwach, gdy nie skonfigurowano ID
    if wanted.is_empty() {
        if let Ok(all) = gid.roles(&ctx.http).await {
            for (rid, role) in all {
                let name = normalize(&role.name);
                if name.contains("wlasciciel")
                    || name.contains("wspolwlasciciel")
                    || name.contains("technik")
                    || name.contains("zarzad")
                    || name.contains("opiekunadministracji")
                    || name == "opiekun"
                {
                    wanted.insert(rid);
                }
            }
        }
    }

    if wanted.is_empty() {
        return false;
    }

    let have: HashSet<RoleId> = member.roles.into_iter().collect();
    if wanted.iter().any(|r| have.contains(r)) {
        return true;
    }

    // opcjonalnie: administrator ma dostęp
    if let Ok(perms) = gid.member(&ctx.http, uid).await.and_then(|m| m.permissions(&ctx.cache)) {
        if perms.administrator() {
            return true;
        }
    }

    false
}

fn normalize(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    for ch in lower.chars() {
        let mapped = match ch {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ź' | 'ż' => 'z',
            ' ' | '-' | '_' | '/' | '\\' | '.' => continue,
            _ => ch,
        };
        out.push(mapped);
    }
    out
}

async fn edit_ephemeral(ctx: &Context, cmd: &CommandInteraction, msg: &str) -> Result<()> {
    cmd.edit_response(&ctx.http, EditInteractionResponse::new().content(msg))
        .await?;
    Ok(())
}
