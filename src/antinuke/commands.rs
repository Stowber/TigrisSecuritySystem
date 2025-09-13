use anyhow::Result;
use serenity::all::{
    CommandDataOptionValue, CommandOptionType, Context, CreateCommand, CreateCommandOption, GuildId,
    Interaction,
};
use serenity::builder::EditInteractionResponse;

use crate::AppContext;
use crate::permissions::Role;
use crate::registry::env_roles;

use super::{approve, restore, snapshot};

pub async fn register_commands(ctx: &Context, guild_id: GuildId) -> Result<()> {
    guild_id
        .create_command(
            &ctx.http,
            CreateCommand::new("antinuke")
                .description("Antinuke utilities")
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::SubCommand,
                        "approve",
                        "Approve an incident",
                    )
                    .add_sub_option(
                        CreateCommandOption::new(
                            CommandOptionType::Integer,
                            "incident_id",
                            "Incident identifier",
                        )
                        .required(true),
                    ),
                )
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::SubCommand,
                        "restore",
                        "Restore a snapshot for incident",
                    )
                    .add_sub_option(
                        CreateCommandOption::new(
                            CommandOptionType::Integer,
                            "incident_id",
                            "Incident identifier",
                        )
                        .required(true),
                    ),
                )
                .add_option(CreateCommandOption::new(
                    CommandOptionType::SubCommand,
                    "status",
                    "List recent incidents",
                ))
                .add_option(CreateCommandOption::new(
                    CommandOptionType::SubCommand,
                    "test",
                    "Trigger test incident",
                ))
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::SubCommandGroup,
                        "maintenance",
                        "Maintenance mode",
                    )
                    .add_sub_option(CreateCommandOption::new(
                        CommandOptionType::SubCommand,
                        "start",
                        "Start maintenance",
                    ))
                    .add_sub_option(CreateCommandOption::new(
                        CommandOptionType::SubCommand,
                        "stop",
                        "Stop maintenance",
                    )),
                ),
        )
        .await?;
    Ok(())
}

/// Obsługa logiki subkomend. Zwraca gotowy tekst odpowiedzi.
pub async fn handle_subcommand(
    app: &AppContext,
    http: &serenity::all::Http,
    guild_id: u64,
    user_id: u64,
    name: &str,
    incident_id: Option<i64>,
) -> String {
    // Sprawdzamy uprawnienia: najpierw pełna nazwa, potem fallback do pierwszego segmentu
    let perm_full = format!("antinuke.{name}");
    let perm_group = format!(
        "antinuke.{}",
        name.split('.').next().unwrap_or_default()
    );

    let allowed = app.command_acl().has_permission(user_id, &perm_full).await
        || app
            .command_acl()
            .has_permission(user_id, &perm_group)
            .await;

    if !allowed {
        return "missing permission".into();
    }

    match name {
        "approve" => {
            if let Some(id) = incident_id {
                match cmd_approve(app, id, user_id).await {
                    Ok(_) => format!("incident {id} approved"),
                    Err(e) => format!("approve failed: {e}"),
                }
            } else {
                "missing incident_id".into()
            }
        }
        "restore" => {
            if let Some(id) = incident_id {
                match cmd_restore(app, http, guild_id, id).await {
                    Ok(_) => format!("incident {id} restored"),
                    Err(e) => format!("restore failed: {e}"),
                }
            } else {
                "missing incident_id".into()
            }
        }
        "test" => match cmd_test(app, guild_id).await {
            Ok(_) => "test incident triggered".into(),
            Err(e) => format!("test failed: {e}"),
        },
        "status" => match cmd_status(app, guild_id).await {
            Ok(list) => {
                if list.is_empty() {
                    "no incidents".to_string()
                } else {
                    list.iter()
                        .map(|(id, reason)| format!("{id}: {reason}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            Err(e) => format!("status error: {e}"),
        },
        "maintenance.start" => match cmd_maintenance_start(app, guild_id).await {
            Ok(_) => "maintenance started".into(),
            Err(e) => format!("maintenance start failed: {e}"),
        },
        "maintenance.stop" => match cmd_maintenance_stop(app, guild_id).await {
            Ok(_) => "maintenance stopped".into(),
            Err(e) => format!("maintenance stop failed: {e}"),
        },
        _ => format!("unknown subcommand: {name}"),
    }
}

/// Podstawowy handler interakcji `/antinuke`.
pub async fn on_interaction(ctx: &Context, app: &AppContext, interaction: Interaction) {
    let Some(cmd) = interaction.command() else {
        return;
    };

    if cmd.data.name != "antinuke" {
        return;
    }

    if let Err(err) = cmd.defer_ephemeral(&ctx.http).await {
        tracing::warn!("failed to defer antinuke interaction: {:?}", err);
    }

    let guild_id = match cmd.guild_id {
        Some(g) => g,
        None => {
            let _ = cmd
                .edit_response(
                    &ctx.http,
                    EditInteractionResponse::new()
                        .content("this command must be used in a guild"),
                )
                .await;
            return;
        }
    };

    let Some(sub) = cmd.data.options.first() else {
        let _ = cmd
            .edit_response(
                &ctx.http,
                EditInteractionResponse::new().content("missing subcommand"),
            )
            .await;
        return;
    };

    // Wyciągnięcie incident_id z subkomendy
    let incident_id_from_sub = |sub: &serenity::all::CommandDataOption| -> Option<i64> {
        match &sub.value {
            CommandDataOptionValue::SubCommand(options) => options.iter().find_map(|o| {
                if o.name == "incident_id" {
                    if let CommandDataOptionValue::Integer(id) = &o.value {
                        Some(*id)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }),
            _ => None,
        }
    };

    // Nazwa subkomendy – osobna ścieżka dla grupy maintenance
    let sub_name = if sub.name == "maintenance" {
        match &sub.value {
            CommandDataOptionValue::SubCommandGroup(options) => {
                if let Some(inner) = options.first() {
                    format!("maintenance.{}", inner.name)
                } else {
                    let _ = cmd
                        .edit_response(
                            &ctx.http,
                            EditInteractionResponse::new()
                                .content("invalid maintenance usage"),
                        )
                        .await;
                    return;
                }
            }
            _ => {
                let _ = cmd
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new().content("invalid maintenance usage"),
                    )
                    .await;
                return;
            }
        }
    } else {
        sub.name.clone()
    };

    let roles = match ctx.http.get_member(guild_id, cmd.user.id).await {
        Ok(member) => {
            let env = app.env();
            member
                .roles
                .iter()
                .filter_map(|rid| {
                    let rid = rid.get();
                    if rid == env_roles::owner_id(&env) {
                        Some(Role::Wlasciciel)
                    } else if rid == env_roles::co_owner_id(&env) {
                        Some(Role::WspolWlasciciel)
                    } else if rid == env_roles::technik_zarzad_id(&env) {
                        Some(Role::TechnikZarzad)
                    } else if rid == env_roles::opiekun_id(&env) {
                        Some(Role::Opiekun)
                    } else if rid == env_roles::admin_id(&env) {
                        Some(Role::Admin)
                    } else if rid == env_roles::moderator_id(&env) {
                        Some(Role::Moderator)
                    } else if rid == env_roles::test_moderator_id(&env) {
                        Some(Role::TestModerator)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        }
        Err(e) => {
            tracing::warn!(error=?e, "failed to fetch member");
            Vec::new()
        }
    };
    {
        let mut map = app.user_roles.lock().unwrap();
        if roles.is_empty() {
            map.remove(&cmd.user.id.get());
        } else {
            map.insert(cmd.user.id.get(), roles);
        }
    }
    let content = handle_subcommand(
        app,
        &ctx.http,
        guild_id.get(),
        cmd.user.id.get(),
        &sub_name,
        incident_id_from_sub(sub),
    )
    .await;

    if let Err(err) = cmd
        .edit_response(
            &ctx.http,
            EditInteractionResponse::new().content(content),
        )
        .await
    {
        tracing::warn!("failed to edit antinuke response: {:?}", err);
    }
}

/// Handle `/antinuke approve <incident_id>`.
pub async fn cmd_approve(app: &AppContext, incident_id: i64, moderator_id: u64) -> Result<()> {
    approve::approve(app, incident_id, moderator_id).await
}

/// Handle `/antinuke restore <incident_id>` – wykonuje snapshot i przywraca go.
pub async fn cmd_restore(
    app: &AppContext,
    http: &serenity::all::Http,
    guild_id: u64,
    incident_id: i64,
) -> Result<()> {
    let api = snapshot::SerenityApi { http };
    let snap = snapshot::take_snapshot(&api, guild_id).await?;
    restore::apply_snapshot(&api, app, guild_id, incident_id, &snap).await
}

/// Trigger a test cut to verify antinuke functionality.
pub async fn cmd_test(app: &AppContext, guild_id: u64) -> Result<()> {
    app.antinuke().cut(guild_id, "test").await
}

/// Report basic status of the monitoring service.
pub async fn cmd_status(app: &AppContext, guild_id: u64) -> Result<Vec<(i64, String)>> {
    app.antinuke().incidents(guild_id).await
}

pub async fn cmd_maintenance_start(app: &AppContext, guild_id: u64) -> Result<()> {
    // Te metody zwracają () – nie ma czego propagować przez ?
    app.antinuke().start_maintenance(guild_id).await;
    Ok(())
}

pub async fn cmd_maintenance_stop(app: &AppContext, guild_id: u64) -> Result<()> {
    app.antinuke().stop_maintenance(guild_id).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{App, ChatGuardConfig, Database, Discord, Logging, Settings};
    use crate::permissions::Role;
    use sqlx::postgres::PgPoolOptions;
    use std::sync::Arc;

    fn ctx() -> Arc<AppContext> {
        let settings = Settings {
            env: "test".into(),
            app: App { name: "test".into() },
            discord: Discord {
                token: String::new(),
                app_id: None,
                intents: vec![],
            },
            database: Database {
                url: "postgres://localhost:1/test?connect_timeout=1".into(),
                max_connections: Some(1),
                statement_timeout_ms: Some(5_000),
            },
            logging: Logging {
                json: Some(false),
                level: Some("info".into()),
            },
            chatguard: ChatGuardConfig { racial_slurs: vec![] },
            antinuke: Default::default(),
        };
        let db = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy(&settings.database.url)
            .unwrap();
        AppContext::new_testing(settings, db)
    }

    fn http() -> serenity::all::Http {
        serenity::all::Http::new("")
    }

    #[tokio::test]
    async fn approve_permission_error() {
        let ctx = ctx();
        let http = http();
        let msg = handle_subcommand(&ctx, &http, 1, 1, "approve", Some(1)).await;
        assert!(msg.contains("missing permission"));
    }

    #[tokio::test]
    async fn approve_permission_ok() {
        let ctx = ctx();
        let http = http();
        ctx.user_roles.lock().unwrap().insert(1, vec![Role::Admin]);
        let msg = handle_subcommand(&ctx, &http, 1, 1, "approve", Some(1)).await;
        assert!(msg.starts_with("approve failed:"));
    }

    #[tokio::test]
    async fn restore_error_message() {
        let ctx = ctx();
        let http = http();
        ctx.user_roles.lock().unwrap().insert(1, vec![Role::Admin]);
        let msg = handle_subcommand(&ctx, &http, 1, 1, "restore", Some(1)).await;
        assert!(msg.starts_with("restore failed:"));
    }

    #[tokio::test]
    async fn test_triggers_cut() {
        let ctx = ctx();
        let http = http();
        ctx.user_roles.lock().unwrap().insert(1, vec![Role::Admin]);
        let msg = handle_subcommand(&ctx, &http, 1, 1, "test", None).await;
        assert_eq!(msg, "test incident triggered");
    }

    #[tokio::test]
    async fn unknown_subcommand_is_reported() {
        let ctx = ctx();
        let http = http();
        ctx.user_roles.lock().unwrap().insert(1, vec![Role::Admin]);
        let msg = handle_subcommand(&ctx, &http, 1, 1, "doesnotexist", None).await;
        assert!(msg.starts_with("unknown subcommand:"));
    }
}
