use anyhow::Result;
use once_cell::sync::Lazy;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use futures_util::future::join_all;
use reqwest::{redirect, Client};
use serenity::all::{
    ActionRowComponent, ButtonStyle, ChannelId, ComponentInteraction, Context, CreateActionRow,
    CreateAttachment, CreateButton, CreateEmbed, CreateEmbedFooter, CreateInputText,
    InputTextStyle,
    CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage, CreateModal,
    EditInteractionResponse, Message, ModalInteraction, Timestamp, UserId,
};
use sqlx::{Pool, Postgres, Row};
use tracing::{info, warn};
use url::Url;

use crate::admin_points::AdminPoints;
use crate::registry::env_channels;
use crate::AppContext;
use crate::chatguard::{BRAND_FOOTER, is_staff_member_comp, log_violation};

const MAX_ATTACHMENTS: usize = 10; // limit Discorda
const HTTP_TIMEOUT_SECS: u64 = 15;
const MAX_FALLBACK_BYTES: usize = 25 * 1024 * 1024; // 25 MiB (fallback download)

static INIT_DONE: AtomicBool = AtomicBool::new(false);

static HTTP: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .user_agent("TSS-ChatGuard/1.0")
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .redirect(redirect::Policy::limited(3))
        .build()
        .expect("HTTP client")
});

pub async fn maybe_ensure_tables(db: &Pool<Postgres>) {
    if !INIT_DONE.load(Ordering::Relaxed) {
        if let Err(e) = ensure_tables(db).await {
            warn!(error=?e, "Fotosystem.ensure_tables failed");
        } else {
            INIT_DONE.store(true, Ordering::Relaxed);
        }
    }
}

pub async fn on_component(ctx: &Context, app: &AppContext, comp: &ComponentInteraction) -> Result<()> {
    let Some(_gid) = comp.guild_id else { return Ok(()); };
    let env = app.env();

    if !is_staff_member_comp(&env, comp.member.as_ref()) {
        let _ = comp
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("Brak uprawnień.")
                        .ephemeral(true),
                ),
            )
            .await;
        return Ok(());
    }

    let cid = comp.data.custom_id.as_str();
    if !cid.starts_with("cgq:") {
        return Ok(());
    }

    let parts: Vec<&str> = cid.split(':').collect();
    if parts.len() != 3 {
        return Ok(());
    }
    let qid: i64 = parts[1].parse().unwrap_or(0);
    let action = parts[2];

    match action {
        "approve" => approve_flow_button(ctx, app, comp, qid).await?,
        "reject" => {
            let modal = CreateModal::new(format!("cgq:{}:rejmodal", qid), "Odrzuć zdjęcie")
                .components(vec![CreateActionRow::InputText(
                    CreateInputText::new(InputTextStyle::Paragraph, "reason", "Powód (opcjonalnie)")
                        .placeholder("Np. nie na temat kanału / NSFW / niska jakość ...")
                        .required(false),
                )]);
            let _ = comp
                .create_response(&ctx.http, CreateInteractionResponse::Modal(modal))
                .await;
        }
        _ => {}
    }

    Ok(())
}

pub async fn on_modal_submit(ctx: &Context, app: &AppContext, modal: &ModalInteraction) -> Result<()> {
    let cid = modal.data.custom_id.as_str();
    if !(cid.starts_with("cgq:") && cid.ends_with(":rejmodal")) {
        return Ok(());
    }

    let parts: Vec<&str> = cid.split(':').collect();
    if parts.len() != 3 {
        return Ok(());
    }
    let qid: i64 = parts[1].parse().unwrap_or(0);

    let mut reason: Option<String> = None;
    for row in &modal.data.components {
        for comp in &row.components {
            if let ActionRowComponent::InputText(it) = comp {
                let v = it.value.as_deref().unwrap_or("").trim().to_string();
                if !v.is_empty() {
                    reason = Some(v);
                }
            }
        }
    }

    let Some(item) = load_photo_queue(&app.db, qid).await? else {
        let _ = modal
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("Nie znaleziono elementu kolejki.")
                        .ephemeral(true),
                ),
            )
            .await;
        return Ok(());
    };

    if !claim_photo(&app.db, qid, modal.user.id.get() as i64).await? {
        let _ = modal
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("⚠️ Ten element został już rozpatrzony przez innego moderatora.")
                        .ephemeral(true),
                ),
            )
            .await;
        return Ok(());
    }

    let changed = set_status_rejected(&app.db, qid, modal.user.id.get() as i64).await?;
    if !changed {
        let _ = modal
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("⚠️ Ten element został już rozpatrzony.")
                        .ephemeral(true),
                ),
            )
            .await;
        return Ok(());
    }

    if let (Some(vch), Some(vmsg)) = (item.verify_channel_id, item.verify_message_id) {
        let _ = ChannelId::new(vch as u64)
            .delete_message(&ctx.http, vmsg as u64)
            .await;
    }

    let _ = dm_decision(
        ctx,
        item.author_id as u64,
        false,
        item.channel_id as u64,
        modal.user.id.get(),
        reason,
    )
    .await;

    let _ = modal
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("⛔ Odrzucono.")
                    .ephemeral(true),
            ),
        )
        .await;

    tracing::info!(
        queue_id = qid,
        moderator_id = modal.user.id.get(),
        "Photo REJECTED – no points awarded",
    );

    Ok(())
}

async fn approve_flow_button(
    ctx: &Context,
    app: &AppContext,
    comp: &ComponentInteraction,
    qid: i64,
) -> Result<()> {
    let _ = comp
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Zatwierdzono – publikuję…")
                    .ephemeral(true),
            ),
        )
        .await;

    let Some(item) = load_photo_queue(&app.db, qid).await? else {
        let _ = comp
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new().content("Nie znaleziono elementu kolejki."),
            )
            .await;
        return Ok(());
    };

    if !claim_photo(&app.db, qid, comp.user.id.get() as i64).await? {
        let _ = comp
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new()
                    .content("⚠️ Ktoś już rozpatrzył ten element (odśwież)."),
            )
            .await;
        return Ok(());
    }

    let publish_text = build_publish_text(&item);
    let mut msg_builder = CreateMessage::new().content(publish_text);

    let mut added_any = false;
    let urls: Vec<String> = item
        .attachment_urls
        .iter()
        .take(MAX_ATTACHMENTS)
        .cloned()
        .collect();
    let http = ctx.http.clone();
    let tasks = urls.into_iter().map(|url| {
        let http = http.clone();
        async move {
            match CreateAttachment::url(&http, &url).await {
                Ok(att) => Some(att),
                Err(_) => download_to_bytes_named(&url)
                    .await
                    .map(|(bytes, fname)| CreateAttachment::bytes(bytes, fname)),
            }
        }
    });
    let results = join_all(tasks).await;
    for att in results.into_iter().flatten() {
        msg_builder = msg_builder.add_file(att);
        added_any = true;
    }

    if !added_any {
        let _ = release_claim(&app.db, qid, comp.user.id.get() as i64).await;
        let _ = comp
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new()
                    .content("❌ Nie udało się pobrać plików z kolejki (brak załączników)."),
            )
            .await;
        return Ok(());
    }

    let send_res = ChannelId::new(item.channel_id as u64)
        .send_message(&ctx.http, msg_builder)
        .await;

    match send_res {
        Ok(_) => {
            if set_status_approved(&app.db, qid, comp.user.id.get() as i64).await? {
                let deleted = if let (Some(vch), Some(vmsg)) =
                    (item.verify_channel_id, item.verify_message_id)
                {
                    ChannelId::new(vch as u64)
                        .delete_message(&ctx.http, vmsg as u64)
                        .await
                        .is_ok()
                } else {
                    comp.message.delete(&ctx.http).await.is_ok()
                };
                if !deleted {
                    let _ = comp.message.delete(&ctx.http).await;
                }

                let _ = dm_decision(
                    ctx,
                    item.author_id as u64,
                    true,
                    item.channel_id as u64,
                    comp.user.id.get(),
                    None,
                )
                .await;

                match AdminPoints::award_photo_approved(&app.db, comp.user.id.get()).await {
                    Ok(total_after) => {
                        tracing::info!(
                            queue_id = qid,
                            moderator_id = comp.user.id.get(),
                            delta = 0.1f32,
                            total_after = %format!("{:.1}", total_after),
                            "Photo APPROVED – points awarded",
                        );
                        let _ = comp
                            .edit_response(
                                &ctx.http,
                                EditInteractionResponse::new()
                                    .content("✅ Zatwierdzono i opublikowano. (+0.1 pkt)"),
                            )
                            .await;
                    }
                    Err(e) => {
                        warn!(error=?e, "AdminPoints.award_photo_approved failed");
                        let _ = comp
                            .edit_response(
                                &ctx.http,
                                EditInteractionResponse::new().content(
                                    "✅ Zatwierdzono i opublikowano. (nie udało się naliczyć punktów)",
                                ),
                            )
                            .await;
                    }
                }
            } else {
                let _ = comp
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new().content(
                            "⚠️ Element został już rozpatrzony (publikacja mogła zostać zdublowana).",
                        ),
                    )
                    .await;
            }
        }
        Err(e) => {
            let _ = release_claim(&app.db, qid, comp.user.id.get() as i64).await;
            let _ = comp
                .edit_response(
                    &ctx.http,
                    EditInteractionResponse::new()
                        .content(format!("❌ Błąd publikacji: {e}")),
                )
                .await;
        }
    }

    Ok(())
}

pub async fn handle_attachments(ctx: &Context, app: &AppContext, msg: &Message, is_staff: bool) {
    let env = app.env();

    if is_staff {
        return;
    }

    let allowed_media = vec![
        env_channels::fun::clips_id(&env),
        env_channels::fun::photos_id(&env),
        env_channels::fun::memes_id(&env),
        env_channels::fun::show_off_id(&env),
        env_channels::fun::selfie_id(&env),
        env_channels::fun::nsfw_id(&env),
    ];

    let is_media_channel = allowed_media.contains(&msg.channel_id.get());
    let all_images = msg
        .attachments
        .iter()
        .all(|a| a.content_type.as_deref().unwrap_or("").starts_with("image/"));

    if !is_media_channel {
        let _ = msg.delete(&ctx.http).await;
        log_violation(ctx, app, msg, "Pliki/zdjęcia dozwolone tylko w kanałach mediowych").await;
        return;
    }
    if !all_images {
        let _ = msg.delete(&ctx.http).await;
        log_violation(ctx, app, msg, "Tylko obrazy dozwolone w kanałach mediowych").await;
        return;
    }

    let content = if msg.content.trim().is_empty() {
        None
    } else {
        Some(msg.content.clone())
    };
    let qid = insert_photo_queue(
        &app.db,
        msg.guild_id.map(|g| g.get()).unwrap_or(0) as i64,
        msg.channel_id.get() as i64,
        msg.author.id.get() as i64,
        content.clone(),
        &[],
    )
    .await
    .unwrap_or(0);

    let verify_chan = verify_channel_shortcut::photos_id(&env);
    if verify_chan != 0 {
        let title = "🖼️ Nowe zdjęcie do akceptacji";
        let desc = format!(
            "Autor: <@{}>\nKanał docelowy: <#{}>\nLiczba załączników: {}\n\nID kolejki: **{}**",
            msg.author.id.get(),
            msg.channel_id.get(),
            msg.attachments.len().min(MAX_ATTACHMENTS),
            qid
        );

        let embed = CreateEmbed::new()
            .title(title)
            .description(desc)
            .footer(CreateEmbedFooter::new(BRAND_FOOTER));

        let row = CreateActionRow::Buttons(vec![
            CreateButton::new(format!("cgq:{}:approve", qid))
                .label("✅ Approve")
                .style(ButtonStyle::Success),
            CreateButton::new(format!("cgq:{}:reject", qid))
                .label("⛔ Reject")
                .style(ButtonStyle::Danger),
        ]);

        let mut out = CreateMessage::new().embed(embed).components(vec![row]);
        for a in msg.attachments.iter().take(MAX_ATTACHMENTS) {
            if let Ok(att) = CreateAttachment::url(&ctx.http, &a.url).await {
                out = out.add_file(att);
            }
        }

        match ChannelId::new(verify_chan).send_message(&ctx.http, out).await {
            Ok(sent) => {
                let new_urls: Vec<String> =
                    sent.attachments.iter().map(|a| a.url.clone()).collect();
                let _ = update_photo_files_and_verify_ref(
                    &app.db,
                    qid,
                    verify_chan as i64,
                    sent.id.get() as i64,
                    &new_urls,
                )
                .await;

                let _ = msg.delete(&ctx.http).await;

                let _ = dm_pending(
                    ctx,
                    msg.author.id.get(),
                    msg.channel_id.get(),
                    qid,
                    new_urls.len(),
                    content,
                )
                .await;
            }
            Err(e) => {
                warn!(error=?e, "verify publish failed; leaving original message");
                let _ = delete_photo_queue(&app.db, qid).await;
                let _ = msg
                    .reply(
                        &ctx.http,
                        "❌ Nie udało się przekazać do weryfikacji — spróbuj ponownie później.",
                    )
                    .await;
            }
        }
    } else {
        warn!("verify_channel not configured; skipping queue");
    }
}

async fn dm_pending(
    ctx: &Context,
    user_id: u64,
    channel_id: u64,
    qid: i64,
    files: usize,
    content: Option<String>,
) -> Result<()> {
    let embed = CreateEmbed::new()
        .title("🕘 Zdjęcie trafiło do weryfikacji")
        .description(format!(
            "Kanał docelowy: <#{}>\nZałączniki: {}\nID kolejki: **{}**\n\n{}",
            channel_id,
            files,
            qid,
            content.as_deref().unwrap_or(" ")
        ))
        .footer(CreateEmbedFooter::new(BRAND_FOOTER))
        .timestamp(Timestamp::now());

    if let Ok(ch) = UserId::new(user_id).create_dm_channel(&ctx.http).await {
        let _ = ch
            .id
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }
    Ok(())
}

async fn dm_decision(
    ctx: &Context,
    user_id: u64,
    approved: bool,
    channel_id: u64,
    moderator_id: u64,
    reason: Option<String>,
) -> Result<()> {
    let (title, color) = if approved {
        ("✅ Zdjęcie zaakceptowane", 0x2ecc71)
    } else {
        ("⛔ Zdjęcie odrzucone", 0xe74c3c)
    };

    let mut embed = CreateEmbed::new()
        .title(title)
        .description(format!("Kanał: <#{}>", channel_id))
        .field("Moderator", format!("<@{}>", moderator_id), true)
        .footer(CreateEmbedFooter::new(BRAND_FOOTER))
        .timestamp(Timestamp::now())
        .colour(serenity::all::Colour::new(color));

    if let Some(r) = reason {
        if !r.trim().is_empty() {
            embed = embed.field("Powód", r, false);
        }
    }

    if let Ok(ch) = UserId::new(user_id).create_dm_channel(&ctx.http).await {
        let _ = ch
            .id
            .send_message(&ctx.http, CreateMessage::new().embed(embed))
            .await;
    }
    Ok(())
}

fn build_publish_text(item: &PhotoQueueItem) -> String {
    let mut s = format!("Dodane przez <@{}>", item.author_id as u64);
    if let Some(c) = item.content.as_ref() {
        let c = c.trim();
        if !c.is_empty() {
            s.push('\n');
            s.push_str(c);
            if s.len() > 1999 {
                s.truncate(1999);
            }
        }
    }
    s
}

fn host_is_discord_cdn(url: &str) -> bool {
    if let Ok(u) = Url::parse(url) {
        if let Some(host) = u.host_str() {
            let h = host.to_ascii_lowercase();
            return h.ends_with(".discordapp.net")
                || h.ends_with(".discordapp.com")
                || h.ends_with(".discord.com")
                || h == "discordapp.com"
                || h == "discord.com";
        }
    }
    false
}

async fn download_to_bytes(url: &str) -> Option<Vec<u8>> {
    if !host_is_discord_cdn(url) {
        return None;
    }
    let resp = HTTP.get(url).send().await.ok()?;
    if let Some(len) = resp.content_length() {
        if len > MAX_FALLBACK_BYTES as u64 {
            return None;
        }
    }
    let bytes = resp.bytes().await.ok()?;
    if bytes.len() > MAX_FALLBACK_BYTES {
        return None;
    }
    if bytes.is_empty() {
        return None;
    }
    Some(bytes.to_vec())
}

async fn download_to_bytes_named(url: &str) -> Option<(Vec<u8>, String)> {
    let bytes = download_to_bytes(url).await?;
    let name = Url::parse(url)
        .ok()
        .and_then(|u| u.path_segments().and_then(|segments| segments.last().map(|s| s.to_string())))
        .unwrap_or_else(|| "file.bin".to_string());
    Some((bytes, name))
}

#[derive(Debug, Clone)]
struct PhotoQueueItem {
    _id: i64,
    channel_id: i64,
    author_id: i64,
    content: Option<String>,
    attachment_urls: Vec<String>,
    verify_channel_id: Option<i64>,
    verify_message_id: Option<i64>,
}

async fn insert_photo_queue(
    db: &Pool<Postgres>,
    guild_id: i64,
    channel_id: i64,
    author_id: i64,
    content: Option<String>,
    urls: &[String],
) -> Result<i64> {
    let v = json!(urls);
    let row = sqlx::query(
        r#"INSERT INTO tss.photo_queue
            (guild_id, channel_id, author_id, content, attachment_urls, status, created_at)
           VALUES ($1, $2, $3, $4, $5::jsonb, 'PENDING', now())
           RETURNING id"#,
    )
    .bind(guild_id)
    .bind(channel_id)
    .bind(author_id)
    .bind(content)
    .bind(v)
    .fetch_one(db)
    .await?;
    let id: i64 = row.try_get("id")?;
    Ok(id)
}

async fn update_photo_files_and_verify_ref(
    db: &Pool<Postgres>,
    id: i64,
    verify_channel_id: i64,
    verify_message_id: i64,
    urls: &[String],
) -> Result<()> {
    let _ = ensure_tables(db).await;
    let has_verify = has_verify_columns(db).await.unwrap_or(false);
    let v = json!(urls);

    if has_verify {
        let _ = sqlx::query(
            r#"UPDATE tss.photo_queue
               SET attachment_urls = $2::jsonb,
                   verify_channel_id = $3,
                   verify_message_id = $4
               WHERE id = $1"#,
        )
        .bind(id)
        .bind(v)
        .bind(verify_channel_id)
        .bind(verify_message_id)
        .execute(db)
        .await?;
    } else {
        let _ = sqlx::query(
            r#"UPDATE tss.photo_queue
               SET attachment_urls = $2::jsonb
               WHERE id = $1"#,
        )
        .bind(id)
        .bind(v)
        .execute(db)
        .await?;
    }

    Ok(())
}

async fn delete_photo_queue(db: &Pool<Postgres>, id: i64) -> Result<()> {
    let _ = sqlx::query(
        r#"DELETE FROM tss.photo_queue
           WHERE id = $1 AND status = 'PENDING' AND verify_message_id IS NULL"#,
    )
    .bind(id)
    .execute(db)
    .await?;
    Ok(())
}

async fn has_verify_columns(db: &Pool<Postgres>) -> Result<bool> {
    let exists: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS (
          SELECT 1
          FROM information_schema.columns
          WHERE table_schema = 'tss'
            AND table_name   = 'photo_queue'
            AND column_name  = 'verify_channel_id'
        )
        "#,
    )
    .fetch_one(db)
    .await?;
    Ok(exists)
}

async fn load_photo_queue(db: &Pool<Postgres>, id: i64) -> Result<Option<PhotoQueueItem>> {
    let _ = ensure_tables(db).await;
    let has_verify = has_verify_columns(db).await.unwrap_or(false);

    let sql_with = r#"
        SELECT id, channel_id, author_id, content, attachment_urls,
               verify_channel_id, verify_message_id
        FROM tss.photo_queue WHERE id=$1
    "#;

    let sql_without = r#"
        SELECT id, channel_id, author_id, content, attachment_urls,
               NULL::BIGINT AS verify_channel_id,
               NULL::BIGINT AS verify_message_id
        FROM tss.photo_queue WHERE id=$1
    "#;

    let row = sqlx::query(if has_verify { sql_with } else { sql_without })
        .bind(id)
        .fetch_optional(db)
        .await?;

    if let Some(r) = row {
        let id: i64 = r.try_get("id")?;
        let channel_id: i64 = r.try_get("channel_id")?;
        let author_id: i64 = r.try_get("author_id")?;
        let content: Option<String> = r.try_get("content")?;
        let urls_val: serde_json::Value = r.try_get("attachment_urls")?;
        let verify_channel_id: Option<i64> = r.try_get("verify_channel_id").ok();
        let verify_message_id: Option<i64> = r.try_get("verify_message_id").ok();

        let mut urls = Vec::new();
        if let Some(arr) = urls_val.as_array() {
            for v in arr {
                if let Some(s) = v.as_str() {
                    urls.push(s.to_string());
                }
            }
        }

        Ok(Some(PhotoQueueItem {
            _id: id,
            channel_id,
            author_id,
            content,
            attachment_urls: urls,
            verify_channel_id,
            verify_message_id,
        }))
    } else {
        Ok(None)
    }
}

async fn claim_photo(db: &Pool<Postgres>, id: i64, moderator_id: i64) -> Result<bool> {
    let res = sqlx::query(
        r#"UPDATE tss.photo_queue
           SET moderator_id = $2
           WHERE id=$1 AND status='PENDING' AND (moderator_id IS NULL OR moderator_id=$2)"#,
    )
    .bind(id)
    .bind(moderator_id)
    .execute(db)
    .await?;
    Ok(res.rows_affected() > 0)
}

async fn release_claim(db: &Pool<Postgres>, id: i64, moderator_id: i64) -> Result<bool> {
    let res = sqlx::query(
        r#"UPDATE tss.photo_queue
           SET moderator_id = NULL
           WHERE id=$1 AND status='PENDING' AND moderator_id=$2"#,
    )
    .bind(id)
    .bind(moderator_id)
    .execute(db)
    .await?;
    Ok(res.rows_affected() > 0)
}

async fn set_status_approved(db: &Pool<Postgres>, id: i64, moderator_id: i64) -> Result<bool> {
    let res = sqlx::query(
        r#"UPDATE tss.photo_queue
           SET status='APPROVED', decided_at=now(), moderator_id=$2
           WHERE id=$1 AND status='PENDING' AND moderator_id=$2"#,
    )
    .bind(id)
    .bind(moderator_id)
    .execute(db)
    .await?;
    Ok(res.rows_affected() > 0)
}

async fn set_status_rejected(db: &Pool<Postgres>, id: i64, moderator_id: i64) -> Result<bool> {
    let res = sqlx::query(
        r#"UPDATE tss.photo_queue
           SET status='REJECTED', decided_at=now(), moderator_id=$2
           WHERE id=$1 AND status='PENDING' AND moderator_id=$2"#,
    )
    .bind(id)
    .bind(moderator_id)
    .execute(db)
    .await?;
    Ok(res.rows_affected() > 0)
}

async fn ensure_tables(db: &Pool<Postgres>) -> Result<()> {
    let _ = sqlx::query(r#"CREATE SCHEMA IF NOT EXISTS tss"#)
        .execute(db)
        .await?;

    let _ = sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS tss.photo_queue (
            id                BIGSERIAL PRIMARY KEY,
            guild_id          BIGINT NOT NULL,
            channel_id        BIGINT NOT NULL,
            author_id         BIGINT NOT NULL,
            content           TEXT,
            attachment_urls   JSONB NOT NULL DEFAULT '[]',
            status            TEXT  NOT NULL DEFAULT 'PENDING',
            moderator_id      BIGINT,
            verify_channel_id BIGINT,
            verify_message_id BIGINT,
            created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
            decided_at        TIMESTAMPTZ
        )
        "#,
    )
    .execute(db)
    .await?;

    let _ = sqlx::query(
        r#"
        ALTER TABLE tss.photo_queue
            ADD COLUMN IF NOT EXISTS attachment_urls   JSONB NOT NULL DEFAULT '[]',
            ADD COLUMN IF NOT EXISTS status            TEXT  NOT NULL DEFAULT 'PENDING',
            ADD COLUMN IF NOT EXISTS moderator_id      BIGINT,
            ADD COLUMN IF NOT EXISTS verify_channel_id BIGINT,
            ADD COLUMN IF NOT EXISTS verify_message_id BIGINT,
            ADD COLUMN IF NOT EXISTS created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
            ADD COLUMN IF NOT EXISTS decided_at        TIMESTAMPTZ
        "#,
    )
    .execute(db)
    .await?;

    let _ = sqlx::query(
        r#"
        DO $$
        BEGIN
          IF NOT EXISTS (
            SELECT 1 FROM pg_constraint
            WHERE conname = 'chk_photo_queue_status'
              AND conrelid = 'tss.photo_queue'::regclass
          ) THEN
            ALTER TABLE tss.photo_queue
              ADD CONSTRAINT chk_photo_queue_status
              CHECK (status IN ('PENDING','APPROVED','REJECTED'));
          END IF;
        END $$;
        "#,
    )
    .execute(db)
    .await?;

    let _ = sqlx::query(
        r#"CREATE INDEX IF NOT EXISTS idx_photo_queue_status ON tss.photo_queue(status)"#,
    )
    .execute(db)
    .await?;
    let _ = sqlx::query(
        r#"CREATE INDEX IF NOT EXISTS idx_photo_queue_created_at ON tss.photo_queue(created_at)"#,
    )
    .execute(db)
    .await?;
    let _ = sqlx::query(
        r#"CREATE INDEX IF NOT EXISTS idx_photo_queue_status_created ON tss.photo_queue(status, created_at DESC)"#,
    )
    .execute(db)
    .await?;

    info!("Fotosystem: tables ensured / migrated.");
    Ok(())
}

mod verify_channel_shortcut {
    pub fn photos_id(env: &str) -> u64 {
        use crate::registry::channels;
        let prod = env.eq_ignore_ascii_case("production") || env.eq_ignore_ascii_case("prod");
        if prod {
            channels::prod::VERIFY_PHOTOS
        } else {
            channels::dev::VERIFY_PHOTOS
        }
    }
}