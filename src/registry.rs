//! Centralny rejestr identyfikatorów (role itd.) z obsługą profili PROD/DEV.
//! PROD = stałe z realnego serwera (poniżej w `roles::*`).
//! DEV  = stałe z serwera testowego (poniżej w `dev::*` – UZUPEŁNIONE).
//!
//! `env_roles::*` fallbackuje w DEV na PROD (dla ról).
//! `env_channels::*` w DEV **nie** fallbackuje (zwraca 0, jeśli brak ID).

#![allow(non_upper_case_globals)]
#![allow(dead_code)]

/* =========================
   PROD: role
   ========================= */
pub mod roles {
    pub mod core {
        pub const WLASCICIEL: u64         = 801432291271901186;
        pub const WSPOL_WLASCICIEL: u64   = 1384977244425945219;
        pub const TECHNIK_ZARZAD: u64     = 860220132020977665;
        pub const GUMIS_OD_BOTOW: u64     = 1379156192588206141;
        pub const OPIEKUN: u64            = 1383134326165602496;
        pub const HEAD_ADMIN: u64         = 0;
        pub const ADMIN: u64              = 801453007048146964;
        pub const HEAD_MODERATOR: u64     = 0;
        pub const MODERATOR: u64          = 801452307715063808;
        pub const TEST_MODERATOR: u64     = 1383135968558715021;

        pub const AI: u64                 = 801473642130571325;
        pub const REKRUTER: u64           = 1389633467280523274;
        pub const II_ETAP_REKRUTACJI: u64 = 1389633599925391411;
    }
    pub mod colors {
        pub const SZARY: u64        = 1379155350997176441;
        pub const ZIELONY: u64      = 1379154309673128026;
        pub const CZERWONY: u64     = 1379154635885117642;
        pub const POMARANCZOWY: u64 = 1383440059012878336;
        pub const ROZOWY: u64       = 1379154939330297876;
        pub const ZOLTY: u64        = 1379155164920807425;
        pub const NIEBIESKI: u64    = 1379154782530310304;
        pub const FIOLETOWY: u64    = 1383439543587569765;
    }
    pub mod special {
        pub const TIGRIS_KALWARYJSKI: u64 = 801452347015561257;
        pub const PRZYAJCIEL_SERWERA: u64 = 797377895261011968;
        pub const SERVER_BOOSTER: u64     = 883307624127426611;
        pub const SEROWY_KOTEK: u64       = 1383526633218248724;
        pub const SERWEROWY_SMIESZEK: u64 = 1383526450593923165;
        pub const ZWERYFIKOWANY: u64      = 873482805433233439;
        pub const MEMBER: u64             = 801431595189141574;
        pub const UNFA_18: u64            = 797377812318650418;
    }
    pub mod gender {
        pub const DZIEWCZYNA: u64 = 1379162080346771647;
        pub const MEZCZYZNA: u64  = 1379162186827432008;
        pub const PLEC_INNA: u64  = 1379162508685873284;
    }
    pub mod relationship {
        pub const WOLNY: u64 = 1383436418218590270;
        pub const ZAJETY: u64 = 1383436674972782653;
    }
    pub mod age {
        pub const AGE_13_PLUS: u64 = 1383161689460969553;
        pub const AGE_16_PLUS: u64 = 1383161974585823242;
        pub const AGE_18_PLUS: u64 = 1383162160380645457;
    }
    pub mod region {
        pub const DOLNOSLASKIE: u64         = 1383172874847785082;
        pub const KUJAWSKO_POMORSKIE: u64   = 1383178394421694514;
        pub const LUBELSKIE: u64            = 1383179272541175901;
        pub const LUBUSKIE: u64             = 1383180134416121946;
        pub const LODZKIE: u64              = 1383183857117036626;
        pub const MALOPOLSKIE: u64          = 1383184254628003851;
        pub const MAZOWIECKIE: u64          = 1383184572392411286;
        pub const OPOLSKIE: u64             = 1383185047045279764;
        pub const PODKARPACKIE: u64         = 1383185456006565928;
        pub const POMORSKIE: u64            = 1383185816146415657;
        pub const SLASKIE: u64              = 1383186090181005332;
        pub const SWIETOKRZYSKIE: u64       = 1383186632047591484;
        pub const PODLASKIE: u64            = 1383185605835493396;
        pub const WARMINSSKO_MAZURSKIE: u64 = 1383186817699942532;
        pub const WIELKOPOLSKIE: u64        = 1383187133807984791;
        pub const ZACHODNIOPOMORSKIE: u64   = 1383187270349361302;
        pub const ZAGRANICA: u64            = 1383512077087543398;
    }
    pub mod interests {
        pub const WEDKARSTWO: u64 = 1383435105002983494;
        pub const GAMING: u64     = 1383443984894132234;
        pub const ARTY: u64       = 1383443116098453504;
        pub const KOTY: u64       = 1383169988470509568;
        pub const PIESKI: u64     = 1383170090668916846;
    }
    pub mod levels {
        pub const LVL_100_PLUS_VOICE: u64 = 1383172327805550713;
        pub const LVL_100_PLUS_TEXT:  u64 = 1383172302321221732;
        pub const LVL_75_PLUS_VOICE:  u64 = 1383172280536010762;
        pub const LVL_75_PLUS_TEXT:   u64 = 1383172257073074287;
        pub const LVL_50_PLUS_VOICE:  u64 = 1383172219815067648;
        pub const LVL_50_PLUS_TEXT:   u64 = 1383172193336164465;
        pub const LVL_40_PLUS_VOICE:  u64 = 1383172169550528592;
        pub const LVL_40_PLUS_TEXT:   u64 = 1383172106912792736;
        pub const LVL_30_PLUS_VOICE:  u64 = 1383172081453371402;
        pub const LVL_30_PLUS_TEXT:   u64 = 1383172052621463632;
        pub const LVL_25_PLUS_VOICE:  u64 = 1383172004173189140;
        pub const LVL_25_PLUS_TEXT:   u64 = 1383171990734504057;
        pub const LVL_20_PLUS_VOICE:  u64 = 1383171967737401455;
        pub const LVL_20_PLUS_TEXT:   u64 = 1383171944140243014;
        pub const LVL_15_PLUS_VOICE:  u64 = 1383171899294748752;
        pub const LVL_15_PLUS_TEXT:   u64 = 1383171876209033326;
        pub const LVL_10_PLUS_VOICE:  u64 = 1383171823440629882;
        pub const LVL_10_PLUS_TEXT:   u64 = 1383171767656255631;
        pub const LVL_5_PLUS_VOICE:   u64 = 1383171698530193571;
        pub const LVL_5_PLUS_TEXT:    u64 = 1383171506049126564;
    }
}

/* =========================
   DEV: role
   ========================= */
pub mod dev {
    pub mod core {
        pub const WLASCICIEL: u64         = 1408795533790806064;
        pub const WSPOL_WLASCICIEL: u64   = 1408795533790806063;
        pub const TECHNIK_ZARZAD: u64     = 1408795533790806061;
        pub const GUMIS_OD_BOTOW: u64     = 1408795533790806060;
        pub const OPIEKUN: u64            = 1408795533790806059;
        pub const HEAD_ADMIN: u64         = 1413504366505234513;
        pub const ADMIN: u64              = 1408795533790806057;
        pub const HEAD_MODERATOR: u64     = 1413503524376940636;
        pub const MODERATOR: u64          = 1408795533736149132;
        pub const TEST_MODERATOR: u64     = 1408795533736149131;

        pub const AI: u64                 = 1408795533736149130;
        pub const REKRUTER: u64           = 1408795533736149129;
        pub const II_ETAP_REKRUTACJI: u64 = 1408795533736149128;
    }
    pub mod colors {
        pub const SZARY: u64        = 1408795533736149126;
        pub const ZIELONY: u64      = 1408795533736149125;
        pub const CZERWONY: u64     = 1408795533736149124;
        pub const POMARANCZOWY: u64 = 1408795533736149123;
        pub const ROZOWY: u64       = 1408795533673369629;
        pub const ZOLTY: u64        = 1408795533673369628;
        pub const NIEBIESKI: u64    = 1408795533673369627;
        pub const FIOLETOWY: u64    = 1408795533673369626;
    }
    pub mod special {
        pub const TIGRIS_KALWARYJSKI: u64 = 1408795533673369624;
        pub const PRZYAJCIEL_SERWERA: u64 = 1408795533673369623;
        pub const SERVER_BOOSTER: u64     = 0;
        pub const SEROWY_KOTEK: u64       = 1408795533673369622;
        pub const SERWEROWY_SMIESZEK: u64 = 1408795533673369621;
        pub const ZWERYFIKOWANY: u64      = 1408795533614645338;
        pub const MEMBER: u64             = 1408795533614645337;
        pub const UNFA_18: u64            = 1408795533329563699;
    }
    pub mod gender {
        pub const DZIEWCZYNA: u64 = 1408795533614645335;
        pub const MEZCZYZNA: u64  = 1408795533614645334;
        pub const PLEC_INNA: u64  = 1408795533614645333;
    }
    pub mod relationship {
        pub const WOLNY: u64 = 1408795533614645332;
        pub const ZAJETY: u64 = 1408795533614645331;
    }
    pub mod age {
        pub const AGE_13_PLUS: u64 = 1408795533614645330;
        pub const AGE_16_PLUS: u64 = 1408795533614645329;
        pub const AGE_18_PLUS: u64 = 1408795533564182672;
    }
    pub mod region {
        pub const DOLNOSLASKIE: u64         = 1408795533564182671;
        pub const KUJAWSKO_POMORSKIE: u64   = 1408795533564182670;
        pub const LUBELSKIE: u64            = 1408795533564182669;
        pub const LUBUSKIE: u64             = 1408795533564182668;
        pub const LODZKIE: u64              = 1408795533564182667;
        pub const MALOPOLSKIE: u64          = 1408795533564182666;
        pub const MAZOWIECKIE: u64          = 1408795533564182665;
        pub const OPOLSKIE: u64             = 1408795533564182664;
        pub const PODKARPACKIE: u64         = 1408795533564182663;
        pub const POMORSKIE: u64            = 1408795533522374727;
        pub const SLASKIE: u64              = 1408795533522374726;
        pub const SWIETOKRZYSKIE: u64       = 1408795533522374725;
        pub const PODLASKIE: u64            = 1408795533522374724;
        pub const WARMINSSKO_MAZURSKIE: u64 = 1408795533522374723;
        pub const WIELKOPOLSKIE: u64        = 1408795533522374722;
        pub const ZACHODNIOPOMORSKIE: u64   = 1408795533522374721;
        pub const ZAGRANICA: u64            = 1408795533522374720;
    }
    pub mod interests {
        pub const WEDKARSTWO: u64 = 1408795533522374718;
        pub const GAMING: u64     = 1408795533459456080;
        pub const ARTY: u64       = 1408795533459456079;
        pub const KOTY: u64       = 1408795533459456078;
        pub const PIESKI: u64     = 1408795533459456077;
    }
    pub mod levels {
        pub const LVL_100_PLUS_VOICE: u64 = 1408795533459456075;
        pub const LVL_100_PLUS_TEXT:  u64 = 1408795533459456074;
        pub const LVL_75_PLUS_VOICE:  u64 = 1408795533459456073;
        pub const LVL_75_PLUS_TEXT:   u64 = 1408795533459456072;
        pub const LVL_50_PLUS_VOICE:  u64 = 1408795533459456071;
        pub const LVL_50_PLUS_TEXT:   u64 = 1408795533383827596;
        pub const LVL_40_PLUS_VOICE:  u64 = 1408795533383827595;
        pub const LVL_40_PLUS_TEXT:   u64 = 1408795533383827594;
        pub const LVL_30_PLUS_VOICE:  u64 = 1408795533383827593;
        pub const LVL_30_PLUS_TEXT:   u64 = 1408795533383827592;
        pub const LVL_25_PLUS_VOICE:  u64 = 1408795533383827591;
        pub const LVL_25_PLUS_TEXT:   u64 = 1408795533383827590;
        pub const LVL_20_PLUS_VOICE:  u64 = 1408795533383827589;
        pub const LVL_20_PLUS_TEXT:   u64 = 1408795533383827588;
        pub const LVL_15_PLUS_VOICE:  u64 = 1408795533383827587;
        pub const LVL_15_PLUS_TEXT:   u64 = 1408795533329563707;
        pub const LVL_10_PLUS_VOICE:  u64 = 1408795533329563706;
        pub const LVL_10_PLUS_TEXT:   u64 = 1408795533329563705;
        pub const LVL_5_PLUS_VOICE:   u64 = 1408795533329563704;
        pub const LVL_5_PLUS_TEXT:    u64 = 1408795533329563703;
    }
}

/* =========================
   KANAŁY (PROD/DEV)
   ========================= */
pub mod channels {
    pub mod prod {
        pub const LOGS_NEW_CHANNELS: u64        = 0;
        pub const LOGS_NEW_CHANNELS_PARENT: u64 = 0;
        pub const VERIFY_PHOTOS: u64            = 0;
        pub const WATCHLIST_CATEGORY_CHANNELS: u64 = 0;

        // Statystyki
        pub const STATS_DATE: u64        = 861189742271791134;
        pub const STATS_POPULATION: u64  = 861192199291273216;
        pub const STATS_ONLINE: u64      = 861202890311335957;
        pub const STATS_LAST_JOINED: u64 = 861197400411734016;

        // Strefa logów
        pub const LOGS_BAN_KICK_MUTE: u64     = 1383137645516816554;
        pub const LOGS_COMMANDS: u64          = 861475243059183616;
        pub const LOGS_CHANNEL_EDITS: u64     = 1383137994621583492;
        pub const LOGS_VOICE: u64             = 1383140533899100160;
        pub const LOGS_TIMEOUTS: u64          = 1383139114865393684;

        pub const LOGS_MESSAGE_DELETE: u64    = 1383137734536990882;
        pub const LOGS_JOINS_LEAVES: u64      = 1383138889761427588;
        pub const LOGS_ROLES: u64             = 1383140396783243405;
        pub const LOGS_TICKETS: u64           = 1390821357738135552;
        pub const LOGS_ALTGUARD: u64          = 0; // brak/placeholder
        pub const ADMINS_POINTS: u64          = 0; // brak/placeholder
        pub const LOGS_TECH: u64              = 0; // Dziennik Techniczny

        // Początek (Global)
        pub const GLOBAL_WELCOME: u64         = 861472964042162217;
        pub const GLOBAL_GOODBYE: u64         = 1390799803210006580;
        pub const VERIFY: u64                 = 0;

        // Kontakt (Global)
        pub const CONTACT_CREATE_TICKET: u64  = 1379160396216139876;
        pub const CONTACT_APPEALS: u64        = 1379161454808403998;

        // Oficjalne (Global)
        pub const OFFICIAL_EVENTS: u64        = 1379166978589069485;
        pub const OFFICIAL_CALENDAR: u64      = 807736209769365546;
        pub const CHAT_LEVELS: u64         = 0;

        // Chaty
        pub const CHAT_GENERAL: u64           = 1383146853499146361;
        pub const CHAT_LFP: u64               = 1395040943182315540;
        pub const CHAT_GRIND: u64             = 871645287528161301;
        pub const CHAT_COMMANDS_PUBLIC: u64   = 861475243059183616;
        pub const CHAT_SUGGESTIONS: u64       = 861500573227417620;

        // Rozrywka (Fun)
        pub const FUN_CLIPS: u64              = 1383482698034712617;
        pub const FUN_PHOTOS: u64             = 1383147084475142165;
        pub const FUN_MEMES: u64              = 780913017994346497;
        pub const FUN_SHOW_OFF: u64           = 861477535324700682;
        pub const FUN_SELFIE: u64             = 1383504688883962047;
        pub const FUN_LAST_LETTER: u64        = 1383483833671745667;
        pub const FUN_NSFW: u64               = 861477715441090571;

        // Tematy
        pub const TOPICS_GAMES: u64           = 1383470708444631092;
        pub const TOPICS_TV_SERIES: u64       = 1383505148076363957;
        pub const TOPICS_DRAWING: u64         = 1383471383023063121;
        pub const TOPICS_POLITICS: u64        = 1383471716877074512;
        pub const TOPICS_MUSIC: u64           = 1383472720653586542;

        // === OBSERWOWANE KATEGORIE ===
        pub const WATCH_CAT_1: u64 = 1383137462011953172; // Strefa logów ADM
        pub const WATCH_CAT_2: u64 = 1383139797580779710; // Strefa logów ogólna
        pub const WATCH_CAT_3: u64 = 861465169812783145;  // Początek
        pub const WATCH_CAT_4: u64 = 1379159653878988830; // Kontakt
        pub const WATCH_CAT_5: u64 = 861178045490135040;  // Oficjalne
        pub const WATCH_CAT_6: u64 = 861478355568558101;  // Informacje
        pub const WATCH_CAT_7: u64 = 861474525526622248;  // Main Chat
        pub const WATCH_CAT_8: u64 = 861475720543076362;  // Rozrywka
        pub const WATCH_CAT_9: u64 = 1383470407189008524; // Porozmwiajmy
    }

    pub mod dev {
        // Statystyki
        pub const STATS_DATE: u64        = 1408795534596116685;
        pub const STATS_POPULATION: u64  = 1408795534596116686;
        pub const STATS_ONLINE: u64      = 1408795534596116687;
        pub const STATS_LAST_JOINED: u64 = 1408795534596116689;
        pub const WATCHLIST_CATEGORY_CHANNELS: u64 = 1414193215694831696;

        // Strefa logów
        pub const LOGS_BAN_KICK_MUTE: u64  = 1408795534973468793;
        pub const LOGS_COMMANDS: u64       = 1408795534973468794;
        pub const LOGS_CHANNEL_EDITS: u64  = 1408795534973468795;
        pub const LOGS_VOICE: u64          = 1408795534973468798;
        pub const LOGS_TIMEOUTS: u64       = 1408795534973468796;
        pub const LOGS_MESSAGE_DELETE: u64 = 1408795534973468799;
        pub const LOGS_JOINS_LEAVES: u64   = 1408795534973468800;
        pub const LOGS_ROLES: u64          = 1408795534973468801;
        pub const LOGS_TICKETS: u64        = 1408795534973468802;
        pub const LOGS_ALTGUARD: u64       = 1408924518894010461;
        pub const VERIFY_PHOTOS: u64       = 1409235511607824515;
        pub const ADMINS_POINTS: u64       = 1409828170638561322;
        pub const LOGS_TECH: u64           = 1414296544626217065; // Dziennik Techniczny

        // Początek (Global)
        pub const GLOBAL_WELCOME: u64 = 1408795536265314315;
        pub const GLOBAL_GOODBYE: u64 = 1408795536265314316;
        pub const VERIFY: u64         = 1408905135781969930;
        pub const CHAT_LEVELS: u64    = 1408795536751984696;

        // Kontakt (Global)
        pub const CONTACT_CREATE_TICKET: u64 = 1408795536265314321;
        pub const CONTACT_APPEALS: u64       = 1408795536265314322;

        // Oficjalne (Global)
        pub const OFFICIAL_EVENTS: u64   = 1408795536751984690;
        pub const OFFICIAL_CALENDAR: u64 = 1408795536751984691;

        // Chaty
        pub const CHAT_GENERAL: u64         = 1408795537154642041;
        pub const CHAT_LFP: u64             = 1408795537154642042;
        pub const CHAT_GRIND: u64           = 1408795537154642043;
        pub const CHAT_COMMANDS_PUBLIC: u64 = 1408795537154642044;
        pub const CHAT_SUGGESTIONS: u64     = 1408795537154642045;

        // Rozrywka
        pub const FUN_CLIPS: u64       = 1408795538853466204;
        pub const FUN_PHOTOS: u64      = 1408795538853466205;
        pub const FUN_MEMES: u64       = 1408795538853466206;
        pub const FUN_SHOW_OFF: u64    = 1408795538853466208;
        pub const FUN_SELFIE: u64      = 1408795538853466209;
        pub const FUN_LAST_LETTER: u64 = 1408795538853466210;
        pub const FUN_NSFW: u64        = 1408795538853466211;

        // Tematy
        pub const TOPICS_GAMES: u64     = 1408795538853466213;
        pub const TOPICS_TV_SERIES: u64 = 1408795539247595601;
        pub const TOPICS_DRAWING: u64   = 1408795539247595602;
        pub const TOPICS_POLITICS: u64  = 1408795539247595603;
        pub const TOPICS_MUSIC: u64     = 1408795539247595604;

        // === Nowe kanały (DEV) ===
        pub const LOGS_NEW_CHANNELS: u64        = 1408830491263504465; // #nowe-kanały
        pub const LOGS_NEW_CHANNELS_PARENT: u64 = 1408795534596116690; // Strefa logów ADM

        // === Obserwowane kategorie (DEV) ===
        pub const WATCH_CAT_1: u64 = 1408795534596116690;
        pub const WATCH_CAT_2: u64 = 1408795534973468797;
        pub const WATCH_CAT_3: u64 = 1408795536265314314;
        pub const WATCH_CAT_4: u64 = 1408795536265314320;
        pub const WATCH_CAT_5: u64 = 1408795536265314323;
        pub const WATCH_CAT_6: u64 = 1408795536751984694;
        pub const WATCH_CAT_7: u64 = 1408795537154642040;
        pub const WATCH_CAT_8: u64 = 1408795538484236428;
        pub const WATCH_CAT_9: u64 = 1408795538853466212;
    }
}

/* ==========================================
   ENV switch: role
   ========================================== */
pub mod env_roles {
    use super::{dev, roles};

    #[inline] fn is_prod(env: &str) -> bool {
        env.eq_ignore_ascii_case("production") || env.eq_ignore_ascii_case("prod")
    }
    #[inline] fn pick(env: &str, dev_id: u64, prod_id: u64) -> u64 {
        if is_prod(env) { prod_id } else { if dev_id != 0 { dev_id } else { prod_id } }
    }

    pub fn owner_id(env: &str) -> u64 { pick(env, dev::core::WLASCICIEL, roles::core::WLASCICIEL) }
    pub fn co_owner_id(env: &str) -> u64 { pick(env, dev::core::WSPOL_WLASCICIEL, roles::core::WSPOL_WLASCICIEL) }
    pub fn admin_id(env: &str) -> u64 { pick(env, dev::core::ADMIN, roles::core::ADMIN) }
    pub fn moderator_id(env: &str) -> u64 { pick(env, dev::core::MODERATOR, roles::core::MODERATOR) }
    pub fn test_moderator_id(env: &str) -> u64 { pick(env, dev::core::TEST_MODERATOR, roles::core::TEST_MODERATOR) }
    pub fn opiekun_id(env: &str) -> u64 { pick(env, dev::core::OPIEKUN, roles::core::OPIEKUN) }
    pub fn technik_zarzad_id(env: &str) -> u64 { pick(env, dev::core::TECHNIK_ZARZAD, roles::core::TECHNIK_ZARZAD) }
    pub fn gumis_od_botow_id(env: &str) -> u64 { pick(env, dev::core::GUMIS_OD_BOTOW, roles::core::GUMIS_OD_BOTOW) }
    pub fn verified_id(env: &str) -> u64 { pick(env, dev::special::ZWERYFIKOWANY, roles::special::ZWERYFIKOWANY) }
    pub fn member_id(env: &str) -> u64 { pick(env, dev::special::MEMBER, roles::special::MEMBER) }

    pub fn staff_set(env: &str) -> Vec<u64> {
        vec![
            owner_id(env), co_owner_id(env), technik_zarzad_id(env), gumis_od_botow_id(env),
            opiekun_id(env), admin_id(env), moderator_id(env), test_moderator_id(env),
        ]
    }
    pub fn moderator_set(env: &str) -> Vec<u64> {
        vec![
            admin_id(env), moderator_id(env), test_moderator_id(env), opiekun_id(env),
            technik_zarzad_id(env), gumis_od_botow_id(env), co_owner_id(env), owner_id(env),
        ]
    }
    pub fn color_roles(env: &str) -> Vec<u64> {
        use super::{dev::colors as D, roles::colors as P};
        vec![pick(env, D::SZARY, P::SZARY), pick(env, D::ZIELONY, P::ZIELONY), pick(env, D::CZERWONY, P::CZERWONY),
             pick(env, D::POMARANCZOWY, P::POMARANCZOWY), pick(env, D::ROZOWY, P::ROZOWY), pick(env, D::ZOLTY, P::ZOLTY),
             pick(env, D::NIEBIESKI, P::NIEBIESKI), pick(env, D::FIOLETOWY, P::FIOLETOWY)]
    }
    pub fn age_roles(env: &str) -> Vec<u64> {
        use super::{dev::age as D, roles::age as P};
        vec![pick(env, D::AGE_13_PLUS, P::AGE_13_PLUS), pick(env, D::AGE_16_PLUS, P::AGE_16_PLUS), pick(env, D::AGE_18_PLUS, P::AGE_18_PLUS)]
    }
    pub fn gender_roles(env: &str) -> Vec<u64> {
        use super::{dev::gender as D, roles::gender as P};
        vec![pick(env, D::DZIEWCZYNA, P::DZIEWCZYNA), pick(env, D::MEZCZYZNA, P::MEZCZYZNA), pick(env, D::PLEC_INNA, P::PLEC_INNA)]
    }
    pub fn region_roles(env: &str) -> Vec<u64> {
        use super::{dev::region as D, roles::region as P};
        vec![
            pick(env, D::DOLNOSLASKIE, P::DOLNOSLASKIE), pick(env, D::KUJAWSKO_POMORSKIE, P::KUJAWSKO_POMORSKIE),
            pick(env, D::LUBELSKIE, P::LUBELSKIE), pick(env, D::LUBUSKIE, P::LUBUSKIE), pick(env, D::LODZKIE, P::LODZKIE),
            pick(env, D::MALOPOLSKIE, P::MALOPOLSKIE), pick(env, D::MAZOWIECKIE, P::MAZOWIECKIE),
            pick(env, D::OPOLSKIE, P::OPOLSKIE), pick(env, D::PODKARPACKIE, P::PODKARPACKIE),
            pick(env, D::POMORSKIE, P::POMORSKIE), pick(env, D::SLASKIE, P::SLASKIE),
            pick(env, D::SWIETOKRZYSKIE, P::SWIETOKRZYSKIE), pick(env, D::PODLASKIE, P::PODLASKIE),
            pick(env, D::WARMINSSKO_MAZURSKIE, P::WARMINSSKO_MAZURSKIE), pick(env, D::WIELKOPOLSKIE, P::WIELKOPOLSKIE),
            pick(env, D::ZACHODNIOPOMORSKIE, P::ZACHODNIOPOMORSKIE), pick(env, D::ZAGRANICA, P::ZAGRANICA),
        ]
    }
    pub fn interest_roles(env: &str) -> Vec<u64> {
        use super::{dev::interests as D, roles::interests as P};
        vec![pick(env, D::WEDKARSTWO, P::WEDKARSTWO), pick(env, D::GAMING, P::GAMING),
             pick(env, D::ARTY, P::ARTY), pick(env, D::KOTY, P::KOTY), pick(env, D::PIESKI, P::PIESKI)]
    }
    pub fn level_roles(env: &str) -> Vec<u64> {
        use super::{dev::levels as D, roles::levels as P};
        vec![
            pick(env, D::LVL_5_PLUS_TEXT, P::LVL_5_PLUS_TEXT), pick(env, D::LVL_5_PLUS_VOICE, P::LVL_5_PLUS_VOICE),
            pick(env, D::LVL_10_PLUS_TEXT, P::LVL_10_PLUS_TEXT), pick(env, D::LVL_10_PLUS_VOICE, P::LVL_10_PLUS_VOICE),
            pick(env, D::LVL_15_PLUS_TEXT, P::LVL_15_PLUS_TEXT), pick(env, D::LVL_15_PLUS_VOICE, P::LVL_15_PLUS_VOICE),
            pick(env, D::LVL_20_PLUS_TEXT, P::LVL_20_PLUS_TEXT), pick(env, D::LVL_20_PLUS_VOICE, P::LVL_20_PLUS_VOICE),
            pick(env, D::LVL_25_PLUS_TEXT, P::LVL_25_PLUS_TEXT), pick(env, D::LVL_25_PLUS_VOICE, P::LVL_25_PLUS_VOICE),
            pick(env, D::LVL_30_PLUS_TEXT, P::LVL_30_PLUS_TEXT), pick(env, D::LVL_30_PLUS_VOICE, P::LVL_30_PLUS_VOICE),
            pick(env, D::LVL_40_PLUS_TEXT, P::LVL_40_PLUS_TEXT), pick(env, D::LVL_40_PLUS_VOICE, P::LVL_40_PLUS_VOICE),
            pick(env, D::LVL_50_PLUS_TEXT, P::LVL_50_PLUS_TEXT), pick(env, D::LVL_50_PLUS_VOICE, P::LVL_50_PLUS_VOICE),
            pick(env, D::LVL_75_PLUS_TEXT, P::LVL_75_PLUS_TEXT), pick(env, D::LVL_75_PLUS_VOICE, P::LVL_75_PLUS_VOICE),
            pick(env, D::LVL_100_PLUS_TEXT, P::LVL_100_PLUS_TEXT), pick(env, D::LVL_100_PLUS_VOICE, P::LVL_100_PLUS_VOICE),
        ]
    }
}

/* =========================
   ENV PICKER DLA KANAŁÓW
   ========================= */
pub mod env_channels {
    pub fn verify_photos_id(env: &str) -> u64 {
        pick_channel(env, super::channels::dev::VERIFY_PHOTOS, super::channels::prod::VERIFY_PHOTOS)
    }

    pub fn watchlist_category_channels_id(env: &str) -> u64 {
        pick_channel(
            env,
            super::channels::dev::WATCHLIST_CATEGORY_CHANNELS,
            super::channels::prod::WATCHLIST_CATEGORY_CHANNELS,
        )
    }

    #[inline]
    fn is_prod(env: &str) -> bool {
        env.eq_ignore_ascii_case("production") || env.eq_ignore_ascii_case("prod")
    }

    #[inline]
    fn pick_channel(env: &str, dev_id: u64, prod_id: u64) -> u64 {
        if is_prod(env) { prod_id } else { dev_id }
    }

    // pomocnik (root-level): log altguard
    pub fn altguard_id(env: &str) -> u64 {
        pick_channel(env, super::channels::dev::LOGS_ALTGUARD, super::channels::prod::LOGS_ALTGUARD)
    }

    // Statystyki
    pub fn stats_date_id(env: &str)        -> u64 { pick_channel(env, super::channels::dev::STATS_DATE,        super::channels::prod::STATS_DATE) }
    pub fn stats_population_id(env: &str)  -> u64 { pick_channel(env, super::channels::dev::STATS_POPULATION,  super::channels::prod::STATS_POPULATION) }
    pub fn stats_online_id(env: &str)      -> u64 { pick_channel(env, super::channels::dev::STATS_ONLINE,      super::channels::prod::STATS_ONLINE) }
    pub fn stats_last_joined_id(env: &str) -> u64 { pick_channel(env, super::channels::dev::STATS_LAST_JOINED, super::channels::prod::STATS_LAST_JOINED) }

    // Logi
    pub mod logs {
        use super::pick_channel;
        use crate::registry::channels;

        pub fn ban_kick_mute_id(env: &str)   -> u64 { pick_channel(env, channels::dev::LOGS_BAN_KICK_MUTE,  channels::prod::LOGS_BAN_KICK_MUTE) }
        pub fn commands_id(env: &str)        -> u64 { pick_channel(env, channels::dev::LOGS_COMMANDS,       channels::prod::LOGS_COMMANDS) }
        pub fn channel_edits_id(env: &str)   -> u64 { pick_channel(env, channels::dev::LOGS_CHANNEL_EDITS,  channels::prod::LOGS_CHANNEL_EDITS) }
        pub fn voice_id(env: &str)           -> u64 { pick_channel(env, channels::dev::LOGS_VOICE,          channels::prod::LOGS_VOICE) }
        pub fn timeouts_id(env: &str)        -> u64 { pick_channel(env, channels::dev::LOGS_TIMEOUTS,       channels::prod::LOGS_TIMEOUTS) }
        pub fn message_delete_id(env: &str)  -> u64 { pick_channel(env, channels::dev::LOGS_MESSAGE_DELETE, channels::prod::LOGS_MESSAGE_DELETE) }
        pub fn joins_leaves_id(env: &str)    -> u64 { pick_channel(env, channels::dev::LOGS_JOINS_LEAVES,   channels::prod::LOGS_JOINS_LEAVES) }
        pub fn roles_id(env: &str)           -> u64 { pick_channel(env, channels::dev::LOGS_ROLES,          channels::prod::LOGS_ROLES) }
        pub fn tickets_id(env: &str)         -> u64 { pick_channel(env, channels::dev::LOGS_TICKETS,        channels::prod::LOGS_TICKETS) }
        pub fn altguard_id(env: &str)        -> u64 { pick_channel(env, channels::dev::LOGS_ALTGUARD,       channels::prod::LOGS_ALTGUARD) }
        pub fn technical_id(env: &str)       -> u64 { pick_channel(env, channels::dev::LOGS_TECH,          channels::prod::LOGS_TECH) }
    }

    // Weryfikacja
    pub mod verify {
        use super::pick_channel;
        use crate::registry::channels;
        /// ID kanału #weryfikacje (DEV/PROD; w DEV bez fallbacku)
        pub fn id(env: &str) -> u64 {
            pick_channel(env, channels::dev::VERIFY, channels::prod::VERIFY)
        }
    }

    // Początek (Global)
    pub mod global {
        use super::pick_channel;
        use crate::registry::channels;
        pub fn welcome_id(env: &str) -> u64 { pick_channel(env, channels::dev::GLOBAL_WELCOME, channels::prod::GLOBAL_WELCOME) }
        pub fn goodbye_id(env: &str) -> u64 { pick_channel(env, channels::dev::GLOBAL_GOODBYE, channels::prod::GLOBAL_GOODBYE) }
    }

    // Kontakt (Global)
    pub mod contact {
        use super::pick_channel;
        use crate::registry::channels;
        pub fn create_ticket_id(env: &str) -> u64 { pick_channel(env, channels::dev::CONTACT_CREATE_TICKET, channels::prod::CONTACT_CREATE_TICKET) }
        pub fn appeals_id(env: &str)       -> u64 { pick_channel(env, channels::dev::CONTACT_APPEALS,      channels::prod::CONTACT_APPEALS) }
    }

    // Oficjalne (Global)
    pub mod official {
        use super::pick_channel;
        use crate::registry::channels;
        pub fn events_id(env: &str)   -> u64 { pick_channel(env, channels::dev::OFFICIAL_EVENTS,   channels::prod::OFFICIAL_EVENTS) }
        pub fn calendar_id(env: &str) -> u64 { pick_channel(env, channels::dev::OFFICIAL_CALENDAR, channels::prod::OFFICIAL_CALENDAR) }
    }

    // Chaty
    pub mod chats {
        use super::pick_channel;
        use crate::registry::channels;
        pub fn general_id(env: &str)             -> u64 { pick_channel(env, channels::dev::CHAT_GENERAL,         channels::prod::CHAT_GENERAL) }
        pub fn looking_for_players_id(env: &str) -> u64 { pick_channel(env, channels::dev::CHAT_LFP,             channels::prod::CHAT_LFP) }
        pub fn grind_id(env: &str)               -> u64 { pick_channel(env, channels::dev::CHAT_GRIND,           channels::prod::CHAT_GRIND) }
        pub fn commands_public_id(env: &str)     -> u64 { pick_channel(env, channels::dev::CHAT_COMMANDS_PUBLIC, channels::prod::CHAT_COMMANDS_PUBLIC) }
        pub fn suggestions_id(env: &str)         -> u64 { pick_channel(env, channels::dev::CHAT_SUGGESTIONS,     channels::prod::CHAT_SUGGESTIONS) }
        pub fn levels_id(env: &str)              -> u64 { pick_channel(env, channels::dev::CHAT_LEVELS,         channels::prod::CHAT_LEVELS) }
    }

    // Rozrywka
    pub mod fun {
        use super::pick_channel;
        use crate::registry::channels;
        pub fn clips_id(env: &str)       -> u64 { pick_channel(env, channels::dev::FUN_CLIPS,        channels::prod::FUN_CLIPS) }
        pub fn photos_id(env: &str)      -> u64 { pick_channel(env, channels::dev::FUN_PHOTOS,       channels::prod::FUN_PHOTOS) }
        pub fn memes_id(env: &str)       -> u64 { pick_channel(env, channels::dev::FUN_MEMES,        channels::prod::FUN_MEMES) }
        pub fn show_off_id(env: &str)    -> u64 { pick_channel(env, channels::dev::FUN_SHOW_OFF,     channels::prod::FUN_SHOW_OFF) }
        pub fn selfie_id(env: &str)      -> u64 { pick_channel(env, channels::dev::FUN_SELFIE,       channels::prod::FUN_SELFIE) }
        pub fn last_letter_id(env: &str) -> u64 { pick_channel(env, channels::dev::FUN_LAST_LETTER,  channels::prod::FUN_LAST_LETTER) }
        pub fn nsfw_id(env: &str)        -> u64 { pick_channel(env, channels::dev::FUN_NSFW,         channels::prod::FUN_NSFW) }
    }

    // Tematy
    pub mod topics {
        use super::pick_channel;
        use crate::registry::channels;
        pub fn games_id(env: &str)     -> u64 { pick_channel(env, channels::dev::TOPICS_GAMES,     channels::prod::TOPICS_GAMES) }
        pub fn tv_series_id(env: &str) -> u64 { pick_channel(env, channels::dev::TOPICS_TV_SERIES, channels::prod::TOPICS_TV_SERIES) }
        pub fn drawing_id(env: &str)   -> u64 { pick_channel(env, channels::dev::TOPICS_DRAWING,   channels::prod::TOPICS_DRAWING) }
        pub fn politics_id(env: &str)  -> u64 { pick_channel(env, channels::dev::TOPICS_POLITICS,  channels::prod::TOPICS_POLITICS) }
        pub fn music_id(env: &str)     -> u64 { pick_channel(env, channels::dev::TOPICS_MUSIC,     channels::prod::TOPICS_MUSIC) }
    }

    // Lista obserwowanych kategorii
    pub mod watch {
        use super::pick_channel;
        use crate::registry::channels;
        pub fn categories(env: &str) -> Vec<u64> {
            let all = [
                pick_channel(env, channels::dev::WATCH_CAT_1, channels::prod::WATCH_CAT_1),
                pick_channel(env, channels::dev::WATCH_CAT_2, channels::prod::WATCH_CAT_2),
                pick_channel(env, channels::dev::WATCH_CAT_3, channels::prod::WATCH_CAT_3),
                pick_channel(env, channels::dev::WATCH_CAT_4, channels::prod::WATCH_CAT_4),
                pick_channel(env, channels::dev::WATCH_CAT_5, channels::prod::WATCH_CAT_5),
                pick_channel(env, channels::dev::WATCH_CAT_6, channels::prod::WATCH_CAT_6),
                pick_channel(env, channels::dev::WATCH_CAT_7, channels::prod::WATCH_CAT_7),
                pick_channel(env, channels::dev::WATCH_CAT_8, channels::prod::WATCH_CAT_8),
                pick_channel(env, channels::dev::WATCH_CAT_9, channels::prod::WATCH_CAT_9),
            ];
            all.into_iter().filter(|id| *id != 0).collect()
        }
    }

    // --- Watcher nowych kanałów (root-level) ---
    pub fn new_channels_id(env: &str) -> u64 {
        pick_channel(
            env,
            super::channels::dev::LOGS_NEW_CHANNELS,
            super::channels::prod::LOGS_NEW_CHANNELS,
        )
    }

    pub fn new_channels_parent_id(env: &str) -> u64 {
        pick_channel(
            env,
            super::channels::dev::LOGS_NEW_CHANNELS_PARENT,
            super::channels::prod::LOGS_NEW_CHANNELS_PARENT,
        )
    }

    /// Lista kategorii obserwowanych pod kątem nowych kanałów (root helper).
    pub fn watch_categories(env: &str) -> Vec<u64> {
        use crate::registry::channels;
        let all = [
            pick_channel(env, channels::dev::WATCH_CAT_1, channels::prod::WATCH_CAT_1),
            pick_channel(env, channels::dev::WATCH_CAT_2, channels::prod::WATCH_CAT_2),
            pick_channel(env, channels::dev::WATCH_CAT_3, channels::prod::WATCH_CAT_3),
            pick_channel(env, channels::dev::WATCH_CAT_4, channels::prod::WATCH_CAT_4),
            pick_channel(env, channels::dev::WATCH_CAT_5, channels::prod::WATCH_CAT_5),
            pick_channel(env, channels::dev::WATCH_CAT_6, channels::prod::WATCH_CAT_6),
            pick_channel(env, channels::dev::WATCH_CAT_7, channels::prod::WATCH_CAT_7),
            pick_channel(env, channels::dev::WATCH_CAT_8, channels::prod::WATCH_CAT_8),
            pick_channel(env, channels::dev::WATCH_CAT_9, channels::prod::WATCH_CAT_9),
        ];
        all.into_iter().filter(|id| *id != 0).collect()
    }
}
