// src/commands_sync.rs

use anyhow::Result;
use serenity::all::{
    Command, CommandInteraction, Context, CreateCommand, CreateInteractionResponse,
    CreateInteractionResponseMessage, GuildId, Member,
};

use crate::chatguard::ChatGuard;
use crate::verify::Verify;
use crate::admin_points::AdminPoints;

// ⬇⬇⬇ DODAJ:
use crate::ban::Ban;
use crate::kick::Kick;
use crate::warn::Warns;
use crate::command_acl;
use crate::mdel::MDel;
use crate::mute::Mute;
use crate::userinfo::UserInfo;
use crate::admcheck::AdmCheck;
use crate::antinuke::commands as antinuke_commands;
use crate::watchlist::Watchlist;

pub const CLEAN_NAME: &str = "slash-clean";
pub const RESYNC_NAME: &str = "slash-resync";

pub async fn register_commands(ctx: &Context, guild_id: GuildId) -> Result<()> {
    guild_id
        .create_command(
            &ctx.http,
            CreateCommand::new(CLEAN_NAME)
                .description("Wyczyść WSZYSTKIE komendy (gildyjne + globalne) dla tej aplikacji"),
        )
        .await?;
    guild_id
        .create_command(
            &ctx.http,
            CreateCommand::new(RESYNC_NAME).description(
                "Wyczyść global/guild i zarejestruj na nowo tylko wymagane komendy w tej gildii",
            ),
        )
        .await?;
    Ok(())
}

pub async fn handle_slash(ctx: &Context, cmd: &CommandInteraction) -> Result<()> {
    match cmd.data.name.as_str() {
        CLEAN_NAME => handle_clean(ctx, cmd).await,
        RESYNC_NAME => handle_resync(ctx, cmd).await,
        _ => Ok(()),
    }
}

/// Register all application commands for the guild.
pub async fn register_all_commands(ctx: &Context, guild_id: GuildId) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();

    macro_rules! reg {
        ($fut:expr, $name:expr) => {{
            match $fut.await {
                Ok(_) => names.push(format!("/{}", $name)),
                Err(e) => tracing::warn!(error=?e, "rejestracja {} nie powiodła się", $name),
            }
        }};
    }

    reg!(Verify::register_commands(ctx, guild_id), "verify-panel");
    reg!(ChatGuard::register_commands(ctx, guild_id), "chatguard");
    reg!(AdminPoints::register_commands(ctx, guild_id), "punkty");
    reg!(Ban::register_commands(ctx, guild_id), "ban");
    reg!(Kick::register_commands(ctx, guild_id), "kick");
    reg!(Warns::register_commands(ctx, guild_id), "warn");
    reg!(MDel::register_commands(ctx, guild_id), "mdel");
    reg!(Mute::register_commands(ctx, guild_id), "mute");
    reg!(UserInfo::register_commands(ctx, guild_id), "userinfo");
    reg!(AdmCheck::register_commands(ctx, guild_id), "admcheck");
    reg!(antinuke_commands::register_commands(ctx, guild_id), "antinuke");
    reg!(Watchlist::register_commands(ctx, guild_id), "watchlist");
    reg!(register_commands(ctx, guild_id), "slash-clean / slash-resync");

    if let Err(e) = command_acl::apply_permissions(ctx, guild_id).await {
        tracing::warn!(error=?e, "apply_permissions failed");
    }

    names
}

async fn handle_clean(ctx: &Context, cmd: &CommandInteraction) -> Result<()> {
    if !is_allowed(ctx, cmd).await {
        return reply_ephemeral(ctx, cmd, "⛔ Brak uprawnień.").await;
    }
    let Some(gid) = cmd.guild_id else {
        return reply_ephemeral(ctx, cmd, "Ta komenda działa tylko w gildii.").await;
    };

    let guild_before = gid.get_commands(&ctx.http).await.unwrap_or_default().len();
    let global_before = Command::get_global_commands(&ctx.http).await.unwrap_or_default().len();

    gid.set_commands(&ctx.http, Vec::<CreateCommand>::new()).await?;
    Command::set_global_commands(&ctx.http, Vec::<CreateCommand>::new()).await?;

    let guild_after = gid.get_commands(&ctx.http).await.unwrap_or_default().len();
    let global_after = Command::get_global_commands(&ctx.http).await.unwrap_or_default().len();

    reply_ephemeral(
        ctx,
        cmd,
        &format!(
            "🧹 Wyczyściłem komendy.\n• Guild: **{} → {}**\n• Global: **{} → {}**",
            guild_before, guild_after, global_before, global_after
        ),
    )
    .await
}

async fn handle_resync(ctx: &Context, cmd: &CommandInteraction) -> Result<()> {
    if !is_allowed(ctx, cmd).await {
        return reply_ephemeral(ctx, cmd, "⛔ Brak uprawnień.").await;
    }
    let Some(gid) = cmd.guild_id else {
        return reply_ephemeral(ctx, cmd, "Ta komenda działa tylko w gildii.").await;
    };

    // 1) Wyzeruj GLOBAL i GUILD
    Command::set_global_commands(&ctx.http, Vec::<CreateCommand>::new()).await?;
    gid.set_commands(&ctx.http, Vec::<CreateCommand>::new()).await?;

    register_all_commands(ctx, gid).await;


    let guild_now = gid.get_commands(&ctx.http).await.unwrap_or_default();
    let global_now = Command::get_global_commands(&ctx.http).await.unwrap_or_default();

    let list = if guild_now.is_empty() {
        "_(brak)_".to_string()
    } else {
        guild_now.iter().map(|c| format!("/{}", c.name)).collect::<Vec<_>>().join(", ")
    };

    reply_ephemeral(
        ctx,
        cmd,
        &format!(
            "🔁 Przeładowano komendy.\n• Guild: {}\n• Global: {}",
            list,
            if global_now.is_empty() {
                "_(brak)_".to_string()
            } else {
                global_now.iter().map(|c| format!("/{}", c.name)).collect::<Vec<_>>().join(", ")
            }
        ),
    )
    .await
}

/* ---------------- helpers ---------------- */

async fn is_allowed(ctx: &Context, cmd: &CommandInteraction) -> bool {
    let Some(gid) = cmd.guild_id else { return false; };
    if let Ok(pg) = gid.to_partial_guild(&ctx.http).await {
        if pg.owner_id == cmd.user.id {
            return true;
        }
    }
    let env = std::env::var("TSS_ENV").unwrap_or_else(|_| "production".to_string());
    if let Ok(member) = gid.member(&ctx.http, cmd.user.id).await {
        use crate::permissions::{Permission, Role, role_has_permission};
        let perms = [
            Permission::SlashClean,
            Permission::SlashResync,
        ];
        for perm in perms {
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
        if has_administrator(ctx, gid, &member).await {
            return true;
        }
    }
    false
}

async fn has_administrator(ctx: &Context, gid: GuildId, member: &Member) -> bool {
    if let Ok(roles_map) = gid.roles(&ctx.http).await {
        for rid in &member.roles {
            if let Some(role) = roles_map.get(rid) {
                if role.permissions.administrator() {
                    return true;
                }
            }
        }
    }
    false
}

async fn reply_ephemeral(ctx: &Context, cmd: &CommandInteraction, msg: &str) -> Result<()> {
    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(msg)
                .ephemeral(true),
        ),
    )
    .await?;
    Ok(())
}
