// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! The MCP handler: eight read-only tools and the `zim://` resources.

use std::future::Future;
use std::sync::Arc;

use rmcp::handler::server::common::schema_for_output;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ListResourceTemplatesResult, ListResourcesResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    Resource, ResourceContents, ResourceTemplate, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use zimz_search::{
    ArchiveInfo, ArticleRequest, ContextRequest, Format, HealthRequest, Library, LinksRequest,
    ScanFailure, SearchRequest, SuggestRequest,
};

use crate::render;

/// Shown to the client at `initialize`; tells the agent how to use the tools.
pub const INSTRUCTIONS: &str = "zimz serves offline ZIM archives (Wikipedia, DevDocs, Gutenberg, \
LibreTexts, ...). Start with `context` for a question: it searches every archive and returns \
the most relevant excerpts with `zim://archive/path#Section` citations, packed under a \
character budget. Use `search` when you want a ranked list to choose from: words are ANDed \
and stemmed, a \"quoted phrase\" must appear in that order in the article, hits flagged \
`partial` matched only some words (added when the strict search fell short), and \
`next_cursor` fetches the next page. Then `read_article` with the hit's `archive` and `path` \
(a title or `zim://` URI also works); it never returns more than `max_chars`, and when cut it \
includes the outline so you can ask for one `section` or continue from `next_offset`. \
`suggest` completes titles, `outline` and `links` help you navigate an article, \
`list_archives` shows what is available and `archive_health` diagnoses indexes and caches. \
Restrict `archives` to relevant names or globs (e.g. `devdocs_*`) to save time. Cite sources \
by their `zim://` URI.";

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListArchivesParams {
    /// Case-insensitive substring matched against name, title, description, language,
    /// category and tags. Omit to list everything.
    #[serde(default)]
    pub filter: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ListArchivesResponse {
    pub archives: Vec<ArchiveInfo>,
    pub count: usize,
    /// Files that could not be opened when the library was scanned.
    pub failures: Vec<ScanFailure>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EntryParams {
    /// Archive name from `list_archives` or a search hit.
    pub archive: String,
    /// Article path from a search hit, a `zim://` URI, or a title.
    pub path: String,
}

/// Per-transport policy.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    /// Allow `archive_health` with `verify: full` (reads whole archives; minutes on
    /// Wikipedia). On by default for stdio, off for HTTP.
    pub allow_full_verify: bool,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            allow_full_verify: true,
        }
    }
}

/// An MCP server over one [`Library`]. Cheap to clone (shares the library).
#[derive(Clone)]
pub struct ZimServer {
    library: Arc<Library>,
    options: ServerOptions,
    tool_router: ToolRouter<Self>,
}

impl std::fmt::Debug for ZimServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZimServer")
            .field("archives", &self.library.len())
            .finish_non_exhaustive()
    }
}

/// A tool result carrying both a text rendering and the structured JSON. Library
/// errors become tool-level errors (`isError`) so the model can read and correct them.
fn respond<T: Serialize>(
    result: zimz_search::Result<T>,
    text: impl FnOnce(&T) -> String,
) -> Result<CallToolResult, McpError> {
    match result {
        Ok(v) => {
            let rendered = text(&v);
            let value = serde_json::to_value(&v)
                .map_err(|e| McpError::internal_error(format!("serialising result: {e}"), None))?;
            let mut r = CallToolResult::structured(value);
            r.content = vec![ContentBlock::text(rendered)];
            Ok(r)
        }
        Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
            "error: {e}"
        ))])),
    }
}

impl ZimServer {
    pub fn new(library: Arc<Library>) -> Self {
        Self::with_options(library, ServerOptions::default())
    }

    pub fn with_options(library: Arc<Library>, options: ServerOptions) -> Self {
        Self {
            library,
            options,
            tool_router: Self::tool_router(),
        }
    }

    pub fn options(&self) -> &ServerOptions {
        &self.options
    }

    pub fn library(&self) -> &Arc<Library> {
        &self.library
    }

    /// Library calls block on I/O and decompression; keep them off the async runtime.
    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Library) -> T + Send + 'static,
    ) -> Result<T, McpError> {
        let library = self.library.clone();
        tokio::task::spawn_blocking(move || f(&library))
            .await
            .map_err(|e| McpError::internal_error(format!("worker failed: {e}"), None))
    }

    fn list_archives_response(library: &Library, filter: Option<&str>) -> ListArchivesResponse {
        let archives: Vec<ArchiveInfo> = library.list(filter).into_iter().cloned().collect();
        ListArchivesResponse {
            count: archives.len(),
            archives,
            failures: library.failures().to_vec(),
        }
    }
}

#[tool_router]
impl ZimServer {
    #[tool(
        name = "list_archives",
        description = "List the ZIM archives in the library with their language, size, article counts, index availability and search mode. Use the returned `name` in other tools.",
        annotations(title = "List archives", read_only_hint = true, idempotent_hint = true, open_world_hint = false),
        output_schema = schema_for_output::<ListArchivesResponse>()
    )]
    async fn list_archives(
        &self,
        Parameters(p): Parameters<ListArchivesParams>,
    ) -> Result<CallToolResult, McpError> {
        let r = self
            .blocking(move |lib| Self::list_archives_response(lib, p.filter.as_deref()))
            .await?;
        respond(Ok(r), render::archives)
    }

    #[tool(
        name = "search",
        description = "Full-text search across the selected archives (all by default). Terms are ANDed and stemmed per archive language; quote a phrase (\"borrow checker\") to require the exact word order, verified in the article text. Rankings are fused across archives and exact title matches are boosted. When AND matches fewer than a page, OR matches are appended and flagged `partial`. Returns paths for `read_article`, snippets and a `next_cursor`.",
        annotations(title = "Search archives", read_only_hint = true, idempotent_hint = true, open_world_hint = false),
        output_schema = schema_for_output::<zimz_search::SearchResponse>()
    )]
    async fn search(
        &self,
        Parameters(req): Parameters<SearchRequest>,
    ) -> Result<CallToolResult, McpError> {
        let r = self.blocking(move |lib| lib.search(&req)).await?;
        respond(r, render::search)
    }

    #[tool(
        name = "read_article",
        description = "Read an article as Markdown (default), plain text or HTML, at most `max_chars` characters from `offset`. When the article is longer the response carries the outline; ask for one `section` (index or heading text) or continue from `next_offset`.",
        annotations(title = "Read article", read_only_hint = true, idempotent_hint = true, open_world_hint = false),
        output_schema = schema_for_output::<zimz_search::ArticleResponse>()
    )]
    async fn read_article(
        &self,
        Parameters(req): Parameters<ArticleRequest>,
    ) -> Result<CallToolResult, McpError> {
        let r = self.blocking(move |lib| lib.read_article(&req)).await?;
        respond(r, render::article)
    }

    #[tool(
        name = "outline",
        description = "Heading tree of an article with the size of each section, to pick a `section` for `read_article`.",
        annotations(title = "Article outline", read_only_hint = true, idempotent_hint = true, open_world_hint = false),
        output_schema = schema_for_output::<zimz_search::OutlineResponse>()
    )]
    async fn outline(
        &self,
        Parameters(p): Parameters<EntryParams>,
    ) -> Result<CallToolResult, McpError> {
        let r = self
            .blocking(move |lib| lib.outline(&p.archive, &p.path))
            .await?;
        respond(r, render::outline)
    }

    #[tool(
        name = "suggest",
        description = "Complete a title prefix across the selected archives (type-ahead over the title indexes). Good for finding the exact article name before `read_article`.",
        annotations(title = "Suggest titles", read_only_hint = true, idempotent_hint = true, open_world_hint = false),
        output_schema = schema_for_output::<zimz_search::SuggestResponse>()
    )]
    async fn suggest(
        &self,
        Parameters(req): Parameters<SuggestRequest>,
    ) -> Result<CallToolResult, McpError> {
        let r = self.blocking(move |lib| lib.suggest(&req)).await?;
        respond(r, render::suggest)
    }

    #[tool(
        name = "context",
        description = "One-call retrieval for a question: search all selected archives, take the most relevant section of each top hit and pack the excerpts under `budget_chars`, each with a `zim://archive/path#section` citation. Use `read_article` on a citation for more.",
        annotations(title = "Gather context", read_only_hint = true, idempotent_hint = true, open_world_hint = false),
        output_schema = schema_for_output::<zimz_search::ContextResponse>()
    )]
    async fn context(
        &self,
        Parameters(req): Parameters<ContextRequest>,
    ) -> Result<CallToolResult, McpError> {
        let r = self.blocking(move |lib| lib.context(&req)).await?;
        respond(r, render::context)
    }

    #[tool(
        name = "links",
        description = "Internal links of an article (anchor text, target path and title, whether the target exists), for browsing from one article to related ones.",
        annotations(title = "Article links", read_only_hint = true, idempotent_hint = true, open_world_hint = false),
        output_schema = schema_for_output::<zimz_search::LinksResponse>()
    )]
    async fn links(
        &self,
        Parameters(req): Parameters<LinksRequest>,
    ) -> Result<CallToolResult, McpError> {
        let r = self.blocking(move |lib| lib.links(&req)).await?;
        respond(r, render::links)
    }

    #[tool(
        name = "archive_health",
        description = "Diagnostics: index presence and coverage, cache statistics, scan failures, and optional structural (`quick`) or full checksum (`full`, slow) verification.",
        annotations(title = "Archive health", read_only_hint = true, idempotent_hint = true, open_world_hint = false),
        output_schema = schema_for_output::<zimz_search::HealthResponse>()
    )]
    async fn archive_health(
        &self,
        Parameters(req): Parameters<HealthRequest>,
    ) -> Result<CallToolResult, McpError> {
        if req.verify == zimz_search::Verify::Full && !self.options.allow_full_verify {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "error: `verify: full` is disabled on this transport (it reads whole archives); use `verify: quick`",
            )]));
        }
        let r = self.blocking(move |lib| lib.health(&req)).await?;
        respond(r, render::health)
    }
}

fn not_found(msg: impl Into<String>) -> McpError {
    McpError::resource_not_found(msg.into(), None)
}

// The generated `list_tools` is an `async fn` without awaits; that is the SDK's shape.
#[allow(clippy::unused_async_trait_impl)]
#[tool_handler(router = self.tool_router)]
impl ServerHandler for ZimServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(
            Implementation::new("zimz", env!("CARGO_PKG_VERSION"))
                .with_title("zimz ZIM library")
                .with_description("Search and read offline ZIM archives"),
        )
        .with_instructions(INSTRUCTIONS)
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, McpError>> + Send + '_ {
        let resources = self
            .library
            .archives()
            .map(|a| {
                let mut r = Resource::new(format!("zim://{}", a.name), a.name.clone())
                    .with_mime_type("application/json");
                if let Some(t) = &a.title {
                    r = r.with_title(t.clone());
                }
                if let Some(d) = &a.description {
                    r = r.with_description(d.clone());
                }
                r
            })
            .collect();
        std::future::ready(Ok(ListResourcesResult::with_all_items(resources)))
    }

    fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, McpError>> + Send + '_ {
        let templates = vec![
            ResourceTemplate::new("zim://{archive}/{path}", "article")
                .with_title("Article as Markdown")
                .with_description(
                    "An article (whole, as Markdown). Append `?format=text` or `?format=html` \
                     for other renderings; `{path}` may be a path from a search hit or a title.",
                )
                .with_mime_type("text/markdown"),
            ResourceTemplate::new("zim://{archive}", "archive")
                .with_title("Archive metadata")
                .with_description("Catalogue entry of one archive as JSON.")
                .with_mime_type("application/json"),
        ];
        std::future::ready(Ok(ListResourceTemplatesResult::with_all_items(templates)))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let uri = request.uri.clone();
        let rest = uri.strip_prefix("zim://").ok_or_else(|| {
            not_found(format!(
                "unsupported URI {uri} (expected zim://archive/path)"
            ))
        })?;
        let (archive, path_and_query) = match rest.split_once('/') {
            Some((a, p)) => (a.to_string(), Some(p.to_string())),
            None => (rest.to_string(), None),
        };
        let uri_out = uri.clone();
        let contents = self
            .blocking(move |lib| -> zimz_search::Result<ResourceContents> {
                let slot = lib.get(&archive)?;
                match path_and_query {
                    None => {
                        let json = serde_json::to_string_pretty(&slot.info)
                            .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"));
                        Ok(
                            ResourceContents::text(json, uri_out)
                                .with_mime_type("application/json"),
                        )
                    }
                    Some(pq) => {
                        let (path, query) = pq.split_once('?').unwrap_or((&pq, ""));
                        let format = query
                            .split('&')
                            .find_map(|kv| kv.strip_prefix("format="))
                            .map_or(Format::Markdown, |v| match v {
                                "text" => Format::Text,
                                "html" => Format::Html,
                                _ => Format::Markdown,
                            });
                        let article = lib.read_article(&ArticleRequest {
                            archive: slot.name().to_string(),
                            path: path.to_string(),
                            format,
                            max_chars: usize::MAX / 2,
                            offset: 0,
                            section: None,
                        })?;
                        let mime = match format {
                            Format::Markdown => "text/markdown",
                            Format::Text => "text/plain",
                            Format::Html => "text/html",
                        };
                        Ok(ResourceContents::text(article.content, uri_out).with_mime_type(mime))
                    }
                }
            })
            .await?
            .map_err(|e| not_found(format!("{uri}: {e}")))?;
        Ok(ReadResourceResult::new(vec![contents]).into())
    }
}
