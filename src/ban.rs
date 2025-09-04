// src/ban.rs

use std::time::Duration;

use anyhow::Result;
use dashmap::DashMap;
use once_cell::sync::Lazy;
use tokio::time::sleep;

use serenity::all::{
    ActionRowComponent, ButtonStyle, ChannelId, CommandDataOptionValue, CommandInteraction,
    CommandOptionType, ComponentInteraction, Context, CreateActionRow, CreateButton, CreateCommand,
    CreateCommandOption, CreateEmbed, CreateEmbedFooter, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateInputText, CreateModal, CreateSelectMenu,
    CreateSelectMenuKind, CreateSelectMenuOption, EditMessage, GuildId, InputTextStyle,
    Interaction, MessageId, Permissions, UserId, Colour, Timestamp,
};

use crate::AppContext;

/* ==========================================
   Konfiguracja
   ========================================== */

const SYSTEM_NAME: &str = "Tigris Ban Panel";
const SERVER_NAME: &str = "Unfaitful";

// zostaw puste "" jeśli nie chcesz logów
const LOG_CHANNEL_ID: &str = "1408795534973468793";
// === Konfiguracja (na górze pliku) ===
const PERMABAN_VIDEO_URL: &str = "https://www.youtube.com/watch?v=PLteDgvYKIM&ab_channel=BrzydkiBurak";

/* ==========================================
   State & typy
   ========================================== */

#[derive(Clone, Copy, PartialEq, Eq)]
enum BanType {
    Perma,
    Temp,
}

#[derive(Clone)]
struct CaseState {
    guild_id: GuildId,
    moderator_id: UserId,
    target_id: UserId,
    kind: Option<BanType>,
    duration: Option<Duration>,
    reason: Option<String>,
    panel_msg: Option<(ChannelId, MessageId)>,
}

static CASES: Lazy<DashMap<String, CaseState>> = Lazy::new(DashMap::new);

fn case_id_from(inter_id: u64, moderator: UserId) -> String {
    format!("{inter_id}-{}", moderator.get())
}

/* ==========================================
   Public API
   ========================================== */

pub struct BanPanel;
pub type Ban = BanPanel;

impl BanPanel {
    pub async fn register_commands(ctx: &Context, guild_id: GuildId) -> Result<()> {
        guild_id
            .create_command(
                &ctx.http,
                CreateCommand::new("ban")
                    .description("Panel bana (perm/temp) z potwierdzeniem")
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::User,
                            "user",
                            "Użytkownik do zbanowania",
                        )
                        .required(true),
                    )
                    .default_member_permissions(Permissions::BAN_MEMBERS),
            )
            .await?;
        Ok(())
    }

    pub async fn on_interaction(ctx: &Context, _app: &AppContext, interaction: Interaction) {
        if let Some(cmd) = interaction.clone().command() {
            if cmd.data.name == "ban" {
                if let Err(e) = handle_ban_slash(ctx, &cmd).await {
                    tracing::warn!(error=?e, "ban slash failed");
                }
            }
            return;
        }

        if let Some(comp) = interaction.clone().message_component() {
            let id = comp.data.custom_id.as_str();

            if id.starts_with("banp:type:")    { let _ = on_type_select(ctx, &comp).await;    return; }
            if id.starts_with("banp:dur:")     { let _ = on_duration_select(ctx, &comp).await; return; }
            if id.starts_with("banp:reason:")  { let _ = on_reason_modal_open(ctx, &comp).await; return; }
            if id.starts_with("banp:refresh:") { let _ = on_refresh(ctx, &comp).await;        return; }
            if id.starts_with("banp:proceed:") { let _ = on_proceed(ctx, &comp).await;        return; }
            if id.starts_with("banp:cancel:")  { let _ = on_cancel(ctx, &comp).await;         return; }
            if id.starts_with("banp:confirm:") { let _ = on_confirm(ctx, &comp).await;        return; }

            return;
        }

        if let Some(modal) = interaction.modal_submit() {
            if modal.data.custom_id.starts_with("banp:modalreason:") {
                let _ = on_reason_modal_submit(ctx, &modal).await;
            }
            return;
        }
    }
}

/* ==========================================
   Handlery SLASH / komponenty / modale
   ========================================== */

async fn handle_ban_slash(ctx: &Context, cmd: &CommandInteraction) -> Result<()> {
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

    if !user_can_ban(ctx, gid, cmd.user.id).await {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("⛔ Brak uprawnień do banowania.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    }

    // target
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

    if target == cmd.user.id || target.get() == ctx.cache.current_user().id.get() {
        cmd.create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Nie można zbanować tego użytkownika.")
                    .ephemeral(true),
            ),
        )
        .await?;
        return Ok(());
    }

    // zainicjuj state
    let case_id = case_id_from(cmd.id.get(), cmd.user.id);
    CASES.insert(
        case_id.clone(),
        CaseState {
            guild_id: gid,
            moderator_id: cmd.user.id,
            target_id: target,
            kind: None,
            duration: None,
            reason: None,
            panel_msg: None,
        },
    );

    // panel startowy
    let embed = summary_embed(&case_id);
    let components = vec![CreateActionRow::SelectMenu(
        CreateSelectMenu::new(
            format!("banp:type:{case_id}"),
            CreateSelectMenuKind::String {
                options: vec![
                    CreateSelectMenuOption::new("Permanentny", "perma"),
                    CreateSelectMenuOption::new("Tymczasowy", "temp"),
                ],
            },
        )
        .placeholder("Wybierz rodzaj bana")
        .min_values(1)
        .max_values(1),
    )];

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

    // zapisz kanał/wiadomość
    if let Ok(msg) = cmd.get_response(&ctx.http).await {
        CASES.alter(&case_id, |_, mut s| {
            s.panel_msg = Some((msg.channel_id, msg.id));
            s
        });
    }

    Ok(())
}

async fn on_type_select(ctx: &Context, comp: &ComponentInteraction) -> Result<()> {
    let Some(case_id) = comp.data.custom_id.split(':').nth(2).map(|s| s.to_string()) else { return Ok(()); };
    if !guard_current_panel(ctx, &case_id, comp).await? { return Ok(()); }
    let Some(val) = first_value(comp) else { return Ok(()); };
    let Some(case_id) = comp.data.custom_id.split(':').nth(2).map(|s| s.to_string()) else { return Ok(()); };
    let kind = if val == "perma" { BanType::Perma } else { BanType::Temp };

    CASES.alter(&case_id, |_, mut s| { s.kind = Some(kind); s });

    let (embed, comps) = ui_for_case(&case_id);
    comp.create_response(
        &ctx.http,
        CreateInteractionResponse::UpdateMessage(
            CreateInteractionResponseMessage::new().add_embed(embed).components(comps),
        ),
    )
    .await?;
    Ok(())
}

async fn on_duration_select(ctx: &Context, comp: &ComponentInteraction) -> Result<()> {
    let Some(case_id) = comp.data.custom_id.split(':').nth(2).map(|s| s.to_string()) else { return Ok(()); };
    if !guard_current_panel(ctx, &case_id, comp).await? { return Ok(()); }
    let Some(secs) = first_value(comp).and_then(|s| s.parse::<u64>().ok()) else { return Ok(()); };
    let Some(case_id) = comp.data.custom_id.split(':').nth(2).map(|s| s.to_string()) else { return Ok(()); };

    CASES.alter(&case_id, |_, mut s| { s.duration = Some(Duration::from_secs(secs)); s });

    let (embed, comps) = ui_for_case(&case_id);
    comp.create_response(
        &ctx.http,
        CreateInteractionResponse::UpdateMessage(
            CreateInteractionResponseMessage::new().add_embed(embed).components(comps),
        ),
    )
    .await?;
    Ok(())
}

async fn on_reason_modal_open(ctx: &Context, comp: &ComponentInteraction) -> Result<()> {
    let Some(case_id) = comp.data.custom_id.split(':').nth(2).map(|s| s.to_string()) else { return Ok(()); };

    let modal = CreateModal::new(format!("banp:modalreason:{case_id}"), "Powód bana")
        .components(vec![CreateActionRow::InputText(
            CreateInputText::new(InputTextStyle::Paragraph, "reason", "Powód (wymagany)")
                .required(true)
                .max_length(512),
        )]);

    comp.create_response(&ctx.http, CreateInteractionResponse::Modal(modal))
        .await?;
    Ok(())
}

async fn on_reason_modal_submit(
    ctx: &Context,
    modal: &serenity::all::ModalInteraction,
) -> Result<()> {
    let Some(case_id) = modal.data.custom_id.split(':').nth(2).map(|s| s.to_string()) else {
        return Ok(());
    };

    // wyciągamy treść z InputText
    let mut reason_val: Option<String> = None;
    for row in &modal.data.components {
        for comp in &row.components {
            if let ActionRowComponent::InputText(input) = comp {
                if input.custom_id == "reason" || reason_val.is_none() {
                    if let Some(v) = &input.value {
                        reason_val = Some(v.trim().to_string());
                    }
                }
            }
        }
    }

    let reason = reason_val.unwrap_or_default();
    if reason.is_empty() {
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

    // zapis do stanu
    CASES.alter(&case_id, |_, mut s| { s.reason = Some(reason); s });

    // zbuduj nowy panel i wyślij go jako ODPOWIEDŹ na modal
    let (embed, comps) = ui_for_case(&case_id);
    modal.create_response(
        &ctx.http,
        CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("✅ Powód zapisany.")
                .add_embed(embed)
                .components(comps)
                .ephemeral(true),
        ),
    ).await?;

    // pobierz id tej nowej wiadomości i ustaw jako „aktualny panel”
    if let Ok(msg) = modal.get_response(&ctx.http).await {
        CASES.alter(&case_id, |_, mut s| { s.panel_msg = Some((msg.channel_id, msg.id)); s });
    }

    Ok(())
}

async fn on_refresh(ctx: &Context, comp: &ComponentInteraction) -> Result<()> {
    let Some(case_id) = comp.data.custom_id.split(':').nth(2).map(|s| s.to_string()) else { return Ok(()); };
    if !guard_current_panel(ctx, &case_id, comp).await? { return Ok(()); }
    let Some(case_id) = comp.data.custom_id.split(':').nth(2).map(|s| s.to_string()) else { return Ok(()); };
    let (embed, comps) = ui_for_case(&case_id);
    comp.create_response(
        &ctx.http,
        CreateInteractionResponse::UpdateMessage(
            CreateInteractionResponseMessage::new().add_embed(embed).components(comps),
        ),
    )
    .await?;
    Ok(())
}

async fn on_proceed(ctx: &Context, comp: &ComponentInteraction) -> Result<()> {
    let Some(case_id) = comp.data.custom_id.split(':').nth(2).map(|s| s.to_string()) else { return Ok(()); };
    if !guard_current_panel(ctx, &case_id, comp).await? { return Ok(()); }
    let Some(case_id) = comp.data.custom_id.split(':').nth(2).map(|s| s.to_string()) else { return Ok(()); };
    let Some(st) = CASES.get(&case_id).map(|e| e.clone()) else { return Ok(()); };

    match st.kind {
        Some(BanType::Perma) => {
            if st.reason.as_deref().unwrap_or("").trim().is_empty() {
                ephemeral_note(ctx, comp, "Wpisz powód (przycisk **Wpisz powód**).").await?;
                return Ok(());
            }
        }
        Some(BanType::Temp) => {
            if st.duration.is_none() || st.reason.as_deref().unwrap_or("").trim().is_empty() {
                ephemeral_note(ctx, comp, "Ustaw czas i powód, następnie spróbuj ponownie.").await?;
                return Ok(());
            }
        }
        None => {
            ephemeral_note(ctx, comp, "Najpierw wybierz rodzaj bana.").await?;
            return Ok(());
        }
    }

    let conf_embed = confirm_embed(&st);
    let conf_rows = vec![CreateActionRow::Buttons(vec![
        CreateButton::new(format!("banp:confirm:{case_id}"))
            .label("✅ Potwierdź ban")
            .style(ButtonStyle::Danger),
        CreateButton::new(format!("banp:cancel:{case_id}"))
            .label("Anuluj")
            .style(ButtonStyle::Secondary),
    ])];

    comp.create_response(
        &ctx.http,
        CreateInteractionResponse::UpdateMessage(
            CreateInteractionResponseMessage::new().add_embed(conf_embed).components(conf_rows),
        ),
    )
    .await?;

    Ok(())
}

async fn on_cancel(ctx: &Context, comp: &ComponentInteraction) -> Result<()> {
    let Some(case_id) = comp.data.custom_id.split(':').nth(2).map(|s| s.to_string()) else { return Ok(()); };
    if !guard_current_panel(ctx, &case_id, comp).await? { return Ok(()); }
    let Some(case_id) = comp.data.custom_id.split(':').nth(2).map(|s| s.to_string())
        else { return Ok(()); };

    // wyczyść panel, na którym kliknięto
    comp.create_response(
        &ctx.http,
        CreateInteractionResponse::UpdateMessage(
            CreateInteractionResponseMessage::new()
                .content("✅ Anulowano panel bana.")
                .components(vec![])
                .embeds(vec![]),
        ),
    ).await?;

    // dodatkowo wyczyść PIERWOTNY panel (jeśli istniał)
    try_clear_existing_panel(ctx, &case_id, "✅ Panel bana został anulowany.").await;

    // usuń stan
    let _ = CASES.remove(&case_id);

    Ok(())
}

async fn on_confirm(ctx: &Context, comp: &ComponentInteraction) -> Result<()> {
    let Some(case_id) = comp.data.custom_id.split(':').nth(2).map(|s| s.to_string()) else { return Ok(()); };
    let Some(st) = CASES.remove(&case_id).map(|(_, v)| v) else { return Ok(()); };

    if !user_can_ban(ctx, st.guild_id, st.moderator_id).await {
        ephemeral_note(ctx, comp, "⛔ Utracono uprawnienia do banowania.").await?;
        return Ok(());
    }

    let reason_text = st.reason.clone().unwrap_or_else(|| "Brak powodu".into());

    let _ = send_formal_dm(ctx, st.target_id, &st, &reason_text).await;

    let del_days = 0u8;
    let reason_for_audit = format!("[{}] {}", SYSTEM_NAME, reason_text);
    if let Err(e) = st
        .guild_id
        .ban_with_reason(&ctx.http, st.target_id, del_days, &reason_for_audit)
        .await
    {
        comp.create_response(
            &ctx.http,
            CreateInteractionResponse::UpdateMessage(
                CreateInteractionResponseMessage::new()
                    .content(format!("⛔ Nie udało się zbanować użytkownika: {e}"))
                    .components(vec![])
                    .embeds(vec![]),
            ),
        )
        
        .await
        .ok();
        return Ok(());
    }

    if let Some(dur) = st.duration {
        let http = ctx.http.clone();
        let gid = st.guild_id;
        let uid = st.target_id;
        tokio::spawn(async move {
            sleep(dur).await;
            let _ = gid.unban(&http, uid).await;
        });
    }

    if let Ok(cid) = LOG_CHANNEL_ID.parse::<u64>() {
        if cid != 0 {
            let _ = ChannelId::new(cid)
                .send_message(&ctx.http, make_log_embed(&st, &reason_text))
                .await;
        }
    }

    comp.create_response(
        &ctx.http,
        CreateInteractionResponse::UpdateMessage(
            CreateInteractionResponseMessage::new()
                .content(match st.kind {
                    Some(BanType::Perma) => format!("✅ Zbanowano <@{}> **permanentnie**.", st.target_id.get()),
                    Some(BanType::Temp)  => format!("✅ Zbanowano <@{}> **tymczasowo**.",   st.target_id.get()),
                    None                 => "✅ Zbanowano.".into(),
                })
                .components(vec![])
                .embeds(vec![]),
        ),
    )
    .await
    .ok();
    try_clear_existing_panel(ctx, &case_id, "✅ Ban wykonany.").await;
    Ok(())
}

/* ==========================================
   UI helpers (ładny, „na wypasie”)
   ========================================== */

const DURATIONS: &[(u64, &str)] = &[
    (30 * 60, "30 minut"),
    (60 * 60, "1 godzina"),
    (6 * 3600, "6 godzin"),
    (12 * 3600, "12 godzin"),
    (24 * 3600, "1 dzień"),
    (3 * 86400, "3 dni"),
    (7 * 86400, "7 dni"),
    (14 * 86400, "14 dni"),
    (30 * 86400, "30 dni"),
];

fn summary_embed(case_id: &str) -> CreateEmbed {
    let s = CASES.get(case_id).map(|e| e.clone());

    let (emoji, title_colour) = match s.as_ref().and_then(|x| x.kind) {
        Some(BanType::Perma) => ("🛑", Colour::new(0xE74C3C)),
        Some(BanType::Temp)  => ("⏳", Colour::new(0xF39C12)),
        None                 => ("❔", Colour::new(0x95A5A6)),
    };

    let mut e = CreateEmbed::new()
        .title(format!("{emoji} Panel bana"))
        .colour(title_colour)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME));

    if let Some(st) = s {
        let target = format!("<@{}>", st.target_id.get());
        let moderator = format!("<@{}>", st.moderator_id.get());

        let (typ_emoji, typ_txt) = match st.kind {
            Some(BanType::Perma) => ("🛑", "Permanentny"),
            Some(BanType::Temp)  => ("⏳", "Tymczasowy"),
            None                 => ("❔", "—"),
        };

        let czas = match st.kind {
            Some(BanType::Temp) => st.duration.map(fmt_duration).unwrap_or_else(|| "—".into()),
            _ => "—".into(),
        };

        let have_type   = st.kind.is_some();
        let have_time   = matches!(st.kind, Some(BanType::Temp)) && st.duration.is_some() || matches!(st.kind, Some(BanType::Perma));
        let have_reason = st.reason.as_ref().map(|r| !r.trim().is_empty()).unwrap_or(false);

        let steps = [
            (1, "Wybierz typ", have_type),
            (2, "Ustaw czas",  have_time),
            (3, "Wpisz powód", have_reason),
            (4, "Zatwierdź",   false), // ostatni krok – zawsze ostatni
        ]
        .iter()
        .map(|(n, label, done)| format!("{} **{}.** {}", if *done { "✅" } else { "⬜" }, n, label))
        .collect::<Vec<_>>()
        .join("\n");

        let reason_preview = st.reason.as_deref().unwrap_or("—");
        let reason_preview = shorten_code_block(reason_preview, 420);

        e = e
            .description(format!(
                "**Użytkownik:** {target}\n**Administrator:** {moderator}\n**Typ:** {typ_emoji} {typ}\n\n**Czas:** {czas}\n**Powód:**\n```{reason}```\n\n__Status kroków__:\n{steps}",
                typ = typ_txt,
                reason = reason_preview,
            ));
    }

    e
}

fn ui_for_case(case_id: &str) -> (CreateEmbed, Vec<CreateActionRow>) {
    let s = CASES.get(case_id).map(|e| e.clone());

    let embed = summary_embed(case_id);
    let mut rows: Vec<CreateActionRow> = Vec::new();

    rows.push(CreateActionRow::SelectMenu(
        CreateSelectMenu::new(
            format!("banp:type:{case_id}"),
            CreateSelectMenuKind::String {
                options: vec![
                    CreateSelectMenuOption::new("Permanentny", "perma"),
                    CreateSelectMenuOption::new("Tymczasowy", "temp"),
                ],
            },
        )
        .placeholder("Wybierz rodzaj bana")
        .min_values(1)
        .max_values(1),
    ));

    // gotowość „Dalej”
    let mut proceed_enabled = false;

    match s.as_ref().and_then(|x| x.kind) {
        Some(BanType::Perma) => {
            proceed_enabled = s.as_ref().and_then(|x| x.reason.as_ref()).map(|r| !r.trim().is_empty()).unwrap_or(false);
            rows.push(CreateActionRow::Buttons(vec![
                CreateButton::new(format!("banp:reason:{case_id}"))
                    .label("Wpisz powód")
                    .style(ButtonStyle::Primary),
                CreateButton::new(format!("banp:proceed:{case_id}"))
                    .label("Dalej")
                    .style(ButtonStyle::Success)
                    .disabled(!proceed_enabled),
                CreateButton::new(format!("banp:cancel:{case_id}"))
                    .label("Anuluj")
                    .style(ButtonStyle::Danger),
            ]));
        }
        Some(BanType::Temp) => {
            let have_time = s.as_ref().and_then(|x| x.duration).is_some();
            let have_reason = s.as_ref().and_then(|x| x.reason.as_ref()).map(|r| !r.trim().is_empty()).unwrap_or(false);
            proceed_enabled = have_time && have_reason;

            rows.push(CreateActionRow::SelectMenu(
                CreateSelectMenu::new(
                    format!("banp:dur:{case_id}"),
                    CreateSelectMenuKind::String {
                        options: DURATIONS
                            .iter()
                            .map(|(secs, label)| CreateSelectMenuOption::new(*label, secs.to_string()))
                            .collect(),
                    },
                )
                .placeholder("Wybierz czas bana")
                .min_values(1)
                .max_values(1),
            ));
            rows.push(CreateActionRow::Buttons(vec![
                CreateButton::new(format!("banp:reason:{case_id}"))
                    .label("Wpisz powód")
                    .style(ButtonStyle::Primary),
                CreateButton::new(format!("banp:refresh:{case_id}"))
                    .label("🔄 Odśwież")
                    .style(ButtonStyle::Secondary),
                CreateButton::new(format!("banp:proceed:{case_id}"))
                    .label("Dalej")
                    .style(ButtonStyle::Success)
                    .disabled(!proceed_enabled),
                CreateButton::new(format!("banp:cancel:{case_id}"))
                    .label("Anuluj")
                    .style(ButtonStyle::Danger),
            ]));
        }
        None => {}
    }

    (embed, rows)
}

fn confirm_embed(st: &CaseState) -> CreateEmbed {
    let typ = match st.kind {
        Some(BanType::Perma) => "Permanentny",
        Some(BanType::Temp)  => "Tymczasowy",
        None                 => "—",
    };

    let mut e = CreateEmbed::new()
        .title("Potwierdzenie bana")
        .colour(color_for_kind(st.kind))
        .footer(CreateEmbedFooter::new(SYSTEM_NAME))
        .field("Użytkownik", format!("<@{}>", st.target_id.get()), true)
        .field("Administrator", format!("<@{}>", st.moderator_id.get()), true)
        .field("Typ", typ, true);

    match st.kind {
        Some(BanType::Temp) => {
            let czas_txt = st.duration.map(fmt_duration).unwrap_or_else(|| "—".into());
            e = e.field("Czas", czas_txt, true);
            if let Some(dur) = st.duration {
                if let Some(ts) = end_timestamp_from_now(dur) {
                    let unix = ts.unix_timestamp();
                    e = e.field("Wygasa", format!("<t:{unix}:R>  •  <t:{unix}:F>"), false);
                }
            }
        }
        _ => { e = e.field("Czas", "—", true); }
    }

    let reason = st.reason.as_deref().unwrap_or("—");
    e.description(format!("**Powód**:\n```{}```", shorten_code_block(reason, 900)))
}

fn make_log_embed(st: &CaseState, reason: &str) -> serenity::all::CreateMessage {
    let (typ, col) = match st.kind {
        Some(BanType::Perma) => ("PERMA", Colour::new(0xE74C3C)),
        Some(BanType::Temp)  => ("TEMP",  Colour::new(0xF39C12)),
        None                 => ("—",     Colour::new(0x95A5A6)),
    };

    let mut e = CreateEmbed::new()
        .title("✅ Ban wykonany")
        .colour(col)
        .footer(CreateEmbedFooter::new(SYSTEM_NAME))
        .field("Użytkownik", format!("<@{}>", st.target_id.get()), true)
        .field("Administrator", format!("<@{}>", st.moderator_id.get()), true)
        .field("Typ", typ, true);

    match st.kind {
        Some(BanType::Temp) => {
            let czas_txt = st.duration.map(fmt_duration).unwrap_or_else(|| "—".into());
            e = e.field("Czas", czas_txt, true);
            if let Some(dur) = st.duration {
                if let Some(ts) = end_timestamp_from_now(dur) {
                    let unix = ts.unix_timestamp();
                    e = e.field("Wygasa", format!("<t:{unix}:R>  •  <t:{unix}:F>"), false);
                }
            }
        }
        _ => { e = e.field("Czas", "—", true); }
    }

    e = e.field("Powód", format!("```{}```", shorten_code_block(reason, 900)), false);
    serenity::all::CreateMessage::new().embed(e)
}

/* ==========================================
   DM formalny
   ========================================== */

async fn send_formal_dm(ctx: &Context, target: UserId, st: &CaseState, reason: &str) -> Result<()> {
    let user = target.to_user(&ctx.http).await?;
    let kind_txt = match st.kind {
        Some(BanType::Perma) => "ban permanentny",
        Some(BanType::Temp)  => "ban tymczasowy",
        None                 => "ban",
    };

    let mut e = CreateEmbed::new()
        .title(format!("Informacja o nałożonej karze – {SERVER_NAME}"))
        .colour(color_for_kind(st.kind))
        .footer(CreateEmbedFooter::new(SYSTEM_NAME))
        .field("Typ kary", kind_txt, true)
        .field("Administrator", format!("<@{}>", st.moderator_id.get()), true);

    if let Some(dur) = st.duration {
        let czas_txt = fmt_duration(dur);
        e = e.field("Czas trwania", czas_txt, true);
        if let Some(ts) = end_timestamp_from_now(dur) {
            let unix = ts.unix_timestamp();
            e = e.field("Wygasa", format!("<t:{unix}:R>  •  <t:{unix}:F>"), false);
        }
    }

    let desc = format!(
        "Szanowny Użytkowniku,\n\n\
         Informujemy, że na Twoje konto został nałożony ban na serwerze **{server}**.\n\
         Jeśli uważasz, że zaszła pomyłka, możesz złożyć odwołanie u zespołu administracji.\n\n\
         **Powód:**",
        server = SERVER_NAME
    );

    e = e.description(desc)
         .field("Powód", format!("```{}```", shorten_code_block(reason, 900)), false);

    if let Some(url) = user.avatar_url() {
        e = e.thumbnail(url);
    }

    // DM otwieramy raz
    let dm = user.create_dm_channel(&ctx.http).await?;

    // 1) wiadomość z embedem
    let _ = dm
        .send_message(&ctx.http, serenity::all::CreateMessage::new().embed(e))
        .await;

    // 2) jeśli PERMA i mamy link — doślij sam URL, aby Discord wstawił player
    if matches!(st.kind, Some(BanType::Perma)) && !PERMABAN_VIDEO_URL.is_empty() {
        // ważne: czysty URL, bez formatowania
        let _ = dm.say(&ctx.http, PERMABAN_VIDEO_URL).await;
    }

    Ok(())
}

/* ==========================================
   Helpers
   ========================================== */

async fn user_can_ban(ctx: &Context, gid: GuildId, uid: UserId) -> bool {
    if let Ok(member) = gid.member(&ctx.http, uid).await {
        if let Ok(perms) = member.permissions(&ctx.cache) {
            return perms.ban_members() || perms.administrator();
        }
    }
    false
}

fn fmt_duration(d: Duration) -> String {
    let total = d.as_secs();
    let days = total / 86_400;
    let hours = (total % 86_400) / 3600;
    let mins = (total % 3600) / 60;
    let mut parts = vec![];
    if days > 0 { parts.push(format!("{days}d")); }
    if hours > 0 { parts.push(format!("{hours}h")); }
    if mins > 0 { parts.push(format!("{mins}m")); }
    if parts.is_empty() { "0m".into() } else { parts.join(" ") }
}

async fn ephemeral_note(ctx: &Context, comp: &ComponentInteraction, msg: &str) -> Result<()> {
    comp.create_response(
        &ctx.http,
        CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new().content(msg).ephemeral(true),
        ),
    )
    .await?;
    Ok(())
}

fn first_value(comp: &ComponentInteraction) -> Option<String> {
    if let serenity::all::ComponentInteractionDataKind::StringSelect { values } = &comp.data.kind {
        return values.first().cloned();
    }
    None
}

async fn try_update_existing_panel(ctx: &Context, case_id: &str) {
    if let Some(st) = CASES.get(case_id).map(|e| e.clone()) {
        if let Some((ch, mid)) = st.panel_msg {
            let (embed, comps) = ui_for_case(case_id);
            let _ = ch.edit_message(&ctx.http, mid, EditMessage::new().embed(embed).components(comps)).await;
        }
    }
}

fn color_for_kind(kind: Option<BanType>) -> Colour {
    match kind {
        Some(BanType::Perma) => Colour::new(0xE74C3C), // czerwony
        Some(BanType::Temp)  => Colour::new(0xF39C12), // pomarańcz
        None                 => Colour::new(0x95A5A6), // szary
    }
}

fn end_timestamp_from_now(dur: Duration) -> Option<Timestamp> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let end = now + dur.as_secs();
    Timestamp::from_unix_timestamp(end as i64).ok()
}

fn shorten_code_block(s: &str, max_chars: usize) -> String {
    let mut out = s.trim().replace("```", "`\u{200B}`"); // rozbij ewentualne trójbacktiki
    if out.len() > max_chars {
        out.truncate(max_chars.saturating_sub(1));
        out.push('…');
    }
    out
}

async fn try_clear_existing_panel(ctx: &Context, case_id: &str, content: &str) {
    if let Some(st) = CASES.get(case_id).map(|e| e.clone()) {
        if let Some((ch, mid)) = st.panel_msg {
            let _ = ch
                .edit_message(
                    &ctx.http,
                    mid,
                    EditMessage::new()
                        .content(content)
                        .components(Vec::<CreateActionRow>::new())
                        .embeds(Vec::<CreateEmbed>::new()),
                )
                .await;
        }
    }
}

async fn guard_current_panel(ctx: &Context, case_id: &str, comp: &ComponentInteraction) -> Result<bool> {
    if let Some(st) = CASES.get(case_id) {
        if let Some((_, mid)) = st.panel_msg {
            if comp.message.id != mid {
                comp.create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("⚠️ Ten panel jest nieaktualny. Proszę użyj najnowszego panelu poniżej.")
                            .ephemeral(true),
                    ),
                ).await.ok();
                return Ok(false);
            }
        }
    }
    Ok(true)
}

