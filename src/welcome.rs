// src/welcome.rs
use serenity::all::{
    ChannelId, Context, CreateEmbed, CreateEmbedFooter, CreateMessage, GuildId, Timestamp, User, Member, Colour,
};
use crate::AppContext;
use crate::registry::env_channels;

pub struct Welcome;

impl Welcome {
    pub async fn send_welcome(ctx: &Context, app: &AppContext, member: &Member) {
        let env = app.env();
        let ch_id = env_channels::global::welcome_id(&env);
        if ch_id == 0 { return; }

        let user = &member.user;
        let mention = format!("<@{}>", user.id.get());
        let avatar = user.avatar_url().unwrap_or_else(|| user.default_avatar_url());
        let profile_url = format!("https://discord.com/users/{}", user.id.get());

        let user_label: String = match &user.global_name {
            Some(g) => format!("{} ({})", user.name, g),
            None => user.name.clone(),
        };

        let embed = CreateEmbed::new()
            .title("👋 Witaj na serwerze!")
            .description(format!("{mention}\nMiło Cię widzieć. Zapoznaj się z zasadami i baw się dobrze!"))
            .thumbnail(avatar)
            .field("Użytkownik", user_label, true)
            .field("ID", format!("`{}`", user.id.get()), true)
            .field("Profil", format!("[Otwórz profil]({profile_url})"), true)
            .timestamp(Timestamp::now())
            .colour(Colour::BLURPLE)
            .footer(CreateEmbedFooter::new("Tigris Security System™ • Unfaithful"));

        let _ = ChannelId::new(ch_id)
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }

    pub async fn send_goodbye(ctx: &Context, app: &AppContext, _guild_id: GuildId, user: &User) {
        let env = app.env();
        let ch_id = env_channels::global::goodbye_id(&env);
        if ch_id == 0 { return; }

        let mention = format!("<@{}>", user.id.get());
        let avatar = user.avatar_url().unwrap_or_else(|| user.default_avatar_url());
        let profile_url = format!("https://discord.com/users/{}", user.id.get());

        let user_label: String = match &user.global_name {
            Some(g) => format!("{} ({})", user.name, g),
            None => user.name.clone(),
        };

        let embed = CreateEmbed::new()
            .title("👋 Użytkownik opuścił serwer")
            .description(mention)
            .thumbnail(avatar)
            .field("Użytkownik", user_label, true)
            .field("ID", format!("`{}`", user.id.get()), true)
            .field("Profil", format!("[Otwórz profil]({profile_url})"), true)
            .timestamp(Timestamp::now())
            .colour(Colour::ORANGE)
            .footer(CreateEmbedFooter::new("Tigris Security System™ • Unfaithful"));

        let _ = ChannelId::new(ch_id)
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }
}
