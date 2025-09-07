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

/// (guild_id, user_id) -> (channel_id, joined_at)
static VOICE_CACHE: Lazy<DashMap<(u64, u64), (u64, Instant)>> = Lazy::new(|| DashMap::new());
/// (guild_id, user_id) -> kiedy włączono kamerkę
static VIDEO_CACHE: Lazy<DashMap<(u64, u64), Instant>> = Lazy::new(|| DashMap::new());
/// (guild_id, user_id) -> kiedy uruchomiono livestream (Go Live)
static STREAM_CACHE: Lazy<DashMap<(u64, u64), Instant>> = Lazy::new(|| DashMap::new());

pub struct Watchlist;

/// Attachment info przekazywane do [`log`].
enum LogAttachment {
    Attachment(Attachment),
}

/* ===========================
   Kolory i drobne stałe
   =========================== */
const COLOR_SUCCESS: u32 = 0x2ECC71;
const COLOR_INFO:    u32 = 0x3498DB;
const COLOR_ERROR:   u32 = 0xE74C3C;
const COLOR_DEFAULT: u32 = 0x5865F2;

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
                    // Wybór przez picker Discorda (nick)
                    .add_sub_option(
                        CreateCommandOption::new(CommandOptionType::User, "user", "Wybierz użytkownika (nick)")
                            .required(false),
                    )
                    // Lub wpisanie ID jako tekst (można wkleić 123... lub <@123...>)
                    .add_sub_option(
                        CreateCommandOption::new(CommandOptionType::String, "id", "ID użytkownika (np. 123456789012345678 lub @wzmianka)")
                            .required(false),
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
            Self::respond_embed_ephemeral(
                ctx,
                &cmd,
                "⛔ Brak uprawnień",
                "Nie posiadasz uprawnień do zarządzania watchlistą.",
                COLOR_ERROR,
            )
            .await;
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
                    Self::respond_embed_ephemeral(
                        ctx,
                        &cmd,
                        "❌ Nie udało się dodać",
                        &format!("{}", e),
                        COLOR_ERROR,
                    )
                    .await;
                }
            }
            "remove" => {
                if let Err(e) = Self::handle_remove(ctx, app, &cmd, sub_opts).await {
                    tracing::warn!(?e, "watchlist remove failed");
                    Self::respond_embed_ephemeral(
                        ctx,
                        &cmd,
                        "❌ Błąd usuwania",
                        &format!("{}", e),
                        COLOR_ERROR,
                    )
                    .await;
                }
            }
            "list" => {
                if let Err(e) = Self::handle_list(ctx, app, &cmd).await {
                    tracing::warn!(?e, "watchlist list failed");
                    Self::respond_embed_ephemeral(
                        ctx,
                        &cmd,
                        "❌ Błąd pobierania listy",
                        &format!("{}", e),
                        COLOR_ERROR,
                    )
                    .await;
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
        let user_opt: Option<UserId> = opts.iter().find_map(|o| {
            if o.name == "user" {
                match &o.value {
                    CommandDataOptionValue::User(uid) => Some(*uid),
                    _ => None,
                }
            } else { None }
        });

        let id_opt: Option<String> = opts.iter().find_map(|o| {
            if o.name == "id" {
                match &o.value {
                    CommandDataOptionValue::String(s) => Some(s.clone()),
                    _ => None,
                }
            } else { None }
        });

        // ACK w 3s
        cmd.defer_ephemeral(&ctx.http).await?;

        // Walidacja: dokładnie jedno z user/id
        match (user_opt, id_opt.as_deref()) {
            (Some(_), Some(_)) => {
                Self::respond_embed_ephemeral(
                    ctx,
                    cmd,
                    "⚠️ Błędne użycie",
                    "Podaj **albo** `user`, **albo** `id` — nie oba jednocześnie.",
                    COLOR_ERROR,
                )
                .await;
                return Ok(());
            }
            (None, None) => {
                Self::respond_embed_ephemeral(
                    ctx,
                    cmd,
                    "⚠️ Brak argumentów",
                    "Podaj użytkownika `user` lub wpisz `id`.",
                    COLOR_ERROR,
                )
                .await;
                return Ok(());
            }
            _ => {}
        }

        // Ustal docelowe UserId na podstawie user/id
        let target_uid: UserId = if let Some(u) = user_opt {
            u
        } else {
            // parse ID string (akceptuj też formę <@123...> / <@!123...>)
            let s = id_opt.unwrap();
            let parsed = parse_user_id_str(&s).ok_or_else(|| anyhow::anyhow!("błędny format ID"))?;
            // Spróbuj pobrać użytkownika – jeśli nie istnieje, zgłoś błąd
            match UserId::new(parsed).to_user(&ctx.http).await {
                Ok(_u) => UserId::new(parsed),
                Err(_e) => {
                    Self::respond_embed_ephemeral(
                        ctx,
                        cmd,
                        "🚫 Błędne ID",
                        &format!("Nie znaleziono użytkownika o ID `{}`.", parsed),
                        COLOR_ERROR,
                    )
                    .await;
                    return Ok(());
                }
            }
        };

        let gid = cmd.guild_id.unwrap();

        // Czy już na liście?
        let existing = sqlx::query("SELECT channel_id FROM tss.watchlist WHERE guild_id=$1 AND user_id=$2")
            .bind(gid.get() as i64)
            .bind(target_uid.get() as i64)
            .fetch_optional(&app.db)
            .await?;
        if let Some(r) = existing {
            let ch: i64 = r.get("channel_id");
            Self::respond_embed_ephemeral(
                ctx,
                cmd,
                "ℹ️ Już na watchliście",
                &format!(
                    "Użytkownik <@{}> **jest już** obserwowany.\nKanał logów: <#{}>.",
                    target_uid.get(),
                    ch as u64
                ),
                COLOR_INFO,
            )
            .await;
            return Ok(());
        }

        let env = app.env();
        let overwrites = Self::build_overwrites(&env, gid);

        // --- POPRAWIONE: bez Future w .or_else(), dwa jawne awaity ---
        let mut nick = if let Ok(m) = gid.member(&ctx.http, target_uid).await {
            m.nick.clone().unwrap_or(m.user.name)
        } else if let Ok(u) = UserId::new(target_uid.get()).to_user(&ctx.http).await {
            u.name
        } else {
            target_uid.to_string()
        };
        // -------------------------------------------------------------

        nick.make_ascii_lowercase();
        let nick: String = nick.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let channel_name = format!("watchlist-{}", nick);

        let mut builder = serenity::builder::CreateChannel::new(channel_name)
            .kind(ChannelType::Text)
            .permissions(overwrites)
            .topic(format!("Logi obserwacji dla <@{}>", target_uid.get()));

        // opcjonalnie kategoria z ENV
        let cat_id = env_channels::watchlist_category_channels_id(&env);
        if cat_id != 0 {
            builder = builder.category(ChannelId::new(cat_id));
        }

        let channel = gid.create_channel(&ctx.http, builder).await?;

        sqlx::query("INSERT INTO tss.watchlist (guild_id,user_id,channel_id) VALUES ($1,$2,$3)")
            .bind(gid.get() as i64)
            .bind(target_uid.get() as i64)
            .bind(channel.id.get() as i64)
            .execute(&app.db)
            .await?;

        Self::respond_embed_ephemeral(
            ctx,
            cmd,
            "✅ Dodano do watchlisty",
            &format!(
                "Użytkownik: <@{}>\nKanał logów: <#{}>.",
                target_uid.get(),
                channel.id.get()
            ),
            COLOR_SUCCESS,
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

        cmd.defer_ephemeral(&ctx.http).await?;

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
            Self::respond_embed_ephemeral(
                ctx,
                cmd,
                "✅ Usunięto z watchlisty",
                &format!("Użytkownik: <@{}>", user_id.get()),
                COLOR_SUCCESS,
            )
            .await;
        } else {
            Self::respond_embed_ephemeral(
                ctx,
                cmd,
                "ℹ️ Nie ma na watchliście",
                &format!(
                    "Użytkownik <@{}> **nie znajduje się** na liście.",
                    user_id.get()
                ),
                COLOR_INFO,
            )
            .await;
        }
        Ok(())
    }

    async fn handle_list(
        ctx: &Context,
        app: &AppContext,
        cmd: &CommandInteraction,
    ) -> Result<()> {
        let gid = cmd.guild_id.unwrap();
        cmd.defer_ephemeral(&ctx.http).await?;

        let rows = sqlx::query("SELECT user_id FROM tss.watchlist WHERE guild_id=$1 ORDER BY added_at")
            .bind(gid.get() as i64)
            .fetch_all(&app.db)
            .await?;

        if rows.is_empty() {
            Self::respond_embed_ephemeral(ctx, cmd, "👁️ Watchlist", "Brak obserwowanych użytkowników.", COLOR_INFO).await;
            return Ok(());
        }

        let mut users: Vec<u64> = Vec::with_capacity(rows.len());
        for r in rows {
            users.push(r.get::<i64, _>("user_id") as u64);
        }

        // Podział na chunki <= 4000 znaków
        let mut chunks: Vec<String> = Vec::new();
        let mut current = String::new();
        for uid in users {
            let line = format!("<@{}>\n", uid);
            if current.len() + line.len() > 4000 {
                chunks.push(current);
                current = String::new();
            }
            current.push_str(&line);
        }
        if !current.is_empty() {
            chunks.push(current);
        }

        let total = chunks.iter().map(|c| c.lines().count()).sum::<usize>();
        let mut embeds: Vec<CreateEmbed> = Vec::new();
        for (i, chunk) in chunks.iter().enumerate() {
            let title = if i == 0 {
                format!(
                    "👁️ Watchlist – {} użytkownik{}",
                    total,
                    if total == 1 {
                        ""
                    } else if (2..=4).contains(&(total % 10)) && (total / 10) % 10 != 1 {
                        "i"
                    } else {
                        "ów"
                    }
                )
            } else {
                "👁️ Watchlist (ciąg dalszy)".to_string()
            };
            embeds.push(
                CreateEmbed::new()
                    .title(title)
                    .description(chunk)
                    .color(Color::from_rgb(
                        ((COLOR_DEFAULT >> 16) & 0xFF) as u8,
                        ((COLOR_DEFAULT >> 8) & 0xFF) as u8,
                        (COLOR_DEFAULT & 0xFF) as u8,
                    ))
                    .timestamp(Timestamp::now()),
            );
        }

        Self::respond_embeds_ephemeral(ctx, cmd, embeds).await;
        Ok(())
    }

    /* ===========================
       Hooki / zdarzenia
       =========================== */

    /// Logowanie nowej wiadomości (treść + statystyki).
    pub async fn on_message(ctx: &Context, app: &AppContext, msg: &Message) {
        let Some(gid) = msg.guild_id else { return; };
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
            "💬 Wiadomość w <#{}> • [{}]\nmentions: users={}, roles={}, everyone={} • links={} • attachments={}\n{}",
            msg.channel_id.get(),
            jump,
            mentions_u,
            mentions_r,
            if everyone { "tak" } else { "nie" },
            links,
            attachments,
            snippet
        );
        let first_attachment = msg.attachments.first().cloned().map(LogAttachment::Attachment);
        Self::log(ctx, &app.db, gid.get(), uid, text, first_attachment).await;
    }

    /// Edycja wiadomości – loguje diff (stara -> nowa).
    pub async fn on_message_update(ctx: &Context, app: &AppContext, ev: &MessageUpdateEvent) {
        let Some(gid) = ev.guild_id else { return; };
        let mid = ev.id.get();

        let author_id = ev
            .author
            .as_ref()
            .map(|u| u.id.get())
            .or_else(|| MESSAGE_CACHE.get(&mid).map(|e| e.value().0));
        let Some(uid) = author_id else { return; };

        let old = MESSAGE_CACHE
            .get(&mid)
            .map(|e| e.value().1.clone())
            .unwrap_or_default();

        if let Some(new) = ev.content.clone() {
            MESSAGE_CACHE.insert(mid, (uid, new.clone()));
            if new != old {
                let jump = jump_link(gid.get(), ev.channel_id.get(), mid);
                let text = format!(
                    "✏️ Edycja wiadomości w <#{}> • [{}]\n**stara:** {}\n**nowa:** {}",
                    ev.channel_id.get(),
                    jump,
                    clamp(&old, 500),
                    clamp(&new, 500)
                );
                Self::log(ctx, &app.db, gid.get(), uid, text, None).await;
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
        let Some(gid) = guild_id else { return; };
        let mid = message_id.get();
        if let Some((_, (uid, content))) = MESSAGE_CACHE.remove(&mid) {
            let text = format!(
                "🗑️ Usunięto wiadomość w <#{}> • (id:{})\n{}",
                channel_id.get(),
                mid,
                clamp(&content, 900)
            );
            Self::log(ctx, &app.db, gid.get(), uid, text, None).await;
        }
    }

    /// Reakcja dodana.
    pub async fn on_reaction_add(ctx: &Context, app: &AppContext, r: &Reaction) {
        let Some(gid) = r.guild_id else { return; };
        let Some(uid) = r.user_id else { return; };
        let uid = uid.get();
        let jump = jump_link(gid.get(), r.channel_id.get(), r.message_id.get());
        let emoji = format!("{:?}", r.emoji);
        let text = format!("➕ Reakcja {} na [{}] w <#{}>", emoji, jump, r.channel_id.get());
        Self::log(ctx, &app.db, gid.get(), uid, text, None).await;
    }

    /// Reakcja usunięta.
    pub async fn on_reaction_remove(ctx: &Context, app: &AppContext, r: &Reaction) {
        let Some(gid) = r.guild_id else { return; };
        let Some(uid) = r.user_id else { return; };
        let uid = uid.get();
        let jump = jump_link(gid.get(), r.channel_id.get(), r.message_id.get());
        let emoji = format!("{:?}", r.emoji);
        let text = format!("➖ Reakcja {} z [{}] w <#{}>", emoji, jump, r.channel_id.get());
        Self::log(ctx, &app.db, gid.get(), uid, text, None).await;
    }

    /// Voice: join/leave/move + video/stream (z licznikami).
    pub async fn on_voice_state_update(
        ctx: &Context,
        app: &AppContext,
        old: Option<VoiceState>,
        new: &VoiceState,
    ) {
        let gid = match new.guild_id { Some(g) => g.get(), None => return };
        let uid = new.user_id.get();
        let old_id = old.as_ref().and_then(|o| o.channel_id.map(|c| c.get()));
        let new_id = new.channel_id.map(|c| c.get());
        let now = Instant::now();
        let key = (gid, uid);

        /* ===== Wejścia/wyjścia/przenosiny ===== */
        let mut voice_msg: Option<String> = None;
        match (old_id, new_id) {
            (None, Some(n)) => {
                VOICE_CACHE.insert(key, (n, now));
                voice_msg = Some(format!("🎙 Dołączył do <#{}>", n));
            }
            (Some(o), None) => {
                let dur = VOICE_CACHE
                    .remove(&key)
                    .and_then(|(_, (cid, t))| if cid == o { Some(now - t) } else { None });

                voice_msg = Some(if let Some(d) = dur {
                    format!("🎙 Wyszedł z <#{}> (czas: {})", o, format_duration(d))
                } else {
                    format!("🎙 Wyszedł z <#{}>", o)
                });

                if let Some((_, started)) = VIDEO_CACHE.remove(&key) {
                    let d = now - started;
                    let msg =
                        format!("📷 Wyłączył kamerę (opuszczając <#{}>) (czas: {})", o, format_duration(d));
                    Self::log(ctx, &app.db, gid, uid, msg, None).await;
                }

                if let Some((_, started)) = STREAM_CACHE.remove(&key) {
                    let d = now - started;
                    let msg = format!(
                        "📺 Zakończył transmisję (opuszczając <#{}>) (czas: {})",
                        o,
                        format_duration(d)
                    );
                    Self::log(ctx, &app.db, gid, uid, msg, None).await;
                }
            }
            (Some(o), Some(n)) if o != n => {
                let dur = VOICE_CACHE
                    .remove(&key)
                    .and_then(|(_, (cid, t))| if cid == o { Some(now - t) } else { None });
                VOICE_CACHE.insert(key, (n, now));
                let dur_text = dur.map(|d| format!(" (czas: {})", format_duration(d))).unwrap_or_default();
                voice_msg = Some(format!("🎙 Przeniósł się z <#{}> do <#{}>{}", o, n, dur_text));
            }
            _ => {}
        }
        if let Some(msg) = voice_msg {
            Self::log(ctx, &app.db, gid, uid, msg, None).await;
        }

        let ch_for_msg = new_id.or(old_id);

        /* ===== Kamera: start/stop + czas ===== */
        let old_video = old.as_ref().map(|o| o.self_video).unwrap_or(false);
        let new_video = new.self_video;

        if !old_video && new_video {
            VIDEO_CACHE.insert(key, now);
            if let Some(ch) = ch_for_msg {
                Self::log(ctx, &app.db, gid, uid, format!("📷 Włączył kamerę w <#{}>", ch), None).await;
            }
        } else if old_video && !new_video {
            let dur = VIDEO_CACHE.remove(&key).map(|(_, t)| now - t);
            if let Some(ch) = ch_for_msg {
                let msg = match dur {
                    Some(d) => format!("📷 Wyłączył kamerę w <#{}> (czas: {})", ch, format_duration(d)),
                    None => format!("📷 Wyłączył kamerę w <#{}>", ch),
                };
                Self::log(ctx, &app.db, gid, uid, msg, None).await;
            }
        }

        /* ===== Livestream (Go Live): start/stop + czas ===== */
        let old_stream = old.as_ref().and_then(|o| o.self_stream).unwrap_or(false);
        let new_stream = new.self_stream.unwrap_or(false);

        if !old_stream && new_stream {
            STREAM_CACHE.insert(key, now);
            if let Some(ch) = ch_for_msg {
                Self::log(ctx, &app.db, gid, uid, format!("📺 Rozpoczął transmisję (Go Live) w <#{}>", ch), None).await;
            }
        } else if old_stream && !new_stream {
            let dur = STREAM_CACHE.remove(&key).map(|(_, t)| now - t);
            if let Some(ch) = ch_for_msg {
                let msg = match dur {
                    Some(d) => format!("📺 Zakończył transmisję w <#{}> (czas: {})", ch, format_duration(d)),
                    None => format!("📺 Zakończył transmisję w <#{}>", ch),
                };
                Self::log(ctx, &app.db, gid, uid, msg, None).await;
            }
        }
    }

    /// Zmiana ról.
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
            let s = added.iter().map(|r| format!("<@&{}>", r.get())).collect::<Vec<_>>().join(", ");
            parts.push(format!("dodane: {}", s));
        }
        if !removed.is_empty() {
            let s = removed.iter().map(|r| format!("<@&{}>", r.get())).collect::<Vec<_>>().join(", ");
            parts.push(format!("usunięte: {}", s));
        }
        let text = format!("🛡️ Zmiana ról ({})", parts.join("; "));
        Self::log(ctx, &app.db, gid, uid, text, None).await;
    }

    /// Dołączenie do serwera.
    pub async fn on_member_add(ctx: &Context, app: &AppContext, new: &Member) {
        let gid = new.guild_id.get();
        let uid = new.user.id.get();
        Self::log(ctx, &app.db, gid, uid, "👋 Dołączył do serwera".into(), None).await;
    }

    /// Wyjście z serwera.
    pub async fn on_member_remove(ctx: &Context, app: &AppContext, guild_id: GuildId, user: &User) {
        Self::log(
            ctx,
            &app.db,
            guild_id.get(),
            user.id.get(),
            "🚪 Opuścił serwer".into(),
            None,
        )
        .await;
    }

    /// Presence + aktywności (wymaga GUILD_PRESENCES).
    pub async fn on_presence_update(ctx: &Context, app: &AppContext, p: &Presence) {
        let Some(gid) = p.guild_id else { return; };
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
        let text = format!("🟢 Status: **{}** • aktywność: {}", status, activity);
        Self::log(ctx, &app.db, gid.get(), uid, text, None).await;
    }

    /// Dowolna interakcja użytkownika (slash, przyciski, selecty).
    pub async fn on_any_interaction(ctx: &Context, app: &AppContext, i: &Interaction) {
        if let Some(cmd) = i.clone().command() {
            if let Some(gid) = cmd.guild_id {
                let uid = cmd.user.id.get();
                let opts = summarize_options(&cmd.data.options);
                let text = format!("⌨️ Komenda: **/{} {}**", cmd.data.name, opts);
                Self::log(ctx, &app.db, gid.get(), uid, text, None).await;
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
                let text = format!("⚙️ Interakcja: **{}** (custom_id: `{}`)", kind, comp.data.custom_id);
                Self::log(ctx, &app.db, gid.get(), uid, text, None).await;
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

    pub async fn log_action(
        ctx: &Context,
        db: &Pool<Postgres>,
        guild_id: u64,
        user_id: u64,
        moderator_id: Option<u64>,
        description: &str,
    ) {
        let mut text = description.to_string();
        if let Some(mid) = moderator_id {
            text.push_str(&format!(" • mod: <@{}>", mid));
        }
        Self::log(ctx, db, guild_id, user_id, text, None).await;
    }

    /// Uniwersalny logger – wysyła embed o spójnym wyglądzie.
    async fn log(
        ctx: &Context,
        db: &Pool<Postgres>,
        guild_id: u64,
        user_id: u64,
        text: String,
        attachment: Option<LogAttachment>,
    ) {
        // znajdź kanał z watchlisty
        let row =
            sqlx::query("SELECT channel_id FROM tss.watchlist WHERE guild_id=$1 AND user_id=$2")
                .bind(guild_id as i64)
                .bind(user_id as i64)
                .fetch_optional(db)
                .await;
        let Some(r) = row.ok().flatten() else { return; };
        let ch: i64 = r.get("channel_id");

        // przygotuj dane do ładnego embeda
        let (mut title, mut body) = split_title_and_body(&text);
        let jump = extract_and_strip_jump_url(&mut title); // usuwa " • [link]" z tytułu i zwraca URL

        if title.chars().count() > 256 { title = clamp(&title, 250); }
        body = clamp(&body, 4000);

        let color_hex = embed_color_from_title(&title, &body);
        let color = Color::from_rgb(
            ((color_hex >> 16) & 0xFF) as u8,
            ((color_hex >> 8) & 0xFF) as u8,
            (color_hex & 0xFF) as u8,
        );

        let mut embed = CreateEmbed::new()
            .title(title)
            .description(body)
            .color(color)
            .timestamp(Timestamp::now())
            .field("Użytkownik", format!("<@{}>", user_id), true);

        if let Some(url) = jump.clone() {
            embed = embed.url(url.clone()).field("Link", format!("[Przejdź]({})", url), true);
        }

        if let Ok(user) = UserId::new(user_id).to_user(&ctx.http).await {
            let mut author = serenity::builder::CreateEmbedAuthor::new(user.name.clone());
            if let Some(icon) = user.avatar_url() { author = author.icon_url(icon); }
            embed = embed.author(author);
        }

        let mut msg = CreateMessage::new().embed(embed);

        if let Some(att) = attachment {
            match att {
                LogAttachment::Attachment(att) => {
                    msg = msg.add_embed(CreateEmbed::new().image(att.url.clone()));
                    if let Ok(a) = CreateAttachment::url(&ctx.http, &att.url).await {
                        msg = msg.add_file(a);
                    }
                }
            }
        }

        let _ = ChannelId::new(ch as u64).send_message(&ctx.http, msg).await;
    }

    /// Odpowiedź ephem. z pojedynczym embedem: create_response albo edit_response (po deferze).
    async fn respond_embed_ephemeral(
        ctx: &Context,
        cmd: &CommandInteraction,
        title: &str,
        desc: &str,
        color_hex: u32,
    ) {
        let embed = || {
            CreateEmbed::new()
                .title(title)
                .description(desc)
                .color(Color::from_rgb(
                    ((color_hex >> 16) & 0xFF) as u8,
                    ((color_hex >> 8) & 0xFF) as u8,
                    (color_hex & 0xFF) as u8,
                ))
                .timestamp(Timestamp::now())
        };

        let try_create = cmd
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new().ephemeral(true).add_embed(embed()),
                ),
            )
            .await;

        if try_create.is_err() {
            let _ = cmd
                .edit_response(&ctx.http, EditInteractionResponse::new().embeds(vec![embed()]))
                .await;
        }
    }

    /// Odpowiedź ephem. z wieloma embedami (np. długie listy).
    async fn respond_embeds_ephemeral(
        ctx: &Context,
        cmd: &CommandInteraction,
        embeds: Vec<CreateEmbed>,
    ) {
        let try_create = cmd
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .ephemeral(true)
                        .embeds(embeds.clone()),
                ),
            )
            .await;

        if try_create.is_err() {
            let _ = cmd
                .edit_response(&ctx.http, EditInteractionResponse::new().embeds(embeds))
                .await;
        }
    }

    fn cache_message(msg: &Message) {
        if MESSAGE_CACHE.len() > MESSAGE_CACHE_LIMIT {
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
    s.matches("http://").count() + s.matches("https://").count()
}

fn jump_link(guild_id: u64, channel_id: u64, message_id: u64) -> String {
    format!("https://discord.com/channels/{}/{}/{}", guild_id, channel_id, message_id)
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    let mut parts = Vec::new();
    if h > 0 { parts.push(format!("{}h", h)); }
    if m > 0 { parts.push(format!("{}m", m)); }
    if s > 0 || parts.is_empty() { parts.push(format!("{}s", s)); }
    parts.join(" ")
}

fn summarize_options(options: &[CommandDataOption]) -> String {
    if options.is_empty() { return String::new(); }
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

/// Dzieli tekst na tytuł (pierwsza linia) i treść (reszta).
fn split_title_and_body(s: &str) -> (String, String) {
    let mut lines = s.lines();
    let first = lines.next().unwrap_or_default().to_string();
    let rest = lines.collect::<Vec<_>>().join("\n");
    (first, rest)
}

/// Szuka w tytule fragmentu " • [URL]" i go usuwa, zwracając URL (do osadzenia w emb).
fn extract_and_strip_jump_url(title: &mut String) -> Option<String> {
    if let Some(dotpos) = title.find("• [") {
        if let Some(br_start) = title[dotpos..].find('[') {
            let start = dotpos + br_start + 1;
            if let Some(br_end_rel) = title[start..].find(']') {
                let end = start + br_end_rel;
                let url = title[start..end].to_string();
                // usuń " • [ ... ]"
                let mut remove_start = dotpos;
                if remove_start > 0 && title.as_bytes()[remove_start.saturating_sub(1)] == b' ' {
                    remove_start -= 1;
                }
                title.replace_range(remove_start..(end + 1), "");
                *title = title.trim_end().to_string();
                return Some(url);
            }
        }
    }
    None
}

/// Prosta heurystyka wyboru koloru na podstawie emoji/typu.
fn embed_color_from_title(title: &str, body: &str) -> u32 {
    let t = title;
    if t.starts_with('🎙') { return 0xF39C12; }  // voice
    if t.starts_with('📷') { return 0x9B59B6; }  // camera
    if t.starts_with('📺') { return 0xE74C3C; }  // stream
    if t.starts_with('💬') { return 0x3498DB; }  // message
    if t.starts_with('✏')  { return 0xF1C40F; }  // edit
    if t.starts_with('🗑')  { return 0x95A5A6; }  // delete
    if t.starts_with('➕')  { return 0x2ECC71; }  // reaction add
    if t.starts_with('➖')  { return 0xE67E22; }  // reaction remove
    if t.starts_with('🛡')  { return 0x34495E; }  // roles
    if t.starts_with('👋')  { return 0x2ECC71; }  // member join
    if t.starts_with('🚪')  { return 0xE74C3C; }  // member leave
    if t.starts_with('🟢')  { return 0x2ECC71; }  // presence
    if t.starts_with('⚙')  { return 0x1ABC9C; }  // component
    if t.starts_with('⌨')  { return 0x2980B9; }  // slash
    let lower = format!("{} {}", title.to_lowercase(), body.to_lowercase());
    if lower.contains("transmisję") || lower.contains("transmisje") || lower.contains("stream") { return 0xE74C3C; }
    if lower.contains("kamera") || lower.contains("video") { return 0x9B59B6; }
    if lower.contains("reakcj") { return 0x27AE60; }
    if lower.contains("wiadomość") { return 0x3498DB; }
    if lower.contains("edycj") { return 0xF1C40F; }
    if lower.contains("usun") || lower.contains("wyj") { return 0x95A5A6; }
    COLOR_DEFAULT
}

/// Parsuje ID użytkownika z ciągu znaków.
/// Akceptuje:
/// - czyste cyfry: "1234567890"
/// - wzmianki: "<@1234567890>" lub "<@!1234567890>"
fn parse_user_id_str(input: &str) -> Option<u64> {
    let s = input.trim();
    let digits = if s.starts_with("<@") && s.ends_with('>') {
        let inside = &s[2..s.len()-1];
        let inside = inside.strip_prefix('!').unwrap_or(inside);
        inside
    } else {
        s
    };
    if digits.chars().all(|c| c.is_ascii_digit()) {
        digits.parse::<u64>().ok()
    } else {
        None
    }
}
