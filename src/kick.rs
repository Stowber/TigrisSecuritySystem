// src/kick.rs

use anyhow::Result;
use once_cell::sync::Lazy;
use std::time::Duration;

use serenity::all::{
    ButtonStyle, ChannelId, Colour, CommandDataOptionValue, CommandInteraction, CommandOptionType,
    ComponentInteraction, Context, CreateActionRow, CreateButton, CreateCommand,
    CreateCommandOption, CreateEmbed, CreateEmbedFooter, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, GuildId, Interaction, MessageId,
    UserId, CreateModal, CreateInputText, InputTextStyle, ModalInteraction,};

use dashmap::DashMap;

use crate::AppContext;

const LOG_CHANNEL_ID: &str = "1408795534973468793";
const SYSTEM_NAME: &str = "Tigris Kick Panel";

// Parsujemy kanał logów raz, nie przy każdym użyciu
static LOG_CHAN: Lazy<Option<ChannelId>> = Lazy::new(|| {
    LOG_CHANNEL_ID
        .parse::<u64>()
        .ok()
        .map(ChannelId::new)
});

// Współbieżny magazyn spraw /kick
static KICK_CASES: Lazy<DashMap<String, KickCase>> = Lazy::new(|| DashMap::new());

#[derive(Clone)]
struct KickCase {
    guild_id: GuildId,
    moderator_id: UserId,
    target_id: UserId,
    reason: Option<String>,
    // Zostawiamy, jeśli kiedyś zechcesz edytować/usuwać panel; na razie nieużywane
    _panel_msg: Option<(ChannelId, MessageId)>,
}

pub struct Kick;

impl Kick {
    /// Rejestr /kick (per gildia)
    pub async fn register_commands(ctx: &Context, guild_id: GuildId) -> Result<()> {
        guild_id
            .create_command(
                &ctx.http,
                CreateCommand::new("kick")
                    .description("Panel wyrzucenia użytkownika z potwierdzeniem")
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::User,
                            "user",
                            "Użytkownik do wyrzucenia",
                        )
                        .required(true),
                    ),
            )
            .await?;
        Ok(())
    }

    /// Router interakcji
    pub async fn on_interaction(ctx: &Context, _app: &AppContext, interaction: Interaction) {
        if let Some(cmd) = interaction.clone().command() {
            if cmd.data.name == "kick" {
                if let Err(e) = handle_kick_slash(ctx, &cmd).await {
                    tracing::warn!(error=?e, "kick slash failed");
                }
                return;
            }
        }
        if let Some(comp) = interaction.clone().message_component() {
            let id = comp.data.custom_id.as_str();
            if id.starts_with("kick:reason:") {
                let _ = on_reason_modal_open(ctx, &comp).await;
                return;
            }
            if id.starts_with("kick:proceed:") {
                let _ = on_proceed(ctx, &comp).await;
                return;
            }
            if id.starts_with("kick:cancel:") {
                let _ = on_cancel(ctx, &comp).await;
                return;
            }
            if id.starts_with("kick:confirm:") {
                let _ = on_confirm(ctx, &comp).await;
                return;
            }
        }
        if let Some(modal) = interaction.modal_submit() {
            if modal.data.custom_id.starts_with("kick:modalreason:") {
                let _ = on_reason_modal_submit(ctx, &modal).await;
            }
        }
    }
}

/* ---------------- core ---------------- */

async fn handle_kick_slash(ctx: &Context, cmd: &CommandInteraction) -> Result<()> {
    let Some(gid) = cmd.guild_id else {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Użyj na serwerze.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    };

    if !has_permission(ctx, gid, cmd.user.id, crate::permissions::Permission::Kick).await {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("⛔ Brak uprawnień do wyrzucania użytkowników.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    }

    let mut target: Option<UserId> = None;
    if let Some(first) = cmd.data.options.first() {
        if first.name == "user" {
            if let CommandDataOptionValue::User(u) = first.value {
                target = Some(u);
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

    // Blokady wstępne
    if target == cmd.user.id || target.get() == ctx.cache.current_user().id.get() {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Nie można wyrzucić tego użytkownika.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    }

    // Pre-check możliwości bota (perms + hierarchia + owner)
    if let Err(e) = ensure_bot_can_kick(ctx, gid, target).await {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(e)
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    }

    let case_id = format!("{}-{}", cmd.id.get(), cmd.user.id.get());
    KICK_CASES.insert(
        case_id.clone(),
        KickCase {
            guild_id: gid,
            moderator_id: cmd.user.id,
            target_id: target,
            reason: None,
            _panel_msg: None,
        },
    );

    // TTL na 15 minut – sprzątamy, jeśli panel wygaśnie
    let case_id_for_cleanup = case_id.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(900)).await;
        KICK_CASES.remove(&case_id_for_cleanup);
    });

    let embed = summary_embed(&case_id);
    let components = vec![CreateActionRow::Buttons(vec![
        CreateButton::new(format!("kick:reason:{case_id}"))
            .label("Wpisz powód")
            .style(ButtonStyle::Primary),
        CreateButton::new(format!("kick:proceed:{case_id}"))
            .label("Dalej")
            .style(ButtonStyle::Success),
        CreateButton::new(format!("kick:cancel:{case_id}"))
            .label("Anuluj")
            .style(ButtonStyle::Danger),
    ])];

    cmd.create_response(
        &ctx.http,
        CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .add_embed(embed)
                .components(components)
                .ephemeral(true),
        ),
    )
    .await?;

    Ok(())
}

async fn on_reason_modal_open(ctx: &Context, comp: &ComponentInteraction) -> Result<()> {
    let Some(case_id) = comp.data.custom_id.split(':').nth(2).map(|s| s.to_string()) else { return Ok(()); };
    let modal = CreateModal::new(format!("kick:modalreason:{case_id}"), "Powód wyrzucenia")
        .components(vec![CreateActionRow::InputText(
            CreateInputText::new(
                InputTextStyle::Paragraph,
                "reason",
                "Powód (wymagany)",
            )
            .required(true)
            .max_length(512),
        )]);
    comp.create_response(&ctx.http, CreateInteractionResponse::Modal(modal)).await?;
    Ok(())
}

async fn on_reason_modal_submit(ctx: &Context, modal: &ModalInteraction) -> Result<()> {
    let Some(case_id) = modal.data.custom_id.split(':').nth(2).map(|s| s.to_string()) else { return Ok(()); };
    let mut reason_val: Option<String> = None;
    for row in &modal.data.components {
        for comp in &row.components {
            if let serenity::all::ActionRowComponent::InputText(input) = comp {
                if input.custom_id == "reason" || reason_val.is_none() {
                    if let Some(v) = &input.value {
                        reason_val = Some(v.trim().to_string());
                    }
                }
            }
        }
    }
    let mut reason = reason_val.unwrap_or_default();
    if reason.trim().is_empty() {
        modal.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Powód nie może być pusty.")
                    .ephemeral(true),
            ),
        ).await?;
        return Ok(());
    }
    // (opcjonalne) twardy limit embed field
    if reason.chars().count() > 1024 { reason = reason.chars().take(1024).collect(); }

    if let Some(mut entry) = KICK_CASES.get_mut(&case_id) {
        entry.reason = Some(reason);
    }

    let embed = summary_embed(&case_id);
    let components = vec![CreateActionRow::Buttons(vec![
        CreateButton::new(format!("kick:proceed:{case_id}"))
            .label("Dalej")
            .style(ButtonStyle::Success),
        CreateButton::new(format!("kick:cancel:{case_id}"))
            .label("Anuluj")
            .style(ButtonStyle::Danger),
    ])];
    modal.create_response(
        &ctx.http,
        CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("✅ Powód zapisany.")
                .add_embed(embed)
                .components(components)
                .ephemeral(true),
        ),
    ).await?;
    Ok(())
}

async fn on_proceed(ctx: &Context, comp: &ComponentInteraction) -> Result<()> {
    let Some(case_id) = comp.data.custom_id.split(':').nth(2).map(|s| s.to_string()) else { return Ok(()); };
    let Some(case) = KICK_CASES.get(&case_id) else { return Ok(()); };

    if case.reason.as_deref().unwrap_or("").trim().is_empty() {
        comp.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Wpisz powód przed potwierdzeniem.")
                    .ephemeral(true),
            ),
        ).await?;
        return Ok(());
    }

    let embed = confirm_embed(&case);
    let components = vec![CreateActionRow::Buttons(vec![
        CreateButton::new(format!("kick:confirm:{case_id}"))
            .label("✅ Potwierdź wyrzucenie")
            .style(ButtonStyle::Danger),
        CreateButton::new(format!("kick:cancel:{case_id}"))
            .label("Anuluj")
            .style(ButtonStyle::Secondary),
    ])];
    comp.create_response(
        &ctx.http,
        CreateInteractionResponse::UpdateMessage(
            CreateInteractionResponseMessage::new()
                .add_embed(embed)
                .components(components),
        ),
    ).await?;
    Ok(())
}

async fn on_cancel(ctx: &Context, comp: &ComponentInteraction) -> Result<()> {
    let Some(case_id) = comp.data.custom_id.split(':').nth(2).map(|s| s.to_string()) else { return Ok(()); };
    KICK_CASES.remove(&case_id);
    comp.create_response(
        &ctx.http,
        CreateInteractionResponse::UpdateMessage(
            CreateInteractionResponseMessage::new()
                .content("✅ Panel wyrzucenia anulowany.")
                .components(vec![])
                .embeds(vec![]),
        ),
    ).await?;
    Ok(())
}

async fn on_confirm(ctx: &Context, comp: &ComponentInteraction) -> Result<()> {
    let Some(case_id) = comp.data.custom_id.split(':').nth(2).map(|s| s.to_string()) else { return Ok(()); };
    let case = KICK_CASES.remove(&case_id).map(|(_, v)| v);
    let Some(case) = case else { return Ok(()); };

    // Sprawdź uprawnienia moderatora jeszcze raz (mogły się zmienić)
    if !has_permission(ctx, case.guild_id, case.moderator_id, crate::permissions::Permission::Kick).await {
        comp.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("⛔ Utracono uprawnienia do wyrzucania.")
                    .ephemeral(true),
            ),
        ).await?;
        return Ok(());
    }

    // Sprawdź możliwości bota jeszcze raz
    if let Err(e) = ensure_bot_can_kick(ctx, case.guild_id, case.target_id).await {
        comp.create_response(
            &ctx.http,
            CreateInteractionResponse::UpdateMessage(
                CreateInteractionResponseMessage::new()
                    .content(e)
                    .components(vec![])
                    .embeds(vec![]),
            ),
        ).await?;
        return Ok(());
    }

    let reason_text = case.reason.clone().unwrap_or_else(|| "Brak powodu".into());
    let _ = send_formal_dm(ctx, case.target_id, case.moderator_id, &reason_text).await;

    if let Err(e) = case.guild_id.kick_with_reason(&ctx.http, case.target_id, &reason_text).await {
        // Rozróżnianie typowych przypadków mogłoby tu być rozbudowane po kodach błędów
        comp.create_response(
            &ctx.http,
            CreateInteractionResponse::UpdateMessage(
                CreateInteractionResponseMessage::new()
                    .content(format!("⛔ Nie udało się wyrzucić użytkownika: {e}"))
                    .components(vec![])
                    .embeds(vec![]),
            ),
        ).await?;
        return Ok(());
    }

    if let Some(chan) = LOG_CHAN.as_ref() {
        let embed = make_log_embed(&case, &reason_text);
        let _ = chan.send_message(&ctx.http, embed).await;
    }

    comp.create_response(
        &ctx.http,
        CreateInteractionResponse::UpdateMessage(
            CreateInteractionResponseMessage::new()
                .content(format!("✅ Wyrzucono <@{}>.", case.target_id.get()))
                .components(vec![])
                .embeds(vec![]),
        ),
    ).await?;
    Ok(())
}

/* ---------------- embeds ---------------- */

fn summary_embed(case_id: &str) -> CreateEmbed {
    let mut e = CreateEmbed::new()
        .title("Panel wyrzucenia użytkownika")
        .colour(Colour::new(0xE67E22))
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    if let Some(c) = KICK_CASES.get(case_id) {
        e = e
            .field("Użytkownik", format!("<@{}>", c.target_id.get()), true)
            .field("Moderator", format!("<@{}>", c.moderator_id.get()), true)
            .field("Powód", c.reason.clone().unwrap_or_else(|| "—".into()), false);
    }
    e
}

fn confirm_embed(case: &KickCase) -> CreateEmbed {
    CreateEmbed::new()
        .title("Potwierdzenie wyrzucenia")
        .colour(Colour::new(0xE67E22))
        .footer(CreateEmbedFooter::new(SYSTEM_NAME))
        .field("Użytkownik", format!("<@{}>", case.target_id.get()), true)
        .field("Moderator", format!("<@{}>", case.moderator_id.get()), true)
        .field("Powód", case.reason.clone().unwrap_or_else(|| "—".into()), false)
}

fn make_log_embed(case: &KickCase, reason: &str) -> CreateMessage {
    let embed = CreateEmbed::new()
        .title("✅ Kick wykonany")
        .colour(Colour::new(0xE67E22))
        .footer(CreateEmbedFooter::new(SYSTEM_NAME))
        .field("Użytkownik", format!("<@{}>", case.target_id.get()), true)
        .field("Moderator", format!("<@{}>", case.moderator_id.get()), true)
        .field("Powód", reason, false);
    CreateMessage::new().embed(embed)
}

async fn send_formal_dm(ctx: &Context, target: UserId, moderator: UserId, reason: &str) -> Result<()> {
    let user = target.to_user(&ctx.http).await?;
    let mod_user = moderator.to_user(&ctx.http).await?;
    let mut e = CreateEmbed::new()
        .title("Informacja o wyrzuceniu z serwera")
        .colour(Colour::new(0xE67E22))
        .description("Zostałeś wyrzucony z serwera przez moderatora.")
        .field("Moderator", format!("<@{}>", moderator.get()), true)
        .field("Powód", reason, false)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));
    if let Some(avatar) = mod_user.avatar_url() {
        e = e.thumbnail(avatar);
    }
    let dm = user.create_dm_channel(&ctx.http).await?;
    let _ = dm.send_message(&ctx.http, CreateMessage::new().embed(e)).await;
    Ok(())
}

/* ---------------- helpers ---------------- */

async fn has_permission(ctx: &Context, gid: GuildId, uid: UserId, perm: crate::permissions::Permission) -> bool {
    if let Ok(member) = gid.member(&ctx.http, uid).await {
        use crate::permissions::{Role, role_has_permission};
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

/// Zwraca komunikat błędu w `Err(String)`, jeśli bot nie może kopać celu.
async fn ensure_bot_can_kick(ctx: &Context, gid: GuildId, target: UserId) -> Result<(), String> {
    // Pobierz podstawowe dane gildii i członków
    let guild = gid.to_partial_guild(&ctx.http).await.map_err(|_| "⛔ Nie udało się odczytać danych serwera.".to_string())?;

    if guild.owner_id == target {
        return Err("⛔ Nie można wyrzucić właściciela serwera.".into());
    }

    let bot_id = ctx.cache.current_user().id;
    let bot_m = gid.member(&ctx.http, bot_id).await.map_err(|_| "⛔ Nie udało się odczytać ról bota.".to_string())?;
    let tgt_m = gid.member(&ctx.http, target).await.map_err(|_| "⛔ Ten użytkownik nie jest na serwerze.".to_string())?;

    // Sprawdzenie uprawnienia KICK_MEMBERS na bocie
    #[allow(deprecated)]
    let perms = guild.member_permissions(&bot_m);
    if !perms.kick_members() && !perms.administrator() {
        return Err("⛔ Bot nie ma uprawnienia do wyrzucania (KICK_MEMBERS).".into());
    }

    // Hierarchia ról: najwyższa rola bota musi być wyżej niż najwyższa rola celu
    let top_pos = |m: &serenity::all::Member| {
        m.roles
            .iter()
            .filter_map(|rid| guild.roles.get(rid))
            .map(|r| r.position)
            .max()
            .unwrap_or(0)
    };

    if top_pos(&bot_m) <= top_pos(&tgt_m) && !perms.administrator() {
        return Err("⛔ Bot ma zbyt niską pozycję ról, aby wyrzucić tego użytkownika.".into());
    }

    Ok(())
}
