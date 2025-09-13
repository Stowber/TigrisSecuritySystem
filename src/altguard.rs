//! src/altguard.rs
//! AltGuard – zaawansowany system szacowania ryzyka multikont (alts) w jednym pliku.
//!
//! Zawiera:
//! - Scoring (wiek konta, burst join 60s/10m, invite affinity, podobieństwo nazw,
//!   historia banów, *avatar aHash 8×8*, *BehaviorPattern* pierwszych wiadomości)
//! - Cache okien czasowych, join_times, buffory wiadomości
//! - Whitelist (mem + DB), zapis wyników do DB (best-effort)
//! - API: record_join, record_message, score_user, whitelist_{add,remove}, is_whitelisted, warmup_cache, push_punished_*
//!
//! Wymagane tabele (best-effort; jeśli ich nie ma, logujemy i działamy dalej):
//!   tss.alt_scores(guild_id BIGINT, user_id BIGINT, score INT, verdict TEXT, top_signals JSONB, created_at TIMESTAMPTZ)
//!   tss.alt_whitelist(guild_id BIGINT, user_id BIGINT, note TEXT, added_by BIGINT, created_at TIMESTAMPTZ)
//!   tss.alt_config(guild_id BIGINT PRIMARY KEY, config JSONB)
//!   tss.cases(...)  -- używane do heurystyki historii i (opcjonalnie) wyciągania nazw
//!
//! Uwaga: aHash liczymy z avatara (URL); jeśli brak/wykrzaczy się pobieranie, po prostu pomijamy ten sygnał.

use std::{
    cmp::{max, min},
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use dashmap::DashMap;
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::{Pool, Postgres, Row};
use tokio::sync::Mutex;
use tracing::debug;
use url::Url;
use unicode_normalization::UnicodeNormalization;

use crate::AppContext;

static HTTP_CLIENT: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .timeout(Duration::from_millis(1500))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
        .expect("http client")
});

/* ==============================
   Konfiguracja i typy publiczne
   ============================== */

#[derive(Clone, Debug)]
pub struct BluntCloneHit {
    pub matched_user_id: u64,
    pub avatar_hamming: Option<u32>,
    pub same_name: bool,
    pub same_global: bool,
}

#[derive(Debug, Clone)]
struct VerifiedFP {
    user_id: u64,
    name_norm: String,
    global_norm: String,
    avatar_hash: Option<u64>,
    at: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AltConfig {
    pub enabled: bool,
    pub thresholds: Thresholds,
    pub weights: Weights,
    pub min_signals_for_auto: u8,
    pub raidaware_enabled: bool,
    pub raidaware_join_per_60s: u32,
    pub profile: PolicyProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thresholds {
    pub low: u8,  // < low  => LOW/flag
    pub high: u8, // >= high => HIGH/quarantine
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Weights {
    // klasyczne
    pub account_age_max: i32,
    pub burst_60s_max: i32,
    pub burst_10m_max: i32,
    pub invite_affinity_max: i32,
    pub name_similarity_max: i32,
    pub history_base_max: i32,
    pub trusted_relief: i32,
    // nowe:
    #[serde(alias = "avatar_phash_max")]
    pub avatar_ahash_max: i32,     // aHash z avatara (0..20); alias dla kompatybilności ze starą nazwą
    pub behavior_pattern_max: i32, // wzorzec pierwszych wiadomości (0..15)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PolicyProfile {
    Lenient,
    Balanced,
    Strict,
}

impl Default for AltConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            thresholds: Thresholds { low: 40, high: 70 },
            weights: Weights {
                account_age_max: 20,
                burst_60s_max: 20,
                burst_10m_max: 15,
                invite_affinity_max: 20,
                name_similarity_max: 15,
                history_base_max: 25,
                trusted_relief: 15,
                avatar_ahash_max: 20,
                behavior_pattern_max: 15,
            },
            min_signals_for_auto: 2,
            raidaware_enabled: true,
            raidaware_join_per_60s: 8,
            profile: PolicyProfile::Balanced,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AltVerdict {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum AltSignalKind {
    AccountAge,
    Burst60s,
    Burst10m,
    InviteAffinity,
    NameSimilarity,
    HistoryBase,
    TrustedRelief,
    // Zostawiamy nazwę dla kompatybilności z danymi (to i tak aHash):
    AvatarPHash,
    BehaviorPattern,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AltSignal {
    pub kind: AltSignalKind,
    pub weight: i32,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AltScore {
    pub score: u8,
    pub verdict: AltVerdict,
    pub top_signals: Vec<AltSignal>,
    pub explain: String,
}

#[derive(Debug, Clone)]
pub struct ScoreInput {
    pub guild_id: u64,
    pub user_id: u64,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub global_name: Option<String>,
    pub invite_code: Option<String>,
    pub inviter_id: Option<u64>,
    pub has_trusted_role: bool,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct JoinMeta {
    pub guild_id: u64,
    pub user_id: u64,
    pub invite_code: Option<String>,
    pub inviter_id: Option<u64>,
    pub at: Option<Instant>,
}

/* ==============================
   Stan wewnętrzny AltGuard
   ============================== */

#[derive(Debug, Clone)]
struct PunishedProfile {
    username_norm: String,
    when_instant: Instant,
}

#[derive(Debug)]
struct GuildJoinWindows {
    total_60s: Mutex<VecDeque<Instant>>,
    total_10m: Mutex<VecDeque<Instant>>,
    per_invite_60s: Mutex<HashMap<String, VecDeque<Instant>>>,
    per_invite_10m: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl GuildJoinWindows {
    fn new() -> Self {
        Self {
            total_60s: Mutex::new(VecDeque::with_capacity(128)),
            total_10m: Mutex::new(VecDeque::with_capacity(256)),
            per_invite_60s: Mutex::new(HashMap::new()),
            per_invite_10m: Mutex::new(HashMap::new()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MessageFP {
    pub at: Instant,
    pub has_link: bool,
    pub mentions: u32,
    pub len: usize,
    pub sig: u64, // FNV-1a znormalizowanej treści (krótki podpis)
    pub repeated_special: bool,
    pub entropy: f32,
}

#[derive(Debug)]
pub struct AltGuard {
    ctx: Arc<AppContext>,
    config_cache: DashMap<u64, AltConfig>,
    whitelist_mem: DashMap<(u64, u64), ()>,
    guild_windows: DashMap<u64, Arc<GuildJoinWindows>>,
    join_times: DashMap<(u64, u64), Instant>, // (guild,user) -> kiedy dołączył
    punished_names: DashMap<u64, Arc<Mutex<Vec<PunishedProfile>>>>,
    punished_avatars: DashMap<u64, Arc<Mutex<Vec<(u64 /*aHash*/, Instant)>>>>, // per-guild
    punished_names_global: DashMap<String, Instant>,
    punished_avatars_global: DashMap<u64, Instant>,
    msg_buffers: DashMap<(u64, u64), Arc<Mutex<VecDeque<MessageFP>>>>, // (guild,user)->ostatnie N
    recent_verified: DashMap<u64, Arc<Mutex<Vec<VerifiedFP>>>>,

    // Cache hashy aHash po URL z TTL
    avatar_hash_cache: DashMap<String, (u64, Instant)>,
}

impl AltGuard {
    pub fn new(ctx: Arc<AppContext>) -> Arc<Self> {
        let this = Arc::new(Self {
            ctx,
            config_cache: DashMap::new(),
            whitelist_mem: DashMap::new(),
            guild_windows: DashMap::new(),
            join_times: DashMap::new(),
            punished_names: DashMap::new(),
            punished_avatars: DashMap::new(),
            punished_names_global: DashMap::new(),
            punished_avatars_global: DashMap::new(),
            msg_buffers: DashMap::new(),
            recent_verified: DashMap::new(),
            avatar_hash_cache: DashMap::new(),
        });

        Self::spawn_prune_task(&this);

        this
    }

    fn spawn_prune_task(this: &Arc<Self>) {
        let weak = Arc::downgrade(this);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                if let Some(strong) = weak.upgrade() {
                    strong.prune_expired().await;
                } else {
                    break;
                }
            }
        });
    }

    async fn prune_expired(&self) {
        let now = Instant::now();

        // join_times TTL: 24h
        let ttl_join = Duration::from_secs(24 * 3600);
        self.join_times
            .retain(|_, t| now.duration_since(*t) <= ttl_join);

        // avatar_hash_cache TTL: 24h
        let ttl_cache = Duration::from_secs(24 * 3600);
        self.avatar_hash_cache
            .retain(|_, v| now.duration_since(v.1) <= ttl_cache);

        // recent_verified TTL: 7 dni
        let ttl_verified = Duration::from_secs(7 * 24 * 3600);
        for entry in self.recent_verified.iter() {
            let mut guard = entry.value().lock().await;
            guard.retain(|v| now.duration_since(v.at) <= ttl_verified);
        }

        // punished_* TTL: 14 dni
        let ttl_punished = Duration::from_secs(14 * 24 * 3600);
        for entry in self.punished_names.iter() {
            let mut guard = entry.value().lock().await;
            guard.retain(|p| now.duration_since(p.when_instant) <= ttl_punished);
        }
        for entry in self.punished_avatars.iter() {
            let mut guard = entry.value().lock().await;
            guard.retain(|(_, t)| now.duration_since(*t) <= ttl_punished);
        }
    }

    /// Sprawdza “klona 1:1” względem ostatnio zweryfikowanych i dopisuje bieżącego do indeksu.
    pub async fn blunt_clone_check_and_record(
        self: &Arc<Self>,
        guild_id: u64,
        user_id: u64,
        username: &str,
        global_name: Option<&str>,
        avatar_url: Option<&str>,
    ) -> Option<BluntCloneHit> {
        let name_norm = normalize_name(username);
        let global_norm = global_name.map(normalize_name).unwrap_or_default();

        // Spróbuj policzyć aHash avatara (bez blokowania flow, jak się nie uda -> None)
        let avatar_hash = match avatar_url {
            Some(url) => match self.fetch_and_ahash_cached(url).await {
                Ok(h) => h,
                Err(_) => None,
            },
            None => None,
        };

        // Sprawdź globalny indeks ukaranych
        let mut hit: Option<BluntCloneHit> = None;
        let now = Instant::now();
        let ttl = Duration::from_secs(14 * 24 * 3600);

        let mut to_remove = Vec::new();
        let mut same_name_global = false;
        let mut same_global_global = false;
        for e in self.punished_names_global.iter() {
            if now.duration_since(*e.value()) > ttl {
                to_remove.push(e.key().clone());
                continue;
            }
            if e.key() == &name_norm {
                same_name_global = true;
            }
            if !global_norm.is_empty() && e.key() == &global_norm {
                same_global_global = true;
            }
        }
        for k in to_remove {
            self.punished_names_global.remove(&k);
        }

        let mut to_remove_h = Vec::new();
        let mut avatar_match_global = false;
        if let Some(h) = avatar_hash {
            for e in self.punished_avatars_global.iter() {
                if now.duration_since(*e.value()) > ttl {
                    to_remove_h.push(*e.key());
                    continue;
                }
                if *e.key() == h {
                    avatar_match_global = true;
                }
            }
        }
        for k in to_remove_h {
            self.punished_avatars_global.remove(&k);
        }

        if avatar_match_global && (same_name_global || same_global_global) {
            hit = Some(BluntCloneHit {
                matched_user_id: 0,
                avatar_hamming: Some(0),
                same_name: same_name_global,
                same_global: same_global_global,
            });
        }

        // Pobierz bufor dla gildii
        let list = self
            .recent_verified
            .entry(guild_id)
            .or_insert_with(|| Arc::new(Mutex::new(Vec::with_capacity(1024))))
            .clone();

        let mut guard = list.lock().await;

        // Pruning: max 7 dni i twardy limit 2000 wpisów
        let now = Instant::now();
        let ttl = Duration::from_secs(7 * 24 * 3600);
        guard.retain(|v| now.duration_since(v.at) <= ttl);
        if guard.len() > 2000 {
            let drop_n = guard.len() - 2000;
            guard.drain(0..drop_n);
        }

        // Szukamy “klona 1:1”
        for v in guard.iter().rev() {
            if v.user_id == user_id {
                continue;
            }

            let same_name = !v.name_norm.is_empty() && v.name_norm == name_norm;
            let same_global = !v.global_norm.is_empty()
                && !global_norm.is_empty()
                && v.global_norm == global_norm;

            let mut ham = None;
            if let (Some(h1), Some(h2)) = (avatar_hash, v.avatar_hash) {
                ham = Some((h1 ^ h2).count_ones());
            }

            let obvious = match ham {
                Some(0) => same_name || same_global,
                Some(d) if d <= 2 => same_name && same_global,
                _ => false,
            };

            if obvious {
                hit = Some(BluntCloneHit {
                    matched_user_id: v.user_id,
                    avatar_hamming: ham,
                    same_name,
                    same_global,
                });
                break;
            }
        }

        // Dopisz aktualnego użytkownika do indeksu
        guard.push(VerifiedFP {
            user_id,
            name_norm,
            global_norm,
            avatar_hash,
            at: now,
        });

        hit
    }

    #[inline]
    fn db(&self) -> &Pool<Postgres> {
        &self.ctx.db
    }

    /* --------- Cache warmup --------- */

    pub async fn warmup_cache(self: &Arc<Self>, guild_id: u64) {
        if let Ok(Some(cfg)) = load_config_db(self.db(), guild_id).await {
            self.config_cache.insert(guild_id, cfg);
        }
        if let Ok(ids) = load_whitelist_db(self.db(), guild_id).await {
            for uid in ids {
                self.whitelist_mem.insert((guild_id, uid), ());
            }
        }
        if let Ok(list) = load_recent_punished_names(self.db(), guild_id, 200).await {
            let vec = list
                .into_iter()
                .map(|name| PunishedProfile {
                    username_norm: normalize_name(&name),
                    when_instant: Instant::now(),
                })
                .collect::<Vec<_>>();
            self.punished_names
                .insert(guild_id, Arc::new(Mutex::new(vec)));
        }
        // avatary: zaczynamy pustą listę; będą wpadać podczas kar przez push_punished_avatar_hash_*
        self.punished_avatars
            .entry(guild_id)
            .or_insert_with(|| Arc::new(Mutex::new(Vec::with_capacity(200))));
    }

    /* --------- JOIN & MESSAGE rekordery --------- */

    pub async fn record_join(&self, meta: JoinMeta) {
        let now = meta.at.unwrap_or_else(Instant::now);
        self.join_times.insert((meta.guild_id, meta.user_id), now);

        let gw = self
            .guild_windows
            .entry(meta.guild_id)
            .or_insert_with(|| Arc::new(GuildJoinWindows::new()))
            .clone();

        {
            let mut w60 = gw.total_60s.lock().await;
            w60.push_back(now);
            prune_older_than(&mut *w60, Duration::from_secs(60), now);
        }
        {
            let mut w10 = gw.total_10m.lock().await;
            w10.push_back(now);
            prune_older_than(&mut *w10, Duration::from_secs(600), now);
        }
        if let Some(code) = meta.invite_code {
            {
                let mut map = gw.per_invite_60s.lock().await;
                let q = map.entry(code.clone()).or_default();
                q.push_back(now);
                prune_older_than(q, Duration::from_secs(60), now);
                // GC pustych kolejek
                map.retain(|_, q| !q.is_empty());
            }
            {
                let mut map = gw.per_invite_10m.lock().await;
                let q = map.entry(code.clone()).or_default();
                q.push_back(now);
                prune_older_than(q, Duration::from_secs(600), now);
                map.retain(|_, q| !q.is_empty());
            }
        }
    }

    /// Wołaj w handlerze MESSAGE_CREATE (tylko w gildiach; DM pomiń).
    pub async fn record_message(&self, guild_id: u64, user_id: u64, content: &str, mentions: u32) {
        let norm = normalize_content_for_sig(content);
        let sig = fnv1a64(norm.as_bytes());
        static LINK_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"https?://[^\s<>()]+"#).unwrap());
        let has_link = LINK_RE.is_match(content);
        let repeated_special = has_repeated_special(content);
        let entropy = shannon_entropy(content);
        let fp = MessageFP {
            at: Instant::now(),
            has_link,
            mentions,
            len: content.len(),
            sig,
            repeated_special,
            entropy,
        };
        let key = (guild_id, user_id);
        let buf = self
            .msg_buffers
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(VecDeque::with_capacity(32))))
            .clone();
        let mut guard = buf.lock().await;
        if guard.len() >= 32 {
            guard.pop_front();
        }
        guard.push_back(fp);
        // tniemy do ostatnich 30 minut
        prune_msgs_older_than(&mut *guard, Duration::from_secs(1800));
    }

    /* --------- Scoring główny --------- */

    pub async fn score_user(&self, input: &ScoreInput) -> Result<AltScore> {
        if self.is_whitelisted(input.guild_id, input.user_id).await {
            return Ok(AltScore {
                score: 0,
                verdict: AltVerdict::Low,
                top_signals: vec![AltSignal {
                    kind: AltSignalKind::TrustedRelief,
                    weight: 0,
                    detail: "Whitelisted user".into(),
                }],
                explain: "Użytkownik na whitelist – pomijamy scoring.".into(),
            });
        }

        let cfg = self
            .config_cache
            .get(&input.guild_id)
            .map(|e| e.clone())
            .unwrap_or_default();

        if !cfg.enabled {
            return Ok(AltScore {
                score: 0,
                verdict: AltVerdict::Low,
                top_signals: vec![],
                explain: "AltGuard disabled in config.".into(),
            });
        }

        let mut signals: Vec<AltSignal> = Vec::with_capacity(10);

        // A) Wiek konta
        if let Some(age) = account_age_hours(input.user_id) {
            let w = weight_account_age(age, cfg.weights.account_age_max);
            if w > 0 {
                signals.push(AltSignal {
                    kind: AltSignalKind::AccountAge,
                    weight: w,
                    detail: format!("age={}h", age),
                });
            }
        }

        // B) Burst join – 60s / 10m + C) Invite affinity
        if let Some(gw) = self.guild_windows.get(&input.guild_id) {
            let (c60, c10) = {
                let c60 = gw.total_60s.lock().await.len() as u32;
                let c10 = gw.total_10m.lock().await.len() as u32;
                (c60, c10)
            };
            let w60 = weight_burst_count(c60, cfg.weights.burst_60s_max);
            let w10 = weight_burst10_count(c10, cfg.weights.burst_10m_max);
            if w60 > 0 {
                signals.push(AltSignal {
                    kind: AltSignalKind::Burst60s,
                    weight: w60,
                    detail: format!("joins60s={}", c60),
                });
            }
            if w10 > 0 {
                signals.push(AltSignal {
                    kind: AltSignalKind::Burst10m,
                    weight: w10,
                    detail: format!("joins10m={}", c10),
                });
            }
            if let Some(code) = &input.invite_code {
                let (i60, i10) = {
                    let i60 = gw
                        .per_invite_60s
                        .lock()
                        .await
                        .get(code)
                        .map(|q| q.len() as u32)
                        .unwrap_or(0);
                    let i10 = gw
                        .per_invite_10m
                        .lock()
                        .await
                        .get(code)
                        .map(|q| q.len() as u32)
                        .unwrap_or(0);
                    (i60, i10)
                };
                let wi = weight_invite_affinity(i60, i10, cfg.weights.invite_affinity_max);
                if wi > 0 {
                    signals.push(AltSignal {
                        kind: AltSignalKind::InviteAffinity,
                        weight: wi,
                        detail: format!("invite={} i60={} i10={}", code, i60, i10),
                    });
                }
            }
        }

        // D) Podobieństwo nazw do niedawno ukaranych (z pruningiem TTL)
        if let Some(sim_w) = self
            .similarity_to_punished(input.guild_id, &collect_names(input))
            .await?
        {
            if sim_w > 0 {
                signals.push(AltSignal {
                    kind: AltSignalKind::NameSimilarity,
                    weight: min(sim_w, cfg.weights.name_similarity_max),
                    detail: "levenshtein≈ niedawno ukarani".into(),
                });
            }
        }

        // G) Historia bazowa: świeże bany w 24h (mały dopalacz)
        if let Ok(recent_bans) = count_recent_bans(self.db(), input.guild_id, 24).await {
            if recent_bans > 0 {
                let w = min(5 + (recent_bans as i32 * 2), cfg.weights.history_base_max);
                signals.push(AltSignal {
                    kind: AltSignalKind::HistoryBase,
                    weight: w,
                    detail: format!("recent_bans_24h={}", recent_bans),
                });
            }
        }

        // H) Trusted relief
        if input.has_trusted_role {
            signals.push(AltSignal {
                kind: AltSignalKind::TrustedRelief,
                weight: -cfg.weights.trusted_relief.abs(),
                detail: "trusted_role=true".into(),
            });
        }

        // *** Avatar aHash – porównanie do avatarów ukaranych (z TTL i cache URL) ***
        if let Some(url) = &input.avatar_url {
            if let Ok(Some(my_hash)) = self.fetch_and_ahash_cached(url).await {
                if let Some(pool) = self.punished_avatars.get(&input.guild_id) {
                    let mut guard = pool.lock().await;
                    // TTL: 14 dni
                    let now = Instant::now();
                    let ttl = Duration::from_secs(14 * 24 * 3600);
                    guard.retain(|(_, t)| now.duration_since(*t) <= ttl);

                    if !guard.is_empty() {
                        let mut best_dist = u32::MAX;
                        for (h, _) in guard.iter() {
                            let d = (my_hash ^ *h).count_ones();
                            if d < best_dist {
                                best_dist = d;
                            }
                        }
                        // mapowanie dystansu na wagę
                        let w = if best_dist <= 10 {
                            cfg.weights.avatar_ahash_max
                        } else if best_dist <= 14 {
                            min(10, cfg.weights.avatar_ahash_max)
                        } else {
                            0
                        };
                        if w > 0 {
                            signals.push(AltSignal {
                                kind: AltSignalKind::AvatarPHash,
                                weight: w,
                                detail: format!("hamming={}", best_dist),
                            });
                        }
                    }
                }
            }
        }

        // *** BehaviorPattern – analiza pierwszych wiadomości po joinie ***
        if let Some(join_at) = self
            .join_times
            .get(&(input.guild_id, input.user_id))
            .map(|e| *e)
        {
            let buf_key = (input.guild_id, input.user_id);
            if let Some(buf) = self.msg_buffers.get(&buf_key) {
                let guard = buf.lock().await;
                // rozpatrujemy wiadomości w pierwszych 30 minutach od joinu
                let first_30m = guard
                    .iter()
                    .filter(|m| m.at.duration_since(join_at) <= Duration::from_secs(1800))
                    .cloned()
                    .collect::<Vec<_>>();
                let w =
                    weight_behavior_pattern(&first_30m, join_at, cfg.weights.behavior_pattern_max);
                if w > 0 {
                    let repeats = first_30m.iter().filter(|m| m.repeated_special).count();
                    let avg_len = if !first_30m.is_empty() {
                        first_30m.iter().map(|m| m.len).sum::<usize>() as f32
                            / first_30m.len() as f32
                    } else {
                        0.0
                    };
                    let avg_ent = if !first_30m.is_empty() {
                        first_30m.iter().map(|m| m.entropy).sum::<f32>() / first_30m.len() as f32
                    } else {
                        0.0
                    };
                    signals.push(AltSignal {
                        kind: AltSignalKind::BehaviorPattern,
                        weight: w,
                        detail: format!(
                             "msgs_30m={} links={} mentions_total={} repeats={} avg_len={:.1} avg_ent={:.2}",
                            first_30m.len(),
                            first_30m.iter().filter(|m| m.has_link).count(),
                            first_30m
                                .iter()
                                .map(|m| m.mentions as usize)
                                .sum::<usize>(),
                            repeats,
                            avg_len,
                            avg_ent,
                        ),
                    });
                }
            }
        }

        // Sumowanie + caps
        let mut total = 0i32;
        for s in &signals {
            total += s.weight;
        }
        total = total.clamp(0, 100);
        let mut score = total as u8;

        // RaidAware progi
        let raidaware = if cfg.raidaware_enabled {
            if let Some(gw) = self.guild_windows.get(&input.guild_id) {
                gw.total_60s.lock().await.len() as u32 >= cfg.raidaware_join_per_60s
            } else {
                false
            }
        } else {
            false
        };
        let (low, high) = if raidaware {
            (
                cfg.thresholds.low.saturating_sub(5),
                cfg.thresholds.high.saturating_sub(10),
            )
        } else {
            (cfg.thresholds.low, cfg.thresholds.high)
        };

        let mut verdict = if score >= high {
            AltVerdict::High
        } else if score >= low {
            AltVerdict::Medium
        } else {
            AltVerdict::Low
        };

        // Explain (top3)
        use std::cmp::Reverse;
        let mut signals_sorted = signals.clone();
        signals_sorted.sort_by_key(|s| Reverse(s.weight));
        let top3 = signals_sorted
            .iter()
            .take(3)
            .map(|s| format!("{:?}({})", s.kind, s.weight))
            .collect::<Vec<_>>()
            .join(", ");
        let explain = if top3.is_empty() {
            "Brak znaczących sygnałów.".to_string()
        } else {
            format!("Top sygnały: {}", top3)
        };

        // Minimum niezależnych dodatnich sygnałów do auto-akcji (bez reliefów)
        let pos_signals = signals
            .iter()
            .filter(|s| s.weight > 0 && !matches!(s.kind, AltSignalKind::TrustedRelief))
            .count() as u8;

        if pos_signals < cfg.min_signals_for_auto && verdict != AltVerdict::Low {
            // zbijamy do poniżej progu low
            score = low.saturating_sub(1);
            verdict = AltVerdict::Low;
        }

        // Persist wynik (best-effort) – po finalnym score/verdict i z top N sygnałów
        let top_for_db: Vec<_> = signals_sorted.iter().cloned().take(8).collect();
        if let Err(e) = persist_score(
            self.db(),
            input.guild_id,
            input.user_id,
            score,
            &top_for_db,
            verdict,
        )
        .await
        {
            debug!(err=?e, "persist_score failed (ok to ignore if table missing)");
        }

        Ok(AltScore {
            score,
            verdict,
            top_signals: top_for_db,
            explain,
        })
    }

    /* --------- Whitelist API --------- */

    pub async fn whitelist_add(
        &self,
        guild_id: u64,
        user_id: u64,
        note: Option<&str>,
        added_by: Option<u64>,
    ) -> Result<bool> {
        let key = (guild_id, user_id);
        let existed = self.whitelist_mem.contains_key(&key);
        if !existed {
            self.whitelist_mem.insert(key, ());
        }
        if let Err(e) = persist_whitelist_add(self.db(), guild_id, user_id, note, added_by).await {
            debug!(err=?e, "persist_whitelist_add failed (ok to ignore)");
        }
        Ok(!existed)
    }

    pub async fn whitelist_remove(&self, guild_id: u64, user_id: u64) -> Result<bool> {
        let key = (guild_id, user_id);
        let existed = self.whitelist_mem.remove(&key).is_some();
        if let Err(e) = persist_whitelist_remove(self.db(), guild_id, user_id).await {
            debug!(err=?e, "persist_whitelist_remove failed (ok to ignore)");
        }
        Ok(existed)
    }

    pub async fn is_whitelisted(&self, guild_id: u64, user_id: u64) -> bool {
        self.whitelist_mem.contains_key(&(guild_id, user_id))
    }

    /* --------- Integracje z karami (push punished) --------- */

    pub async fn push_punished_name(&self, guild_id: u64, username_like: &str) {
        let list = self
            .punished_names
            .entry(guild_id)
            .or_insert_with(|| Arc::new(Mutex::new(Vec::with_capacity(200))))
            .clone();
        let mut guard = list.lock().await;
        // TTL 14 dni
        let now = Instant::now();
        let ttl = Duration::from_secs(14 * 24 * 3600);
        guard.retain(|p| now.duration_since(p.when_instant) <= ttl);

        if guard.len() >= 200 {
            guard.remove(0);
        }
        guard.push(PunishedProfile {
            username_norm: normalize_name(username_like),
            when_instant: now,
        });

        // Globalny indeks
        let uname = normalize_name(username_like);
        let mut to_remove = Vec::new();
        for e in self.punished_names_global.iter() {
            if now.duration_since(*e.value()) > ttl {
                to_remove.push(e.key().clone());
            }
        }
        for k in to_remove {
            self.punished_names_global.remove(&k);
        }
        self.punished_names_global.insert(uname, now);
    }

    /// Dodaj aHash avatara (z surowych bajtów).
     pub async fn push_punished_avatar_hash_from_bytes(
        &self,
        guild_id: u64,
        bytes: &[u8],
    ) -> Result<()> {
        let bytes_vec = bytes.to_vec();
        if let Some(h) = tokio::task::spawn_blocking(move || ahash_from_bytes(&bytes_vec))
            .await
            .map_err(|e| anyhow::Error::new(e))??
        {
            let vec = self
                .punished_avatars
                .entry(guild_id)
                .or_insert_with(|| Arc::new(Mutex::new(Vec::with_capacity(200))))
                .clone();
            let mut guard = vec.lock().await;

            // TTL 14 dni
            let now = Instant::now();
            let ttl = Duration::from_secs(14 * 24 * 3600);
            guard.retain(|(_, t)| now.duration_since(*t) <= ttl);

            if guard.len() >= 200 {
                guard.remove(0);
            }
            guard.push((h, now));

            // Globalny indeks
            let mut to_remove = Vec::new();
            for e in self.punished_avatars_global.iter() {
                if now.duration_since(*e.value()) > ttl {
                    to_remove.push(*e.key());
                }
            }
            for k in to_remove {
                self.punished_avatars_global.remove(&k);
            }
            self.punished_avatars_global.insert(h, now);
        }
        Ok(())
    }

    /// Dodaj aHash avatara (pobierając z URL).
    pub async fn push_punished_avatar_hash_from_url(&self, guild_id: u64, url: &str) -> Result<()> {
        if let Some(h) = self.fetch_and_ahash_cached(url).await? {
            let vec = self
                .punished_avatars
                .entry(guild_id)
                .or_insert_with(|| Arc::new(Mutex::new(Vec::with_capacity(200))))
                .clone();
            let mut guard = vec.lock().await;

            // TTL 14 dni
            let now = Instant::now();
            let ttl = Duration::from_secs(14 * 24 * 3600);
            guard.retain(|(_, t)| now.duration_since(*t) <= ttl);

            if guard.len() >= 200 {
                guard.remove(0);
            }
            guard.push((h, now));

            // Globalny indeks
            let mut to_remove = Vec::new();
            for e in self.punished_avatars_global.iter() {
                if now.duration_since(*e.value()) > ttl {
                    to_remove.push(*e.key());
                }
            }
            for k in to_remove {
                self.punished_avatars_global.remove(&k);
            }
            self.punished_avatars_global.insert(h, now);
        }
        Ok(())
    }

    /* --------- Pomocnicze --------- */

    async fn similarity_to_punished(
        &self,
        guild_id: u64,
        candidates: &[String],
    ) -> Result<Option<i32>> {
        let list = if let Some(v) = self.punished_names.get(&guild_id) {
            v.clone()
        } else {
            return Ok(None);
        };
        let mut guard = list.lock().await;
        // TTL 14 dni
        let now = Instant::now();
        let ttl = Duration::from_secs(14 * 24 * 3600);
        guard.retain(|p| now.duration_since(p.when_instant) <= ttl);

        if guard.is_empty() || candidates.is_empty() {
            return Ok(None);
        }
        let mut best = 0i32;
        for cand in candidates {
            let c = normalize_name(cand);
            for p in guard.iter() {
                let d = levenshtein(&c, &p.username_norm);
                let len = max(c.len(), p.username_norm.len()) as i32;
                if len == 0 {
                    continue;
                }
                let sim = (len - d as i32) * 100 / len; // 0..100%
                let w = match sim {
                    90..=100 => 15,
                    80..=89 => 10,
                    70..=79 => 6,
                    _ => 0,
                };
                if w > best {
                    best = w;
                }
            }

            // Globalny indeks nazw
            let mut to_remove = Vec::new();
            for e in self.punished_names_global.iter() {
                if now.duration_since(*e.value()) > ttl {
                    to_remove.push(e.key().clone());
                    continue;
                }
                let d = levenshtein(&c, e.key());
                let len = max(c.len(), e.key().len()) as i32;
                if len == 0 {
                    continue;
                }
                let sim = (len - d as i32) * 100 / len;
                let w = match sim {
                    90..=100 => 15,
                    80..=89 => 10,
                    70..=79 => 6,
                    _ => 0,
                };
                if w > best {
                    best = w;
                }
            }
            for k in to_remove {
                self.punished_names_global.remove(&k);
            }
        }
        if best > 0 { Ok(Some(best)) } else { Ok(None) }
    }

    /// Zcache’owane i bezpieczne liczenie aHash po URL (tylko Discord CDN), TTL 24h.
    async fn fetch_and_ahash_cached(&self, url: &str) -> Result<Option<u64>> {
        // TTL cache: 24h
        let ttl = Duration::from_secs(24 * 3600);
        if let Some((h, at)) = self.avatar_hash_cache.get(url).map(|v| *v) {
            if Instant::now().duration_since(at) <= ttl {
                return Ok(Some(h));
            }
        }
        // Bezpieczna weryfikacja URL – pozwalamy tylko na Discord CDN
        if !is_trusted_discord_cdn(url) {
            return Ok(None);
        }
        let h = fetch_and_ahash_inner(url).await?;
        if let Some(hv) = h {
            self.avatar_hash_cache
                .insert(url.to_string(), (hv, Instant::now()));
        }
        Ok(h)
    }
}

/* ==============================
   Funkcje wag sygnałów
   ============================== */

fn prune_older_than(q: &mut VecDeque<Instant>, window: Duration, now: Instant) {
    while let Some(&front) = q.front() {
        if now.duration_since(front) > window {
            q.pop_front();
        } else {
            break;
        }
    }
}

fn prune_msgs_older_than(q: &mut VecDeque<MessageFP>, window: Duration) {
    let now = Instant::now();
    while let Some(front) = q.front() {
        if now.duration_since(front.at) > window {
            q.pop_front();
        } else {
            break;
        }
    }
}

fn account_age_hours(user_id: u64) -> Option<i64> {
    const DISCORD_EPOCH: i64 = 1420070400000; // 2015-01-01 UTC in ms
    let ts_ms = (user_id >> 22) as i64 + DISCORD_EPOCH; // poprawka: shift na u64
    let now_ms = now_millis();
    if now_ms <= ts_ms {
        return None;
    }
    let diff_ms = now_ms - ts_ms;
    Some(diff_ms / 1000 / 3600)
}

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    now.as_millis() as i64
}

fn weight_account_age(age_hours: i64, max_w: i32) -> i32 {
    if age_hours < 24 {
        max_w
    } else if age_hours < 24 * 7 {
        min(max_w, 12)
    } else if age_hours < 24 * 30 {
        min(max_w, 6)
    } else {
        0
    }
}

fn weight_burst_count(count_60s: u32, max_w: i32) -> i32 {
    match count_60s {
        0..=3 => 0,
        4..=7 => min(max_w, 8),
        8..=12 => min(max_w, 14),
        _ => max_w,
    }
}

fn weight_burst10_count(count_10m: u32, max_w: i32) -> i32 {
    match count_10m {
        0..=10 => 0,
        11..=20 => min(max_w, 8),
        21..=40 => min(max_w, 12),
        _ => max_w,
    }
}

fn weight_invite_affinity(i60: u32, i10: u32, max_w: i32) -> i32 {
    let mut w = 0;
    if i60 >= 3 {
        w += 10;
    } else if i60 == 2 {
        w += 5;
    }
    if i10 >= 5 {
        w += 10;
    } else if i10 >= 3 {
        w += 5;
    }
    min(max_w, w)
}

/// BehaviorPattern: weź zestaw pierwszych wiadomości i policz punkty.
fn weight_behavior_pattern(msgs: &[MessageFP], join_at: Instant, max_w: i32) -> i32 {
    if msgs.is_empty() {
        return 0;
    }
    let n = msgs.len();
    // 0) opóźnione pierwsze wiadomości (>5 minut po joinie)
    let delay = msgs[0].at.duration_since(join_at);
    let mut w = if delay > Duration::from_secs(300) {
        4
    } else {
        0
    };
    // 1) link w pierwszych 3 wiadomościach
    let link_early = msgs.iter().take(3).any(|m| m.has_link);
    if link_early {
        w += 6;
    }

    // 2) mass-mention (sumarycznie w 5 pierwszych)
    let mentions_total: u32 = msgs.iter().take(5).map(|m| m.mentions).sum();
    if mentions_total >= 5 {
        w += 8;
    } else if mentions_total >= 3 {
        w += 4;
    }

    // 3) powtarzalny podpis (spam powielany)
    let mut sig_counts: HashMap<u64, u32> = HashMap::new();
    for m in msgs.iter().take(6) {
        *sig_counts.entry(m.sig).or_insert(0) += 1;
    }
    if sig_counts.values().any(|&c| c >= 3) {
        w += 5;
    } else if sig_counts.values().any(|&c| c == 2) {
        w += 3;
    }

    // 4) tempo (5+ wiadomości w < 20s łącznie)
    if n >= 5 {
        let first = msgs.first().unwrap().at;
        let last = msgs.iter().take(5).last().unwrap().at;
        if last.duration_since(first) <= Duration::from_secs(20) {
            w += 6;
        }
    }

    // 5) powtarzające się emoji/znaki specjalne
    let repeats = msgs.iter().take(5).filter(|m| m.repeated_special).count();
    if repeats >= 2 {
        w += 5;
    } else if repeats == 1 {
        w += 3;
    }

    // 6) średnia długość i entropia wiadomości
    let first5: Vec<&MessageFP> = msgs.iter().take(5).collect();
    let c = first5.len() as f32;
    if c > 0.0 {
        let avg_len = first5.iter().map(|m| m.len).sum::<usize>() as f32 / c;
        if avg_len <= 5.0 || avg_len >= 200.0 {
            w += 2;
        }
        let avg_ent = first5.iter().map(|m| m.entropy).sum::<f32>() / c;
        if avg_ent < 3.5 {
            w += 4;
        }
    }

    min(max_w, w)
}

/* ==============================
   Narzędzia: nazwy, treść, hashe
   ============================== */

fn normalize_name<S: AsRef<str>>(s: S) -> String {
    let s = s.as_ref().nfkc().collect::<String>().to_lowercase();
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            } else if let Some(mapped) = map_confusable(ch) {
            out.push(mapped);
        } else {
            out.push('?');
        }
    }
    out
}

fn map_confusable(ch: char) -> Option<char> {
    match ch {
        // Cyrillic
        '\u{0430}' => Some('a'), // а
        '\u{0435}' => Some('e'), // е
        '\u{043E}' => Some('o'), // о
        '\u{0440}' => Some('p'), // р
        '\u{0441}' => Some('c'), // с
        '\u{0445}' => Some('x'), // х
        '\u{0443}' => Some('y'), // у
        '\u{0456}' => Some('i'), // і
        '\u{04CF}' => Some('l'), // ӏ
        // Greek
        '\u{03b1}' => Some('a'), // α
        '\u{03b5}' => Some('e'), // ε
        '\u{03bf}' => Some('o'), // ο
        '\u{03c1}' => Some('p'), // ρ
        '\u{03c5}' => Some('y'), // υ
        '\u{03c7}' => Some('x'), // χ
        '\u{03ba}' => Some('k'), // κ
        '\u{03bb}' => Some('l'), // λ
        '\u{03c3}' => Some('s'), // σ
        '\u{03bd}' => Some('v'), // ν
        // Fullwidth digits
        '\u{FF10}'..='\u{FF19}' => {
            let d = (ch as u32 - 0xFF10 + b'0' as u32) as u8 as char;
            Some(d)
        }
        _ => None,
    }
}


fn collect_names(input: &ScoreInput) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(u) = &input.username {
        v.push(u.clone());
    }
    if let Some(d) = &input.display_name {
        v.push(d.clone());
    }
    if let Some(g) = &input.global_name {
        v.push(g.clone());
    }
    v
}

fn has_repeated_special(s: &str) -> bool {
    let mut prev: Option<char> = None;
    let mut count = 0;
    for ch in s.chars() {
        if Some(ch) == prev {
            count += 1;
            if count >= 3 && !ch.is_alphanumeric() && !ch.is_whitespace() {
                return true;
            }
        } else {
            prev = Some(ch);
            count = 0;
        }
    }
    false
}

fn shannon_entropy(s: &str) -> f32 {
    let mut freq: HashMap<char, usize> = HashMap::new();
    let mut len = 0usize;
    for ch in s.chars() {
        len += 1;
        *freq.entry(ch).or_insert(0) += 1;
    }
    if len == 0 {
        return 0.0;
    }
    let len_f = len as f32;
    let mut entropy = 0.0f32;
    for &count in freq.values() {
        let p = count as f32 / len_f;
        entropy -= p * p.log2();
    }
    entropy
}


fn normalize_content_for_sig(s: &str) -> String {
    // prosta normalizacja: lower + wyciskamy spacje i znaki diakrytyczne podstawowe
    let s = s.to_lowercase();
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || c.is_whitespace())
        .collect()
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x00000100000001B3;
    let mut hash = FNV_OFFSET;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn levenshtein(a: &str, b: &str) -> usize {
    let (len_a, len_b) = (a.len(), b.len());
    if len_a == 0 {
        return len_b;
    }
    if len_b == 0 {
        return len_a;
    }
    let mut prev: Vec<usize> = (0..=len_b).collect();
    let mut curr = vec![0usize; len_b + 1];

    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = min(min(curr[j] + 1, prev[j + 1] + 1), prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[len_b]
}

/* ==============================
   Avatar aHash (8×8 z URL)
   ============================== */

const MAX_IMAGE_BYTES: usize = 3 * 1024 * 1024; // 3 MiB
const MAX_IMAGE_DIMENSION: u32 = 4096; // limit obrazków do 4096×4096

fn is_trusted_discord_cdn(url: &str) -> bool {
    if let Ok(u) = Url::parse(url) {
        if u.scheme() != "https" {
            return false;
        }
        if let Some(host) = u.host_str() {
            let host = host.to_ascii_lowercase();
            return host.ends_with(".discordapp.com")
                || host.ends_with(".discordapp.net")
                || host.ends_with(".discord.com")
                || host == "discordapp.com"
                || host == "discord.com";
        }
    }
    false
}

async fn fetch_and_ahash_inner(url: &str) -> Result<Option<u64>> {
    // krótkie pobranie z timeoutem i limitami – jak padnie, wracamy None
    let resp = match HTTP_CLIENT.get(url).send().await {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    if let Some(len) = resp.content_length() {
        if len > MAX_IMAGE_BYTES as u64 {
            return Ok(None);
        }
    }
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    if bytes.len() > MAX_IMAGE_BYTES {
        return Ok(None);
    }
    let bytes_for_hash = bytes.clone();
    let h = tokio::task::spawn_blocking(move || ahash_from_bytes(&bytes_for_hash))
        .await
        .map_err(|e| anyhow::Error::new(e))??;
    Ok(h)
}

fn ahash_from_bytes(bytes: &[u8]) -> Result<Option<u64>> {
    use image::{imageops::FilterType, ImageReader, Limits};
    use std::io::Cursor;

    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);

    // Wstępnie odczytaj nagłówki i odrzuć obrazy o zbyt dużych wymiarach.
    {
        let mut reader = match ImageReader::new(Cursor::new(bytes)).with_guessed_format() {
            Ok(r) => r,
            Err(_) => return Ok(None),
        };
        reader.limits(limits.clone());
        if reader.into_dimensions().is_err() {
            return Ok(None);
        }
    }

    let mut reader = match ImageReader::new(Cursor::new(bytes)).with_guessed_format() {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    reader.limits(limits);
    let img = match reader.decode() {
        Ok(i) => i,
        Err(_) => return Ok(None),
    };
    
    let gray = img.resize_exact(8, 8, FilterType::Triangle).to_luma8();
    let mut sum: u64 = 0;
    let mut px = [0u8; 64];
    for (i, p) in gray.pixels().enumerate() {
        let v = p.0[0] as u64;
        sum += v;
        px[i] = p.0[0];
    }
    let avg = (sum / 64) as u8;
    let mut bits: u64 = 0;
    for (i, &v) in px.iter().enumerate() {
        if v > avg {
            bits |= 1u64 << i;
        }
    }
    Ok(Some(bits))
}

/* ==============================
   DB I/O (best-effort)
   ============================== */

async fn load_config_db(db: &Pool<Postgres>, guild_id: u64) -> Result<Option<AltConfig>> {
    let row = sqlx::query("SELECT config FROM tss.alt_config WHERE guild_id = $1")
        .bind(guild_id as i64)
        .fetch_optional(db)
        .await?;
    if let Some(rec) = row {
        let val: serde_json::Value = rec.try_get("config")?;
        let cfg: AltConfig = serde_json::from_value(val)?;
        Ok(Some(cfg))
    } else {
        Ok(None)
    }
}

async fn load_whitelist_db(db: &Pool<Postgres>, guild_id: u64) -> Result<Vec<u64>> {
    let rows = sqlx::query("SELECT user_id FROM tss.alt_whitelist WHERE guild_id = $1")
        .bind(guild_id as i64)
        .fetch_all(db)
        .await
        .unwrap_or_default();
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let uid: i64 = r.try_get("user_id").unwrap_or_default();
        if uid > 0 {
            out.push(uid as u64);
        }
    }
    Ok(out)
}

async fn persist_whitelist_add(
    db: &Pool<Postgres>,
    guild_id: u64,
    user_id: u64,
    note: Option<&str>,
    added_by: Option<u64>,
) -> Result<()> {
    let q = r#"INSERT INTO tss.alt_whitelist (guild_id, user_id, note, added_by, created_at)
               VALUES ($1, $2, $3, $4, now())
               ON CONFLICT (guild_id, user_id) DO NOTHING"#;
    let _ = sqlx::query(q)
        .bind(guild_id as i64)
        .bind(user_id as i64)
        .bind(note.unwrap_or(""))
        .bind(added_by.map(|x| x as i64))
        .execute(db)
        .await?;
    Ok(())
}

async fn persist_whitelist_remove(db: &Pool<Postgres>, guild_id: u64, user_id: u64) -> Result<()> {
    let _ = sqlx::query("DELETE FROM tss.alt_whitelist WHERE guild_id = $1 AND user_id = $2")
        .bind(guild_id as i64)
        .bind(user_id as i64)
        .execute(db)
        .await?;
    Ok(())
}

async fn persist_score(
    db: &Pool<Postgres>,
    guild_id: u64,
    user_id: u64,
    score: u8,
    signals: &[AltSignal],
    verdict: AltVerdict,
) -> Result<()> {
    let signals_json = serde_json::to_value(signals).unwrap_or(serde_json::json!([]));
    let verdict_str = match verdict {
        AltVerdict::High => "HIGH",
        AltVerdict::Medium => "MEDIUM",
        AltVerdict::Low => "LOW",
    };
    let q1 = r#"INSERT INTO tss.alt_scores (guild_id, user_id, score, verdict, top_signals, created_at)
                VALUES ($1, $2, $3, $4, $5, now())"#;
    let _ = sqlx::query(q1)
        .bind(guild_id as i64)
        .bind(user_id as i64)
        .bind(score as i32)
        .bind(verdict_str)
        .bind(signals_json)
        .execute(db)
        .await?;
    Ok(())
}

async fn count_recent_bans(db: &Pool<Postgres>, guild_id: u64, hours: i64) -> Result<i64> {
    let q = r#"SELECT COUNT(*) AS c
               FROM tss.cases
               WHERE guild_id = $1 AND action = 'BAN' AND created_at >= (now() - ($2::text || ' hours')::interval)"#;
    let row = sqlx::query(q)
        .bind(guild_id as i64)
        .bind(hours.to_string())
        .fetch_one(db)
        .await?;
    let c: i64 = row.try_get("c").unwrap_or(0);
    Ok(c)
}

async fn load_recent_punished_names(
    db: &Pool<Postgres>,
    guild_id: u64,
    limit: i64,
) -> Result<Vec<String>> {
    let q = r#"SELECT reason
               FROM tss.cases
               WHERE guild_id = $1 AND action IN ('BAN','MUTE','TIMEOUT','KICK','WARN')
               ORDER BY created_at DESC
               LIMIT $2"#;
    let rows = sqlx::query(q)
        .bind(guild_id as i64)
        .bind(limit)
        .fetch_all(db)
        .await?;
    let mut names = Vec::new();
    for r in rows {
        if let Ok(Some(reason)) = r.try_get::<Option<String>, _>("reason") {
            if let Some(name) = extract_at_name(&reason) {
                names.push(name.to_string());
            }
        }
    }
    Ok(names)
}

fn extract_at_name(s: &str) -> Option<&str> {
    if let Some(pos) = s.find('@') {
        let rest = &s[pos + 1..];
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        if end > 0 {
            return Some(&rest[..end]);
        }
    }
    None
}

pub async fn test_fetch_and_ahash_inner(url: &str) -> Result<Option<u64>> {
    fetch_and_ahash_inner(url).await
}

pub fn test_is_trusted_discord_cdn(url: &str) -> bool {
    is_trusted_discord_cdn(url)
}

pub const TEST_MAX_IMAGE_BYTES: usize = MAX_IMAGE_BYTES;

pub use MessageFP as TestMessageFP;

pub fn test_weight_behavior_pattern(msgs: &[MessageFP], join_at: Instant, max_w: i32) -> i32 {
    weight_behavior_pattern(msgs, join_at, max_w)
}

impl AltGuard {
    pub async fn test_similarity_to_punished(
        &self,
        guild_id: u64,
        candidates: &[String],
    ) -> Result<Option<i32>> {
        self.similarity_to_punished(guild_id, candidates).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{App as AppCfg, ChatGuardConfig, Database, Discord, Logging, Settings};
    use image::ImageOutputFormat;
    use once_cell::sync::OnceCell;
    use sqlx::postgres::PgPoolOptions;
    use std::io::Cursor;

    #[test]
    fn is_trusted_discord_cdn_rejects_non_https() {
        assert!(is_trusted_discord_cdn("https://cdn.discordapp.com/x.png"));
        assert!(!is_trusted_discord_cdn("http://cdn.discordapp.com/x.png"));
        assert!(!is_trusted_discord_cdn("https://example.com/x.png"));
    }

    #[test]
    fn ahash_from_bytes_rejects_large_images() {
        let img = image::DynamicImage::new_rgb8(MAX_IMAGE_DIMENSION + 1, 10);
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), ImageOutputFormat::Png)
            .unwrap();
        let res = ahash_from_bytes(&buf).unwrap();
        assert!(res.is_none());
    }

    #[tokio::test]
    async fn prune_expired_removes_outdated_entries() {
        let settings = Settings {
            env: "test".into(),
            app: AppCfg { name: "t".into() },
             discord: Discord {
                token: String::new(),
                app_id: None,
                intents: vec![],
            },
            database: Database {
                url: "postgres://localhost/test".into(),
                max_connections: None,
                statement_timeout_ms: None,
            },
            logging: Logging {
                json: None,
                level: None,
            },
            chatguard: ChatGuardConfig {
                racial_slurs: vec![],
            },
            antinuke: Default::default(),
        };
        let db = PgPoolOptions::new()
            .acquire_timeout(Duration::from_secs(1))
            .connect_lazy(&settings.database.url)
            .unwrap();
        let ctx = Arc::new(AppContext {
            settings,
            db,
            altguard: OnceCell::new(),
            idguard: OnceCell::new(),
            antinuke: OnceCell::new(),
            user_roles: Arc::new(std::sync::Mutex::new(HashMap::new())),
        });
        let ag = AltGuard::new(ctx);

        let now = Instant::now();
        // expired entries
        ag.avatar_hash_cache
            .insert("old".into(), (1, now - Duration::from_secs(48 * 3600)));
        ag.join_times
            .insert((1, 1), now - Duration::from_secs(48 * 3600));
        ag.recent_verified.insert(
            1,
            Arc::new(Mutex::new(vec![VerifiedFP {
                user_id: 1,
                name_norm: String::new(),
                global_norm: String::new(),
                avatar_hash: None,
                at: now - Duration::from_secs(8 * 24 * 3600),
            }])),
        );
        ag.punished_names.insert(
            1,
            Arc::new(Mutex::new(vec![PunishedProfile {
                username_norm: "old".into(),
                when_instant: now - Duration::from_secs(15 * 24 * 3600),
            }])),
        );
        ag.punished_avatars.insert(
            1,
            Arc::new(Mutex::new(vec![(
                1,
                now - Duration::from_secs(15 * 24 * 3600),
            )])),
        );

        // fresh entries
        ag.avatar_hash_cache.insert("new".into(), (2, now));
        ag.join_times.insert((1, 2), now);
        ag.recent_verified.insert(
            2,
            Arc::new(Mutex::new(vec![VerifiedFP {
                user_id: 2,
                name_norm: String::new(),
                global_norm: String::new(),
                avatar_hash: None,
                at: now,
            }])),
        );
        ag.punished_names.insert(
            2,
            Arc::new(Mutex::new(vec![PunishedProfile {
                username_norm: "new".into(),
                when_instant: now,
            }])),
        );
        ag.punished_avatars
            .insert(2, Arc::new(Mutex::new(vec![(2, now)])));

        ag.prune_expired().await;

        assert!(ag.avatar_hash_cache.contains_key("new"));
        assert!(!ag.avatar_hash_cache.contains_key("old"));
        assert!(ag.join_times.contains_key(&(1, 2)));
        assert!(!ag.join_times.contains_key(&(1, 1)));

        let list = ag.recent_verified.get(&1).unwrap();
        assert!(list.lock().await.is_empty());
        let list = ag.recent_verified.get(&2).unwrap();
        assert_eq!(list.lock().await.len(), 1);

        let pn = ag.punished_names.get(&1).unwrap();
        assert!(pn.lock().await.is_empty());
        let pn = ag.punished_names.get(&2).unwrap();
        assert_eq!(pn.lock().await.len(), 1);

        let pa = ag.punished_avatars.get(&1).unwrap();
        assert!(pa.lock().await.is_empty());
        let pa = ag.punished_avatars.get(&2).unwrap();
        assert_eq!(pa.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn detects_delayed_spam_message() {
        let settings = Settings {
            env: "test".into(),
            app: AppCfg { name: "t".into() },
            discord: Discord {
                token: String::new(),
                app_id: None,
                intents: vec![],
            },
            database: Database {
                url: "postgres://localhost/test".into(),
                max_connections: None,
                statement_timeout_ms: None,
            },
            logging: Logging {
                json: None,
                level: None,
            },
            chatguard: ChatGuardConfig {
                racial_slurs: vec![],
            },
            antinuke: Default::default(),
        };
        let db = PgPoolOptions::new()
            .acquire_timeout(Duration::from_secs(1))
            .connect_lazy(&settings.database.url)
            .unwrap();
        let ctx = Arc::new(AppContext {
            settings,
            db,
            altguard: OnceCell::new(),
            idguard: OnceCell::new(),
            antinuke: OnceCell::new(),
            user_roles: Arc::new(std::sync::Mutex::new(HashMap::new())),
        });
        let ag = AltGuard::new(ctx);

        let join_time = Instant::now() - Duration::from_secs(6 * 60);
        ag.record_join(JoinMeta {
            guild_id: 1,
            user_id: 1,
            invite_code: None,
            inviter_id: None,
            at: Some(join_time),
        })
        .await;

        ag.record_message(1, 1, "check this https://spam.example", 0)
            .await;

        let input = ScoreInput {
            guild_id: 1,
            user_id: 1,
            username: None,
            display_name: None,
            global_name: None,
            invite_code: None,
            inviter_id: None,
            has_trusted_role: false,
            avatar_url: None,
        };
        let score = ag.score_user(&input).await.unwrap();
        assert!(
            score
                .top_signals
                .iter()
                .any(|s| matches!(s.kind, AltSignalKind::BehaviorPattern) && s.weight > 0)
        );
    }
    #[test]
    fn normalize_name_handles_confusables() {
        // "раураl" uses Cyrillic letters that look like Latin "paypal"
        assert_eq!(normalize_name("раураl"), "paypal");
    }

    #[tokio::test]
    async fn similarity_detects_confusable_names() {
        let settings = Settings {
            env: "test".into(),
            app: AppCfg { name: "t".into() },
            discord: Discord {
                token: String::new(),
                app_id: None,
                intents: vec![],
            },
            database: Database {
                url: "postgres://localhost/test".into(),
                max_connections: None,
                statement_timeout_ms: None,
            },
            logging: Logging {
                json: None,
                level: None,
            },
            chatguard: ChatGuardConfig {
                racial_slurs: vec![],
            },
            antinuke: Default::default(),
        };
        let db = PgPoolOptions::new()
            .connect_lazy(&settings.database.url)
            .unwrap();
        let ctx = Arc::new(AppContext {
            settings,
            db,
            altguard: OnceCell::new(),
            idguard: OnceCell::new(),
            antinuke: OnceCell::new(),
            user_roles: Arc::new(std::sync::Mutex::new(HashMap::new())),
        });
        let ag = AltGuard::new(ctx);

        let guild_id = 42;
        let list = Arc::new(Mutex::new(vec![PunishedProfile {
            username_norm: normalize_name("paypal"),
            when_instant: Instant::now(),
        }]));
        ag.punished_names.insert(guild_id, list);

        let cand = "раураl".to_string();
        let weight = ag.similarity_to_punished(guild_id, &[cand]).await.unwrap();
        assert!(matches!(weight, Some(w) if w > 0));
    }
}