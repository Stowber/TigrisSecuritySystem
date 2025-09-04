use serenity::all::{
    Channel, ChannelId, ChannelType, Colour, Context, CreateEmbed, CreateMessage, GuildChannel,
    GuildId, Message, Timestamp, UserId,
};
use serenity::builder::CreateChannel;
use serenity::model::guild::audit_log::{Action, ChannelAction};

use crate::AppContext;
use crate::registry::env_channels;

pub struct NewChannels;

impl NewChannels {
    /// Loguje utworzenie nowego kanału (tylko jeśli jego parent znajduje się na liście obserwowanych kategorii).
    pub async fn on_channel_create(ctx: &Context, app: &AppContext, ch: &GuildChannel) {
        let env = app.env();

        let parent_id = match ch.parent_id {
            Some(p) => p,
            None => return, // bez kategorii – pomijamy
        };

        // Czy kategoria jest obserwowana?
        if !env_channels::watch::categories(&env)
            .into_iter()
            .any(|id| id == parent_id.get())
        {
            return;
        }

        // Kanał do logów "nowe-kanały" (DEV), ewentualnie utwórz pod wskazaną kategorią.
        let mut log_id = env_channels::new_channels_id(&env);
        if log_id == 0 {
            let parent_cat = env_channels::new_channels_parent_id(&env);
            if parent_cat == 0 {
                tracing::warn!("Brak LOGS_NEW_CHANNELS i LOGS_NEW_CHANNELS_PARENT – pomijam log.");
                return;
            }
            match ch.guild_id
                .create_channel(
                    &ctx.http,
                    CreateChannel::new("nowe-kanały")
                        .kind(ChannelType::Text)
                        .category(ChannelId::new(parent_cat)),
                )
                .await
            {
                Ok(created) => log_id = created.id.get(),
                Err(e) => {
                    tracing::warn!(?e, "Nie mogę utworzyć kanału 'nowe-kanały' – pomijam log.");
                    return;
                }
            }
        }

        // Spróbuj ustalić kto utworzył kanał (audit log).
        let executor = Self::resolve_executor_for_channel_action(
            ctx,
            ch.guild_id,
            ch.id,
            /* want_create = */ true,
        )
        .await;

        let category_name = Self::category_name(ctx, parent_id).await
            .unwrap_or_else(|| format!("(kategoria ID `{}`)", parent_id.get()));

        let (emoji, kind_name) = Self::kind_label(ch.kind);

        let mut embed = CreateEmbed::new()
            .title("➕ Nowy kanał")
            .colour(Colour::new(0x2ECC71)) // zielony
            .timestamp(Timestamp::now())
            .description(format!(
                "{} <#{}> — **{}**\n`ID:` `{}`",
                emoji,
                ch.id.get(),
                ch.name,
                ch.id.get()
            ))
            .field("Typ", kind_name, true)
            .field("Kategoria", category_name, true);

        embed = embed.field(
            "Utworzył",
            executor
                .map(|u| format!("<@{}> (`{}`)", u.get(), u.get()))
                .unwrap_or_else(|| "nieznany".into()),
            true,
        );

        let _ = ChannelId::new(log_id)
            .send_message(&ctx.http, CreateMessage::new().add_embed(embed))
            .await;
    }

    /// Loguje usunięcie kanału (analogicznie jak utworzenie).
    pub async fn on_channel_delete(
        ctx: &Context,
        app: &AppContext,
        ch: &GuildChannel,
        _messages: Option<Vec<Message>>,
    ) {
        let env = app.env();

        let parent_id = match ch.parent_id {
            Some(p) => p,
            None => return,
        };

        if !env_channels::watch::categories(&env)
            .into_iter()
            .any(|id| id == parent_id.get())
        {
            return;
        }

        let mut log_id = env_channels::new_channels_id(&env);
        if log_id == 0 {
            let parent_cat = env_channels::new_channels_parent_id(&env);
            if parent_cat == 0 {
                tracing::warn!("Brak LOGS_NEW_CHANNELS i LOGS_NEW_CHANNELS_PARENT – pomijam log (delete).");
                return;
            }
            match ch.guild_id
                .create_channel(
                    &ctx.http,
                    CreateChannel::new("nowe-kanały")
                        .kind(ChannelType::Text)
                        .category(ChannelId::new(parent_cat)),
                )
                .await
            {
                Ok(created) => log_id = created.id.get(),
                Err(e) => {
                    tracing::warn!(?e, "Nie mogę utworzyć kanału 'nowe-kanały' – pomijam log (delete).");
                    return;
                }
            }
        }

        // Spróbuj ustalić kto usunął kanał (audit log).
        let executor = Self::resolve_executor_for_channel_action(
            ctx,
            ch.guild_id,
            ch.id,
            /* want_create = */ false,
        )
        .await;

        let category_name = Self::category_name(ctx, parent_id).await
            .unwrap_or_else(|| format!("(kategoria ID `{}`)", parent_id.get()));

        let (emoji, kind_name) = Self::kind_label(ch.kind);

        let mut embed = CreateEmbed::new()
            .title("🗑️ Kanał usunięty")
            .colour(Colour::new(0xE74C3C)) // czerwony
            .timestamp(Timestamp::now())
            .description(format!(
                "{} **{}**\n`ID:` `{}`",
                emoji,
                ch.name,
                ch.id.get()
            ))
            .field("Typ", kind_name, true)
            .field("Kategoria", category_name, true);

        embed = embed.field(
            "Usunął",
            executor
                .map(|u| format!("<@{}> (`{}`)", u.get(), u.get()))
                .unwrap_or_else(|| "nieznany".into()),
            true,
        );

        let _ = ChannelId::new(log_id)
            .send_message(&ctx.http, CreateMessage::new().add_embed(embed))
            .await;
    }

    /// Próbuje znaleźć wykonawcę akcji (create/delete) na podstawie dziennika audytu.
    /// Zwraca `Some(UserId)` gdy dopasowano wpis; w przeciwnym razie `None`.
    async fn resolve_executor_for_channel_action(
        ctx: &Context,
        guild_id: GuildId,
        target_channel_id: ChannelId,
        want_create: bool,
    ) -> Option<UserId> {
        let audit = match guild_id.audit_logs(&ctx.http, None, None, None, None).await {
            Ok(a) => a,
            Err(_) => return None,
        };

        // Szukamy najnowszego wpisu dot. naszego kanału i akcji create/delete.
        for entry in audit.entries {
            // dopasuj typ akcji do kanałów
            match &entry.action {
                Action::Channel(chan_action) => {
                    let is_match = match chan_action {
                        ChannelAction::Create => want_create,
                        ChannelAction::Delete => !want_create,
                        _ => false,
                    };
                    if !is_match {
                        continue;
                    }

                    // Czy dotyczy właściwego kanału?
                    if let Some(tid) = entry.target_id {
                        if tid.get() == target_channel_id.get() {
                            // W 0.12 `user_id` jest `UserId`, nie `Option<UserId>`
                            return Some(entry.user_id);
                        }
                    }
                }
                _ => continue,
            }
        }
        None
    }

    /// Nazwa kategorii (jeśli uda się pobrać).
    async fn category_name(ctx: &Context, parent_id: ChannelId) -> Option<String> {
        match parent_id.to_channel(&ctx.http).await {
            Ok(Channel::Guild(gch)) if gch.kind == ChannelType::Category => Some(gch.name.clone()),
            _ => None,
        }
    }

    /// Etykieta i emoji dla typu kanału.
    fn kind_label(kind: ChannelType) -> (&'static str, &'static str) {
        match kind {
            ChannelType::Text => ("#️⃣", "tekstowy"),
            ChannelType::Voice => ("🔊", "głosowy"),
            ChannelType::News => ("📣", "ogłoszenia"),
            ChannelType::Forum => ("🗂️", "forum"),
            ChannelType::Category => ("📁", "kategoria"),
            _ => ("📦", "inny"),
        }
    }
}
