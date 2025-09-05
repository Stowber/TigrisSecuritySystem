use anyhow::Result;
use sqlx::{Pool, Postgres, postgres::PgPoolOptions};


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