//! Statystyki w nazwach kanałów (Data / Populacja / Online / Ostatnio dołączył).
//!
//! Użycie:
//! - w `lib.rs`: `pub mod stats_channels;`
//! - w `discord/mod.rs` (eventy):
//!     - READY/GUILD_CREATE: `StatsChannels::sync_on_ready(&ctx, &self.app, guild_id).await;`
//!                           `StatsChannels::spawn_tasks(ctx, self.app.clone(), guild_id);`
//!     - GUILD_MEMBER_ADD:   `StatsChannels::handle_member_join(&ctx, &self.app, &member).await;`
//!     - GUILD_MEMBER_REMOVE:`StatsChannels::handle_member_remove(&ctx, &self.app, guild_id.get()).await;`
//!
//! ID kanałów są brane z `registry::env_channels` (PROD/DEV switch).

use std::{sync::Arc, time::Duration};

use chrono::Local;
use serenity::all::*;
use serenity::builder::EditChannel;

use crate::{registry::env_channels, AppContext};

pub struct StatsChannels;

impl StatsChannels {
    /* ----------------- API „wysokiego poziomu” dla eventów ----------------- */

    /// Wywołaj przy starcie (READY/GUILD_CREATE) – ustawi datę i liczniki.
    pub async fn sync_on_ready(ctx: &Context, app: &AppContext, guild_id: u64) {
        let _ = Self::update_date(ctx, app).await;
        let _ = Self::update_counts(ctx, app, guild_id).await;
    }

    /// Wywołaj na GUILD_MEMBER_ADD – liczniki + ostatnio dołączył.
    pub async fn handle_member_join(ctx: &Context, app: &AppContext, member: &Member) {
        let gid = member.guild_id.get();
        if let Err(e) = Self::update_counts(ctx, app, gid).await {
            tracing::warn!(guild_id = gid, error = ?e, "stats: failed to update counts");
        }
    }

    /// Ustaw „🔥 {nick}” na kanale „Ostatnio dołączył/a”.
    pub async fn update_last_joined(
        ctx: &Context,
        app: &AppContext,
        member: &Member,
    ) -> serenity::Result<()> {
        let display = member.display_name().to_owned();
        let new_name = format!("🔥 {}", trim_for_channel_name(&display));

        if let Some(ch) = find_last_joined_channel(ctx, app, member.guild_id.get()).await {
            rename_channel(ctx, ch, new_name.clone()).await?;
            tracing::info!(
                guild_id = member.guild_id.get(),
                ch_id = ch.get(),
                user_id = member.user.id.get(),
                new_name,
                "stats: last_joined updated",
            );
        } else {
            tracing::warn!(
                guild_id = member.guild_id.get(),
                "stats: last_joined channel not found (ID invalid and fallback search failed)",
            );
        }
        Ok(())
    }

    /// Wywołaj na GUILD_MEMBER_REMOVE – zaktualizuj liczniki.
    pub async fn handle_member_remove(ctx: &Context, app: &AppContext, guild_id: u64) {
        if let Err(e) = Self::update_counts(ctx, app, guild_id).await {
            tracing::warn!(guild_id, error = ?e, "stats: failed to update counts");
        }
    }

    /// Uruchom w tle dwie pętle:
    ///  - po północy aktualizuje „Data” (i dla pewności liczniki),
    ///  - co kilka minut odświeża liczniki (gdy zdarzenia przepadną).
    pub fn spawn_tasks(ctx: Context, app: Arc<AppContext>, guild_id: u64) {
        // 1) codziennie po północy (lokalnej) – Data + liczniki
        let ctx1 = ctx.clone();
        let app1 = app.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = Self::update_date(&ctx1, &app1).await {
                    tracing::warn!(guild_id, error = ?e, "stats: failed to update date");
                }
                if let Err(e) = Self::update_counts(&ctx1, &app1, guild_id).await {
                    tracing::warn!(guild_id, error = ?e, "stats: failed to update counts");
                }

                // śpij do jutrzejszej 00:00:05
                let now = Local::now();
                let target = (now + chrono::Duration::days(1))
                    .date_naive()
                    .and_hms_opt(0, 0, 5)
                    .unwrap();
                let dur = (target - now.naive_local())
                    .to_std()
                    .unwrap_or(Duration::from_secs(3600));
                tokio::time::sleep(dur).await;
            }
        });

        // 2) lekki poller liczników co 10 minut (bez spamu)
        tokio::spawn(async move {
            loop {
                if let Err(e) = Self::update_counts(&ctx, &app, guild_id).await {
                    tracing::warn!(guild_id, error = ?e, "stats: failed to update counts");
                }
                tokio::time::sleep(Duration::from_secs(600)).await; // 10 min
            }
        });
    }

    /* -------------------- Konkretne aktualizacje kanałów ------------------- */

    /// Ustaw „⇒ Data:  DD.MM.YYYY”.
    pub async fn update_date(ctx: &Context, app: &AppContext) -> serenity::Result<()> {
        let env = app.env();
        let ch_id = env_channels::stats_date_id(&env);
        if ch_id == 0 {
            return Ok(());
        }

        let today = Local::now().format("%d.%m.%Y").to_string();

        rename_channel(ctx, ChannelId::new(ch_id), format!("⇒ Data:  {}", today)).await?;
        Ok(())
    }

    /// Ustaw „⇒ Populacja:  X” i „⇒ Online:  Y”.
    ///
    /// Najpierw próbujemy z cache (szybkie), jeśli brak – prosimy REST z „with_counts”
    /// o PartialGuild i bierzemy pola approximate_* (jeśli są dostępne).
    pub async fn update_counts(
        ctx: &Context,
        app: &AppContext,
        guild_id: u64,
    ) -> serenity::Result<()> {
        let env = app.env();
        let ch_pop = env_channels::stats_population_id(&env);
        let ch_onl = env_channels::stats_online_id(&env);
        if ch_pop == 0 && ch_onl == 0 {
            return Ok(());
        }

        let mut total: Option<u64> = None;
        let mut online: Option<u64> = None;
        let mut source_total = "none";
        let mut source_online = "none";

        // 1) Cache: policz online wg statusów presences
        if let Some(g) = GuildId::new(guild_id).to_guild_cached(&ctx.cache) {
            total = Some(g.member_count as u64);
            source_total = "cache";

            let pres_online = g
                .presences
                .values()
                .filter(|p| is_status_online(p.status))
                .count() as u64;

            if pres_online > 0 {
                online = Some(pres_online);
                source_online = "cache_presences_status";
            }
        }

        // 2) REST with_counts → approximate_* (jeśli czegoś brakuje w cache)
        if total.is_none() || online.is_none() {
            if let Ok(pg) = ctx.http.get_guild_with_counts(GuildId::new(guild_id)).await {
                if total.is_none() {
                    total = pg.approximate_member_count.map(|x| x as u64);
                    if total.is_some() {
                        source_total = "rest_counts";
                    }
                }
                if online.is_none() {
                    online = pg.approximate_presence_count.map(|x| x as u64);
                    if online.is_some() {
                        source_online = "rest_counts";
                    }
                }
            }
        }

        let total = total.unwrap_or(0);
        let online = online.unwrap_or(0);

        tracing::debug!(
            guild_id,
            total,
            online,
            source_total,
            source_online,
            "stats: computed counts (online = presence statuses only)",
        );

        if ch_pop != 0 {
            rename_channel(
                ctx,
                ChannelId::new(ch_pop),
                format!("⇒ Populacja:  {}", total),
            )
            .await?;
        }
        if ch_onl != 0 {
            rename_channel(
                ctx,
                ChannelId::new(ch_onl),
                format!("⇒ Online:  {}", online),
            )
            .await?;
        }

        Ok(())
    }
}

/* --------------------------------- Utils ---------------------------------- */

/// Znajdź kanał "ostatnio dołączył": najpierw ID z rejestru, potem fallback po nazwie i typie.
async fn find_last_joined_channel(
    ctx: &Context,
    app: &AppContext,
    guild_id: u64,
) -> Option<ChannelId> {
    // A) Pobierz mapę kanałów gildii (jedno zapytanie)
    if let Ok(map) = GuildId::new(guild_id).channels(&ctx.http).await {
        // Spróbuj ID z rejestru
        let env = app.env();
        let wanted = env_channels::stats_last_joined_id(&env);
        if wanted != 0 {
            if let Some(gc) = map.get(&ChannelId::new(wanted)) {
                if matches!(gc.kind, ChannelType::Voice | ChannelType::Stage) {
                    return Some(ChannelId::new(wanted));
                }
            }
        }

        // B) Fallback: heurystyka po nazwie + typ kanału
        for (id, gc) in map {
            let name_l = gc.name.to_lowercase();

            let looks_like_last_joined = name_l.starts_with('🔥')
                || name_l.contains("ostatnio dołączy")
                || name_l.contains("ostatnio dolaczy"); // bez ogonków

            let is_voice_like = matches!(gc.kind, ChannelType::Voice | ChannelType::Stage);

            if is_voice_like && looks_like_last_joined {
                return Some(id);
            }
        }
    }

    None
}

/// Zmień nazwę kanału (z logowaniem i zwrotem błędu do wywołującego).
async fn rename_channel(ctx: &Context, ch_id: ChannelId, name: String) -> serenity::Result<()> {
    if let Err(e) = ch_id
        .edit(&ctx.http, EditChannel::new().name(name))
        .await
    {
        tracing::warn!(ch_id = ch_id.get(), error = ?e, "stats: failed to rename channel");
        return Err(e);
    }
    Ok(())
}

/// Discord ogranicza długość nazwy kanału do ~100 znaków.
/// Przytnij z zachowaniem całych znaków (UTF-8) i dodaj „…” jeśli trzeba.
fn trim_for_channel_name(name: &str) -> String {
    const MAX: usize = 90; // zostaw trochę zapasu na prefiks
    if name.chars().count() <= MAX {
        return name.to_string();
    }
    let mut out = String::with_capacity(MAX + 1);
    for (i, ch) in name.chars().enumerate() {
        if i >= MAX {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

#[inline]
fn is_status_online(s: OnlineStatus) -> bool {
    matches!(
        s,
        OnlineStatus::Online | OnlineStatus::Idle | OnlineStatus::DoNotDisturb
    )
}
