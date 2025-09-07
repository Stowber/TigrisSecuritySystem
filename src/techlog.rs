use crate::{AppContext, registry::env_channels};
use serenity::all::*;
use std::time::Duration;

pub struct TechLog;

impl TechLog {
    pub async fn log_command(
        ctx: &Context,
        app: &AppContext,
        cmd: &CommandInteraction,
        duration: Duration,
        status: &str,
        error: Option<&str>,
    ) {
        let env = app.env();
        let ch_id = env_channels::logs::technical_id(&env);
        if ch_id == 0 {
            return;
        }
        let mut embed = CreateEmbed::new()
            .title("📓 Dziennik Techniczny")
            .colour(Colour::new(0x95A5A6))
            .field("Komenda", &cmd.data.name, true)
            .field("Użytkownik", format!("<@{}>", cmd.user.id.get()), true)
            .field("Czas", format!("{} ms", duration.as_millis()), true)
            .field("Status", status, true)
            .footer(CreateEmbedFooter::new("Tigris – Dziennik Techniczny"));
        if let Some(err) = error {
            embed = embed.field("Błąd", err, false);
        }
        let _ = ChannelId::new(ch_id)
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }
}