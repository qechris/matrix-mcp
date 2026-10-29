//! Matrix MCP server.
//!
//! Exposes Matrix chat operations (login, list rooms, read/send messages, join
//! rooms) as Model Context Protocol tools, built on the official `rmcp` and
//! `matrix-sdk` Rust SDKs.
//!
//! Two transports are supported, selected via `MATRIX_MCP_TRANSPORT`:
//!
//! * `stdio` (default) - classic stdio transport for local MCP clients.
//! * `http` / `sse`    - SSE-based streamable-HTTP transport served over a TCP
//!   socket, for remote/networked clients.

// The deeply-nested futures inside `matrix-sdk`'s send path push rustc's
// layout computation past the default query depth limit.
#![recursion_limit = "256"]

mod matrix;
mod server;

use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::{
    transport::{
        stdio,
        streamable_http_server::{
            session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
        },
    },
    ServiceExt,
};
use tracing_subscriber::EnvFilter;

use crate::matrix::MatrixManager;
use crate::server::MatrixServer;

const DEFAULT_HTTP_ADDRESS: &str = "127.0.0.1:8000";
const DEFAULT_HTTP_PATH: &str = "/mcp";

#[tokio::main]
async fn main() -> Result<()> {
    // IMPORTANT: in stdio mode stdout is reserved for the MCP JSON-RPC stream,
    // so all logging must go to stderr.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("matrix_mcp=info")),
        )
        .init();

    let manager = MatrixManager::from_env()?;

    match manager.try_restore().await {
        Ok(true) => tracing::info!("restored existing Matrix session"),
        Ok(false) => tracing::info!("no saved session found; use the `login` tool to authenticate"),
        Err(e) => tracing::warn!("could not restore saved session: {e:#}"),
    }

    if let Err(e) = manager.maybe_login_from_env().await {
        tracing::warn!("automatic login from environment failed: {e:#}");
    }

    let manager = Arc::new(manager);

    if let Some(interval) = auto_join_interval() {
        spawn_auto_join(manager.clone(), interval);
    }

    match transport_kind() {
        Transport::Stdio => serve_stdio(manager).await,
        Transport::Http => serve_http(manager).await,
    }
}

enum Transport {
    Stdio,
    Http,
}

fn transport_kind() -> Transport {
    match std::env::var("MATRIX_MCP_TRANSPORT")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "http" | "sse" | "streamable-http" | "streamable_http" => Transport::Http,
        _ => Transport::Stdio,
    }
}

/// `MATRIX_AUTO_JOIN` turns on the background auto-join loop: "true"/"1"
/// runs it every 60 seconds, a number sets the interval in seconds.
fn auto_join_interval() -> Option<std::time::Duration> {
    let value = std::env::var("MATRIX_AUTO_JOIN").ok()?;
    let secs = match value.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "no" | "off" => return None,
        "1" | "true" | "yes" | "on" => 60,
        other => other.parse().ok().filter(|s| *s > 0)?,
    };
    Some(std::time::Duration::from_secs(secs))
}

/// Periodically accept invites and join rooms in this account's spaces.
fn spawn_auto_join(manager: Arc<MatrixManager>, interval: std::time::Duration) {
    tracing::info!("auto-join enabled, every {}s", interval.as_secs());
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            match manager.auto_join().await {
                Ok(result) => {
                    let joined = result["joined"].as_array().map_or(0, Vec::len);
                    if joined > 0 {
                        tracing::info!("auto-join: {result}");
                    }
                }
                // Not logged in yet, or the server is briefly unreachable.
                Err(e) => tracing::debug!("auto-join skipped: {e:#}"),
            }
        }
    });
}

/// Serve over stdio (default).
async fn serve_stdio(manager: Arc<MatrixManager>) -> Result<()> {
    tracing::info!("starting Matrix MCP server on stdio");
    let service = MatrixServer::new(manager).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

/// Serve over the SSE-based streamable-HTTP transport.
async fn serve_http(manager: Arc<MatrixManager>) -> Result<()> {
    let address =
        std::env::var("MATRIX_MCP_ADDRESS").unwrap_or_else(|_| DEFAULT_HTTP_ADDRESS.to_string());
    let path = std::env::var("MATRIX_MCP_PATH").unwrap_or_else(|_| DEFAULT_HTTP_PATH.to_string());

    // One MatrixServer is created per session, all sharing the same underlying
    // Matrix client/state via the Arc.
    let service = StreamableHttpService::new(
        move || Ok(MatrixServer::new(manager.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );

    let app = axum::Router::new().nest_service(&path, service);
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .with_context(|| format!("failed to bind {address}"))?;

    tracing::info!("starting Matrix MCP server on http://{address}{path} (SSE streamable-HTTP)");

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("shutdown signal received");
        })
        .await
        .context("HTTP server error")?;
    Ok(())
}
