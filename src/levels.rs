use anyhow::Result;
use chrono::Utc;
use dashmap::DashMap;
use once_cell::sync::Lazy;
use serenity::all::*;
use sqlx::Row;

use crate::{AppContext, db::Db, registry::env_channels};

static VOICE_START: Lazy<DashMap<(u64, u64), i64>> = Lazy::new(|| DashMap::new());

const TEXT_PER_LEVEL: i64 = 100; // messages per level
const VOICE_SECONDS_PER_LEVEL: i64 = 2 * 3600; // 2h per level => lvl5 after 10h

pub struct Levels;

impl Levels {
    pub async fn ensure_tables(db: &Db) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS tss.user_xp (\n             guild_id BIGINT NOT NULL,\n             user_id BIGINT NOT NULL,\n             text_xp BIGINT NOT NULL DEFAULT 0,\n             voice_seconds BIGINT NOT NULL DEFAULT 0,\n             updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),\n             PRIMARY KEY (guild_id, user_id)\n            )"
        )
        .execute(db)
        .await?;
        Ok(())
    }

    pub async fn on_message(ctx: &Context, app: &AppContext, msg: &Message) {
        let Some(gid) = msg.guild_id else { return; };
        if msg.author.bot { return; }
        let gid = gid.get();
        let uid = msg.author.id.get();

        if let Err(e) = Self::add_text_xp(&app.db, gid, uid, 1, ctx, app).await {
            tracing::warn!(?e, gid, uid, "levels: add_text_xp failed");
        }
    }

    pub async fn on_voice_state_update(ctx: &Context, app: &AppContext, old: Option<VoiceState>, new: &VoiceState) {
        let gid = new.guild_id.or(old.as_ref().and_then(|s| s.guild_id)).map(|g| g.get());
        let Some(gid) = gid else { return; };
        let uid = new.user_id.get();
        let now = Utc::now().timestamp();
        let old_chan = old.as_ref().and_then(|s| s.channel_id.map(|c| c.get()));
        let new_chan = new.channel_id.map(|c| c.get());

        match (old_chan, new_chan) {
            (None, Some(_)) => {
                VOICE_START.insert((gid, uid), now);
            }
            (Some(_), None) => {
                if let Some((_, start)) = VOICE_START.remove(&(gid, uid)) {
                    let delta = now - start;
                    if let Err(e) = Self::add_voice_seconds(&app.db, gid, uid, delta, ctx, app).await {
                        tracing::warn!(?e, gid, uid, "levels: add_voice_seconds failed");
                    }
                }
            }
            (Some(o), Some(n)) if o != n => {
                if let Some((_, start)) = VOICE_START.remove(&(gid, uid)) {
                    let delta = now - start;
                    if let Err(e) = Self::add_voice_seconds(&app.db, gid, uid, delta, ctx, app).await {
                        tracing::warn!(?e, gid, uid, "levels: add_voice_seconds failed");
                    }
                }
                VOICE_START.insert((gid, uid), now);
            }
            _ => {}
        }
    }

    async fn add_text_xp(db: &Db, gid: u64, uid: u64, delta: i64, ctx: &Context, app: &AppContext) -> Result<()> {
        let rec = sqlx::query(
            "INSERT INTO tss.user_xp (guild_id, user_id, text_xp, voice_seconds) VALUES ($1,$2,$3,0)\n             ON CONFLICT (guild_id,user_id) DO UPDATE SET text_xp = tss.user_xp.text_xp + $3, updated_at = now()\n             RETURNING text_xp"
        )
        .bind(gid as i64)
        .bind(uid as i64)
        .bind(delta)
        .fetch_one(db)
        .await?;
        let new_xp: i64 = rec.get(0);
        let old_xp = new_xp - delta;
        let old_level = old_xp / TEXT_PER_LEVEL;
        let new_level = new_xp / TEXT_PER_LEVEL;
        if new_level > old_level {
            Self::announce_level(ctx, app, uid, new_level, true).await;
        }
        Ok(())
    }

    async fn add_voice_seconds(db: &Db, gid: u64, uid: u64, delta: i64, ctx: &Context, app: &AppContext) -> Result<()> {
        if delta <= 0 { return Ok(()); }
        let rec = sqlx::query(
            "INSERT INTO tss.user_xp (guild_id, user_id, text_xp, voice_seconds) VALUES ($1,$2,0,$3)\n             ON CONFLICT (guild_id,user_id) DO UPDATE SET voice_seconds = tss.user_xp.voice_seconds + $3, updated_at = now()\n             RETURNING voice_seconds"
        )
        .bind(gid as i64)
        .bind(uid as i64)
        .bind(delta)
        .fetch_one(db)
        .await?;
        let new_sec: i64 = rec.get(0);
        let old_sec = new_sec - delta;
        let old_level = old_sec / VOICE_SECONDS_PER_LEVEL;
        let new_level = new_sec / VOICE_SECONDS_PER_LEVEL;
        if new_level > old_level {
            Self::announce_level(ctx, app, uid, new_level, false).await;
        }
        Ok(())
    }

    async fn announce_level(ctx: &Context, app: &AppContext, uid: u64, level: i64, is_text: bool) {
        let env = app.env();
        let ch_id = env_channels::chats::levels_id(&env);
        if ch_id == 0 { return; }
        let channel = ChannelId::new(ch_id);
        let kind = if is_text { "tekstowy" } else { "głosowy" };
        let content = format!("<@{}> osiągnął {} poziom {}", uid, level, kind);
        let _ = channel
            .send_message(&ctx.http, CreateMessage::new().content(content))
            .await;
    }
}