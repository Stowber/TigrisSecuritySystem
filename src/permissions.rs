use once_cell::sync::Lazy;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Wlasciciel,
    WspolWlasciciel,
    TechnikZarzad,
    Opiekun,
    HeadAdmin,
    Admin,
    HeadModerator,
    Moderator,
    TestModerator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    Admcheck,
    Ban,
    Idguard,
    Kick,
    Mdel,
    Mute,
    MuteConfig,
    Punkty,
    SlashClean,
    SlashResync,
    Teach,
    Unmute,
    User,
    VerifyPanel,
    Warn,
    WarnRemove,
    Warns,
    Test,
    Watchlist,
    TestCmd,
    AntinukeApprove,
    AntinukeRestore,
    AntinukeStatus,
    AntinukeTest,
    AntinukeMaintenance,
    AntinukeAll,
}

pub static PERMISSIONS: Lazy<HashMap<Permission, Vec<Role>>> = Lazy::new(|| {
    use Permission::*;
    use Role::*;
    HashMap::from([
        (Test, vec![TechnikZarzad]),
        (Admcheck, vec![Opiekun, Wlasciciel, WspolWlasciciel, TechnikZarzad]),
        (Ban, vec![HeadAdmin, Admin, Opiekun, Wlasciciel, WspolWlasciciel, TechnikZarzad]),
        (Idguard, vec![TechnikZarzad]),
        (Kick, vec![Admin, HeadAdmin, HeadModerator]),
        (Mdel, vec![Moderator, HeadModerator, Admin, HeadAdmin, Opiekun, Wlasciciel, WspolWlasciciel, TechnikZarzad]),
        (Mute, vec![TestModerator, Moderator, HeadModerator, Admin, HeadAdmin, Opiekun, Wlasciciel, WspolWlasciciel, TechnikZarzad]),
        (MuteConfig, vec![TechnikZarzad]),
        (Punkty, vec![Opiekun, Wlasciciel, WspolWlasciciel, TechnikZarzad]),
        (SlashClean, vec![TechnikZarzad]),
        (SlashResync, vec![TechnikZarzad]),
        (Teach, vec![TechnikZarzad]),
        (Unmute, vec![HeadModerator, Admin, HeadAdmin, Opiekun, Wlasciciel, WspolWlasciciel, TechnikZarzad]),
        (User, vec![Moderator, HeadModerator, Admin, HeadAdmin, Opiekun, Wlasciciel, WspolWlasciciel, TechnikZarzad]),
        (VerifyPanel, vec![TechnikZarzad]),
        (Warn, vec![TestModerator, Moderator, HeadModerator, Admin, HeadAdmin, Opiekun, Wlasciciel, WspolWlasciciel, TechnikZarzad]),
        (WarnRemove, vec![Admin, HeadAdmin, Opiekun, Wlasciciel, WspolWlasciciel, TechnikZarzad]),
        (Warns, vec![Moderator, HeadModerator, Admin, HeadAdmin, Opiekun, Wlasciciel, WspolWlasciciel, TechnikZarzad]),
        (Watchlist, vec![Moderator, HeadModerator, Admin, HeadAdmin, Opiekun, Wlasciciel, WspolWlasciciel, TechnikZarzad]),
        (TestCmd, vec![TechnikZarzad]),
        (AntinukeApprove, vec![Wlasciciel, WspolWlasciciel, TechnikZarzad]),
        (AntinukeRestore, vec![Wlasciciel, WspolWlasciciel, TechnikZarzad]),
        (AntinukeStatus, vec![Wlasciciel, WspolWlasciciel, TechnikZarzad]),
        (AntinukeTest, vec![Wlasciciel, WspolWlasciciel, TechnikZarzad]),
        (AntinukeMaintenance, vec![Wlasciciel, WspolWlasciciel, TechnikZarzad]),
        (AntinukeAll, vec![Wlasciciel, WspolWlasciciel, TechnikZarzad]),
    ])
});

pub fn role_has_permission(role: Role, permission: Permission) -> bool {
    PERMISSIONS
        .get(&permission)
        .map(|roles| roles.contains(&role))
        .unwrap_or(false)
}