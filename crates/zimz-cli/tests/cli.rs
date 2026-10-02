// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! End-to-end CLI tests: the real `zimz` binary is spawned against the zim-testing-suite
//! fixtures, so argument parsing, output rendering, the stdout/stderr split and exit
//! codes are all exercised. Only the single-file commands run on the fixture paths as-is;
//! the directory commands get a temp directory of symlinks so archive names stay stable.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CLIMATE: &str = "nons/wikipedia_en_climate_change_mini_2024-06.zim";
const SMALL: &str = "nons/small.zim";

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

/// Run `zimz` with `args`, asserting the exit code. Returns the raw output so callers can
/// check stdout, stderr and status.
fn zimz(args: &[&str], expect_ok: bool) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_zimz"))
        .args(args)
        .output()
        .expect("spawn zimz");
    let code = out.status.code();
    if expect_ok {
        assert_eq!(
            code,
            Some(0),
            "expected success: {args:?}\n{}",
            stderr(&out)
        );
    } else {
        assert!(
            !out.status.success(),
            "expected failure: {args:?}\nstdout: {}",
            stdout(&out)
        );
    }
    out
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A temp directory linking the climate archive under a stable name so the catalogue
/// name (from the `Name` metadata, else the file stem) does not depend on the fixture
/// file name.
fn library_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(fixture(CLIMATE), dir.path().join("climate.zim")).unwrap();
    std::os::unix::fs::symlink(fixture(SMALL), dir.path().join("small.zim")).unwrap();
    dir
}

#[test]
fn info_reports_header_metadata_and_indexes() {
    let f = fixture(CLIMATE);
    let out = zimz(&["info", f.to_str().unwrap()], true);
    let s = stdout(&out);
    assert!(s.contains("zim version: 6.2"), "{s}");
    assert!(s.contains("new C/M/W/X"), "{s}");
    assert!(s.contains("namespaces:  C M W X"), "{s}");
    assert!(s.contains("fulltext idx:"), "{s}");
    assert!(s.contains("title idx:"), "{s}");
    assert!(s.contains("metadata:"), "{s}");
    assert!(s.contains("counter (top):"), "{s}");
    // Text goes to stdout only; diagnostics never pollute the payload.
    assert!(stderr(&out).is_empty(), "{}", stderr(&out));
}

#[test]
fn ls_prints_path_title_and_kind() {
    let f = fixture(SMALL);
    let out = zimz(
        &["ls", f.to_str().unwrap(), "--ns", "C", "--limit", "5"],
        true,
    );
    let s = stdout(&out);
    assert!(s.contains("C/main.html"), "{s}");
    assert!(s.contains("text/html"), "{s}");
    // Redirects and namespaces render distinctly.
    let out = zimz(&["ls", f.to_str().unwrap(), "--limit", "0"], true);
    let all = stdout(&out);
    assert!(all.contains("M/Counter"), "{all}");
    assert!(all.contains("-> #"), "redirects show a target: {all}");
}

#[test]
fn cat_writes_entry_bytes_to_stdout() {
    let f = fixture(SMALL);
    let out = zimz(&["cat", f.to_str().unwrap(), "C/main.html"], true);
    // Raw bytes go to stdout unmodified: a real HTML document, not a rendering.
    assert!(
        stdout(&out).contains("<title>Test ZIM file</title>"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn check_passes_on_a_valid_archive() {
    let f = fixture(SMALL);
    let out = zimz(&["check", f.to_str().unwrap()], true);
    assert!(stdout(&out).contains("ok:"), "{}", stdout(&out));
}

#[test]
fn extract_outline_lists_headings() {
    let f = fixture(CLIMATE);
    let out = zimz(
        &["extract", f.to_str().unwrap(), "Carbon_cycle", "--outline"],
        true,
    );
    let s = stdout(&out);
    assert!(s.contains("Carbon cycle"), "{s}");
    assert!(s.contains("chars)"), "{s}");
}

#[test]
fn search_uses_one_archive_index() {
    let f = fixture(CLIMATE);
    let out = zimz(&["search", f.to_str().unwrap(), "carbon", "-n", "3"], true);
    let s = stdout(&out);
    assert!(s.contains("matches for \"carbon\""), "{s}");
    assert!(s.contains("Permafrost_carbon_cycle"), "{s}");
    // The percent column and 1-based rank are part of the contract.
    assert!(s.contains("  1. "), "{s}");
}

#[test]
fn suggest_completes_titles() {
    let f = fixture(CLIMATE);
    let out = zimz(&["suggest", f.to_str().unwrap(), "carbo", "-n", "3"], true);
    let s = stdout(&out);
    assert!(s.contains("matching titles for \"carbo\""), "{s}");
    assert!(s.contains("Black carbon"), "{s}");
}

#[test]
fn archives_lists_the_catalogue_with_and_without_json() {
    let dir = library_dir();
    let d = dir.path().to_str().unwrap();

    let text = stdout(&zimz(&["archives", "--zim-dir", d], true));
    assert!(text.contains("archive(s)"), "{text}");
    assert!(text.contains("- wikipedia_en_climate_change:"), "{text}");
    assert!(text.contains("fulltext"), "{text}");

    let json = stdout(&zimz(&["archives", "--zim-dir", d, "--json"], true));
    let v: serde_json::Value = serde_json::from_str(&json).expect("archives --json is valid JSON");
    let arr = v["archives"].as_array().expect("archives array");
    assert_eq!(arr.len(), 2);
    assert!(
        arr.iter()
            .any(|a| a["name"] == "wikipedia_en_climate_change")
    );
}

#[test]
fn directory_search_renders_text_and_json() {
    let dir = library_dir();
    let d = dir.path().to_str().unwrap();

    let text = stdout(&zimz(&["search", d, "carbon", "-n", "3"], true));
    assert!(text.contains("Permafrost carbon cycle"), "{text}");

    let json = stdout(&zimz(&["search", d, "carbon", "--json"], true));
    let v: serde_json::Value = serde_json::from_str(&json).expect("search --json is valid JSON");
    assert_eq!(v["query"], "carbon");
    let hits = v["hits"].as_array().expect("hits array");
    let empty: Vec<serde_json::Value> = Vec::new();
    assert_ne!(hits, &empty);
    // Citations are part of the interface agents rely on.
    assert!(
        hits[0]["uri"].as_str().unwrap().starts_with("zim://"),
        "{json}"
    );
}

#[test]
fn context_packs_cited_excerpts() {
    let dir = library_dir();
    let d = dir.path().to_str().unwrap();
    let out = zimz(
        &[
            "context",
            d,
            "carbon cycle",
            "--max-hits",
            "2",
            "--budget",
            "800",
        ],
        true,
    );
    let s = stdout(&out);
    assert!(s.contains("Source: zim://"), "{s}");
    assert!(s.contains("excerpt(s)"), "{s}");
}

#[test]
fn broken_pipe_is_not_an_error() {
    // `zimz ls … | head` closes stdout early; the CLI must exit 0, not report a
    // BrokenPipe. `ls --limit 0` on the climate archive prints thousands of lines.
    let f = fixture(CLIMATE);
    let mut child = Command::new(env!("CARGO_BIN_EXE_zimz"))
        .args(["ls", f.to_str().unwrap(), "--limit", "0"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn zimz");
    // Read a little, then drop the pipe to close it under the writer.
    {
        use std::io::Read;
        let mut buf = [0u8; 64];
        let out = child.stdout.as_mut().unwrap();
        let _ = out.read(&mut buf).unwrap();
    }
    drop(child.stdout.take());
    let status = child.wait().expect("wait zimz");
    assert_eq!(status.code(), Some(0), "broken pipe should exit 0");
}

#[test]
fn library_errors_are_reported_on_stderr_with_exit_1() {
    for (args, needle) in [
        (vec!["info", "/nonexistent.zim"], "No such file"),
        (vec!["archives"], "give at least one --zim-dir"),
    ] {
        let out = zimz(&args, false);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let err = stderr(&out);
        assert!(err.contains(needle), "{args:?} -> {err}");
        assert!(stdout(&out).is_empty(), "{args:?} wrote to stdout");
    }
}

#[test]
fn missing_entry_and_empty_query_are_errors() {
    let f = fixture(SMALL);
    let out = zimz(&["cat", f.to_str().unwrap(), "C/DoesNotExist"], false);
    assert!(stderr(&out).contains("entry not found"), "{}", stderr(&out));

    let dir = library_dir();
    let out = zimz(&["search", dir.path().to_str().unwrap(), ""], false);
    assert!(stderr(&out).contains("query is empty"), "{}", stderr(&out));
}

#[test]
fn bad_cursor_is_rejected() {
    let dir = library_dir();
    let out = zimz(
        &[
            "search",
            dir.path().to_str().unwrap(),
            "carbon",
            "--cursor",
            "garbage",
        ],
        false,
    );
    assert!(stderr(&out).contains("cursor"), "{}", stderr(&out));
}

#[test]
fn no_subcommand_prints_usage_to_stderr() {
    let out = zimz(&[], false);
    // clap's usage error is exit 2, distinct from a library error's exit 1.
    assert_eq!(out.status.code(), Some(2));
    let combined = format!("{}{}", stdout(&out), stderr(&out));
    assert!(combined.contains("Usage: zimz"), "{combined}");
    assert!(combined.contains("search"), "{combined}");
}

/// Drive `zimz mcp` over stdio: write the handshake and `requests`, close stdin (which
/// ends the session), then return the parsed responses keyed by their JSON-RPC id.
/// stdout must stay pure protocol; logs and the library scan warnings go to stderr.
fn mcp_session(dir: &Path, requests: &[serde_json::Value]) -> (Vec<serde_json::Value>, String) {
    let mut input = String::new();
    input.push_str(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"e2e","version":"0"}}}"#,
    );
    input.push('\n');
    input.push_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    input.push('\n');
    for (i, r) in requests.iter().enumerate() {
        let id = i + 2;
        let mut v = r.clone();
        v["jsonrpc"] = serde_json::json!("2.0");
        v["id"] = serde_json::json!(id);
        input.push_str(&v.to_string());
        input.push('\n');
    }

    let mut child = Command::new(env!("CARGO_BIN_EXE_zimz"))
        .args(["mcp", "--zim-dir", dir.to_str().unwrap()])
        // Pin the log level so the ready line on stderr is deterministic even when the
        // developer's environment sets RUST_LOG.
        .env("ZIMZ_LOG", "info")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn zimz mcp");
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    // Dropping the handle closes stdin; the server finishes the session and exits.
    drop(child.stdin.take());
    let out = child.wait_with_output().expect("wait zimz mcp");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let responses = stdout(&out)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("non-JSON on stdout: {e}: {l}")))
        .collect();
    (responses, stderr(&out))
}

fn by_id(responses: &[serde_json::Value], id: u64) -> serde_json::Value {
    responses
        .iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("no response with id {id}"))
        .clone()
}

#[test]
fn mcp_stdio_handshakes_and_calls_tools() {
    let dir = library_dir();
    let (res, err) = mcp_session(
        dir.path(),
        &[
            serde_json::json!({"method": "tools/list"}),
            serde_json::json!({
                "method": "tools/call",
                "params": {"name": "list_archives", "arguments": {}}
            }),
            serde_json::json!({
                "method": "tools/call",
                "params": {"name": "search", "arguments": {"query": "carbon", "limit": 2}}
            }),
        ],
    );

    // The handshake advertises the tool capability and the server identity.
    let init = by_id(&res, 1);
    assert_eq!(init["result"]["serverInfo"]["name"], "zimz");
    assert!(
        init["result"]["capabilities"]["tools"].is_object(),
        "{init}"
    );
    assert!(
        init["result"]["instructions"]
            .as_str()
            .is_some_and(|s| s.contains("context")),
        "instructions should steer agents: {init}"
    );

    let tools = by_id(&res, 2);
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"search"), "{names:?}");
    assert!(names.contains(&"read_article"), "{names:?}");

    let archives = by_id(&res, 3);
    assert_eq!(
        archives["result"]["isError"],
        serde_json::json!(false),
        "{archives}"
    );
    let text = archives["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("wikipedia_en_climate_change"), "{text}");

    let search = by_id(&res, 4);
    assert_eq!(
        search["result"]["isError"],
        serde_json::json!(false),
        "{search}"
    );
    let hit_text = search["result"]["content"][0]["text"].as_str().unwrap();
    assert!(hit_text.contains("carbon"), "{hit_text}");
    // The structured payload rides alongside the text rendering.
    assert_eq!(
        search["result"]["structuredContent"]["query"],
        serde_json::json!("carbon"),
        "{search}"
    );

    // stdout carried only JSON-RPC; the scan warnings and logs are on stderr.
    assert!(err.contains("MCP server ready"), "{err}");
}

#[test]
fn mcp_stdio_reports_library_errors_as_tool_errors() {
    let dir = library_dir();
    let (res, _) = mcp_session(
        dir.path(),
        &[serde_json::json!({
            "method": "tools/call",
            "params": {"name": "read_article", "arguments": {"archive": "nope", "path": "x"}}
        })],
    );
    let r = by_id(&res, 2);
    assert_eq!(r["result"]["isError"], serde_json::json!(true), "{r}");
    let text = r["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("no archive matches"), "{text}");
    // A tool error is a normal result, not a JSON-RPC protocol error.
    assert!(r.get("error").is_none(), "{r}");
}
