use anyhow::Result;
use serde::{Deserialize, Serialize};

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

/// Collect guild state and return snapshot. Real implementation would query Discord.
pub async fn take_snapshot(_guild_id: u64) -> Result<GuildSnapshot> {
    Ok(GuildSnapshot {
        roles: vec![],
        channels: vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn snapshot_roundtrip() {
        let snap = GuildSnapshot {
            roles: vec![RoleSnapshot { id: 1, name: "role".into(), position: 1, permissions: 0 }],
            channels: vec![ChannelSnapshot { id: 1, name: "chan".into(), kind: "text".into(), position: 1, parent_id: None }],
        };
        let json = serde_json::to_string(&snap).unwrap();
        let de: GuildSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, de);
    }
}