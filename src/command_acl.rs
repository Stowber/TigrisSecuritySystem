use crate::permissions::{PERMISSIONS, Permission, Role};
use crate::registry::env_roles;
use anyhow::Result;
use serenity::all::{Context, CreateCommandPermission, EditCommandPermissions, GuildId, RoleId};
use std::{collections::HashMap, sync::Arc};
use crate::AppContext;

pub async fn apply_permissions(ctx: &Context, guild_id: GuildId) -> Result<()> {
    let env = std::env::var("TSS_ENV").unwrap_or_else(|_| "production".to_string());
    let cmds = guild_id.get_commands(&ctx.http).await?;
    let map = build_map(&env);

    for cmd in cmds {
        if let Some(roles) = map.get(cmd.name.as_str()) {
            let mut perms: Vec<CreateCommandPermission> = roles
                .iter()
                .map(|&rid| CreateCommandPermission::role(RoleId::new(rid), true))
                .collect();
            perms.push(CreateCommandPermission::everyone(guild_id, false));
            let builder = EditCommandPermissions::new(perms);
            let _ = guild_id
                .edit_command_permissions(&ctx.http, cmd.id, builder)
                .await;
        }
    }
    Ok(())
}

fn build_map(env: &str) -> HashMap<&'static str, Vec<u64>> {
    // Mapowanie Permission -> RoleId
    let mut map: HashMap<&'static str, Vec<u64>> = HashMap::new();
    let role_id = |role: Role| match role {
        Role::Wlasciciel => env_roles::owner_id(env),
        Role::WspolWlasciciel => env_roles::co_owner_id(env),
        Role::TechnikZarzad => env_roles::technik_zarzad_id(env),
        Role::Opiekun => env_roles::opiekun_id(env),
        Role::HeadAdmin => env_roles::admin_id(env),
        Role::Admin => env_roles::admin_id(env),
        Role::HeadModerator => env_roles::moderator_id(env),
        Role::Moderator => env_roles::moderator_id(env),
        Role::TestModerator => env_roles::test_moderator_id(env),
    };

    for (perm, roles) in PERMISSIONS.iter() {
       let ids: Vec<u64> = roles
            .iter()
            .map(|r| role_id(*r))
            .filter(|id| *id != 0)
            .collect();
        let name = match perm {
           Permission::Admcheck => Some("admcheck"),
            Permission::Ban => Some("ban"),
            Permission::Idguard => Some("idguard"),
            Permission::Kick => Some("kick"),
            Permission::Mdel => Some("mdel"),
            Permission::Mute => Some("mute"),
            Permission::MuteConfig => Some("mute-config"),
            Permission::Punkty => Some("punkty"),
            Permission::SlashClean => Some("slash-clean"),
            Permission::SlashResync => Some("slash-resync"),
            Permission::Teach => Some("teach"),
            Permission::Unmute => Some("unmute"),
            Permission::User => Some("user"),
            Permission::VerifyPanel => Some("verify-panel"),
            Permission::Warn => Some("warn"),
            Permission::WarnRemove => Some("warn-remove"),
            Permission::Warns => Some("warns"),
            Permission::Test => Some("test"),
            Permission::Watchlist => Some("watchlist"),
            Permission::TestCmd => Some("test-cmd"),
            Permission::AntinukeApprove
            | Permission::AntinukeRestore
            | Permission::AntinukeStatus
            | Permission::AntinukeTest
            | Permission::AntinukeMaintenance
            | Permission::AntinukeAll => Some("antinuke"),
        };
        if let Some(name) = name {
            map.entry(name).or_default().extend(ids.iter().copied());
        }
    }
    map
}
/// Simplistic command ACL service used for checking if a user has
/// permission to execute a command. The implementation here is a placeholder
/// that denies all permissions – tests can rely on this to simulate missing
/// privileges without setting up any external state.
#[derive(Clone, Debug)]
pub struct CommandAcl {
    ctx: Arc<AppContext>,
}

impl CommandAcl {
    /// Check if a given user has the specified permission.
      pub async fn has_permission(&self, user_id: u64, perm: &str) -> bool {
        fn map_perm(name: &str) -> Option<Permission> {
            use Permission::*;
            match name {
                "admcheck" => Some(Admcheck),
                "ban" => Some(Ban),
                "idguard" => Some(Idguard),
                "kick" => Some(Kick),
                "mdel" => Some(Mdel),
                "mute" => Some(Mute),
                "mute-config" => Some(MuteConfig),
                "punkty" => Some(Punkty),
                "slash-clean" => Some(SlashClean),
                "slash-resync" => Some(SlashResync),
                "teach" => Some(Teach),
                "unmute" => Some(Unmute),
                "user" => Some(User),
                "verify-panel" => Some(VerifyPanel),
                "warn" => Some(Warn),
                "warn-remove" => Some(WarnRemove),
                "warns" => Some(Warns),
                "test" => Some(Test),
                "watchlist" => Some(Watchlist),
                "test-cmd" => Some(TestCmd),
                "antinuke" => Some(AntinukeAll),
                "antinuke.approve" => Some(AntinukeApprove),
                "antinuke.restore" => Some(AntinukeRestore),
                "antinuke.status" => Some(AntinukeStatus),
                "antinuke.test" => Some(AntinukeTest),
                "antinuke.maintenance" => Some(AntinukeMaintenance),
                _ => None,
            }
        }
        let roles = self.ctx.user_roles.lock().unwrap();
        let user_roles = roles.get(&user_id).cloned().unwrap_or_default();
        match map_perm(perm) {
            Some(p) => PERMISSIONS
                .get(&p)
                .map(|rs| user_roles.iter().any(|r| rs.contains(r)))
                .unwrap_or(false),
            None => false,
        }
    }
}

/// Provide access to the command ACL service from [`AppContext`].
impl AppContext {
    pub fn command_acl(&self) -> CommandAcl {
        CommandAcl {
            ctx: Arc::new(self.clone()),
        }
    }
}
