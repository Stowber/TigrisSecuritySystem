// src/kick.rs

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serenity::all::{
    ChannelId, Colour, CommandDataOptionValue, CommandInteraction, CommandOptionType, Context,
    CreateCommand, CreateCommandOption, CreateEmbed, CreateEmbedFooter, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, EditInteractionResponse, GuildId, Member,
    Permissions, User, UserId,
};

use crate::{AppContext, registry::env_channels};

const SYSTEM_NAME: &str = "Tigris Kick System™";
const SERVER_NAME: &str = "Unfaithful";

pub struct Kick;

impl Kick {
    /// Rejestr /kick (per gildia)
    pub async fn register_commands(ctx: &Context, guild_id: GuildId) -> Result<()> {
        guild_id
            .create_command(
                &ctx.http,
                CreateCommand::new("kick")
                    .description("Wyrzuć użytkownika z serwera (z powodem)")
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::User,
                            "user",
                            "Kogo chcesz wyrzucić",
                        )
                        .required(true),
                    )
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::String,
                            "reason",
                            "Powód wyrzucenia",
                        )
                        .required(true),
                    )
                    .default_member_permissions(Permissions::KICK_MEMBERS),
            )
            .await?;
        Ok(())
    }

    /// Router interakcji
    pub async fn on_interaction(ctx: &Context, app: &AppContext, interaction: serenity::all::Interaction) {
        if let Some(cmd) = interaction.command() {
            if cmd.data.name == "kick" {
                if let Err(e) = handle_kick(ctx, app, &cmd).await {
                    tracing::warn!(error=?e, "kick failed");
                }
            }
        }
    }
}

/* ---------------- core ---------------- */

async fn handle_kick(ctx: &Context, app: &AppContext, cmd: &CommandInteraction) -> Result<()> {
    // 0) natychmiastowy ack (ephemeral) — unikamy „Aplikacja nie reaguje”
    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Defer(
            CreateInteractionResponseMessage::new().ephemeral(true)
        ),
    ).await?;

    let Some(gid) = cmd.guild_id else {
        return edit_ephemeral_text(ctx, cmd, "Ta komenda działa tylko w gildii.").await;
    };

    // 1) Pobierz argumenty
    let mut target: Option<UserId> = None;
    let mut reason: Option<String> = None;
    for opt in &cmd.data.options {
        match (opt.name.as_str(), &opt.value) {
            ("user",   CommandDataOptionValue::User(u))   => target = Some(*u),
            ("reason", CommandDataOptionValue::String(s)) => reason = Some(s.clone()),
            _ => {}
        }
    }
    let Some(target_id) = target else {
        return edit_ephemeral_text(ctx, cmd, "Musisz wskazać użytkownika.").await;
    };
    let reason_text = reason.unwrap_or_else(|| "Brak powodu".into());

    // 2) Walidacje: permission + self/bot/owner + hierarchia
    if !user_can_kick(ctx, gid, cmd.user.id).await {
        return edit_ephemeral_text(ctx, cmd, "⛔ Brak uprawnień do wyrzucania.").await;
    }
    if target_id == cmd.user.id || target_id.get() == ctx.cache.current_user().id.get() {
        return edit_ephemeral_text(ctx, cmd, "Nie można wyrzucić tego użytkownika.").await;
    }
    if let Ok(pg) = gid.to_partial_guild(&ctx.http).await {
        if pg.owner_id == target_id {
            return edit_ephemeral_text(ctx, cmd, "Nie można wyrzucić właściciela gildii.").await;
        }
    }
    if !bot_can_target(ctx, gid, target_id).await {
        return edit_ephemeral_text(ctx, cmd, "⛔ Moje uprawnienia/pozycja ról nie pozwalają wyrzucić tego użytkownika.").await;
    }

    // 3) DM – elegancka wiadomość (ignore error)
    let _ = send_kick_dm(ctx, target_id, &reason_text).await;

    // 4) Kick
    let audit_reason = format!("[{}] {}", SYSTEM_NAME, &reason_text);
    if let Err(e) = gid.kick_with_reason(&ctx.http, target_id, &audit_reason).await {
        return edit_ephemeral_text(ctx, cmd, &format!("⛔ Nie udało się wyrzucić użytkownika: {e}")).await;
    }

    // 5) Log na kanale LOGS_BAN_KICK_MUTE (jeśli ustawiono)
    if let Some(log_ch) = log_channel_bkm(app) {
        let embed = kick_log_embed(ctx, gid, cmd.user.id, target_id, &reason_text).await;
        let _ = ChannelId::new(log_ch)
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }

    // 6) Potwierdzenie dla moda – estetyczny embed
    let confirm = kick_confirm_embed(ctx, gid, cmd.user.id, target_id, &reason_text).await;
    cmd.edit_response(&ctx.http, EditInteractionResponse::new().embeds(vec![confirm])).await?;
    Ok(())
}

/* ---------------- embeds ---------------- */

async fn kick_confirm_embed(
    ctx: &Context,
    gid: GuildId,
    moderator_id: UserId,
    target_id: UserId,
    reason: &str,
) -> CreateEmbed {
    let when = now_unix();
    let guild = guild_name(ctx, gid).await.unwrap_or_else(|| "—".into());
    let mut e = CreateEmbed::new()
        .colour(Colour::new(0x2ECC71)) // zielony – sukces
        .title("👢 Kick wykonany")
        .footer(CreateEmbedFooter::new(SYSTEM_NAME))
        .description(format!(
            "**Serwer:** `{guild}`\n**Kiedy:** <t:{when}:F> • <t:{when}:R>",
        ))
        .field("Użytkownik", format!("<@{}> (`{}`)", target_id.get(), target_id.get()), true)
        .field("Administrator", format!("<@{}> (`{}`)", moderator_id.get(), moderator_id.get()), true)
        .field("Powód", format!("```{}```", truncate_code(reason, 900)), false);

    if let Ok(user) = target_id.to_user(&ctx.http).await {
        if let Some(avatar) = user.avatar_url() {
            e = e.thumbnail(avatar);
        }
    }
    e
}

async fn kick_log_embed(
    ctx: &Context,
    gid: GuildId,
    moderator_id: UserId,
    target_id: UserId,
    reason: &str,
) -> CreateEmbed {
    let when_unix = now_unix();
    let guild = guild_name(ctx, gid).await.unwrap_or_else(|| "—".into());

    let mut e = CreateEmbed::new()
        .title("👢 Wyrzucono użytkownika")
        .colour(Colour::new(0xE67E22)) // pomarańcz – action
        .footer(CreateEmbedFooter::new(SYSTEM_NAME))
        .description(format!("**Serwer:** `{guild}` • **Kiedy:** <t:{when_unix}:F> • <t:{when_unix}:R>"))
        .field("Użytkownik", format!("<@{}> (`{}`)", target_id.get(), target_id.get()), true)
        .field("Administrator", format!("<@{}> (`{}`)", moderator_id.get(), moderator_id.get()), true)
        .field("Powód", format!("```{}```", truncate_code(reason, 1500)), false);

    if let Ok(user) = target_id.to_user(&ctx.http).await {
        if let Some(avatar) = user.avatar_url() {
            e = e.thumbnail(avatar);
        }
    }
    e
}

async fn send_kick_dm(ctx: &Context, target: UserId, reason: &str) -> Result<()> {
    let user: User = target.to_user(&ctx.http).await?;
    let mut e = CreateEmbed::new()
        .title(format!("Informacja o wyrzuceniu – {SERVER_NAME}"))
        .colour(Colour::new(0xE67E22))
        .description(
            "Szanowny Użytkowniku,\n\n\
             Informujemy, że Twoje konto zostało **wyrzucone** z serwera. \
             Jeśli uważasz, że zaszła pomyłka, skontaktuj się z administracją.\n",
        )
        .field("Powód", format!("```{}```", truncate_code(reason, 900)), false)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    if let Some(avatar) = user.avatar_url() {
        e = e.thumbnail(avatar);
    }

    let dm = user.create_dm_channel(&ctx.http).await?;
    let _ = dm.send_message(&ctx.http, CreateMessage::new().embed(e)).await;
    Ok(())
}

/* ---------------- helpers ---------------- */

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

async fn guild_name(ctx: &Context, gid: GuildId) -> Option<String> {
    gid.to_partial_guild(&ctx.http).await.ok().map(|g| g.name)
}

async fn edit_ephemeral_text(ctx: &Context, cmd: &CommandInteraction, msg: &str) -> Result<()> {
    cmd.edit_response(&ctx.http, EditInteractionResponse::new().content(msg)).await?;
    Ok(())
}

fn truncate_code(s: &str, max: usize) -> String {
    let mut out = s.trim().to_string();
    if out.len() > max {
        out.truncate(max.saturating_sub(1));
        out.push('…');
    }
    out
}

async fn user_can_kick(ctx: &Context, gid: GuildId, uid: UserId) -> bool {
    if let Ok(member) = gid.member(&ctx.http, uid).await {
        if let Ok(perms) = member.permissions(&ctx.cache) { // (deprecated, ale OK)
            return perms.kick_members() || perms.administrator();
        }
    }
    false
}

/// Czy BOT może celować w tego użytkownika – sprawdzamy hierarchię ról.
async fn bot_can_target(ctx: &Context, gid: GuildId, target: UserId) -> bool {
    let Ok(bot_id) = ctx.http.get_current_user().await.map(|u| u.id) else { return false; };
    let (Ok(target_m), Ok(bot_m)) = (gid.member(&ctx.http, target).await, gid.member(&ctx.http, bot_id).await) else {
        return false;
    };
    // właściciel nie do ruszenia
    if let Ok(pg) = gid.to_partial_guild(&ctx.http).await {
        if pg.owner_id == target { return false; }
    }
    // porównaj najwyższe pozycje ról
    let Ok(roles_map) = gid.roles(&ctx.http).await else { return false; };
    let t_pos = highest_role_position(&target_m, &roles_map);
    let b_pos = highest_role_position(&bot_m, &roles_map);
    b_pos > t_pos
}

fn highest_role_position(
    member: &Member,
    roles_map: &std::collections::HashMap<serenity::all::RoleId, serenity::all::Role>
) -> i64 {
    member.roles.iter()
        .filter_map(|rid| roles_map.get(rid).map(|r| r.position))
        .max()
        .unwrap_or(0) as i64
}

/// Id kanału logów z env (LOGS_BAN_KICK_MUTE). Zwraca None jeśli 0/nieustawione.
fn log_channel_bkm(app: &AppContext) -> Option<u64> {
    let env = app.env();
    let id = env_channels::logs::ban_kick_mute_id(&env);
    if id == 0 { None } else { Some(id) }
}
