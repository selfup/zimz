// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! Streamable HTTP transport for local and LAN use: one warm process shared by every
//! client, protected by a bearer token.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use crate::{BoxError, ZimServer};

/// How to serve over HTTP.
#[derive(Debug, Clone)]
pub struct HttpOptions {
    /// Address to listen on. Anything but a loopback address requires `token`.
    pub bind: SocketAddr,
    /// Bearer token every MCP request must carry (`Authorization: Bearer …`).
    pub token: Option<String>,
    /// Extra `Host` values accepted besides the loopback names (DNS-rebinding guard).
    /// With a token and a non-loopback bind, host validation is disabled unless this
    /// list is given, because the token already authenticates the caller.
    pub allowed_hosts: Vec<String>,
    /// Browser origins allowed to call the server; empty disables origin checks.
    pub allowed_origins: Vec<String>,
    /// URL path of the MCP endpoint.
    pub path: String,
}

impl Default for HttpOptions {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 8765)),
            token: None,
            allowed_hosts: Vec::new(),
            allowed_origins: Vec::new(),
            path: "/mcp".to_string(),
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn require_token(State(token): State<Arc<String>>, req: Request, next: Next) -> Response {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim);
    if presented.is_some_and(|t| constant_time_eq(t.as_bytes(), token.as_bytes())) {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "unauthorized: send `Authorization: Bearer <token>`",
        )
            .into_response()
    }
}

/// The axum application: the MCP endpoint (token-protected when a token is set) and an
/// open `GET /healthz`.
pub fn router(server: ZimServer, opts: &HttpOptions) -> Result<Router, BoxError> {
    if !opts.bind.ip().is_loopback() && opts.token.is_none() {
        return Err(format!(
            "binding to {} exposes the library beyond this machine; set --token (or ZIMZ_TOKEN)",
            opts.bind
        )
        .into());
    }
    let archives = server.library().len();
    // Stateless: every request is self-contained (no server-initiated messages), so
    // plain JSON responses work and no session bookkeeping is needed.
    let mut config = StreamableHttpServerConfig::default();
    config.legacy_session_mode = false;
    config.json_response = true;
    config.allowed_origins.clone_from(&opts.allowed_origins);
    if !opts.allowed_hosts.is_empty() {
        config
            .allowed_hosts
            .extend(opts.allowed_hosts.iter().cloned());
    } else if !opts.bind.ip().is_loopback() {
        config = config.disable_allowed_hosts();
    }
    let mcp = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let mut app = Router::new().nest_service(&opts.path, mcp);
    if let Some(token) = &opts.token {
        app = app.layer(middleware::from_fn_with_state(
            Arc::new(token.clone()),
            require_token,
        ));
    }
    let app =
        app.route(
            "/healthz",
            get(move || async move {
                axum::Json(serde_json::json!({ "ok": true, "archives": archives }))
            }),
        );
    Ok(app)
}

/// Serve until Ctrl-C.
pub fn run(server: ZimServer, opts: &HttpOptions) -> Result<(), BoxError> {
    let app = router(server, opts)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(opts.bind).await?;
        let addr = listener.local_addr()?;
        tracing::info!(
            "zimz MCP server listening on http://{addr}{} ({})",
            opts.path,
            if opts.token.is_some() {
                "bearer token required"
            } else {
                "no token: loopback only"
            }
        );
        tracing::info!(
            "register with: claude mcp add --transport http zimz http://{addr}{}{}",
            opts.path,
            if opts.token.is_some() {
                " --header \"Authorization: Bearer <token>\""
            } else {
                ""
            }
        );
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("shutting down");
            })
            .await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}
