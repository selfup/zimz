// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! End-to-end MCP tests: a real rmcp client talks to the server over an in-memory
//! duplex pipe, so the wire format, schemas and tool results are all exercised.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ReadResourceRequestParams, ResourceContents,
};
use rmcp::service::{RoleClient, RunningService};
use serde_json::{Value, json};
use zimz_mcp::ZimServer;
use zimz_search::{Library, LibraryConfig};

const CLIMATE: &str = "wikipedia_en_climate_change";

fn fixture(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/zim-testing-suite/data")
        .join(name);
    assert!(
        p.is_file(),
        "fixture missing: {} (run scripts/fetch-fixtures.sh)",
        p.display()
    );
    p
}

fn library() -> Arc<Library> {
    Arc::new(
        Library::scan(LibraryConfig {
            files: vec![
                fixture("withns/wikipedia_en_climate_change_mini_2024-06.zim"),
                fixture("nons/small.zim"),
            ],
            ..LibraryConfig::default()
        })
        .unwrap(),
    )
}

async fn connect() -> RunningService<RoleClient, ()> {
    let (client_io, server_io) = tokio::io::duplex(1 << 20);
    let (sr, sw) = tokio::io::split(server_io);
    let (cr, cw) = tokio::io::split(client_io);
    // `serve` completes the initialize handshake, so the server must run in its own
    // task before the client starts talking.
    let server = ZimServer::new(library());
    tokio::spawn(async move {
        let running = server.serve((sr, sw)).await.expect("server start");
        let _ = running.waiting().await;
    });
    ().serve((cr, cw)).await.expect("client start")
}

async fn call(client: &RunningService<RoleClient, ()>, name: &str, args: Value) -> CallToolResult {
    let arguments = args.as_object().cloned().unwrap_or_default();
    client
        .call_tool(CallToolRequestParams::new(name.to_string()).with_arguments(arguments))
        .await
        .unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn text_of(r: &CallToolResult) -> String {
    r.content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn initialize_advertises_tools_resources_and_instructions() {
    let client = connect().await;
    let info = client.peer_info().expect("server info");
    assert_eq!(info.server_info.as_ref().unwrap().name, "zimz");
    assert!(info.capabilities.tools.is_some());
    assert!(info.capabilities.resources.is_some());
    assert!(
        info.instructions
            .as_deref()
            .unwrap()
            .contains("read_article")
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn tool_list_is_complete_annotated_and_matches_snapshot() {
    let client = connect().await;
    let tools = client.list_all_tools().await.unwrap();
    let mut names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "archive_health",
            "context",
            "links",
            "list_archives",
            "outline",
            "read_article",
            "search",
            "suggest",
        ]
    );
    for t in &tools {
        let a = t
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("{} has no annotations", t.name));
        assert_eq!(a.read_only_hint, Some(true), "{}", t.name);
        assert_eq!(a.idempotent_hint, Some(true), "{}", t.name);
        assert_eq!(a.open_world_hint, Some(false), "{}", t.name);
        assert!(t.output_schema.is_some(), "{} has no output schema", t.name);
        assert!(
            t.description.as_ref().is_some_and(|d| d.len() > 40),
            "{}",
            t.name
        );
        assert_eq!(
            t.input_schema.get("type").and_then(Value::as_str),
            Some("object"),
            "{}",
            t.name
        );
    }
    // Field docs reach the schema so agents see what parameters mean.
    let search = tools.iter().find(|t| t.name == "search").unwrap();
    let props = search.input_schema.get("properties").unwrap();
    assert!(props.get("query").is_some());
    assert!(
        props["snippet_chars"]["description"]
            .as_str()
            .unwrap()
            .contains("snippet")
    );

    let mut sorted = tools.clone();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let rendered = serde_json::to_string_pretty(&sorted).unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/tools.json");
    if std::env::var_os("ZIMZ_UPDATE_SNAPSHOTS").is_some() || !path.exists() {
        std::fs::write(&path, format!("{rendered}\n")).unwrap();
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        expected.trim_end(),
        rendered,
        "tool schemas changed; rerun with ZIMZ_UPDATE_SNAPSHOTS=1 to accept"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn list_archives_and_search_return_text_and_structured_content() {
    let client = connect().await;
    let r = call(&client, "list_archives", json!({})).await;
    assert_eq!(r.is_error, Some(false));
    let s = r.structured_content.as_ref().unwrap();
    assert_eq!(s["count"], 2);
    assert!(text_of(&r).contains(CLIMATE));

    let r = call(&client, "list_archives", json!({"filter": "climate"})).await;
    assert_eq!(r.structured_content.unwrap()["count"], 1);

    let r = call(
        &client,
        "search",
        json!({"query": "greenhouse gas emissions", "archives": [CLIMATE], "limit": 3}),
    )
    .await;
    assert_eq!(r.is_error, Some(false));
    let s = r.structured_content.as_ref().unwrap();
    assert_eq!(s["hits"].as_array().unwrap().len(), 3);
    assert_eq!(s["hits"][0]["title"], "Greenhouse gas emissions");
    assert_eq!(s["hits"][0]["mode"], "fulltext");
    assert!(s["next_cursor"].is_string());
    let text = text_of(&r);
    assert!(text.contains("1. Greenhouse gas emissions"), "{text}");
    assert!(text.contains("next_cursor:"), "{text}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn read_article_outline_links_suggest_and_health() {
    let client = connect().await;
    let r = call(
        &client,
        "read_article",
        json!({"archive": CLIMATE, "path": "Carbon dioxide", "max_chars": 400}),
    )
    .await;
    assert_eq!(r.is_error, Some(false));
    let s = r.structured_content.as_ref().unwrap();
    assert_eq!(s["title"], "Carbon dioxide");
    assert_eq!(s["truncated"], true);
    assert_eq!(s["content"].as_str().unwrap().chars().count(), 400);
    assert!(s["outline"].as_array().is_some_and(|o| !o.is_empty()));
    let text = text_of(&r);
    assert!(text.starts_with("# Carbon dioxide"), "{text}");
    assert!(text.contains("pick a section"), "{text}");

    let r = call(
        &client,
        "outline",
        json!({"archive": CLIMATE, "path": "A/Carbon_dioxide"}),
    )
    .await;
    let s = r.structured_content.as_ref().unwrap();
    assert!(!s["sections"].as_array().unwrap().is_empty());

    let r = call(
        &client,
        "read_article",
        json!({"archive": CLIMATE, "path": "A/Carbon_dioxide", "section": "0"}),
    )
    .await;
    assert_eq!(r.is_error, Some(false));
    assert!(r.structured_content.unwrap()["section"].is_string());

    let r = call(
        &client,
        "links",
        json!({"archive": CLIMATE, "path": "A/Carbon_dioxide", "limit": 5}),
    )
    .await;
    let s = r.structured_content.as_ref().unwrap();
    assert_eq!(s["links"].as_array().unwrap().len(), 5);
    assert!(text_of(&r).contains("internal link(s)"));

    let r = call(
        &client,
        "suggest",
        json!({"prefix": "carbon d", "limit": 5}),
    )
    .await;
    let s = r.structured_content.as_ref().unwrap();
    let first = s["suggestions"][0]["title"]
        .as_str()
        .unwrap()
        .to_lowercase();
    assert!(first.starts_with("carbon d"), "{first}");

    let r = call(
        &client,
        "archive_health",
        json!({"archive": "small", "verify": "quick"}),
    )
    .await;
    let s = r.structured_content.as_ref().unwrap();
    assert_eq!(s["archives"].as_array().unwrap().len(), 1);
    assert_eq!(s["archives"][0]["problems"].as_array().unwrap().len(), 0);
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn context_packs_cited_excerpts() {
    let client = connect().await;
    let r = call(
        &client,
        "context",
        json!({"query": "carbon dioxide atmosphere", "budget_chars": 2500, "per_hit_chars": 500, "max_hits": 3}),
    )
    .await;
    assert_eq!(r.is_error, Some(false));
    let s = r.structured_content.as_ref().unwrap();
    let excerpts = s["excerpts"].as_array().unwrap();
    assert!(!excerpts.is_empty() && excerpts.len() <= 3);
    assert!(s["chars_used"].as_u64().unwrap() <= 2500);
    assert!(excerpts[0]["uri"].as_str().unwrap().starts_with("zim://"));
    let text = text_of(&r);
    assert!(text.contains("Source: zim://"), "{text}");
    assert!(text.contains("excerpt(s)"), "{text}");
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn errors_are_tool_errors_not_protocol_errors() {
    let client = connect().await;
    let r = call(
        &client,
        "read_article",
        json!({"archive": "nope", "path": "x"}),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    assert!(
        text_of(&r).contains("no archive matches"),
        "{}",
        text_of(&r)
    );
    let r = call(
        &client,
        "read_article",
        json!({"archive": CLIMATE, "path": "Definitely_missing"}),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    assert!(text_of(&r).contains("not found"));
    let r = call(&client, "search", json!({"query": "   "})).await;
    assert_eq!(r.is_error, Some(true));
    // Schema violations are reported as tool errors too (rmcp turns them into isError).
    let r = call(&client, "search", json!({"limit": 3})).await;
    assert_eq!(r.is_error, Some(true));
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn resources_list_and_read() {
    let client = connect().await;
    let list = client.list_resources(None).await.unwrap();
    let uris: Vec<&str> = list.resources.iter().map(|r| r.uri.as_str()).collect();
    assert!(
        uris.contains(&format!("zim://{CLIMATE}").as_str()),
        "{uris:?}"
    );
    let templates = client.list_resource_templates(None).await.unwrap();
    assert!(
        templates
            .resource_templates
            .iter()
            .any(|t| t.uri_template == "zim://{archive}/{path}")
    );

    let r = client
        .read_resource(ReadResourceRequestParams::new(format!(
            "zim://{CLIMATE}/A/Carbon_dioxide"
        )))
        .await
        .unwrap();
    let ResourceContents::TextResourceContents {
        text, mime_type, ..
    } = &r.contents[0]
    else {
        panic!("expected text");
    };
    assert_eq!(mime_type.as_deref(), Some("text/markdown"));
    assert!(text.contains("Carbon dioxide"));
    assert!(text.len() > 2000);

    let r = client
        .read_resource(ReadResourceRequestParams::new(format!(
            "zim://{CLIMATE}/Carbon_dioxide?format=html"
        )))
        .await
        .unwrap();
    let ResourceContents::TextResourceContents {
        text, mime_type, ..
    } = &r.contents[0]
    else {
        panic!("expected text");
    };
    assert_eq!(mime_type.as_deref(), Some("text/html"));
    assert!(text.contains('<'));

    let r = client
        .read_resource(ReadResourceRequestParams::new(format!("zim://{CLIMATE}")))
        .await
        .unwrap();
    let ResourceContents::TextResourceContents { text, .. } = &r.contents[0] else {
        panic!("expected text");
    };
    let v: Value = serde_json::from_str(text).unwrap();
    assert_eq!(v["name"], CLIMATE);

    assert!(
        client
            .read_resource(ReadResourceRequestParams::new("zim://nope/x"))
            .await
            .is_err()
    );
    assert!(
        client
            .read_resource(ReadResourceRequestParams::new("http://example.com"))
            .await
            .is_err()
    );
    client.cancel().await.unwrap();
}
