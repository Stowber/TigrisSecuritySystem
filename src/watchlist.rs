use anyhow::Result;
use serenity::all::*;
use sqlx::{Pool, Postgres, Row};

use crate::{registry::{env_roles, env_channels}, AppContext};

pub struct Watchlist;

impl Watchlist {
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
                        CreateCommandOption::new(
                            CommandOptionType::User,
                            "user",
                            "Kto?",
                        )
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
                        CreateCommandOption::new(
                            CommandOptionType::User,
                            "user",
                            "Kto?",
                        )
                        .required(true),
                    ),
                )
                .add_option(CreateCommandOption::new(
                    CommandOptionType::SubCommand,
                    "list",
                    "Pokaż listę",
                ))
                .default_member_permissions(Permissions::ADMINISTRATOR),
        )
        .await?;
        Ok(())
    }

    pub async fn on_interaction(ctx: &Context, app: &AppContext, interaction: Interaction) {
        let Some(cmd) = interaction.clone().command() else { return; };
        if cmd.data.name != "watchlist" {
            return;
        }

        let sub = cmd
            .data
            .options
            .first()
            .and_then(|o| match &o.value {
                CommandDataOptionValue::SubCommand(opts) => Some((o.name.clone(), opts.as_slice())),
                _ => None,
            });

        let Some((sub_name, sub_opts)) = sub else { return; };

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

        let existing = sqlx::query(
            "SELECT channel_id FROM tss.watchlist WHERE guild_id=$1 AND user_id=$2",
        )
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
        // resolve user nickname (or username) for channel name
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

        let cat_id = env_channels::watchlist_category_channels_id(&env);
        if cat_id != 0 {
            builder = builder.category(ChannelId::new(cat_id));
        }

        let channel = gid.create_channel(&ctx.http, builder).await?;

        sqlx::query(
            "INSERT INTO tss.watchlist (guild_id,user_id,channel_id) VALUES ($1,$2,$3)",
        )
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
        let rows = sqlx::query(
            "SELECT user_id FROM tss.watchlist WHERE guild_id=$1 ORDER BY added_at",
        )
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

    pub async fn on_message(ctx: &Context, app: &AppContext, msg: &Message) {
        let Some(gid) = msg.guild_id else { return; };
        let uid = msg.author.id.get();
        let text = format!("Wiadomość na <#{}>: {}", msg.channel_id.get(), msg.content);
        Self::log(ctx, &app.db, gid.get(), uid, text).await;
    }

    pub async fn on_voice_state_update(
        ctx: &Context,
        app: &AppContext,
        old: Option<VoiceState>,
        new: &VoiceState,
    ) {
        let gid = match new.guild_id { Some(g) => g.get(), None => return };
        let uid = new.user_id.get();
        let old_id = old.and_then(|o| o.channel_id.map(|c| c.get()));
        let new_id = new.channel_id.map(|c| c.get());
        let msg = match (old_id, new_id) {
            (None, Some(n)) => format!("Dołączył do <#{}>", n),
            (Some(o), None) => format!("Wyszedł z <#{}>", o),
            (Some(o), Some(n)) if o != n => format!("Przeniósł się z <#{}> do <#{}>", o, n),
            _ => return,
        };
        Self::log(ctx, &app.db, gid, uid, msg).await;
    }

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

    fn build_overwrites(env: &str, guild_id: GuildId) -> Vec<PermissionOverwrite> {
        let mut ov = Vec::new();
        ov.push(PermissionOverwrite {
            allow: Permissions::empty(),
            deny: Permissions::VIEW_CHANNEL,
            kind: PermissionOverwriteType::Role(RoleId::new(guild_id.get())),
        });
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
        let row = sqlx::query(
            "SELECT channel_id FROM tss.watchlist WHERE guild_id=$1 AND user_id=$2",
        )
        .bind(guild_id as i64)
        .bind(user_id as i64)
        .fetch_optional(db)
        .await;
        let Some(r) = row.ok().flatten() else { return; };
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
}
