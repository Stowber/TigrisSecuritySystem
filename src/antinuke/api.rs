use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::{Router, routing::{get, post}, extract::{State, Path}, Json};
use serde::Serialize;

use super::{Antinuke, restore, snapshot};



/// Start simple HTTP server exposing healthcheck.
pub async fn serve(addr: SocketAddr, _svc: Arc<Antinuke>) -> Result<()> {
    let app = Router::new().route("/health", get(|| async { "ok" }));
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await?;
    Ok(())
}