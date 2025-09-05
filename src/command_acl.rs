use std::collections::HashMap;

use anyhow::Result;
use serenity::all::{Context, GuildId, RoleId, CreateCommandPermission, EditCommandPermissions};

use crate::registry::env_roles;

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
    let tm = env_roles::test_moderator_id(env);
    let mo = env_roles::moderator_id(env);
    let ad = env_roles::admin_id(env);
    let op = env_roles::opiekun_id(env);
    let tz = env_roles::technik_zarzad_id(env);
    let co = env_roles::co_owner_id(env);
    let ow = env_roles::owner_id(env);
    let gu = env_roles::gumis_od_botow_id(env);

    let tm_plus = vec![tm, mo, ad, op, tz, co, ow, gu];
    let mo_plus = vec![mo, ad, op, tz, co, ow, gu];
    let ad_plus = vec![ad, op, tz, co, ow, gu];
    let op_plus = vec![op, tz, co, ow, gu];
    let tz_plus = vec![tz, co, ow, gu];

    let mut map: HashMap<&'static str, Vec<u64>> = HashMap::new();

    for name in ["warn", "warns", "user"] {
        map.insert(name, tm_plus.clone());
    }
    map.insert("mute", mo_plus.clone());

    for name in ["warn-remove", "unmute", "kick", "mdel", "ban"] {
        map.insert(name, ad_plus.clone());
    }

    map.insert("punkty", op_plus.clone());
    map.insert("admcheck", op_plus.clone());

    for name in [
        "slash-clean",
        "slash-resync",
        "teach",
        "mute-config",
        "verify-panel",
    ] {
        map.insert(name, tz_plus.clone());
    }

    map
}
