use anyhow::Result;
use sqlx::{Pool, Postgres, postgres::PgPoolOptions};
use serde_json::Value;


pub type Db = Pool<Postgres>;

pub async fn connect(url: &str, max: Option<u32>) -> Result<Db> {
    let pool = PgPoolOptions::new()
        .max_connections(max.unwrap_or(10))
        .connect(url)
        .await?;


    Ok(pool)
}


pub async fn migrate(pool: &Db) -> Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}

/// Insert new incident and return its id.
pub async fn create_incident(db: &Db, guild_id: u64, reason: &str) -> Result<i64> {
    let mut tx = db.begin().await?;
    sqlx::query("INSERT INTO tss.antinuke_guilds (guild_id) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(guild_id as i64)
        .execute(&mut *tx)
        .await?;
    let rec: (i64,) = sqlx::query_as(
        "INSERT INTO tss.antinuke_incidents (guild_id, reason) VALUES ($1, $2) RETURNING id",
    )
    .bind(guild_id as i64)
    .bind(reason)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rec.0)
}

/// Record action related to incident.
pub async fn insert_action(
    db: &Db,
    incident_id: i64,
    kind: &str,
    actor_id: Option<u64>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO tss.antinuke_actions (incident_id, actor_id, kind) VALUES ($1, $2, $3)",
    )
    .bind(incident_id)
    .bind(actor_id.map(|v| v as i64))
    .bind(kind)
    .execute(db)
    .await?;
    Ok(())
}

/// Store snapshot JSON for incident.
pub async fn insert_snapshot(db: &Db, incident_id: i64, data: &Value) -> Result<()> {
     sqlx::query("INSERT INTO tss.antinuke_snapshots (incident_id, data) VALUES ($1, $2)")
        .bind(incident_id)
        .bind(data)
        .execute(db)
        .await?;
    Ok(())
}

/// Fetch snapshot JSON for incident.
pub async fn get_snapshot(
    db: &Db,
    incident_id: i64,
) -> Result<Option<crate::antinuke::snapshot::GuildSnapshot>> {
    let row: Option<(Value,)> =
        sqlx::query_as("SELECT data FROM tss.antinuke_snapshots WHERE incident_id = $1")
            .bind(incident_id)
            .fetch_optional(db)
            .await?;
    match row {
        Some((val,)) => {
            let snap = serde_json::from_value(val)?;
            Ok(Some(snap))
        }
        None => Ok(None),
    }
}

/// List incidents for guild (id, reason).
pub async fn list_incidents(db: &Db, guild_id: u64) -> Result<Vec<(i64, String)>> {
    
    let rows = sqlx::query_as::<_, (i64, String)>(
        "SELECT id, reason FROM tss.antinuke_incidents WHERE guild_id = $1 ORDER BY id DESC",
    )
    .bind(guild_id as i64)
    .fetch_all(db)
    .await?;
    Ok(rows)
}