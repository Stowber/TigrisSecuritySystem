// src/discord/mod.rs
use std::sync::Arc;
use anyhow::Result;
use std::panic::AssertUnwindSafe;

use crate::{altguard, AppContext};
use crate::altguard::{JoinMeta, ScoreInput};

use serenity::all::*;
use serenity::async_trait;

use crate::stats_channels::StatsChannels;
use crate::new_channels::NewChannels;
use crate::welcome::Welcome;
use crate::verify::Verify;
use crate::chatguard::ChatGuard;
use crate::ban::Ban;
use crate::kick::Kick;
use crate::warn::Warns;
use crate::mdel::MDel;
use crate::mute::Mute;
use crate::userinfo::UserInfo;
use crate::admcheck::AdmCheck;
use crate::levels::Levels;
use crate::test_cmd::TestCmd;
use crate::watchlist::Watchlist;
use crate::techlog::TechLog;
use std::time::Instant;
use futures_util::FutureExt;

// --- AdminScore (/points)
use crate::admin_points::AdminPoints;

// --- Commands Sync (/slash-clean, /slash-resync)
use crate::commands_sync;
use crate::command_acl;

pub struct Handler {
    pub app: Arc<AppContext>,
    pub altguard: Arc<altguard::AltGuard>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        tracing::info!("Logged in as {}", ready.user.name);

        // Migracje tabel (raz przy starcie procesu)
        AdminPoints::ensure_tables(&self.app.db).await.ok();
        Warns::ensure_tables(&self.app.db).await.ok();
        Mute::ensure_tables(&self.app.db).await.ok();
        Levels::ensure_tables(&self.app.db).await.ok();
        Watchlist::ensure_tables(&self.app.db).await.ok();

        // Rejestr komend slash dla wszystkich gildii
        for g in ready.guilds {
            if let Err(e) = register_commands_for_guild(&ctx, g.id).await {
                tracing::warn!(error=?e, gid=%g.id.get(), "register_commands_for_guild failed (wrapper)");
            }
        }
    }

    async fn channel_create(&self, ctx: Context, channel: GuildChannel) {
        NewChannels::on_channel_create(&ctx, &self.app, &channel).await;
    }

    async fn channel_delete(
        &self,
        ctx: Context,
        channel: GuildChannel,
        messages: Option<Vec<Message>>,
    ) {
        NewChannels::on_channel_delete(&ctx, &self.app, &channel, messages).await;
        self.app
            .antinuke()
            .notify_channel_delete(channel.guild_id.get())
            .await;
    }

    /// Brama interakcji: slash + komponenty
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let started = Instant::now();
        let cmd_copy = interaction.clone().command();

        let fut = async {
            // Najpierw lekki logger watchlist (nie konsumuje Interaction)
            Watchlist::on_any_interaction(&ctx, &self.app, &interaction).await;

            // Potem właściwe handlery (klonujemy, bo Verify na końcu konsumuje Interaction)
            AdminPoints::on_interaction(&ctx, &self.app, interaction.clone()).await;
            ChatGuard::on_interaction(&ctx, &self.app, interaction.clone()).await;
            Ban::on_interaction(&ctx, &self.app, interaction.clone()).await;
            Kick::on_interaction(&ctx, &self.app, interaction.clone()).await;
            Warns::on_interaction(&ctx, &self.app, interaction.clone()).await;
            Mute::on_interaction(&ctx, &self.app, interaction.clone()).await;
            UserInfo::on_interaction(&ctx, &self.app, interaction.clone()).await;
            AdmCheck::on_interaction(&ctx, &self.app, interaction.clone()).await;
            TestCmd::on_interaction(&ctx, &self.app, interaction.clone()).await;
            Watchlist::on_interaction(&ctx, &self.app, interaction.clone()).await;

            // /mdel – PRZED Verify, bo Verify zużywa Interaction
            MDel::on_interaction(&ctx, &self.app, interaction.clone()).await;

            // Verify (panel weryfikacji) — NA KOŃCU (konsumuje Interaction)
            Verify::on_interaction(&ctx, &self.app, interaction).await;
        };

        let result = AssertUnwindSafe(fut).catch_unwind().await;

        if let Some(cmd) = cmd_copy {
            let status = if result.is_ok() { "ok" } else { "panic" };
            let error = if result.is_ok() { None } else { Some("panic") };
            TechLog::log_command(&ctx, &self.app, &cmd, started.elapsed(), status, error).await;
        }
    }

    async fn message(&self, ctx: Context, msg: Message) {
         let Some(gid) = msg.guild_id else {
            return;
        };
        if msg.author.bot {
            return;
        }

        ChatGuard::on_message(&ctx, &self.app, &msg).await;
        Levels::on_message(&ctx, &self.app, &msg).await;
        Watchlist::on_message(&ctx, &self.app, &msg).await;

        let mentions = msg.mentions.len() as u32;
        self.altguard
            .record_message(gid.get(), msg.author.id.get(), &msg.content, mentions)
            .await;
    }

    async fn message_update(
        &self,
        ctx: Context,
        _old: Option<Message>,
        _new: Option<Message>,
        event: MessageUpdateEvent,
    ) {
        Watchlist::on_message_update(&ctx, &self.app, &event).await;
    }

    async fn message_delete(
        &self,
        ctx: Context,
        channel_id: ChannelId,
        message_id: MessageId,
        guild_id: Option<GuildId>,
    ) {
        Watchlist::on_message_delete(&ctx, &self.app, channel_id, message_id, guild_id).await;
    }

    async fn reaction_add(&self, ctx: Context, reaction: Reaction) {
        Watchlist::on_reaction_add(&ctx, &self.app, &reaction).await;
    }

    async fn reaction_remove(&self, ctx: Context, reaction: Reaction) {
        Watchlist::on_reaction_remove(&ctx, &self.app, &reaction).await;
    }

    async fn guild_ban_addition(&self, ctx: Context, guild_id: GuildId, banned_user: User) {
        // NIE konstruujemy ręcznie non-exhaustive eventów.
        // Jeśli potrzebujesz dodatkowej logiki bana, zrób to tutaj lub w osobnym module,
        // ale przekaż proste dane, nie typy eventów.
        Watchlist::log_action(
            &ctx,
            &self.app.db,
            guild_id.get(),
            banned_user.id.get(),
            None,
            "🚫 Użytkownik zbanowany",
        )
        .await;
        self.app.antinuke().notify_ban(guild_id.get()).await;
    }

    async fn guild_ban_removal(&self, ctx: Context, guild_id: GuildId, unbanned_user: User) {
        Watchlist::log_action(
            &ctx,
            &self.app.db,
            guild_id.get(),
            unbanned_user.id.get(),
            None,
            "♻️ Ban zdjęty",
        )
        .await;
    }

    async fn guild_role_delete(&self, _ctx: Context, guild_id: GuildId, _removed_role: Role) {
        self.app.antinuke().notify_role_delete(guild_id.get()).await;
    }

    async fn presence_update(&self, ctx: Context, presence: Presence) {
        Watchlist::on_presence_update(&ctx, &self.app, &presence).await;
    }

    async fn voice_state_update(&self, ctx: Context, old: Option<VoiceState>, new: VoiceState) {
        Levels::on_voice_state_update(&ctx, &self.app, old.clone(), &new).await;
        Watchlist::on_voice_state_update(&ctx, &self.app, old, &new).await;
    }

    async fn guild_member_update(
        &self,
        ctx: Context,
        old: Option<Member>,
        new: Option<Member>,
        _event: GuildMemberUpdateEvent,
    ) {
        if let Some(new) = new.as_ref() {
            Watchlist::on_member_update(&ctx, &self.app, old, new).await;
        }
    }

    // _is_new zgodnie z Serenity 0.12
    async fn guild_create(&self, ctx: Context, guild: Guild, _is_new: Option<bool>) {
        let gid = guild.id.get();

        // AltGuard warmup
        self.altguard.warmup_cache(gid).await;

        // Statystyki kanałów
        StatsChannels::sync_on_ready(&ctx, &self.app, gid).await;
        StatsChannels::spawn_tasks(ctx.clone(), self.app.clone(), gid);

        // Panel weryfikacji
        let any_channel = match guild.id.channels(&ctx.http).await {
            Ok(map) => map.keys().next().cloned().unwrap_or(ChannelId::new(1)),
            Err(_) => ChannelId::new(1),
        };
        let _ = Verify::post_panel(&ctx, &self.app, guild.id, any_channel).await;

        // Rejestr komend dla tej gildii
        if let Err(e) = register_commands_for_guild(&ctx, guild.id).await {
            tracing::warn!(error=?e, gid, "register_commands_for_guild failed (on guild_create)");
        }

        tracing::info!(guild=%guild.name, gid, "AltGuard cache warmed + stats synced + verify panel ensured + commands registered (see warnings if any failed)");
    }

    async fn guild_member_addition(&self, ctx: Context, member: Member) {
        let gid = member.guild_id.get();
        let uid = member.user.id.get();

        self.altguard
            .record_join(JoinMeta {
                guild_id: gid,
                user_id: uid,
                invite_code: None,
                inviter_id: None,
                at: None,
            })
            .await;

        let avatar_url = member.user.avatar_url();
        match self
            .altguard
            .score_user(&ScoreInput {
                guild_id: gid,
                user_id: uid,
                username: Some(member.user.name.clone()),
                display_name: member.nick.clone(),
                global_name: member.user.global_name.clone(),
                invite_code: None,
                inviter_id: None,
                has_trusted_role: false,
                avatar_url,
            })
            .await
        {
            Ok(score) => {
                tracing::info!(
                    gid,
                    uid,
                    verdict=?score.verdict,
                    score=%score.score,
                    explain=%score.explain,
                    "JOIN scored"
                );
            }
            Err(e) => {
                tracing::warn!(error=?e, gid, uid, "AltGuard scoring failed");
            }
        }

        StatsChannels::handle_member_join(&ctx, &self.app, &member).await;
        Watchlist::on_member_add(&ctx, &self.app, &member).await;
    }

    async fn guild_member_removal(
        &self,
        ctx: Context,
        guild_id: GuildId,
        user: User,
        _member: Option<Member>,
    ) {
        Welcome::send_goodbye(&ctx, &self.app, guild_id, &user).await;
        StatsChannels::handle_member_remove(&ctx, &self.app, guild_id.get()).await;
        Watchlist::on_member_remove(&ctx, &self.app, guild_id, &user).await;
    }
}

fn intents_from_settings(names: &[String]) -> GatewayIntents {
    let mut i = GatewayIntents::empty();
    for n in names {
        match n.as_str() {
            "GUILDS" => i |= GatewayIntents::GUILDS,
            "GUILD_MEMBERS" => i |= GatewayIntents::GUILD_MEMBERS,
            "GUILD_MESSAGES" => i |= GatewayIntents::GUILD_MESSAGES,
            "GUILD_MESSAGE_REACTIONS" => i |= GatewayIntents::GUILD_MESSAGE_REACTIONS,
            "GUILD_PRESENCES" => i |= GatewayIntents::GUILD_PRESENCES,
            "MESSAGE_CONTENT" => i |= GatewayIntents::MESSAGE_CONTENT,
            "GUILD_VOICE_STATES" => i |= GatewayIntents::GUILD_VOICE_STATES,
            _ => {}
        }
    }
    i
}

pub async fn run_bot(ctx: Arc<AppContext>) -> Result<()> {
    let token = &ctx.settings.discord.token;
    if token.is_empty() {
        anyhow::bail!("Brak tokenu Discord (TSS_DISCORD_TOKEN). Uzupełnij w .env.");
    }

    let intents = intents_from_settings(&ctx.settings.discord.intents);

    let handler = Handler {
        app: ctx.clone(),
        altguard: ctx.altguard(),
    };

    let mut client = serenity::Client::builder(token, intents)
        .event_handler(handler)
        .await?;

    tracing::info!("Discord client starting…");
    client.start().await?;
    Ok(())
}

/* ============================================================
   REJESTR KOMEND
   ============================================================ */
async fn register_commands_for_guild(ctx: &Context, guild_id: GuildId) -> Result<()> {
    // Każdą komendę rejestruj osobno, z nazwą w logu.
    if let Err(e) = Verify::register_commands(ctx, guild_id).await {
        tracing::warn!(error=?e, gid=%guild_id.get(), "register verify failed");
    }
    if let Err(e) = ChatGuard::register_commands(ctx, guild_id).await {
        tracing::warn!(error=?e, gid=%guild_id.get(), "register chatguard failed");
    }
    if let Err(e) = AdminPoints::register_commands(ctx, guild_id).await {
        tracing::warn!(error=?e, gid=%guild_id.get(), "register punkty failed");
    }
    if let Err(e) = Ban::register_commands(ctx, guild_id).await {
        tracing::warn!(error=?e, gid=%guild_id.get(), "register ban failed");
    }
    if let Err(e) = Kick::register_commands(ctx, guild_id).await {
        tracing::warn!(error=?e, gid=%guild_id.get(), "register kick failed");
    }
    if let Err(e) = Warns::register_commands(ctx, guild_id).await {
        tracing::warn!(error=?e, gid=%guild_id.get(), "register warn failed");
    }
    if let Err(e) = MDel::register_commands(ctx, guild_id).await {
        tracing::warn!(error=?e, gid=%guild_id.get(), "register mdel failed");
    }
    if let Err(e) = Mute::register_commands(ctx, guild_id).await {
        tracing::warn!(error=?e, gid=%guild_id.get(), "register mute failed");
    }
    if let Err(e) = UserInfo::register_commands(ctx, guild_id).await {
        tracing::warn!(error=?e, gid=%guild_id.get(), "register userinfo failed");
    }
    if let Err(e) = AdmCheck::register_commands(ctx, guild_id).await {
        tracing::warn!(error=?e, gid=%guild_id.get(), "register admcheck failed");
    }
    if let Err(e) = TestCmd::register_commands(ctx, guild_id).await {
        tracing::warn!(error=?e, gid=%guild_id.get(), "register test failed");
    }
    if let Err(e) = Watchlist::register_commands(ctx, guild_id).await {
        tracing::warn!(error=?e, gid=%guild_id.get(), "register watchlist failed");
    }
    if let Err(e) = commands_sync::register_commands(ctx, guild_id).await {
        tracing::warn!(error=?e, gid=%guild_id.get(), "commands_sync::register_commands failed");
    }
    if let Err(e) = command_acl::apply_permissions(ctx, guild_id).await {
        tracing::warn!(error=?e, gid=%guild_id.get(), "apply_permissions failed");
    }
    Ok(())
}
