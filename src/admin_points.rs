// src/admin_points.rs

//! Tigris AdminScore – punktacja administracji
//! - Skala: mili-punkty (1000 = 1.0)
//! - Limit: 100.0 pkt
//! - Log każdej zmiany: tss.admin_score_log
//! - Podgląd PROFILU: wyłącznie Administrator (permission) OR role: Test moderator, Moderator, Admin
//! - Ręczne dodawanie/odejmowanie punktów: Właściciel i Opiekun
//! - Log ręcznych zmian na kanale logs::ADMINS_POINTS (per-ENV)

use anyhow::Result;
use sqlx::types::time::OffsetDateTime;
use sqlx::{Pool, Postgres};
use tracing::info;

use serenity::all::{
    ChannelId, Colour, CommandDataOptionValue, CommandInteraction, CommandOptionType,
    ComponentInteraction, ComponentInteractionDataKind, Context, CreateActionRow, CreateCommand,
    CreateCommandOption, CreateEmbed, CreateEmbedFooter, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, CreateSelectMenu, CreateSelectMenuKind,
    CreateSelectMenuOption, GuildId, Interaction, Timestamp, UserId,
};

use crate::{
    registry::{channels, env_roles},
    AppContext,
};

/* =========================
KONFIG
========================= */

pub const SYSTEM_NAME: &str = "Tigris AdminScore";

/// mili-punkty (1000 = 1.0)
const POINTS_SCALE: i64 = 1000;
const MAX_POINTS_CAP: i64 = 100 * POINTS_SCALE; // 100.000

/// Za akceptację jednego zdjęcia: 0.1
const PHOTO_APPROVED_POINTS_MILLI: i64 = 100;
/// Za nadanie ostrzeżenia: 1.0
const WARN_GIVEN_POINTS_MILLI: i64 = POINTS_SCALE;

/* =========================
PUBLIC API (moduł)
========================= */

pub struct AdminPoints;

impl AdminPoints {
    /* ---------- bootstrapping / migracje ---------- */

    pub async fn ensure_tables(db: &Pool<Postgres>) -> Result<()> {
        let _ = sqlx::query(r#"CREATE SCHEMA IF NOT EXISTS tss"#)
            .execute(db)
            .await?;

        // stan
        let _ = sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tss.admin_score (
                user_id       BIGINT PRIMARY KEY,
                points_milli  BIGINT NOT NULL DEFAULT 0,
                updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
            )
        "#,
        )
        .execute(db)
        .await?;

        // log
        let _ = sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tss.admin_score_log (
                id           BIGSERIAL PRIMARY KEY,
                user_id      BIGINT NOT NULL,
                delta_milli  BIGINT NOT NULL,
                reason       TEXT,
                source       TEXT NOT NULL,
                actor_id     BIGINT,
                created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
            )
        "#,
        )
        .execute(db)
        .await?;

        // indeksy
        let _ = sqlx::query(
            r#"CREATE INDEX IF NOT EXISTS idx_admin_score_points ON tss.admin_score(points_milli)"#,
        )
        .execute(db)
        .await?;
        let _ = sqlx::query(
            r#"CREATE INDEX IF NOT EXISTS idx_admin_score_log_user ON tss.admin_score_log(user_id)"#,
        )
        .execute(db)
        .await?;
        let _ = sqlx::query(
            r#"CREATE INDEX IF NOT EXISTS idx_admin_score_log_created ON tss.admin_score_log(created_at)"#,
        )
        .execute(db)
        .await?;

        info!("{SYSTEM_NAME}: tables ensured.");
        Ok(())
    }

    /* ---------- operacje punktów ---------- */

    /// Aktualne punkty w „normalnych” punktach (nie w mili)
    pub async fn get_points(db: &Pool<Postgres>, user_id: u64) -> Result<f64> {
        let milli: Option<i64> =
            sqlx::query_scalar(r#"SELECT points_milli FROM tss.admin_score WHERE user_id = $1"#)
                .bind(user_id as i64)
                .fetch_optional(db)
                .await?;
        Ok(milli_to_points(milli.unwrap_or(0)))
    }

    /// +0.1 pkt za akceptację zdjęcia
    pub async fn award_photo_approved(db: &Pool<Postgres>, moderator_id: u64) -> Result<f64> {
        Self::apply_delta(
            db,
            moderator_id,
            PHOTO_APPROVED_POINTS_MILLI,
            Some(moderator_id),
            "PHOTO_APPROVED",
            Some("Akceptacja zdjęcia"),
        )
        .await
    }

    /// +1 pkt za nadanie ostrzeżenia
    pub async fn award_warn_given(db: &Pool<Postgres>, moderator_id: u64) -> Result<f64> {
        Self::apply_delta(
            db,
            moderator_id,
            WARN_GIVEN_POINTS_MILLI,
            Some(moderator_id),
            "WARN_GIVEN",
            Some("Nadanie ostrzeżenia"),
        )
        .await
    }

    /// Ręczna modyfikacja (±) – Właściciel/Opiekun
    pub async fn adjust_manual(
        db: &Pool<Postgres>,
        env: &str,
        actor_id: u64,
        actor_roles: &[u64],
        target_user_id: u64,
        delta_points: f64,
        reason: &str,
    ) -> Result<f64> {
        if !can_adjust_manually(env, actor_roles) {
            anyhow::bail!("Brak uprawnień do ręcznej zmiany punktów.");
        }
        let delta_milli = points_to_milli(delta_points);
        Self::apply_delta(
            db,
            target_user_id,
            delta_milli,
            Some(actor_id),
            "MANUAL",
            Some(reason),
        )
        .await
    }

    /* ---------- routing interakcji ---------- */

    /// Rejestracja /punkty (per-guild)
    pub async fn register_commands(ctx: &Context, guild_id: GuildId) -> Result<()> {
        guild_id
            .create_command(
                &ctx.http,
                CreateCommand::new(SLASH_NAME)
                    .description("Punkty administracji (podgląd + add + remove + clear + profil).")
                    // /punkty add user amount reason?
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::SubCommand,
                            "add",
                            "Dodaj punkty administratorowi",
                        )
                        .add_sub_option(
                            CreateCommandOption::new(
                                CommandOptionType::User,
                                "user",
                                "Komu przyznać punkty",
                            )
                            .required(true),
                        )
                        .add_sub_option(
                            CreateCommandOption::new(
                                CommandOptionType::Number,
                                "amount",
                                "Ile punktów dodać (np. 0.3)",
                            )
                            .required(true),
                        )
                        .add_sub_option(CreateCommandOption::new(
                            CommandOptionType::String,
                            "reason",
                            "Powód (opcjonalnie)",
                        )),
                    )
                    // /punkty remove user amount reason?
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::SubCommand,
                            "remove",
                            "Odejmij punkty administratorowi",
                        )
                        .add_sub_option(
                            CreateCommandOption::new(
                                CommandOptionType::User,
                                "user",
                                "Komu odjąć punkty",
                            )
                            .required(true),
                        )
                        .add_sub_option(
                            CreateCommandOption::new(
                                CommandOptionType::Number,
                                "amount",
                                "Ile punktów odjąć (np. 0.3)",
                            )
                            .required(true),
                        )
                        .add_sub_option(CreateCommandOption::new(
                            CommandOptionType::String,
                            "reason",
                            "Powód (opcjonalnie)",
                        )),
                    )
                    // /punkty clear user
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::SubCommand,
                            "clear",
                            "Wyczyść punkty administratora do zera",
                        )
                        .add_sub_option(
                            CreateCommandOption::new(
                                CommandOptionType::User,
                                "user",
                                "Komu wyczyścić punkty",
                            )
                            .required(true),
                        ),
                    )
                    // /punkty profil user
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::SubCommand,
                            "profil",
                            "Pokaż profil punktowy wskazanego administratora",
                        )
                        .add_sub_option(
                            CreateCommandOption::new(
                                CommandOptionType::User,
                                "user",
                                "Kogo profil wyświetlić",
                            )
                            .required(true),
                        ),
                    ),
            )
            .await?;
        Ok(())
    }

    /// Router interakcji: slash + komponent select
    pub async fn on_interaction(ctx: &Context, app: &AppContext, interaction: Interaction) {
        // slash
        if let Some(cmd) = interaction.clone().command() {
            if cmd.data.name == SLASH_NAME {
                if let Some(first) = cmd.data.options.first() {
                    match first.name.as_str() {
                        "add" => {
                            let _ = handle_points_add(ctx, app, &cmd, first).await;
                            return;
                        }
                        "remove" => {
                            let _ = handle_points_remove(ctx, app, &cmd, first).await;
                            return;
                        }
                        "clear" => {
                            let _ = handle_points_clear(ctx, app, &cmd, first).await;
                            return;
                        }
                        "profil" => {
                            let _ = handle_points_profil(ctx, app, &cmd, first).await;
                            return;
                        }
                        _ => {}
                    }
                }
                let _ = handle_slash(ctx, app, &cmd).await;
                return;
            }
        }
        // select
        if let Some(comp) = interaction.message_component() {
            if is_points_component(&comp) {
                let _ = handle_points_component(ctx, &app.db, &comp).await;
                return;
            }
        }
    }
}

/* =========================
CORE
========================= */

impl AdminPoints {
    async fn apply_delta(
        db: &Pool<Postgres>,
        target_user_id: u64,
        requested_delta_milli: i64,
        actor_id: Option<u64>,
        source: &str,
        reason: Option<&str>,
    ) -> Result<f64> {
        let mut tx = db.begin().await?;

        // SELECT … FOR UPDATE – jeśli wiersza brak, zwróci None
        let current_opt: Option<i64> = sqlx::query_scalar(
            r#"SELECT points_milli FROM tss.admin_score WHERE user_id = $1 FOR UPDATE"#,
        )
        .bind(target_user_id as i64)
        .fetch_optional(&mut *tx)
        .await?;

        let current = current_opt.unwrap_or(0);

        // cap przy dodatnich przyrostach
        let delta_applied = if requested_delta_milli > 0 {
            let room = MAX_POINTS_CAP.saturating_sub(current.max(0));
            requested_delta_milli.min(room).max(0)
        } else {
            requested_delta_milli
        };

        let mut new_total = current + delta_applied;
        if new_total > MAX_POINTS_CAP {
            new_total = MAX_POINTS_CAP;
        }

        if current_opt.is_some() {
            let _ = sqlx::query(
                r#"UPDATE tss.admin_score
                   SET points_milli = $2, updated_at = now()
                   WHERE user_id = $1"#,
            )
            .bind(target_user_id as i64)
            .bind(new_total)
            .execute(&mut *tx)
            .await?;
        } else {
            let _ = sqlx::query(
                r#"INSERT INTO tss.admin_score (user_id, points_milli, updated_at)
                   VALUES ($1, $2, now())"#,
            )
            .bind(target_user_id as i64)
            .bind(new_total)
            .execute(&mut *tx)
            .await?;
        }

        let _ = sqlx::query(
            r#"INSERT INTO tss.admin_score_log
               (user_id, delta_milli, reason, source, actor_id, created_at)
               VALUES ($1, $2, $3, $4, $5, now())"#,
        )
        .bind(target_user_id as i64)
        .bind(delta_applied)
        .bind(reason.unwrap_or(""))
        .bind(source)
        .bind(actor_id.map(|id| id as i64))
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(milli_to_points(new_total))
    }
}

/* =========================
UTILS
========================= */

fn milli_to_points(m: i64) -> f64 {
    (m as f64) / (POINTS_SCALE as f64)
}
fn points_to_milli(p: f64) -> i64 {
    (p * (POINTS_SCALE as f64)).round() as i64
}

/// Ręczne korekty – **tylko** Owner i Opiekun
fn can_adjust_manually(env: &str, actor_roles: &[u64]) -> bool {
    use crate::permissions::{role_has_permission, Role};
    actor_roles.iter().any(|rid| {
        let role = if *rid == crate::registry::env_roles::owner_id(env) { Role::Wlasciciel }
            else if *rid == crate::registry::env_roles::co_owner_id(env) { Role::WspolWlasciciel }
            else if *rid == crate::registry::env_roles::technik_zarzad_id(env) { Role::TechnikZarzad }
            else if *rid == crate::registry::env_roles::opiekun_id(env) { Role::Opiekun }
            else if *rid == crate::registry::env_roles::admin_id(env) { Role::Admin }
            else if *rid == crate::registry::env_roles::moderator_id(env) { Role::Moderator }
            else if *rid == crate::registry::env_roles::test_moderator_id(env) { Role::TestModerator }
            else { return false };
        role_has_permission(role, crate::permissions::Permission::Punkty)
    })
}

/// Uprawnienia do **oglądania profilu / UI**:
/// wyłącznie: Administrator (permission) **lub** role: test_moderator / moderator / admin.
async fn is_points_view_allowed(ctx: &Context, gid: GuildId, user_id: UserId) -> bool {
    has_permission(ctx, gid, user_id, crate::permissions::Permission::Punkty).await
}

/// uniwersalny helper
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
            let role = if rid == crate::registry::env_roles::owner_id(&env) { Role::Wlasciciel }
                else if rid == crate::registry::env_roles::co_owner_id(&env) { Role::WspolWlasciciel }
                else if rid == crate::registry::env_roles::technik_zarzad_id(&env) { Role::TechnikZarzad }
                else if rid == crate::registry::env_roles::opiekun_id(&env) { Role::Opiekun }
                else if rid == crate::registry::env_roles::admin_id(&env) { Role::Admin }
                else if rid == crate::registry::env_roles::moderator_id(&env) { Role::Moderator }
                else if rid == crate::registry::env_roles::test_moderator_id(&env) { Role::TestModerator }
                else { continue };
            if role_has_permission(role, perm) {
                return true;
            }
        }
    }
    false
}

/* =========================
SLASH /points + komponent
========================= */

pub const SLASH_NAME: &str = "punkty";
const UI_SELECT_ID: &str = "as:punkty:select";

async fn handle_slash(ctx: &Context, app: &AppContext, cmd: &CommandInteraction) -> Result<()> {
    let Some(gid) = cmd.guild_id else {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Ta komenda działa tylko na serwerze.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    };

    let env = app.env();
    // ⬇️ zawężone uprawnienia do UI/profilu
    if !is_points_view_allowed(ctx, gid, cmd.user.id).await {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("⛔ Brak uprawnień do podglądu profili.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    }

    let options = build_admin_select_options(ctx, gid, &env).await?;
    if options.is_empty() {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Nie znalazłem żadnych administratorów do wyświetlenia.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    }

    let embed = CreateEmbed::new()
        .title("Tigris AdminScore — wybierz administratora")
        .description("Użyj listy poniżej, aby podejrzeć profil punktowy.")
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    let menu = CreateSelectMenu::new(UI_SELECT_ID, CreateSelectMenuKind::String { options })
        .placeholder("Wybierz administratora")
        .min_values(1)
        .max_values(1);

    let row = CreateActionRow::SelectMenu(menu);

    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .add_embed(embed)
                .components(vec![row])
                .ephemeral(true),
        ),
    )
    .await?;

    Ok(())
}

/// PUBLIC: obsługa komponentu (select) – używane w routerze gildii
pub async fn handle_points_component(
    ctx: &Context,
    db: &Pool<Postgres>,
    comp: &ComponentInteraction,
) -> Result<()> {
    if comp.data.custom_id.as_str() != UI_SELECT_ID {
        return Ok(());
    }
    let Some(gid) = comp.guild_id else {
        return Ok(());
    };

    let env = std::env::var("TSS_ENV").unwrap_or_else(|_| "production".to_string());

    // ⬇️ zawężone uprawnienia do UI/profilu
    if !is_points_view_allowed(ctx, gid, comp.user.id).await {
        let _ = comp
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("⛔ Brak uprawnień do podglądu profili.")
                        .ephemeral(true),
                ),
            )
            .await;
        return Ok(());
    }

    let user_id: u64 = match &comp.data.kind {
        ComponentInteractionDataKind::StringSelect { values } => values
            .first()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0),
        _ => 0,
    };
    if user_id == 0 {
        let _ = comp
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("Nieprawidłowy wybór.")
                        .ephemeral(true),
                ),
            )
            .await;
        return Ok(());
    }

    // Nowy, bogatszy embed
    let profile = build_profile_embed(ctx, db, user_id).await?;

    // Select – zostaje jak był
    let options = build_admin_select_options(ctx, gid, &env).await?;
    let menu = CreateSelectMenu::new(UI_SELECT_ID, CreateSelectMenuKind::String { options })
        .placeholder("Wybierz administratora")
        .min_values(1)
        .max_values(1);
    let row = CreateActionRow::SelectMenu(menu);

    let _ = comp
        .create_response(
            &ctx.http,
            CreateInteractionResponse::UpdateMessage(
                CreateInteractionResponseMessage::new()
                    .add_embed(profile)
                    .components(vec![row]),
            ),
        )
        .await;

    Ok(())
}

/* ---------- /points add (ręczne nadanie) ---------- */

async fn handle_points_add(
    ctx: &Context,
    app: &AppContext,
    cmd: &CommandInteraction,
    add_opt: &serenity::all::CommandDataOption,
) -> Result<()> {
    let Some(gid) = cmd.guild_id else {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Ta komenda działa tylko na serwerze.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    };

    // Rozpakuj subopcje
    let mut target: Option<UserId> = None;
    let mut amount: Option<f64> = None;
    let mut reason: Option<String> = None;

    if let CommandDataOptionValue::SubCommand(subs) = &add_opt.value {
        for o in subs {
            match o.name.as_str() {
                "user" => {
                    if let CommandDataOptionValue::User(uid) = o.value {
                        target = Some(uid);
                    }
                }
                "amount" => match o.value {
                    CommandDataOptionValue::Number(n) => amount = Some(n),
                    CommandDataOptionValue::Integer(i) => amount = Some(i as f64),
                    _ => {}
                },
                "reason" => {
                    if let CommandDataOptionValue::String(s) = &o.value {
                        if !s.trim().is_empty() {
                            reason = Some(s.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let Some(target_id) = target else {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Musisz wskazać użytkownika.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    };
    let Some(amount) = amount.filter(|v| *v > 0.0) else {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Kwota musi być dodatnia, np. `0.3`.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    };

    // role aktora
    let actor_roles: Vec<u64> = match gid.member(&ctx.http, cmd.user.id).await {
        Ok(m) => m.roles.iter().map(|r| r.get()).collect(),
        Err(_) => vec![],
    };

    let env = app.env();
    match AdminPoints::adjust_manual(
        &app.db,
        &env,
        cmd.user.id.get(),
        &actor_roles,
        target_id.get(),
        amount,
        reason.as_deref().unwrap_or("Ręczne dodanie punktów"),
    )
    .await
    {
        Ok(total) => {
            let text = format!(
                "✅ Dodano **{:.2}** pkt dla <@{}>.\nNowy stan: **{:.1} / 100.0**{}",
                amount,
                target_id.get(),
                total,
                reason
                    .as_ref()
                    .map(|r| format!("\nPowód: _{}_ ", r))
                    .unwrap_or_default(),
            );
            cmd.create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(text)
                        .ephemeral(true),
                ),
            )
            .await?;

            // log na kanale ADMINS_POINTS (z nowym stanem)
            log_points_adjustment(
                ctx,
                app,
                cmd.user.id.get(),
                target_id.get(),
                amount,
                Some(total),
                reason.as_deref(),
            )
            .await;
        }
        Err(e) => {
            cmd.create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(format!("⛔ Nie udało się dodać punktów: {}", e))
                        .ephemeral(true),
                ),
            )
            .await?;
        }
    }

    Ok(())
}

/* ---------- /points remove (ręczne odjęcie) ---------- */

async fn handle_points_remove(
    ctx: &Context,
    app: &AppContext,
    cmd: &CommandInteraction,
    rem_opt: &serenity::all::CommandDataOption,
) -> Result<()> {
    let Some(gid) = cmd.guild_id else {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Ta komenda działa tylko na serwerze.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    };

    // Rozpakuj subopcje
    let mut target: Option<UserId> = None;
    let mut amount: Option<f64> = None;
    let mut reason: Option<String> = None;

    if let CommandDataOptionValue::SubCommand(subs) = &rem_opt.value {
        for o in subs {
            match o.name.as_str() {
                "user" => {
                    if let CommandDataOptionValue::User(uid) = o.value {
                        target = Some(uid);
                    }
                }
                "amount" => match o.value {
                    CommandDataOptionValue::Number(n) => amount = Some(n),
                    CommandDataOptionValue::Integer(i) => amount = Some(i as f64),
                    _ => {}
                },
                "reason" => {
                    if let CommandDataOptionValue::String(s) = &o.value {
                        if !s.trim().is_empty() {
                            reason = Some(s.clone());
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let Some(target_id) = target else {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Musisz wskazać użytkownika.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    };
    let Some(amount_pos) = amount.filter(|v| *v > 0.0) else {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Kwota musi być dodatnia, np. `0.3`.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    };
    let amount_neg = -amount_pos;

    // role aktora
    let actor_roles: Vec<u64> = match gid.member(&ctx.http, cmd.user.id).await {
        Ok(m) => m.roles.iter().map(|r| r.get()).collect(),
        Err(_) => vec![],
    };

    let env = app.env();
    match AdminPoints::adjust_manual(
        &app.db,
        &env,
        cmd.user.id.get(),
        &actor_roles,
        target_id.get(),
        amount_neg,
        reason.as_deref().unwrap_or("Ręczne odjęcie punktów"),
    )
    .await
    {
        Ok(total) => {
            let text = format!(
                "✅ Odjęto **{:.2}** pkt użytkownikowi <@{}>.\nNowy stan: **{:.1} / 100.0**{}",
                amount_pos,
                target_id.get(),
                total,
                reason
                    .as_ref()
                    .map(|r| format!("\nPowód: _{}_ ", r))
                    .unwrap_or_default(),
            );
            cmd.create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(text)
                        .ephemeral(true),
                ),
            )
            .await?;

            // log na kanale ADMINS_POINTS (wartość ujemna) + nowy stan
            log_points_adjustment(
                ctx,
                app,
                cmd.user.id.get(),
                target_id.get(),
                amount_neg,
                Some(total),
                reason.as_deref(),
            )
            .await;
        }
        Err(e) => {
            cmd.create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(format!("⛔ Nie udało się odjąć punktów: {}", e))
                        .ephemeral(true),
                ),
            )
            .await?;
        }
    }

    Ok(())
}

/* ---------- /punkty clear (wyzerowanie) ---------- */

async fn handle_points_clear(
    ctx: &Context,
    app: &AppContext,
    cmd: &CommandInteraction,
    clr_opt: &serenity::all::CommandDataOption,
) -> Result<()> {
    let Some(gid) = cmd.guild_id else {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Ta komenda działa tylko na serwerze.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    };

    // Rozpakuj subopcje
    let mut target: Option<UserId> = None;
    if let CommandDataOptionValue::SubCommand(subs) = &clr_opt.value {
        for o in subs {
            if o.name == "user" {
                if let CommandDataOptionValue::User(uid) = o.value {
                    target = Some(uid);
                }
            }
        }
    }

    let Some(target_id) = target else {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Musisz wskazać użytkownika.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    };

    let current = AdminPoints::get_points(&app.db, target_id.get())
        .await
        .unwrap_or(0.0);
    if current <= 0.0 {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Użytkownik ma już 0 punktów.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    }

    // role aktora
    let actor_roles: Vec<u64> = match gid.member(&ctx.http, cmd.user.id).await {
        Ok(m) => m.roles.iter().map(|r| r.get()).collect(),
        Err(_) => vec![],
    };

    let env = app.env();
    match AdminPoints::adjust_manual(
        &app.db,
        &env,
        cmd.user.id.get(),
        &actor_roles,
        target_id.get(),
        -current,
        "Wyzerowanie punktów",
    )
    .await
    {
        Ok(_) => {
            cmd.create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(format!(
                            "✅ Wyzerowano punkty użytkownika <@{}>.",
                            target_id.get()
                        ))
                        .ephemeral(true),
                ),
            )
            .await?;

            log_points_adjustment(
                ctx,
                app,
                cmd.user.id.get(),
                target_id.get(),
                -current,
                Some(0.0),
                Some("Wyzerowanie punktów"),
            )
            .await;
        }
        Err(e) => {
            cmd.create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(format!("⛔ Nie udało się wyzerować punktów: {}", e))
                        .ephemeral(true),
                ),
            )
            .await?;
        }
    }

    Ok(())
}

/* ---------- /punkty profil (podgląd konkretnego usera) ---------- */

async fn handle_points_profil(
    ctx: &Context,
    app: &AppContext,
    cmd: &CommandInteraction,
    sub_opt: &serenity::all::CommandDataOption,
) -> Result<()> {
    let Some(gid) = cmd.guild_id else {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Ta komenda działa tylko na serwerze.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    };

    // ⬇️ zawężone uprawnienia do PROFILU
    let env = app.env();
    if !is_points_view_allowed(ctx, gid, cmd.user.id).await {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("⛔ Brak uprawnień do podglądu profili.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    }

    // subopcje: user (wymagany)
    let mut target: Option<UserId> = None;
    if let CommandDataOptionValue::SubCommand(subs) = &sub_opt.value {
        for o in subs {
            if o.name == "user" {
                if let CommandDataOptionValue::User(uid) = o.value {
                    target = Some(uid);
                }
            }
        }
    }
    let Some(target) = target else {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Musisz wskazać użytkownika.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    };

    // ⬇️ HARDLOCK: profil tylko dla członków administracji (role: test/mod/admin)
    if !is_target_admin_rank_only(ctx, gid, &env, target).await {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("⛔ Ta osoba nie jest w administracji (wymagana rola: Test moderator / Moderator / Admin).")
                    .ephemeral(true),
            ),
        ).await?;
        return Ok(());
    }

    // zbuduj embed profilu i odpowiedz ephemeral
    let embed = build_profile_embed(ctx, &app.db, target.get()).await?;
    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .add_embed(embed)
                .ephemeral(true),
        ),
    )
    .await?;

    Ok(())
}

/* =========================
uprawnienia i select
========================= */

async fn build_admin_select_options(
    ctx: &Context,
    gid: GuildId,
    env: &str,
) -> Result<Vec<CreateSelectMenuOption>> {
    let members = gid
        .members(&ctx.http, Some(1000), None)
        .await
        .unwrap_or_default();

    // tylko osoby z tymi rolami będą widoczne na liście
    let eligible = [
        env_roles::admin_id(env),
        env_roles::moderator_id(env),
        env_roles::test_moderator_id(env),
    ];

    let mut rows: Vec<(String, String, String)> = members
        .into_iter()
        .filter(|m| m.roles.iter().any(|r| eligible.contains(&r.get())))
        .filter(|m| !m.user.bot)
        .map(|m| {
            let label = m.nick.clone().unwrap_or_else(|| m.user.name.clone());
            let value = m.user.id.get().to_string();
            let desc = format!("ID: {}", m.user.id.get());
            (truncate(&label, 100), value, truncate(&desc, 100))
        })
        .collect();

    rows.sort_by(|a, b| a.0.cmp(&b.0));
    if rows.len() > 25 {
        rows.truncate(25);
    }

    Ok(rows
        .into_iter()
        .map(|(label, value, desc)| CreateSelectMenuOption::new(label, value).description(desc))
        .collect())
}

/* =========================
drobne helpery
========================= */

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        s.chars()
            .take(max.saturating_sub(1))
            .chain("…".chars())
            .collect()
    }
}

/// Czy to nasz komponent?
pub fn is_points_component(comp: &ComponentInteraction) -> bool {
    comp.data.custom_id.as_str() == UI_SELECT_ID
}

/// Czy wskazany użytkownik **jest w administracji** (TYLKO role:
/// test_moderator / moderator / admin – bez patrzenia na permission Administrator)?
async fn is_target_admin_rank_only(
    ctx: &Context,
    gid: GuildId,
    env: &str,
    user_id: UserId,
) -> bool {
    if let Ok(member) = gid.member(&ctx.http, user_id).await {
        let eligible = [
            env_roles::test_moderator_id(env),
            env_roles::moderator_id(env),
            env_roles::admin_id(env),
        ];
        return member.roles.iter().any(|r| eligible.contains(&r.get()));
    }
    false
}

/* =========================
LOG kanał: ADMINS_POINTS
========================= */

fn admin_points_log_channel_id(env: &str) -> u64 {
    // Mapowanie per-ENV na stałą z registry.rs
    let prod = env.eq_ignore_ascii_case("production") || env.eq_ignore_ascii_case("prod");
    if prod {
        channels::prod::ADMINS_POINTS
    } else {
        channels::dev::ADMINS_POINTS
    }
}

fn colour_for_delta(delta: f64) -> u32 {
    if delta > 0.0 {
        0x2ecc71 // zielony
    } else if delta < 0.0 {
        0xe74c3c // czerwony
    } else {
        0x3498db // niebieski
    }
}

async fn log_points_adjustment(
    ctx: &Context,
    app: &AppContext,
    actor_id: u64,
    target_id: u64,
    delta_points: f64,
    new_total_opt: Option<f64>, // nowy stan (jeśli znany)
    reason: Option<&str>,
) {
    let env = app.env();
    let chan_id = admin_points_log_channel_id(&env);
    if chan_id == 0 {
        return;
    }

    // Nazwy i avatary
    let (actor_name, actor_ava) = match UserId::new(actor_id).to_user(&ctx.http).await {
        Ok(u) => (u.name.clone(), u.avatar_url()),
        Err(_) => (format!("ID {}", actor_id), None),
    };

    // target
    let (target_name, target_ava) = match UserId::new(target_id).to_user(&ctx.http).await {
        Ok(u) => (u.name.clone(), u.avatar_url()),
        Err(_) => (format!("ID {}", target_id), None),
    };

    // Dociągnij nowy stan jeśli nie podano
    let new_total = if let Some(t) = new_total_opt {
        t
    } else {
        AdminPoints::get_points(&app.db, target_id)
            .await
            .unwrap_or(0.0)
    };
    let old_total = (new_total - delta_points).clamp(0.0, 100.0);

    let bar = progress_bar(new_total);

    let title = if delta_points >= 0.0 {
        "📈 AdminScore: przyznano punkty"
    } else {
        "📉 AdminScore: odjęto punkty"
    };
    let colour = Colour::new(colour_for_delta(delta_points));

    let delta_str = format!("{:+.2} pkt", delta_points);
    let total_str = format!("{:.1} → **{:.1} / 100.0**", old_total, new_total);

    let mut embed = CreateEmbed::new()
        .title(title)
        .colour(colour)
        .field(
            "Zmienione przez",
            format!("<@{}>\n`{}`", actor_id, actor_name),
            true,
        )
        .field("Cel", format!("<@{}>\n`{}`", target_id, target_name), true)
        .field("Zmiana", delta_str, true)
        .field("Stan (przed → po)", total_str, false)
        .field("Postęp", format!("`{}`", bar), false)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    if let Some(r) = reason {
        if !r.trim().is_empty() {
            embed = embed.field("Powód", format!("_{}_", r.trim()), false);
        }
    }
    if let Some(url) = target_ava {
        embed = embed.thumbnail(url);
    }
    if let Some(icon) = actor_ava {
        embed = embed.footer(CreateEmbedFooter::new(SYSTEM_NAME).icon_url(icon));
    }

    if let Ok(now) = Timestamp::from_unix_timestamp(OffsetDateTime::now_utc().unix_timestamp()) {
        embed = embed.timestamp(now);
    }

    let _ = ChannelId::new(chan_id)
        .send_message(&ctx.http, CreateMessage::new().embed(embed))
        .await;
}

/* =========================
UI: profil + pasek postępu
========================= */

fn progress_bar(points: f64) -> String {
    // 20 „krat” – każda to 5% (5 punktów)
    let total_slots = 20usize;
    let filled = ((points / 100.0) * total_slots as f64).round() as usize;
    let filled = filled.clamp(0, total_slots);
    let empty = total_slots - filled;
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
}

async fn build_profile_embed(
    ctx: &Context,
    db: &Pool<Postgres>,
    user_id: u64,
) -> Result<CreateEmbed> {
    let points = AdminPoints::get_points(db, user_id).await.unwrap_or(0.0);
    let bar = progress_bar(points);

    let mut e = CreateEmbed::new()
        .title("📊 Profil punktowy administratora")
        .colour(Colour::new(0x3498db))
        .field("Użytkownik", format!("<@{}> (`{}`)", user_id, user_id), true)
        .field("Punkty", format!("{:.1} / 100.0", points), true)
        .field("Postęp", format!("`{}`", bar), false)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    if let Ok(u) = UserId::new(user_id).to_user(&ctx.http).await {
        if let Some(ava) = u.avatar_url() {
            e = e.thumbnail(ava);
        }
    }

    Ok(e)
}
