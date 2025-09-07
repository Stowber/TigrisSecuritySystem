// src/watchlist.rs

use anyhow::Result;
use dashmap::DashMap;
use once_cell::sync::Lazy;
use serenity::all::*;
use sqlx::{Pool, Postgres, Row};
use std::time::{Duration, Instant};

use crate::{
    permissions::Permission,
    registry::{env_channels, env_roles},
    AppContext,
};

/// Prosty cache: message_id -> (author_id, content)
static MESSAGE_CACHE: Lazy<DashMap<u64, (u64, String)>> = Lazy::new(|| DashMap::new());
const MESSAGE_CACHE_LIMIT: usize = 5000;

static VOICE_CACHE: Lazy<DashMap<(u64, u64), (u64, Instant)>> =
    Lazy::new(|| DashMap::new());

pub struct Watchlist;

impl Watchlist {
    /* ===========================
       DDL i slash-komendy
       =========================== */

    pub async fn ensure_tables(db: &Pool<Postgres>) -> Result<()> {
        sqlx::query(r#"CREATE SCHEMA IF NOT EXISTS tss"#)
            .execute(db)
            .await?;
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS tss.watchlist (
                guild_id   BIGINT NOT NULL,
                user_id    BIGINT NOT NULL,
                channel_id BIGINT NOT NULL,
                added_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
                PRIMARY KEY(guild_id, user_id)
            )
            "#,
        )
        .execute(db)
        .await?;
        Ok(())
    }

    pub async fn register_commands(ctx: &Context, gid: GuildId) -> Result<()> {
        gid.create_command(
            &ctx.http,
            CreateCommand::new("watchlist")
                .description("Zarządzanie obserwacją użytkowników")
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::SubCommand,
                        "add",
                        "Dodaj użytkownika",
                    )
                    .add_sub_option(
                        CreateCommandOption::new(CommandOptionType::User, "user", "Kto?")
                            .required(true),
                    ),
                )
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::SubCommand,
                        "remove",
                        "Usuń z listy",
                    )
                    .add_sub_option(
                        CreateCommandOption::new(CommandOptionType::User, "user", "Kto?")
                            .required(true),
                    ),
                )
                .add_option(CreateCommandOption::new(
                    CommandOptionType::SubCommand,
                    "list",
                    "Pokaż listę",
                ))
                .default_member_permissions(Permissions::MODERATE_MEMBERS),
        )
        .await?;
        Ok(())
    }

    pub async fn on_interaction(ctx: &Context, app: &AppContext, interaction: Interaction) {
        let Some(cmd) = interaction.clone().command() else {
            return;
        };
        if cmd.data.name != "watchlist" {
            return;
        }

        let Some(gid) = cmd.guild_id else {
            return;
        };
        if !crate::admcheck::has_permission(ctx, gid, cmd.user.id, Permission::Watchlist).await {
            Self::respond_ephemeral(ctx, &cmd, "⛔ Brak uprawnień.").await;
            return;
        }

        let sub = cmd.data.options.first().and_then(|o| match &o.value {
            CommandDataOptionValue::SubCommand(opts) => Some((o.name.clone(), opts.as_slice())),
            _ => None,
        });

        let Some((sub_name, sub_opts)) = sub else {
            return;
        };

        match sub_name.as_str() {
            "add" => {
                if let Err(e) = Self::handle_add(ctx, app, &cmd, sub_opts).await {
                    tracing::warn!(?e, "watchlist add failed");
                }
            }
            "remove" => {
                if let Err(e) = Self::handle_remove(ctx, app, &cmd, sub_opts).await {
                    tracing::warn!(?e, "watchlist remove failed");
                }
            }
            "list" => {
                if let Err(e) = Self::handle_list(ctx, app, &cmd).await {
                    tracing::warn!(?e, "watchlist list failed");
                }
            }
            _ => {}
        }
    }

    async fn handle_add(
        ctx: &Context,
        app: &AppContext,
        cmd: &CommandInteraction,
        opts: &[CommandDataOption],
    ) -> Result<()> {
        let user_id = opts
            .iter()
            .find_map(|o| {
                if o.name == "user" {
                    match &o.value {
                        CommandDataOptionValue::User(uid) => Some(*uid),
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .ok_or_else(|| anyhow::anyhow!("missing user"))?;

        let gid = cmd.guild_id.unwrap();

        let existing =
            sqlx::query("SELECT channel_id FROM tss.watchlist WHERE guild_id=$1 AND user_id=$2")
                .bind(gid.get() as i64)
                .bind(user_id.get() as i64)
                .fetch_optional(&app.db)
                .await?;
        if existing.is_some() {
            Self::respond_ephemeral(ctx, cmd, "Użytkownik już jest obserwowany").await;
            return Ok(());
        }

        let env = app.env();
        let overwrites = Self::build_overwrites(&env, gid);

        // przygotowanie nazwy kanału
        let mut nick = gid
            .member(&ctx.http, user_id)
            .await
            .ok()
            .and_then(|m| m.nick.or(Some(m.user.name)))
            .unwrap_or_else(|| user_id.to_string());
        nick.make_ascii_lowercase();
        let nick: String = nick
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let channel_name = format!("watchlist-{}", nick);

        let mut builder = serenity::builder::CreateChannel::new(channel_name)
            .kind(ChannelType::Text)
            .permissions(overwrites)
            .topic(format!("Logi obserwacji dla <@{}>", user_id.get()));

        // opcjonalnie kategoria z ENV
        let cat_id = env_channels::watchlist_category_channels_id(&env);
        if cat_id != 0 {
            builder = builder.category(ChannelId::new(cat_id));
        }

        let channel = gid.create_channel(&ctx.http, builder).await?;

        sqlx::query("INSERT INTO tss.watchlist (guild_id,user_id,channel_id) VALUES ($1,$2,$3)")
            .bind(gid.get() as i64)
            .bind(user_id.get() as i64)
            .bind(channel.id.get() as i64)
            .execute(&app.db)
            .await?;


        Self::respond_ephemeral(
            ctx,
            cmd,
            &format!("Dodano <@{}> do obserwacji", user_id.get()),
        )
        .await;
        Ok(())
    }

    async fn handle_remove(
        ctx: &Context,
        app: &AppContext,
        cmd: &CommandInteraction,
        opts: &[CommandDataOption],
    ) -> Result<()> {
        let user_id = opts
            .iter()
            .find_map(|o| {
                if o.name == "user" {
                    match &o.value {
                        CommandDataOptionValue::User(uid) => Some(*uid),
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .ok_or_else(|| anyhow::anyhow!("missing user"))?;

        let gid = cmd.guild_id.unwrap();
        let row = sqlx::query(
            "DELETE FROM tss.watchlist WHERE guild_id=$1 AND user_id=$2 RETURNING channel_id",
        )
        .bind(gid.get() as i64)
        .bind(user_id.get() as i64)
        .fetch_optional(&app.db)
        .await?;
        if let Some(r) = row {
            let ch: i64 = r.get("channel_id");
            let _ = ChannelId::new(ch as u64).delete(&ctx.http).await;
            Self::respond_ephemeral(ctx, cmd, "Usunięto z obserwacji").await;
        } else {
            Self::respond_ephemeral(ctx, cmd, "Nie obserwowano").await;
        }
        Ok(())
    }

    async fn handle_list(ctx: &Context, app: &AppContext, cmd: &CommandInteraction) -> Result<()> {
        let gid = cmd.guild_id.unwrap();
        let rows =
            sqlx::query("SELECT user_id FROM tss.watchlist WHERE guild_id=$1 ORDER BY added_at")
                .bind(gid.get() as i64)
                .fetch_all(&app.db)
                .await?;
        if rows.is_empty() {
            Self::respond_ephemeral(ctx, cmd, "Brak obserwowanych").await;
            return Ok(());
        }
        let mut content = String::from("Obserwowani:\n");
        for r in rows {
            let uid: i64 = r.get("user_id");
            content.push_str(&format!("<@{}>\n", uid));
        }
        Self::respond_ephemeral(ctx, cmd, &content).await;
        Ok(())
    }

    /* ===========================
       Hooki / zdarzenia
       =========================== */

    /// Logowanie nowej wiadomości (treść + statystyki).
    pub async fn on_message(ctx: &Context, app: &AppContext, msg: &Message) {
        let Some(gid) = msg.guild_id else {
            return;
        };
        // cache do późniejszych edycji/usunięć
        Self::cache_message(msg);

        let uid = msg.author.id.get();
        let (links, mentions_u, mentions_r, everyone) = (
            count_links(&msg.content),
            msg.mentions.len(),
            msg.mention_roles.len(),
            msg.mention_everyone,
        );
        let attachments = msg.attachments.len();

        let jump = jump_link(gid.get(), msg.channel_id.get(), msg.id.get());

        let mut snippet = clamp(&msg.content, 900);
        if snippet.is_empty() && attachments > 0 {
           snippet = "(załącznik)".into();
        }

        let text = format!(
            "Wiadomość na <#{}> • [{}]\nmentions: users={}, roles={}, everyone={} • links={} • attachments={}\n{}",
            msg.channel_id.get(),
            jump,
            mentions_u,
            mentions_r,
            if everyone { "tak" } else { "nie" },
            links,
            attachments,
            snippet
        );
        Self::log(ctx, &app.db, gid.get(), uid, text).await;
    }

    /// Edycja wiadomości – loguje diff (stara -> nowa).
    pub async fn on_message_update(ctx: &Context, app: &AppContext, ev: &MessageUpdateEvent) {
        let Some(gid) = ev.guild_id else { return; };
        let mid = ev.id.get();

        // autor: z eventu albo z cache
        let author_id = ev
            .author
            .as_ref()
            .map(|u| u.id.get())
            .or_else(|| MESSAGE_CACHE.get(&mid).map(|e| e.value().0));
        let Some(uid) = author_id else {
            return;
        };

        // stara treść z cache
        let old = MESSAGE_CACHE
            .get(&mid)
            .map(|e| e.value().1.clone())
            .unwrap_or_default();
        // nowa treść z eventu (może być None, jeśli partial)
        if let Some(new) = ev.content.clone() {
            // zaktualizuj cache
            MESSAGE_CACHE.insert(mid, (uid, new.clone()));

            if new != old {
                let jump = jump_link(gid.get(), ev.channel_id.get(), mid);
                let text = format!(
                    "Edycja wiadomości na <#{}> • [{}]\n**stara:** {}\n**nowa:** {}",
                    ev.channel_id.get(),
                    jump,
                    clamp(&old, 500),
                    clamp(&new, 500)
                );
                Self::log(ctx, &app.db, gid.get(), uid, text).await;
            }
        }
    }

    /// Usunięcie wiadomości – jeśli mamy w cache, logujemy autora i treść.
    pub async fn on_message_delete(
        ctx: &Context,
        app: &AppContext,
        channel_id: ChannelId,
        message_id: MessageId,
        guild_id: Option<GuildId>,
    ) {
        let Some(gid) = guild_id else {
            return;
        };
        let mid = message_id.get();
        if let Some((_, (uid, content))) = MESSAGE_CACHE.remove(&mid) {
            let text = format!(
                "Usunięto wiadomość na <#{}> • (id:{})\n{}",
                channel_id.get(),
                mid,
                clamp(&content, 900)
            );
            Self::log(ctx, &app.db, gid.get(), uid, text).await;
        } else {
            // brak w cache – nie wiemy, czyja była
        }
    }

    /// Reakcja dodana.
    pub async fn on_reaction_add(ctx: &Context, app: &AppContext, r: &Reaction) {
        let Some(gid) = r.guild_id else {
            return;
        };
        let Some(uid) = r.user_id else {
            return;
        };
        let uid = uid.get();
        let jump = jump_link(gid.get(), r.channel_id.get(), r.message_id.get());
        let emoji = format!("{:?}", r.emoji);
        let text = format!(
            "Dodał reakcję {} na wiadomość [{}] w <#{}>",
            emoji,
            jump,
            r.channel_id.get()
        );
        Self::log(ctx, &app.db, gid.get(), uid, text).await;
    }

    /// Reakcja usunięta.
    pub async fn on_reaction_remove(ctx: &Context, app: &AppContext, r: &Reaction) {
         let Some(gid) = r.guild_id else {
            return;
        };
        let Some(uid) = r.user_id else {
            return;
        };
        let uid = uid.get();
        let jump = jump_link(gid.get(), r.channel_id.get(), r.message_id.get());
        let emoji = format!("{:?}", r.emoji);
        let text = format!(
            "Usunął reakcję {} z wiadomości [{}] w <#{}>",
            emoji,
            jump,
            r.channel_id.get()
        );
        Self::log(ctx, &app.db, gid.get(), uid, text).await;
    }

    /// Voice: join/leave/move (było, zostawiamy).
    pub async fn on_voice_state_update(
        ctx: &Context,
        app: &AppContext,
        old: Option<VoiceState>,
        new: &VoiceState,
    ) {
        let gid = match new.guild_id {
            Some(g) => g.get(),
            None => return,
        };
        let uid = new.user_id.get();
        let old_id = old.as_ref().and_then(|o| o.channel_id.map(|c| c.get()));
        let new_id = new.channel_id.map(|c| c.get());
        let now = Instant::now();
        let key = (gid, uid);
        let msg = match (old_id, new_id) {
           (None, Some(n)) => {
                VOICE_CACHE.insert(key, (n, now));
                format!("🎙 Dołączył do <#{}>", n)
            }
            (Some(o), None) => {
                let dur = VOICE_CACHE
                    .remove(&key)
                    .and_then(|(_, (cid, t))| if cid == o { Some(now - t) } else { None });
                if let Some(d) = dur {
                    format!("🎙 Wyszedł z <#{}> (czas: {})", o, format_duration(d))
                } else {
                    format!("🎙 Wyszedł z <#{}>", o)
                }
            }
            (Some(o), Some(n)) if o != n => {
                let dur = VOICE_CACHE
                    .remove(&key)
                    .and_then(|(_, (cid, t))| if cid == o { Some(now - t) } else { None });
                VOICE_CACHE.insert(key, (n, now));
                let dur_text = dur
                    .map(|d| format!(" (czas: {})", format_duration(d)))
                    .unwrap_or_default();
                format!("🎙 Przeniósł się z <#{}> do <#{}>{}", o, n, dur_text)
            }
            _ => return,
        };
        Self::log(ctx, &app.db, gid, uid, msg).await;
    }

    /// Zmiana ról (było – bez zmian).
    pub async fn on_member_update(
        ctx: &Context,
        app: &AppContext,
        old: Option<Member>,
        new: &Member,
    ) {
        let gid = new.guild_id.get();
        let uid = new.user.id.get();
        let mut added: Vec<RoleId> = Vec::new();
        let mut removed: Vec<RoleId> = Vec::new();
        if let Some(o) = old {
            for r in &new.roles {
                if !o.roles.contains(r) {
                    added.push(*r);
                }
            }
            for r in &o.roles {
                if !new.roles.contains(r) {
                    removed.push(*r);
                }
            }
        } else {
            added.extend(new.roles.iter().copied());
        }
        if added.is_empty() && removed.is_empty() {
            return;
        }
        let mut parts = Vec::new();
        if !added.is_empty() {
            let s = added
                .iter()
                .map(|r| format!("<@&{}>", r.get()))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("dodane: {}", s));
        }
        if !removed.is_empty() {
            let s = removed
                .iter()
                .map(|r| format!("<@&{}>", r.get()))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("usunięte: {}", s));
        }
        let text = format!("Zmiana ról ({})", parts.join("; "));
        Self::log(ctx, &app.db, gid, uid, text).await;
    }

    /// Dołączenie do serwera.
    pub async fn on_member_add(ctx: &Context, app: &AppContext, new: &Member) {
        let gid = new.guild_id.get();
        let uid = new.user.id.get();
        Self::log(ctx, &app.db, gid, uid, "Dołączył do serwera".into()).await;
    }

    /// Wyjście z serwera.
    pub async fn on_member_remove(ctx: &Context, app: &AppContext, guild_id: GuildId, user: &User) {
        Self::log(
            ctx,
            &app.db,
            guild_id.get(),
            user.id.get(),
            "Opuścił serwer".into(),
        )
        .await;
    }

    /// Presence + aktywności (wymaga GUILD_PRESENCES).
    pub async fn on_presence_update(ctx: &Context, app: &AppContext, p: &Presence) {
        let Some(gid) = p.guild_id else {
            return;
        };
        let uid = p.user.id.get();
        let status = format!("{:?}", p.status);
        let activity = p
            .activities
            .get(0)
            .map(|a| match a.url.as_ref() {
                Some(u) => format!("{} ({:?}) {}", a.name, a.kind, u),
                None => format!("{} ({:?})", a.name, a.kind),
            })
            .unwrap_or_else(|| "—".into());
        let text = format!("Presence: **{}**, aktywność: {}", status, activity);
        Self::log(ctx, &app.db, gid.get(), uid, text).await;
    }

    /// Dowolna interakcja użytkownika (slash, przyciski, selecty).
    pub async fn on_any_interaction(ctx: &Context, app: &AppContext, i: &Interaction) {
        if let Some(cmd) = i.clone().command() {
            if let Some(gid) = cmd.guild_id {
                let uid = cmd.user.id.get();
                let opts = summarize_options(&cmd.data.options);
                let text = format!("Użył komendy: **/{} {}**", cmd.data.name, opts);
                Self::log(ctx, &app.db, gid.get(), uid, text).await;
            }
        } else if let Some(comp) = i.clone().message_component() {
            if let Some(gid) = comp.guild_id {
                let uid = comp.user.id.get();
                 let kind = match comp.data.kind {
                    ComponentInteractionDataKind::Button => "button",
                    ComponentInteractionDataKind::StringSelect { .. } => "string_select",
                    ComponentInteractionDataKind::UserSelect { .. } => "user_select",
                    ComponentInteractionDataKind::RoleSelect { .. } => "role_select",
                    ComponentInteractionDataKind::MentionableSelect { .. } => "mentionable_select",
                    ComponentInteractionDataKind::ChannelSelect { .. } => "channel_select",
                    ComponentInteractionDataKind::Unknown(_) => "component",
                };
                 let text = format!(
                    "Interakcja: **{}** (custom_id: `{}`)",
                    kind, comp.data.custom_id
                );
                Self::log(ctx, &app.db, gid.get(), uid, text).await;
            }
        }
    }

    /* ===========================
       Helpers
       =========================== */

    fn build_overwrites(env: &str, guild_id: GuildId) -> Vec<PermissionOverwrite> {
        let mut ov = Vec::new();
        // @everyone – brak VIEW_CHANNEL
        ov.push(PermissionOverwrite {
            allow: Permissions::empty(),
            deny: Permissions::VIEW_CHANNEL,
            kind: PermissionOverwriteType::Role(RoleId::new(guild_id.get())),
        });
        // Staff – dostęp
        for rid in env_roles::staff_set(env) {
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

    async fn log(ctx: &Context, db: &Pool<Postgres>, guild_id: u64, user_id: u64, text: String) {
        let row =
            sqlx::query("SELECT channel_id FROM tss.watchlist WHERE guild_id=$1 AND user_id=$2")
                .bind(guild_id as i64)
                .bind(user_id as i64)
                .fetch_optional(db)
                .await;
        let Some(r) = row.ok().flatten() else {
            return;
        };
        let ch: i64 = r.get("channel_id");
        let _ = ChannelId::new(ch as u64)
            .send_message(&ctx.http, CreateMessage::new().content(text))
            .await;
    }

    async fn respond_ephemeral(ctx: &Context, cmd: &CommandInteraction, msg: &str) {
        let _ = cmd
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(msg)
                        .ephemeral(true),
                ),
            )
            .await;
    }

    fn cache_message(msg: &Message) {
        if MESSAGE_CACHE.len() > MESSAGE_CACHE_LIMIT {
            // proste „odcięcie” najstarszego arbitralnie
            if let Some(any_key) = MESSAGE_CACHE.iter().next().map(|e| *e.key()) {
                MESSAGE_CACHE.remove(&any_key);
            }
        }
        MESSAGE_CACHE.insert(msg.id.get(), (msg.author.id.get(), msg.content.clone()));
    }
}

/* ===========================
   Funkcje pomocnicze modułu
   =========================== */

fn clamp(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out = String::with_capacity(max + 1);
    for (i, ch) in s.chars().enumerate() {
        if i >= max {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

fn count_links(s: &str) -> usize {
    // prosta heurystyka (bez zależności do url)
    s.matches("http://").count() + s.matches("https://").count()
}

fn jump_link(guild_id: u64, channel_id: u64, message_id: u64) -> String {
    format!(
        "https://discord.com/channels/{}/{}/{}",
        guild_id, channel_id, message_id
    )
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    let mut parts = Vec::new();
    if h > 0 {
        parts.push(format!("{}h", h));
    }
    if m > 0 {
        parts.push(format!("{}m", m));
    }
    if s > 0 || parts.is_empty() {
        parts.push(format!("{}s", s));
    }
    parts.join(" ")
}

fn summarize_options(options: &[CommandDataOption]) -> String {
    if options.is_empty() {
        return String::new();
    }
    let mut parts = Vec::new();
    for o in options {
        let val = match &o.value {
            CommandDataOptionValue::String(s) => format!("\"{}\"", clamp(s, 64)),
            CommandDataOptionValue::Integer(i) => i.to_string(),
            CommandDataOptionValue::Number(n) => n.to_string(),
            CommandDataOptionValue::Boolean(b) => b.to_string(),
            CommandDataOptionValue::User(u) => format!("<@{}>", u.get()),
            CommandDataOptionValue::Role(r) => format!("<@&{}>", r.get()),
            CommandDataOptionValue::Channel(c) => format!("<#{}>", c.get()),
            CommandDataOptionValue::Attachment(a) => format!("[att:{}]", a.get()),
            CommandDataOptionValue::SubCommand(inner)
            | CommandDataOptionValue::SubCommandGroup(inner) => {
                format!("{} [{}]", o.name, summarize_options(inner))
            }
            _ => o.name.clone(),
        };
        if !matches!(
            o.value,
            CommandDataOptionValue::SubCommand(_) | CommandDataOptionValue::SubCommandGroup(_)
        ) {
            parts.push(format!("{}={}", o.name, val));
        } else {
            parts.push(val);
        }
    }
    parts.join(" ")
}
