use anyhow::Result;
use once_cell::sync::Lazy;
use regex::Regex;

use serenity::all::{
    ChannelId, Context, CreateEmbed, CreateEmbedFooter, CreateMessage, GuildId, Interaction,
    Member, Message, PartialMember,
};
use tracing::warn;

use crate::admin_points;
use crate::fotosystem;
use crate::registry::{env_channels, env_roles};
use crate::AppContext;

/* =========================================
   Stałe / regexy / słowniki
   ========================================= */

pub(crate) const BRAND_FOOTER: &str = "Tigris Security System™ • ChatGuard";

static RE_LINK: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?ix)\b((https?://|www\.)[^\s<>()]+|discord\.gg/[A-Za-z0-9]+)\b"#).unwrap()
});

static RE_RACIAL: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?i)\bnazi\b").unwrap(),
        Regex::new(r"(?i)\bhitler\b").unwrap(),
        Regex::new(r"(?i)\bheil\b").unwrap(),
        Regex::new(r"(?i)\bkkk\b").unwrap(),
        Regex::new(r"(?i)\bwhite\s*power\b").unwrap(),
        // Uwaga: to nadal szerokie dopasowanie – rozważ doprecyzowanie listy intencji
        Regex::new(r"(?i)\bczarn\w+\b").unwrap(),
    ]
});

static HARD_INSULTS: Lazy<Vec<&'static str>> = Lazy::new(|| {
    vec![
        "zjeb", "cwel", "spierdalaj", "kurwa", "huj", "chuj", "pierdol", "szmata",
        "dziwka", "pedal", "pedał", "ciota",
    ]
});

/* =========================================
   Publiczny interfejs ChatGuard
   ========================================= */

pub struct ChatGuard;
impl ChatGuard {
    /// (Opcjonalnie) rejestracja komend – na razie no-op.
    pub async fn register_commands(_ctx: &Context, _guild_id: GuildId) -> Result<()> {
        Ok(())
    }

    /// Wywoływane z EventHandler::message
    pub async fn on_message(ctx: &Context, app: &crate::AppContext, msg: &Message) {
        // 🔧 upewnij się jednorazowo, że tabele są w aktualnym schemacie
        fotosystem::maybe_ensure_tables(&app.db).await;

        // normalny pipeline moderacji (linki, wulgaryzmy, pliki/obrazy)
        if let Err(e) = moderate_message(ctx, app, msg).await {
            warn!(error=?e, "ChatGuard.on_message failed");
        }

        // Uwaga: brak obsługi komend tekstowych! Wszystko robimy tylko przez slash.
    }

    /// Wywoływane z EventHandler::interaction_create
    pub async fn on_interaction(ctx: &Context, app: &AppContext, interaction: Interaction) {
        // 🔧 jednorazowy DDL
        fotosystem::maybe_ensure_tables(&app.db).await;

        // 1) komponenty (przyciski / selecty)
        if let Some(comp) = interaction.clone().message_component() {
            // a) najpierw AdminScore (select)
            if admin_points::is_points_component(&comp) {
                if let Err(e) = admin_points::handle_points_component(ctx, &app.db, &comp).await {
                    warn!(error=?e, "AdminPoints.handle_points_component failed");
                }
                return;
            }

            // b) FotoSystem (Approve/Reject)
            if let Err(e) = fotosystem::on_component(ctx, app, &comp).await {
                warn!(error=?e, "FotoSystem.on_component failed");
            }
            return;
        }

        // 2) submit modala (powód odrzucenia)
        if let Some(modal) = interaction.modal_submit() {
            if let Err(e) = fotosystem::on_modal_submit(ctx, app, &modal).await {
                warn!(error=?e, "FotoSystem.on_modal_submit failed");
            }
        }
    }
}

/* =========================================
   Pipeline moderacji wiadomości
   ========================================= */

async fn moderate_message(ctx: &Context, app: &AppContext, msg: &Message) -> Result<()> {
    if msg.author.bot {
        return Ok(());
    }

    let env = app.env();
    let is_staff = is_staff_member_msg(&env, msg.member.as_deref());

    if !is_staff && contains_link(&msg.content) {
        let _ = msg.delete(&ctx.http).await;
        log_violation(ctx, app, msg, "Blokada linków (ChatGuard)").await;
        return Ok(());
    }

    if contains_hard_insult(&msg.content) || contains_racial_slur(&msg.content) {
        let _ = msg.delete(&ctx.http).await;
        log_violation(ctx, app, msg, "Obraźliwa/rasistowska treść").await;
        return Ok(());
    }

    if !msg.attachments.is_empty() {
        fotosystem::handle_attachments(ctx, app, msg, is_staff).await;
        return Ok(());
    }

    if !is_staff && message_has_image_embed(msg) {
        let _ = msg.delete(&ctx.http).await;
        log_violation(ctx, app, msg, "Obraz/plik przez embed – zabronione").await;
    }

    Ok(())
}

/* =========================================
   Pomocnicze – detekcja treści
   ========================================= */

fn contains_link(s: &str) -> bool {
    RE_LINK.is_match(s)
}

fn contains_racial_slur(s: &str) -> bool {
    let st = normalize_basic(s);
    RE_RACIAL.iter().any(|re| re.is_match(&st))
}

fn contains_hard_insult(s: &str) -> bool {
    let st = normalize_basic(s);
    let st_nosp = st.replace(|c: char| c.is_whitespace(), "");
    let st_leet = leetspeak_fold(&st_nosp);
    HARD_INSULTS.iter().any(|w| st_leet.contains(w))
}

fn message_has_image_embed(msg: &Message) -> bool {
    // celowane: prawdziwe embed-y z obrazem
    msg.embeds
        .iter()
        .any(|e| e.image.is_some() || e.thumbnail.is_some())
}

fn normalize_basic(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ż' | 'ź' => 'z',
            _ => c,
        })
        .collect()
}
fn leetspeak_fold(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '0' => 'o',
            '1' | '!' => 'i',
            '3' => 'e',
            '4' | '@' => 'a',
            '5' | '$' => 's',
            '7' => 't',
            _ => c,
        })
        .collect()
}

/* =========================================
   Uprawnienia
   ========================================= */

trait HasRoles {
    fn roles(&self) -> &[serenity::all::RoleId];
}
impl HasRoles for Member {
    fn roles(&self) -> &[serenity::all::RoleId] {
        &self.roles
    }
}
impl HasRoles for PartialMember {
    fn roles(&self) -> &[serenity::all::RoleId] {
        &self.roles
    }
}
fn is_staff_member_generic<T: HasRoles>(env: &str, member: Option<&T>) -> bool {
    let staff = env_roles::staff_set(env);
    member
        .map(|m| m.roles().iter().any(|r| staff.contains(&r.get())))
        .unwrap_or(false)
}
pub(crate) fn is_staff_member_msg(env: &str, member: Option<&PartialMember>) -> bool {
    is_staff_member_generic(env, member)
}
pub(crate) fn is_staff_member_comp(env: &str, member: Option<&Member>) -> bool {
    is_staff_member_generic(env, member)
}

/* =========================================
   Logi / embed naruszeń
   ========================================= */

fn clamp(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut out = s[..max.saturating_sub(1)].to_string();
    out.push('…');
    out
}

pub(crate) async fn log_violation(ctx: &Context, app: &AppContext, msg: &Message, reason: &str) {
    let env = app.env();
    let log_ch = env_channels::logs::message_delete_id(&env);
    if log_ch == 0 {
        return;
    }

    let body = if msg.content.is_empty() {
        "—".to_string()
    } else {
        clamp(&msg.content, 3500)
    };

    let embed = CreateEmbed::new()
        .title("ChatGuard: naruszenie")
        .description(format!(
            "Autor: <@{}>\nKanał: <#{}>\nPowód: **{}**\n\nTreść:\n{}",
            msg.author.id.get(),
            msg.channel_id.get(),
            clamp(reason, 256),
            body
        ))
        .footer(CreateEmbedFooter::new(BRAND_FOOTER));

    let _ = ChannelId::new(log_ch)
        .send_message(&ctx.http, CreateMessage::new().embed(embed))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_links() {
        assert!(contains_link("visit http://example.com"));
        assert!(contains_link("join discord.gg/abc123 now"));
        assert!(!contains_link("no links here"));
    }

    #[test]
    fn detects_racial_slurs() {
        assert!(contains_racial_slur("nazi propaganda"));
        assert!(!contains_racial_slur("friendly chat"));
    }

    #[test]
    fn detects_hard_insults() {
        assert!(contains_hard_insult("ty zjeb"));
        assert!(contains_hard_insult("sp!3rdalaj"));
        assert!(!contains_hard_insult("miłego dnia"));
    }
}
