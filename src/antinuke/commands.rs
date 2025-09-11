use anyhow::Result;
use serenity::all::{
    CommandDataOptionValue, CommandOptionType, Context, CreateCommand,
    CreateCommandOption, CreateInteractionResponse, CreateInteractionResponseMessage,
    GuildId, Interaction,
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
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::SubCommand,
                        "status",
                        "List recent incidents",
                    ),
                ),
        )
        .await?;
    Ok(())
}

/// Basic interaction handler for antinuke commands.
pub async fn on_interaction(ctx: &Context, app: &AppContext, interaction: Interaction) {
    let Some(cmd) = interaction.clone().command() else { return; };
    if cmd.data.name != "antinuke" {
        return;
    }
    let guild_id = match cmd.guild_id { Some(g) => g, None => return };
    let Some(sub) = cmd.data.options.first() else { return; };

    // Pomocniczo wyciągamy incident_id z subkomendy
    let incident_id_from_sub = |sub: &serenity::all::CommandDataOption| -> Option<i64> {
        match &sub.value {
            CommandDataOptionValue::SubCommand(options) => {
                options.iter().find_map(|o| {
                    if o.name == "incident_id" {
                        if let CommandDataOptionValue::Integer(id) = &o.value {
                            Some(*id)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
            }
            _ => None,
        }
    };

    let content = match sub.name.as_str() {
        "approve" => {
            if let Some(id) = incident_id_from_sub(sub) {
                let _ = cmd_approve(app, id, cmd.user.id.get()).await;
                format!("incident {id} approved")
            } else {
                "missing incident_id".into()
            }
        }
        "restore" => {
            if let Some(id) = incident_id_from_sub(sub) {
                let _ = cmd_restore(app, guild_id.get(), id).await;
                format!("incident {id} restored")
            } else {
                "missing incident_id".into()
            }
        }
        "status" => match cmd_status(app, guild_id.get()).await {
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
        _ => return,
    };

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
    let snap = snapshot::take_snapshot(guild_id).await?;
    restore::apply_snapshot(app, guild_id, incident_id, &snap).await
}

/// Report basic status of the monitoring service.
pub async fn cmd_status(app: &AppContext, guild_id: u64) -> Result<Vec<(i64, String)>> {
    app.antinuke().incidents(guild_id).await
}