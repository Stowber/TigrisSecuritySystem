use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::http::Request;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    middleware::{self, Next},
    response::IntoResponse,
    routing::{get, post},
};
use tokio::{
    net::TcpListener,
    time::{Duration, timeout},
};

use super::Antinuke;
use subtle::ConstantTimeEq;



/// Start simple HTTP server exposing healthcheck.
pub async fn serve(addr: SocketAddr, svc: Arc<Antinuke>) -> Result<()> {
    let authed = Router::new()
        .route("/guilds/:id/incidents", get(list_incidents))
        .route("/incidents/:id/approve", post(approve_incident))
        .route("/incidents/:id/restore", post(restore_incident))
        .layer(middleware::from_fn_with_state(svc.clone(), auth));
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(authed)
        .with_state(svc);
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn auth(
    State(svc): State<Arc<Antinuke>>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Result<impl IntoResponse, StatusCode> {
    let token = svc.ctx().settings.antinuke.api_token.as_deref();
    if token.is_none() || token == Some("") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let supplied = req
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok());
    if let Some(supplied) = supplied {
        if supplied.as_bytes().ct_eq(token.unwrap().as_bytes()).into() {
            Ok(next.run(req).await)
        } else {
            Err(StatusCode::UNAUTHORIZED)
        }
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

async fn list_incidents(
    State(svc): State<Arc<Antinuke>>,
    Path(guild_id): Path<u64>,
) -> impl IntoResponse {
    let secs = svc.ctx().settings.antinuke.api_timeout_seconds.unwrap_or(1);
    match timeout(Duration::from_secs(secs), svc.incidents(guild_id)).await {
        Ok(Ok(list)) => (StatusCode::OK, Json(list)).into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn approve_incident(
    State(svc): State<Arc<Antinuke>>,
    Path(incident_id): Path<i64>,
) -> StatusCode {
    let secs = svc.ctx().settings.antinuke.api_timeout_seconds.unwrap_or(1);
    match timeout(
        Duration::from_secs(secs),
        super::approve::approve(svc.ctx(), incident_id, 1),
    )
    .await
    {
        Ok(Ok(_)) => StatusCode::OK,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn restore_incident(
    State(svc): State<Arc<Antinuke>>,
    Path(incident_id): Path<i64>,
) -> StatusCode {
    let secs = svc.ctx().settings.antinuke.api_timeout_seconds.unwrap_or(1);
    match timeout(Duration::from_secs(secs), async {
        match crate::db::get_snapshot(&svc.ctx().db, incident_id).await {
            Ok(Some(snap)) => {
                let api = super::snapshot::SerenityApi { http: &svc.http };
                match super::restore::apply_snapshot(&api, svc.ctx(), 0, incident_id, &snap).await {
                    Ok(_) => StatusCode::OK,
                    Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
                }
            }
            Ok(None) => StatusCode::NOT_FOUND,
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    })
    .await
    {
        Ok(code) => code,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AppContext,
        config::{AntinukeConfig, App, ChatGuardConfig, Database, Discord, Logging, Settings},
    };
    use reqwest::StatusCode;
    use sqlx::postgres::PgPoolOptions;
    use tokio::time::Duration;

    fn ctx(token: Option<&str>) -> Arc<AppContext> {
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
            antinuke: AntinukeConfig {
                api_token: token.map(|t| t.into()),
                ..Default::default()
            },
        };
        let db = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy(&settings.database.url)
            .unwrap();
        AppContext::new_testing(settings, db)
    }

    #[tokio::test]
    async fn health_ok() {
        let ctx = ctx(Some("secret"));
        let svc = Antinuke::new(ctx);
        let addr: SocketAddr = ([127, 0, 0, 1], 50056).into();
        let handle = tokio::spawn(serve(addr, svc));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let res = reqwest::get(&format!("http://{addr}/health"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.text().await.unwrap(), "ok");
        handle.abort();
    }
#[tokio::test]
    async fn incidents_route_auth() {
        let ctx = ctx(Some("secret"));
        let svc = Antinuke::new(ctx);
        let addr: SocketAddr = ([127, 0, 0, 1], 50057).into();
        let handle = tokio::spawn(serve(addr, svc));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/guilds/1/incidents");
        let res = client.get(&url).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let res = client
            .get(&url)
            .header("Authorization", "secret")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        handle.abort();
    }

    #[tokio::test]
    async fn approve_route_auth() {
        let ctx = ctx(Some("secret"));
        let svc = Antinuke::new(ctx);
        let addr: SocketAddr = ([127, 0, 0, 1], 50058).into();
        let handle = tokio::spawn(serve(addr, svc));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/incidents/1/approve");
        let res = client.post(&url).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let res = client
            .post(&url)
            .header("Authorization", "secret")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        handle.abort();
    }

    #[tokio::test]
    async fn restore_route_auth() {
        let ctx = ctx(Some("secret"));
        let svc = Antinuke::new(ctx);
        let addr: SocketAddr = ([127, 0, 0, 1], 50059).into();
        let handle = tokio::spawn(serve(addr, svc));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/incidents/1/restore");
        let res = client.post(&url).send().await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let res = client
            .post(&url)
            .header("Authorization", "secret")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
        handle.abort();
    }
#[tokio::test]
    async fn incidents_route_no_token_config() {
        let ctx = ctx(None);
        let svc = Antinuke::new(ctx);
        let addr: SocketAddr = ([127, 0, 0, 1], 50060).into();
        let handle = tokio::spawn(serve(addr, svc));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/guilds/1/incidents");
        let res = client
            .get(&url)
            .header("Authorization", "secret")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        handle.abort();
    }
}