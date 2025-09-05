// src/mdel.rs
use anyhow::Result;
use serenity::all::*;

use crate::{AppContext, registry::env_channels};
use crate::admcheck::has_permission;

pub struct MDel;

impl MDel {
    pub async fn register_commands(ctx: &Context, gid: GuildId) -> Result<()> {
        gid.create_command(
            &ctx.http,
            CreateCommand::new("mdel")
                .description("Masowe usuwanie wiadomości (≤14 dni)")
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::Integer,
                        "count",
                        "Ile wiadomości (1–100)",
                    )
                    .required(true),
                )
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::Channel,
                        "channel",
                        "Kanał (domyślnie bieżący)",
                    )
                )
                .default_member_permissions(Permissions::MANAGE_MESSAGES),
        )
        .await?;
        Ok(())
    }

    pub async fn on_interaction(ctx: &Context, app: &AppContext, interaction: Interaction) {
        if let Some(cmd) = interaction.clone().command() {
            if cmd.data.name.as_str() == "mdel" {
                if let Err(e) = handle_mdel(ctx, app, &cmd).await {
                    // jeśli coś pójdzie nie tak po ACK – przynajmniej zalogujemy
                    tracing::warn!(?e, "mdel failed");
                    let _ = cmd
                        .edit_response(
                            &ctx.http,
                            EditInteractionResponse::new().content("❌ Błąd podczas usuwania wiadomości."),
                        )
                        .await;
                }
            }
        }
    }
}

async fn handle_mdel(ctx: &Context, app: &AppContext, cmd: &CommandInteraction) -> Result<()> {
    // 1) NATYCHMIASTOWY ACK (żeby Discord nie pokazał „Aplikacja nie reaguje”)
    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true)),
    )
    .await?;

    // 2) Podstawowa walidacja + uprawnienia
    let Some(gid) = cmd.guild_id else {
        return edit_ephemeral(ctx, cmd, "Użyj na serwerze.").await;
    };
    if !user_can_manage_messages(ctx, gid, cmd.user.id).await {
        return edit_ephemeral(ctx, cmd, "⛔ Wymagane **Zarządzanie wiadomościami**.").await;
    }
    if !has_permission(ctx, gid, cmd.user.id, crate::permissions::Permission::Mdel).await {
        return edit_ephemeral(ctx, cmd, "⛔ Brak uprawnień do użycia tej komendy.").await;
    }

    // 3) Parametry
    let mut count: i64 = 0;
    let mut target_channel: Option<ChannelId> = None;
    for o in &cmd.data.options {
        match (&o.name[..], &o.value) {
            ("count",   CommandDataOptionValue::Integer(n)) => count = *n,
            ("channel", CommandDataOptionValue::Channel(c)) => target_channel = Some(*c),
            _ => {}
        }
    }
    if count <= 0 { count = 1; }
    let count = count.min(100) as usize; // bulk delete max 100
    let ch = target_channel.unwrap_or(cmd.channel_id);

    // 4) Pobierz i usuń (tylko < 14 dni)
    let mut deleted_total = 0usize;
    let mut before: Option<MessageId> = None;
    let now = Timestamp::now().unix_timestamp();
    let max_age = 14 * 24 * 60 * 60; // 14 dni (w sekundach)

    while deleted_total < count {
        let to_get = ((count - deleted_total).min(100)) as u8;
        let mut builder = GetMessages::new().limit(to_get);
        if let Some(b) = before { builder = builder.before(b); }

        let msgs = match ch.messages(&ctx.http, builder).await {
            Ok(v) => v,
            Err(e) => {
                return edit_ephemeral(ctx, cmd, &format!("❌ Nie mogę pobrać wiadomości: {e}")).await;
            }
        };
        if msgs.is_empty() { break; }

        // przygotuj ID do bulk delete
        let mut to_delete: Vec<MessageId> = Vec::new();
        for m in &msgs {
            let age = now - m.timestamp.unix_timestamp();
            if age < max_age {
                to_delete.push(m.id);
            }
        }
        if to_delete.is_empty() {
            break; // wszystko co dalej jest starsze niż 14 dni
        }

        let delete_res = if to_delete.len() == 1 {
            ch.delete_message(&ctx.http, to_delete[0]).await
        } else {
            ch.delete_messages(&ctx.http, to_delete.clone()).await
        };

        if let Err(e) = delete_res {
            return edit_ephemeral(ctx, cmd, &format!("❌ Nie mogę usunąć wiadomości: {e}")).await;
        }

        deleted_total += to_delete.len();
        before = msgs.last().map(|m| m.id);
    }

    // 5) Logi
    if let Some(log_ch) = log_channel(app) {
        let embed = CreateEmbed::new()
            .title("🧹 Masowe usuwanie wiadomości")
            .colour(Colour::new(0xE74C3C))
            .field("Moderator", format!("<@{}>", cmd.user.id.get()), true)
            .field("Kanał", format!("<#{}>", ch.get()), true)
            .field("Usunięto", format!("**{}**", deleted_total), true)
            .footer(CreateEmbedFooter::new("Tigris – /mdel"));
        let _ = ChannelId::new(log_ch)
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }

    // 6) Odpowiedź dla moderatora
    cmd.edit_response(
        &ctx.http,
        EditInteractionResponse::new().content(format!(
            "✅ Usunięto **{}** wiadomości (młodszych niż 14 dni) w <#{}>.",
            deleted_total,
            ch.get()
        )),
    )
    .await?;

    Ok(())
}

async fn user_can_manage_messages(ctx: &Context, gid: GuildId, uid: UserId) -> bool {
    if let Ok(m) = gid.member(&ctx.http, uid).await {
        if let Ok(p) = m.permissions(&ctx.cache) {
            return p.manage_messages() || p.administrator();
        }
    }
    false
}

async fn edit_ephemeral(ctx: &Context, cmd: &CommandInteraction, msg: &str) -> Result<()> {
    cmd.edit_response(&ctx.http, EditInteractionResponse::new().content(msg)).await?;
    Ok(())
}

fn log_channel(app: &AppContext) -> Option<u64> {
    let env = app.env();
    // LOGS_MESSAGE_DELETE w registry.rs
    let id = env_channels::logs::message_delete_id(&env);
    if id == 0 { None } else { Some(id) }
}
