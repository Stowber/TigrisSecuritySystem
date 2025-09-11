use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::{routing::get, Router};
use tokio::net::TcpListener;

use super::Antinuke;



/// Start simple HTTP server exposing healthcheck.
pub async fn serve(addr: SocketAddr, svc: Arc<Antinuke>) -> Result<()> {
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .with_state(svc);
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{App, ChatGuardConfig, Database, Discord, Logging, Settings},
        AppContext,
    };
    use reqwest::StatusCode;
    use sqlx::postgres::PgPoolOptions;
    use tokio::time::Duration;

    fn ctx() -> Arc<AppContext> {
        let settings = Settings {
            env: "test".into(),
            app: App { name: "test".into() },
            discord: Discord { token: String::new(), app_id: None, intents: vec![] },
            database: Database {
                url: "postgres://localhost:1/test?connect_timeout=1".into(),
                max_connections: Some(1),
                statement_timeout_ms: Some(5_000),
            },
            logging: Logging { json: Some(false), level: Some("info".into()) },
            chatguard: ChatGuardConfig { racial_slurs: vec![] },
            antinuke: Default::default(),
        };
        let db = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy(&settings.database.url)
            .unwrap();
        AppContext::new_testing(settings, db)
    }

    #[tokio::test]
    async fn health_ok() {
        let ctx = ctx();
        let svc = Antinuke::new(ctx);
        let addr: SocketAddr = ([127, 0, 0, 1], 50056).into();
        let handle = tokio::spawn(serve(addr, svc));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let res = reqwest::get(&format!("http://{addr}/health")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text().await.unwrap(), "ok");
        handle.abort();
    }
}