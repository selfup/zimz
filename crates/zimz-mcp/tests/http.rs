// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! The HTTP transport exercised through the axum router without opening a socket:
//! token gate, health endpoint, an MCP session (initialize → initialized → tools/call).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use zimz_mcp::{HttpOptions, ServerOptions, ZimServer};
use zimz_search::{Library, LibraryConfig};

const TOKEN: &str = "s3cret-token";

fn fixture(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/zim-testing-suite/data")
        .join(name);
    assert!(p.is_file(), "fixture missing: {}", p.display());
    p
}

fn app(token: Option<&str>, bind: &str) -> Result<Router, zimz_mcp::BoxError> {
    let library = Library::scan(LibraryConfig {
        files: vec![
            fixture("withns/wikipedia_en_climate_change_mini_2024-06.zim"),
            fixture("nons/small.zim"),
        ],
        ..LibraryConfig::default()
    })
    .unwrap();
    let server = ZimServer::with_options(
        Arc::new(library),
        ServerOptions {
            allow_full_verify: false,
        },
    );
    zimz_mcp::http::router(
        server,
        &HttpOptions {
            bind: bind.parse().unwrap(),
            token: token.map(str::to_string),
            ..HttpOptions::default()
        },
    )
}

async fn body_json(res: axum::response::Response) -> Value {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&bytes)))
}

fn mcp_post(body: &Value, token: Option<&str>, session: Option<&str>) -> Request<Body> {
    let mut b = Request::post("/mcp")
        .header(header::HOST, "localhost")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    if let Some(s) = session {
        b = b.header("Mcp-Session-Id", s);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

#[tokio::test]
async fn healthz_is_open_and_mcp_needs_the_token() {
    let app = app(Some(TOKEN), "127.0.0.1:0").unwrap();
    let res = app
        .clone()
        .oneshot(
            Request::get("/healthz")
                .header(header::HOST, "localhost")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["ok"], true);
    assert_eq!(v["archives"], 2);

    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}});
    let res = app
        .clone()
        .oneshot(mcp_post(&init, None, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers().get(header::WWW_AUTHENTICATE).unwrap(),
        "Bearer"
    );
    let res = app
        .clone()
        .oneshot(mcp_post(&init, Some("wrong"), None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mcp_session_over_http() {
    let app = app(Some(TOKEN), "127.0.0.1:0").unwrap();
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}});
    let res = app
        .clone()
        .oneshot(mcp_post(&init, Some(TOKEN), None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "{:?}", res.headers());
    assert!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|c| c.to_str().unwrap().starts_with("application/json")),
        "stateless mode answers with plain JSON: {:?}",
        res.headers()
    );
    // Stateless servers issue no session id; pass one back only if given.
    let session = res
        .headers()
        .get("Mcp-Session-Id")
        .map(|v| v.to_str().unwrap().to_string());
    let v = body_json(res).await;
    assert_eq!(v["result"]["serverInfo"]["name"], "zimz");
    assert!(v["result"]["capabilities"]["tools"].is_object());

    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let res = app
        .clone()
        .oneshot(mcp_post(&initialized, Some(TOKEN), session.as_deref()))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::ACCEPTED);

    let call = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
        "name": "search", "arguments": {"query": "\"carbon dioxide\"", "limit": 3, "snippet_chars": 0}}});
    let res = app
        .clone()
        .oneshot(mcp_post(&call, Some(TOKEN), session.as_deref()))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let sc = &v["result"]["structuredContent"];
    assert_eq!(sc["phrases"], json!(["carbon dioxide"]));
    assert_eq!(sc["hits"].as_array().unwrap().len(), 3);
    assert_eq!(sc["hits"][0]["title"], "Carbon dioxide");

    // Full verification is disabled on HTTP.
    let call = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
        "name": "archive_health", "arguments": {"archive": "small", "verify": "full"}}});
    let res = app
        .clone()
        .oneshot(mcp_post(&call, Some(TOKEN), session.as_deref()))
        .await
        .unwrap();
    let v = body_json(res).await;
    assert_eq!(v["result"]["isError"], true);
    assert!(
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("disabled")
    );
}

#[tokio::test]
async fn loopback_without_token_is_allowed_but_lan_is_not() {
    assert!(app(None, "127.0.0.1:0").is_ok());
    let err = app(None, "0.0.0.0:0").unwrap_err().to_string();
    assert!(err.contains("--token"), "{err}");
    assert!(app(Some(TOKEN), "0.0.0.0:0").is_ok());
}
