use anyhow::Result;
use serde::{Deserialize, Serialize};
use serenity::all::{ChannelId, ChannelType, GuildId, Permissions, RoleId};
use serenity::async_trait;
use serenity::builder::{CreateChannel, EditChannel, EditRole};

/// Snapshot of a single role.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoleSnapshot {
    pub id: u64,
    pub name: String,
    pub position: i64,
    pub permissions: u64,
}

/// Snapshot of a single channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChannelSnapshot {
    pub id: u64,
    pub name: String,
    pub kind: String,
    pub position: i64,
    pub parent_id: Option<u64>,
}

/// Simplified snapshot of guild state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GuildSnapshot {
    pub roles: Vec<RoleSnapshot>,
    pub channels: Vec<ChannelSnapshot>,
}

/// Abstraction over Discord API used for snapshotting and restoring.
#[async_trait]
pub trait DiscordApi: Send + Sync {
    async fn fetch_roles(&self, guild_id: u64) -> Result<Vec<RoleSnapshot>>;
    async fn fetch_channels(&self, guild_id: u64) -> Result<Vec<ChannelSnapshot>>;
    async fn create_role(&self, guild_id: u64, role: &RoleSnapshot) -> Result<()>;
    async fn create_channel(&self, guild_id: u64, channel: &ChannelSnapshot) -> Result<()>;
    async fn update_role(&self, guild_id: u64, role: &RoleSnapshot) -> Result<()>;
    async fn update_channel(&self, guild_id: u64, channel: &ChannelSnapshot) -> Result<()>;
    async fn delete_role(&self, guild_id: u64, role_id: u64) -> Result<()>;
    async fn delete_channel(&self, guild_id: u64, channel_id: u64) -> Result<()>;
}

/// Implementation of [DiscordApi] backed by `serenity::http::Http`.
pub struct SerenityApi<'a> {
    pub http: &'a serenity::all::Http,
}

#[async_trait]
impl<'a> DiscordApi for SerenityApi<'a> {
    async fn fetch_roles(&self, guild_id: u64) -> Result<Vec<RoleSnapshot>> {
        let roles = self.http.get_guild_roles(GuildId::new(guild_id)).await?;
        Ok(roles
            .into_iter()
            .map(|r| RoleSnapshot {
                id: r.id.get(),
                name: r.name,
                position: r.position as i64,
                permissions: r.permissions.bits(),
            })
            .collect())
    }

    async fn fetch_channels(&self, guild_id: u64) -> Result<Vec<ChannelSnapshot>> {
        let channels = GuildId::new(guild_id).channels(self.http).await?;
        Ok(channels
            .into_values()
            .map(|c| ChannelSnapshot {
                id: c.id.get(),
                name: c.name,
                kind: match c.kind {
                    ChannelType::Voice => "voice",
                    ChannelType::Category => "category",
                    ChannelType::Text => "text",
                    _ => "other",
                }
                .to_string(),
                position: c.position as i64,
                parent_id: c.parent_id.map(|p| p.get()),
            })
            .collect())
    }

    async fn create_role(&self, guild_id: u64, role: &RoleSnapshot) -> Result<()> {
        let builder = EditRole::new()
            .name(&role.name)
            .permissions(Permissions::from_bits_truncate(role.permissions))
            .position(role.position as u16);
        self.http
            .create_role(GuildId::new(guild_id), &builder, None)
            .await?;
        Ok(())
    }

    async fn create_channel(&self, guild_id: u64, channel: &ChannelSnapshot) -> Result<()> {
        let mut builder = CreateChannel::new(&channel.name)
            .kind(match channel.kind.as_str() {
                "voice" => ChannelType::Voice,
                "category" => ChannelType::Category,
                _ => ChannelType::Text,
            })
            .position(channel.position as u16);
        if let Some(pid) = channel.parent_id {
            builder = builder.category(ChannelId::new(pid));
        }
        self.http
            .create_channel(GuildId::new(guild_id), &builder, None)
            .await?;
        Ok(())
    }

    async fn update_role(&self, guild_id: u64, role: &RoleSnapshot) -> Result<()> {
        let builder = EditRole::new()
            .name(&role.name)
            .permissions(Permissions::from_bits_truncate(role.permissions))
            .position(role.position as u16);
        self.http
            .edit_role(GuildId::new(guild_id), RoleId::new(role.id), &builder, None)
            .await?;
        Ok(())
    }

    async fn update_channel(&self, guild_id: u64, channel: &ChannelSnapshot) -> Result<()> {
        let mut builder = EditChannel::new()
            .name(&channel.name)
            .position(channel.position as u16);
        if let Some(pid) = channel.parent_id {
            builder = builder.category(ChannelId::new(pid));
        }
        self.http
            .edit_channel(ChannelId::new(channel.id), &builder, None)
            .await?;
        Ok(())
    }

    async fn delete_role(&self, guild_id: u64, role_id: u64) -> Result<()> {
        self.http
            .delete_role(GuildId::new(guild_id), RoleId::new(role_id), None)
            .await?;
        Ok(())
    }

    async fn delete_channel(&self, _guild_id: u64, channel_id: u64) -> Result<()> {
        self.http
            .delete_channel(ChannelId::new(channel_id), None)
            .await?;
        Ok(())
    }
}

/// Collect guild state via the provided [DiscordApi].
pub async fn take_snapshot(api: &impl DiscordApi, guild_id: u64) -> Result<GuildSnapshot> {
    let roles = api.fetch_roles(guild_id).await?;
    let channels = api.fetch_channels(guild_id).await?;
    Ok(GuildSnapshot { roles, channels })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn snapshot_roundtrip() {
        let snap = GuildSnapshot {
            roles: vec![RoleSnapshot {
                id: 1,
                name: "role".into(),
                position: 1,
                permissions: 0,
            }],
            channels: vec![ChannelSnapshot {
                id: 1,
                name: "chan".into(),
                kind: "text".into(),
                position: 1,
                parent_id: None,
            }],
        };
        let json = serde_json::to_string(&snap).unwrap();
        let de: GuildSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, de);
    }
}