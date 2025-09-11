use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::{routing::get, Router};
use tokio::net::TcpListener;

use super::Antinuke;



/// Start simple HTTP server exposing healthcheck.
pub async fn serve(addr: SocketAddr, _svc: Arc<Antinuke>) -> Result<()> {
    let app = Router::new().route("/health", get(|| async { "ok" }));
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}