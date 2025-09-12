use anyhow::Result;
use serenity::all::{
   CommandDataOptionValue, CommandOptionType, Context, CreateCommand, CreateCommandOption,
    CreateInteractionResponse, CreateInteractionResponseMessage, GuildId, Interaction,
};

use crate::AppContext;

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
                )),
        )
        .await?;
    Ok(())
}
async fn handle_subcommand(
    app: &AppContext,
    guild_id: u64,
    user_id: u64,
    name: &str,
    incident_id: Option<i64>,
) -> String {
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
                match cmd_restore(app, guild_id, id).await {
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
            Err(_) => "status error".into(),
        },
        _ => String::new(),
    }
}

/// Basic interaction handler for antinuke commands.
pub async fn on_interaction(ctx: &Context, app: &AppContext, interaction: Interaction) {
    let Some(cmd) = interaction.clone().command() else {
        return;
    };
    if cmd.data.name != "antinuke" {
        return;
    }
    let guild_id = match cmd.guild_id {
        Some(g) => g,
        None => return,
    };
    let Some(sub) = cmd.data.options.first() else {
        return;
    };

    // Pomocniczo wyciągamy incident_id z subkomendy
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

    let content = handle_subcommand(
        app,
        guild_id.get(),
        cmd.user.id.get(),
        sub.name.as_str(),
        incident_id_from_sub(sub),
    )
    .await;

    let _ = cmd
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(content)
                    .ephemeral(true),
            ),
        )
        .await;
}

/// Handle `/antinuke approve <incident_id>`.
pub async fn cmd_approve(app: &AppContext, incident_id: i64, moderator_id: u64) -> Result<()> {
    approve::approve(app, incident_id, moderator_id).await
}

/// Handle `/antinuke restore <incident_id>` by taking a snapshot and applying
/// it back. This is a placeholder that does not interact with Discord.
pub async fn cmd_restore(app: &AppContext, guild_id: u64, incident_id: i64) -> Result<()> {
    let http = serenity::all::Http::new(&app.settings.discord.token);
    let api = snapshot::SerenityApi { http: &http };
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


#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{App, ChatGuardConfig, Database, Discord, Logging, Settings};
    use sqlx::postgres::PgPoolOptions;
    use std::sync::Arc;

    fn ctx() -> Arc<AppContext> {
        let settings = Settings {
            env: "test".into(),
            app: App {
                name: "test".into(),
            },
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
            chatguard: ChatGuardConfig {
                racial_slurs: vec![],
            },
            antinuke: Default::default(),
        };
        let db = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy(&settings.database.url)
            .unwrap();
        AppContext::new_testing(settings, db)
    }

    #[tokio::test]
    async fn approve_permission_error() {
        let ctx = ctx();
        let msg = handle_subcommand(&ctx, 1, 1, "approve", Some(1)).await;
        assert!(msg.contains("missing permission"));
    }

    #[tokio::test]
    async fn restore_error_message() {
        let ctx = ctx();
        let msg = handle_subcommand(&ctx, 1, 1, "restore", Some(1)).await;
        assert!(msg.starts_with("restore failed:"));
    }
    #[tokio::test]
    async fn test_triggers_cut() {
        let ctx = ctx();
        let msg = handle_subcommand(&ctx, 1, 1, "test", None).await;
        assert_eq!(msg, "test incident triggered");
    }
}