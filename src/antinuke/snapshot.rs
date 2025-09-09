use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Simplified snapshot of guild state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GuildSnapshot {
    pub roles: Vec<String>,
    pub channels: Vec<String>,
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
            roles: vec!["role".into()],
            channels: vec!["chan".into()],
        };
        let json = serde_json::to_string(&snap).unwrap();
        let de: GuildSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, de);
    }
}