use std::collections::HashMap;
use anyhow::Result;
use serenity::all::{Context, GuildId, RoleId, CreateCommandPermission, EditCommandPermissions};
use crate::registry::env_roles;
use crate::permissions::{Permission, Role, PERMISSIONS};

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
            Permission::Admcheck => "admcheck",
            Permission::Ban => "ban",
            Permission::Idguard => "idguard",
            Permission::Kick => "kick",
            Permission::Mdel => "mdel",
            Permission::Mute => "mute",
            Permission::MuteConfig => "mute-config",
            Permission::Punkty => "punkty",
            Permission::SlashClean => "slash-clean",
            Permission::SlashResync => "slash-resync",
            Permission::Teach => "teach",
            Permission::Unmute => "unmute",
            Permission::User => "user",
            Permission::VerifyPanel => "verify-panel",
            Permission::Warn => "warn",
            Permission::WarnRemove => "warn-remove",
            Permission::Warns => "warns",
            Permission::Test => "test",
        };
        map.insert(name, ids);
    }
    map
}
