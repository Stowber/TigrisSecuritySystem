use anyhow::Result;
use serenity::all::{
    ButtonStyle, ChannelId, Colour, CommandInteraction, ComponentInteraction, Context, CreateMessage,
    GuildChannel, GuildId, Interaction, PermissionOverwrite, PermissionOverwriteType, Permissions,
    RoleId,
};
use serenity::builder::{
    CreateActionRow, CreateButton, CreateCommand, CreateEmbed, CreateEmbedFooter,
    CreateInteractionResponse, CreateInteractionResponseMessage, EditChannel,
    EditInteractionResponse,
};

use crate::altguard::{AltVerdict, ScoreInput};
use crate::registry::{env_channels, env_roles};
use crate::{welcome::Welcome, AppContext};

const BRAND_FOOTER: &str = "Tigris Security System™ • Unfaithful";

pub struct Verify;

impl Verify {
    /* ======================
       REJESTR KOMEND
       ====================== */

    /// Rejestruje /verify-panel na danej gildii.
    pub async fn register_commands(ctx: &Context, guild_id: GuildId) -> Result<()> {
        guild_id
            .create_command(
                &ctx.http,
                CreateCommand::new("verify-panel")
                    .description("Publikuje panel weryfikacji w #weryfikacje"),
            )
            .await?;
        Ok(())
    }

    /* ======================
       BRAMA INTERAKCJI
       ====================== */

    /// Jedna brama do obsługi interakcji związanych z weryfikacją.
    /// Wołaj w `interaction_create` (mod.rs).
    pub async fn on_interaction(ctx: &Context, app: &AppContext, interaction: Interaction) {
        // Najpierw slash-komenda (klonujemy, bo metody konsumują enum)
        if let Some(cmd) = interaction.clone().command() {
            if cmd.data.name == "verify-panel" {
                if let Err(e) = Self::on_command(ctx, app, &cmd).await {
                    tracing::warn!(error=?e, "verify-panel failed");
                }
                return;
            }
        }

        // Potem przyciski
        if let Some(component) = interaction.message_component() {
            if component.data.custom_id == "verify_accept" {
                Self::on_component(ctx, app, &component).await;
            }
        }
    }

    /* ======================
       /verify-panel (slash)
       ====================== */

    async fn on_command(ctx: &Context, app: &AppContext, cmd: &CommandInteraction) -> Result<()> {
        // ACL: tylko staff
        let env = app.env();
        let staff = env_roles::staff_set(&env);
        let allowed = cmd
            .member
            .as_ref()
            .map(|m| m.roles.iter().any(|r| staff.contains(&r.get())))
            .unwrap_or(false);

        if !allowed {
            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("Brak uprawnień do użycia tej komendy.")
                            .ephemeral(true),
                    ),
                )
                .await;
            return Ok(());
        }

        let Some(guild_id) = cmd.guild_id else {
            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("Tej komendy można użyć tylko na serwerze.")
                            .ephemeral(true),
                    ),
                )
                .await;
            return Ok(());
        };

        // 1) Szybki ACK (żeby nie wywaliło „The application did not respond”)
        let _ = cmd
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("Publikuję panel weryfikacji…")
                        .ephemeral(true),
                ),
            )
            .await;

        // 2) Stwórz / znajdź #weryfikacje i wyślij panel
        let channel_id = Self::ensure_verify_channel(ctx, app, guild_id).await?;
        Self::send_panel(ctx, channel_id).await?;

        // 3) Edytuj odpowiedź po sukcesie
        let _ = cmd
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new().content(format!(
                    "Panel weryfikacji został opublikowany w <#{}>.",
                    channel_id.get()
                )),
            )
            .await;

        Ok(())
    }

    /// Zostawione dla zgodności – wywołanie legacy.
    pub async fn post_panel(
        ctx: &Context,
        app: &AppContext,
        guild_id: GuildId,
        _target: ChannelId,
    ) -> Result<()> {
        let channel_id = Self::ensure_verify_channel(ctx, app, guild_id).await?;
        Self::send_panel(ctx, channel_id).await
    }

    /* ======================
       Kanał #weryfikacje
       ====================== */

    /// Znajduje lub tworzy kanał `#weryfikacje` i nakłada poprawne nadpisania uprawnień.
    async fn ensure_verify_channel(
        ctx: &Context,
        app: &AppContext,
        guild_id: GuildId,
    ) -> Result<ChannelId> {
        let env = app.env();

        // 0) Jeśli w rejestrze mamy jawne ID kanału, użyj i napraw uprawnienia.
        let preferred = env_channels::verify::id(&env);
        if preferred != 0 {
            let preferred_id = ChannelId::new(preferred);
            if let Ok(map) = guild_id.channels(&ctx.http).await {
                if let Some((_id, ch)) = map.get_key_value(&preferred_id) {
                    let overwrites = Self::build_overwrites(app, guild_id);
                    let _ = ch
                        .id
                        .edit(&ctx.http, EditChannel::new().permissions(overwrites))
                        .await;
                    return Ok(preferred_id);
                }
            }
            // jeśli ID nie pasuje do gildii / nie istnieje — szukamy po nazwie
        }

        // 1) Istniejący tekstowy „weryfikacje”
        if let Ok(map) = guild_id.channels(&ctx.http).await {
            if let Some((id, ch)) = map
                .iter()
                .find(|(_, ch)| ch.kind == serenity::all::ChannelType::Text
                    && ch.name.eq_ignore_ascii_case("weryfikacje"))
            {
                let overwrites = Self::build_overwrites(app, guild_id);
                let _ = ch
                    .id
                    .edit(&ctx.http, EditChannel::new().permissions(overwrites))
                    .await;
                return Ok(*id);
            }
        }

        // 2) Brak? Tworzymy kanał z nadpisaniami
        let overwrites = Self::build_overwrites(app, guild_id);
        let created: GuildChannel = guild_id
            .create_channel(
                &ctx.http,
                serenity::builder::CreateChannel::new("weryfikacje")
                    .kind(serenity::all::ChannelType::Text)
                    .permissions(overwrites)
                    .topic("Panel weryfikacji: zaakceptuj regulamin, aby uzyskać dostęp."),
            )
            .await?;

        Ok(created.id)
    }

    /// Nadpisania uprawnień: @everyone (VIEW+READ; bez SEND), Member (DENY VIEW), staff (ALLOW VIEW+SEND+READ).
    fn build_overwrites(app: &AppContext, guild_id: GuildId) -> Vec<PermissionOverwrite> {
        let env = app.env();
        let member_role = env_roles::member_id(&env);
        let staff_roles = env_roles::staff_set(&env);

        let mut ov = Vec::new();

        // @everyone — ALLOW VIEW + READ_HISTORY, DENY SEND
        ov.push(PermissionOverwrite {
            allow: Permissions::VIEW_CHANNEL | Permissions::READ_MESSAGE_HISTORY,
            deny: Permissions::SEND_MESSAGES,
            kind: PermissionOverwriteType::Role(RoleId::new(guild_id.get())),
        });

        // Member — DENY VIEW (znika po weryfikacji)
        if member_role != 0 {
            ov.push(PermissionOverwrite {
                allow: Permissions::empty(),
                deny: Permissions::VIEW_CHANNEL,
                kind: PermissionOverwriteType::Role(RoleId::new(member_role)),
            });
        }

        // Staff — ALLOW VIEW + SEND + READ_HISTORY
        for rid in staff_roles {
            if rid != 0 {
                ov.push(PermissionOverwrite {
                    allow: Permissions::VIEW_CHANNEL
                        | Permissions::SEND_MESSAGES
                        | Permissions::READ_MESSAGE_HISTORY,
                    deny: Permissions::empty(),
                    kind: PermissionOverwriteType::Role(RoleId::new(rid)),
                });
            }
        }

        ov
    }

    /* ======================
       Panel (embed + button)
       ====================== */

    /// Wysyła panel z przyciskiem „Akceptuję regulamin”.
    async fn send_panel(ctx: &Context, channel: ChannelId) -> Result<()> {
        let embed = CreateEmbed::new()
            .title("Unfaithful — Weryfikacja / Regulamin")
            .description(
                "Kliknij przycisk poniżej, aby **zaakceptować regulamin** \
                 i uzyskać dostęp do serwera.\n\n\
                 > Po akceptacji kanał **#weryfikacje** zniknie (rola **Member** nie widzi tego kanału).",
            )
            .footer(CreateEmbedFooter::new(BRAND_FOOTER));

        let btn_row = CreateActionRow::Buttons(vec![
            CreateButton::new("verify_accept")
                .label("Akceptuję regulamin")
                .style(ButtonStyle::Success),
        ]);

        channel
            .send_message(
                &ctx.http,
                CreateMessage::new().embed(embed).components(vec![btn_row]),
            )
            .await?;

        Ok(())
    }

    /* ======================
       Klik przycisku
       ====================== */

    /// Obsługa kliknięcia przycisku – szybkie ACK, detekcja klona 1:1, role, AltGuard log, publiczny embed powitalny.
    pub async fn on_component(ctx: &Context, app: &AppContext, i: &ComponentInteraction) {
        // Szybki ACK — zawsze najpierw
        let _ = i
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("Przyznaję role…")
                        .ephemeral(true),
                ),
            )
            .await;

        let Some(guild_id) = i.guild_id else {
            let _ = i
                .edit_response(
                    &ctx.http,
                    EditInteractionResponse::new().content("Ta akcja działa tylko na serwerze."),
                )
                .await;
            return;
        };

        let env = app.env();
        let member_role_u64 = env_roles::member_id(&env);
        let verified_role_u64 = env_roles::verified_id(&env);

        // Member
        let Ok(member) = guild_id.member(&ctx.http, i.user.id).await else {
            let _ = i
                .edit_response(
                    &ctx.http,
                    EditInteractionResponse::new()
                        .content("Nie udało się pobrać Twojego profilu z gildii. Spróbuj ponownie."),
                )
                .await;
            return;
        };

        // Jeśli już ma Member — tylko info zwrotne
        let has_member =
            member_role_u64 != 0 && member.roles.iter().any(|r| r.get() == member_role_u64);

        if has_member {
            let _ = i
                .edit_response(
                    &ctx.http,
                    EditInteractionResponse::new().content("Masz już dostęp do serwera 🙂"),
                )
                .await;
            return;
        }

        /* === DETEKCJA KLONA 1:1 (przed nadaniem ról) === */
        if let Some(hit) = app
            .altguard()
            .blunt_clone_check_and_record(
                guild_id.get(),
                member.user.id.get(),
                &member.user.name,
                member.user.global_name.as_deref(),
                member.user.avatar_url().as_deref(),
            )
            .await
        {
            // Log do kanału AltGuard
            let log_id = env_channels::logs::altguard_id(&env);
            if log_id != 0 {
                let details = format!(
                    "Avatar hamming: {}\nTa sama nazwa: {}\nTa sama global name: {}",
                    hit.avatar_hamming
                        .map(|d| d.to_string())
                        .unwrap_or_else(|| "brak".into()),
                    if hit.same_name { "tak" } else { "nie" },
                    if hit.same_global { "tak" } else { "nie" },
                );

                let embed = CreateEmbed::new()
                    .title("AltGuard: Podejrzenie klona 1:1")
                    .description(format!(
                        "<@{}> podejrzany o sklonowanie <@{}> (weryfikacja wstrzymana).",
                        member.user.id.get(),
                        hit.matched_user_id
                    ))
                    .field("Szczegóły", details, false)
                    .colour(Colour::RED)
                    .footer(CreateEmbedFooter::new(BRAND_FOOTER));

                let _ = ChannelId::new(log_id)
                    .send_message(&ctx.http, CreateMessage::new().embed(embed))
                    .await;
            }

            // Komunikat dla użytkownika i STOP – nie nadajemy ról
            let _ = i
                .edit_response(
                    &ctx.http,
                    EditInteractionResponse::new().content(
                        "❌ Wykryto potencjalne **multikonto (klon 1:1)**. \
                         Zgłoszono do administracji – poczekaj na ręczną weryfikację.",
                    ),
                )
                .await;
            return;
        }

        // Role
        if member_role_u64 != 0 {
            if let Err(e) = member
                .add_role(&ctx.http, RoleId::new(member_role_u64))
                .await
            {
                tracing::warn!(error=?e, "Nie udało się dodać roli Member");
                let _ = i
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new().content(
                            "Nie udało się dodać roli Member. Spróbuj ponownie lub skontaktuj się ze staffem.",
                        ),
                    )
                    .await;
                return;
            }
        }

        if verified_role_u64 != 0 {
            if let Err(e) = member
                .add_role(&ctx.http, RoleId::new(verified_role_u64))
                .await
            {
                tracing::warn!(error=?e, "Nie udało się dodać roli Zweryfikowany");
            }
        }

        let _ = i
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new().content("✅ Zweryfikowano. Miłej zabawy!"),
            )
            .await;

        /* ======================
           ALTGUARD: log po weryfikacji
           ====================== */
        let ag = app.altguard();
        let staff_set = env_roles::staff_set(&env);
        let has_trusted = member.roles.iter().any(|r| staff_set.contains(&r.get()));

        let input = ScoreInput {
            guild_id: guild_id.get(),
            user_id: i.user.id.get(),
            username: Some(i.user.name.clone()),
            display_name: member.nick.clone(),
            global_name: i.user.global_name.clone(),
            invite_code: None,
            inviter_id: None,
            has_trusted_role: has_trusted,
            avatar_url: i.user.avatar_url(),
        };

        match ag.score_user(&input).await {
            Ok(score) => {
                let log_id = env_channels::logs::altguard_id(&env);
                if log_id != 0 {
                    let (title, colour) = match score.verdict {
                        AltVerdict::Low => ("AltGuard: Niskie ryzyko", Colour::DARK_GREEN),
                        AltVerdict::Medium => ("AltGuard: Średnie ryzyko", Colour::GOLD),
                        AltVerdict::High => ("AltGuard: Wysokie ryzyko", Colour::RED),
                    };

                    let top = score
                        .top_signals
                        .iter()
                        .take(5)
                        .map(|s| format!("{:?} ({})", s.kind, s.weight))
                        .collect::<Vec<_>>()
                        .join(", ");

                    let user_mention = format!("<@{}>", i.user.id.get());
                    let embed = CreateEmbed::new()
                        .title(title)
                        .description(format!(
                            "{} zweryfikował się przez panel.",
                            user_mention
                        ))
                        .field("User ID", format!("`{}`", i.user.id.get()), true)
                        .field("Score", format!("**{}** / 100", score.score), true)
                        .field("Sygnały", if top.is_empty() { "–".into() } else { top }, false)
                        .footer(CreateEmbedFooter::new(BRAND_FOOTER))
                        .colour(colour);

                    let _ = ChannelId::new(log_id)
                        .send_message(&ctx.http, CreateMessage::new().embed(embed))
                        .await;
                }
            }
            Err(e) => {
                tracing::warn!(error=?e, "AltGuard scoring failed after verify");
            }
        }

        // Publiczny embed powitalny
        Welcome::send_welcome(ctx, app, &member).await;
    }
}
// Popraw wszystkie ostrzeżenia i styl kodu w tym pliku.
