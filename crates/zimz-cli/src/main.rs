// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use zimz_core::integrity::{self, Check};
use zimz_core::{Archive, DirentKind, TitleIndex};
use zimz_search::{Library, LibraryConfig};

#[derive(Parser)]
#[command(
    name = "zimz",
    version,
    about = "Inspect, search and extract ZIM archives; serve a directory of them over MCP"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show header, metadata, listings and embedded-index information
    Info { zim: PathBuf },
    /// List entries (path order by default)
    Ls {
        zim: PathBuf,
        /// Restrict to one namespace (e.g. C, M, W, X, A)
        #[arg(long)]
        ns: Option<char>,
        /// Walk the title-ordered listing instead of path order
        #[arg(long)]
        title: bool,
        /// Only entries whose path (or title, with --title) starts with this prefix
        #[arg(long)]
        prefix: Option<String>,
        /// Maximum number of entries to print (0 = all)
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Write an entry's content to stdout
    Cat {
        zim: PathBuf,
        /// Entry path: `C/foo`, `A/foo`, or a bare path resolved like libzim does
        path: String,
        /// Do not follow redirects
        #[arg(long)]
        no_follow: bool,
    },
    /// Check structure and optionally the checksum / every cluster
    Check {
        zim: PathBuf,
        /// Also verify the MD5 checksum (reads the whole file)
        #[arg(long)]
        checksum: bool,
        /// Also decode every cluster
        #[arg(long)]
        clusters: bool,
    },
    /// Extract an entry as Markdown (default), plain text, or an outline
    Extract {
        zim: PathBuf,
        path: String,
        /// Plain text instead of Markdown
        #[arg(long)]
        text: bool,
        /// Print the heading outline only
        #[arg(long)]
        outline: bool,
        /// Print one section (index or title prefix)
        #[arg(long)]
        section: Option<String>,
        /// Prefix for internal links, e.g. `zim://wikipedia/`
        #[arg(long)]
        link_prefix: Option<String>,
    },
    /// Full-text search: one archive's embedded index, or federated over a directory
    Search {
        /// A `.zim` file or a directory of them
        zim: PathBuf,
        query: String,
        /// Number of results
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
        /// Skip this many results (single archive only)
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Match any term instead of all terms (single archive only)
        #[arg(long)]
        any: bool,
        /// Restrict to archives matching these names or globs (directory only)
        #[arg(long = "archive", value_name = "NAME")]
        archives: Vec<String>,
        /// Cursor from a previous run to fetch the next page (directory only)
        #[arg(long)]
        cursor: Option<String>,
        /// Snippet length in characters, 0 to disable (directory only)
        #[arg(long, default_value_t = 300)]
        snippet_chars: usize,
        /// Print the response as JSON (directory only)
        #[arg(long)]
        json: bool,
        /// Print timings to stderr
        #[arg(long)]
        time: bool,
    },
    /// Title suggestions (type-ahead): one archive, or federated over a directory
    Suggest {
        /// A `.zim` file or a directory of them
        zim: PathBuf,
        prefix: String,
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
        /// Print the response as JSON (directory only)
        #[arg(long)]
        json: bool,
        /// Print timings to stderr
        #[arg(long)]
        time: bool,
    },
    /// Search a directory and pack the best excerpts under a character budget
    Context {
        /// A directory of `.zim` files (or one file)
        zim: PathBuf,
        query: String,
        #[arg(long, default_value_t = 12_000)]
        budget: usize,
        #[arg(long, default_value_t = 1_500)]
        per_hit: usize,
        #[arg(long, default_value_t = 6)]
        max_hits: usize,
        #[arg(long = "archive", value_name = "NAME")]
        archives: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// List the archives of a directory as the MCP server sees them
    Archives {
        #[command(flatten)]
        lib: LibraryArgs,
        /// Substring filter over name, title, description, language, tags
        #[arg(long)]
        filter: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Serve the library to AI agents over MCP (stdio by default, or HTTP with --http)
    Mcp {
        #[command(flatten)]
        lib: LibraryArgs,
        /// Serve streamable HTTP at ADDR instead of stdio (default 127.0.0.1:8765;
        /// bind 0.0.0.0:8765 for the LAN, which requires --token)
        #[arg(long, value_name = "ADDR", num_args = 0..=1, default_missing_value = "127.0.0.1:8765")]
        http: Option<std::net::SocketAddr>,
        /// Bearer token HTTP clients must send (`Authorization: Bearer …`)
        #[arg(long, env = "ZIMZ_TOKEN", hide_env_values = true)]
        token: Option<String>,
        /// Extra Host header values to accept over HTTP (repeatable)
        #[arg(long = "allowed-host", value_name = "HOST")]
        allowed_hosts: Vec<String>,
        /// Browser origins allowed to call the HTTP endpoint (repeatable)
        #[arg(long = "allowed-origin", value_name = "ORIGIN")]
        allowed_origins: Vec<String>,
    },
    /// Write an embedded Xapian index blob to a file
    DumpIndex {
        zim: PathBuf,
        /// `fulltext` or `title`
        #[arg(long, default_value = "fulltext")]
        kind: String,
        #[arg(short, long)]
        output: PathBuf,
    },
}

/// Where the MCP server and the directory-level commands find archives.
#[derive(clap::Args)]
struct LibraryArgs {
    /// Directory of ZIM files (repeatable; scanned recursively unless --no-recursive)
    #[arg(long = "zim-dir", value_name = "DIR")]
    zim_dirs: Vec<PathBuf>,
    /// Single ZIM file (repeatable)
    #[arg(long = "zim", value_name = "FILE")]
    zims: Vec<PathBuf>,
    /// Do not descend into subdirectories
    #[arg(long)]
    no_recursive: bool,
    /// Total budget for decoded clusters, shared across archives
    #[arg(long, default_value_t = 256, value_name = "MB")]
    cluster_cache_mb: usize,
    /// Budget for extracted articles (Markdown + text)
    #[arg(long, default_value_t = 64, value_name = "MB")]
    extract_cache_mb: usize,
    /// Ranking weight for archives matching a glob, e.g. `wikipedia_*=2` (repeatable)
    #[arg(long = "priority", value_name = "GLOB=WEIGHT")]
    priorities: Vec<String>,
}

impl LibraryArgs {
    fn from_path(path: &Path) -> Self {
        let (zim_dirs, zims) = if path.is_dir() {
            (vec![path.to_path_buf()], Vec::new())
        } else {
            (Vec::new(), vec![path.to_path_buf()])
        };
        Self {
            zim_dirs,
            zims,
            no_recursive: false,
            cluster_cache_mb: 256,
            extract_cache_mb: 64,
            priorities: Vec::new(),
        }
    }

    fn config(&self) -> anyhow::Result<LibraryConfig> {
        if self.zim_dirs.is_empty() && self.zims.is_empty() {
            bail!("give at least one --zim-dir or --zim");
        }
        let mut priorities = Vec::new();
        for p in &self.priorities {
            let (glob, weight) = p
                .split_once('=')
                .with_context(|| format!("--priority {p:?}: expected GLOB=WEIGHT"))?;
            let weight: f64 = weight
                .parse()
                .with_context(|| format!("--priority {p:?}: bad weight"))?;
            priorities.push((glob.to_string(), weight));
        }
        Ok(LibraryConfig {
            dirs: self.zim_dirs.clone(),
            files: self.zims.clone(),
            recursive: !self.no_recursive,
            cluster_cache_bytes: self.cluster_cache_mb.saturating_mul(1 << 20),
            extract_cache_bytes: self.extract_cache_mb.saturating_mul(1 << 20),
            priorities,
            ..LibraryConfig::default()
        })
    }

    fn open(&self) -> anyhow::Result<Library> {
        let library = Library::scan(self.config()?).context("scanning the library")?;
        for f in library.failures() {
            eprintln!("warning: {}: {}", f.file, f.error);
        }
        if library.is_empty() {
            bail!("no archives found");
        }
        Ok(library)
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Info { zim } => info(&zim),
        Cmd::Ls {
            zim,
            ns,
            title,
            prefix,
            limit,
        } => ls(&zim, ns, title, prefix.as_deref(), limit),
        Cmd::Cat {
            zim,
            path,
            no_follow,
        } => cat(&zim, &path, !no_follow),
        Cmd::Check {
            zim,
            checksum,
            clusters,
        } => check(&zim, checksum, clusters),
        Cmd::Search {
            zim,
            query,
            limit,
            offset,
            any,
            archives,
            cursor,
            snippet_chars,
            json,
            time,
        } => {
            if zim.is_dir() {
                let opts = DirSearch {
                    limit,
                    archives,
                    cursor,
                    snippet_chars,
                    json,
                    time,
                };
                library_search(&zim, &query, opts)
            } else {
                search(&zim, &query, limit, offset, any, time)
            }
        }
        Cmd::Suggest {
            zim,
            prefix,
            limit,
            json,
            time,
        } => suggest_any(&zim, &prefix, limit, json, time),
        Cmd::Context {
            zim,
            query,
            budget,
            per_hit,
            max_hits,
            archives,
            json,
        } => context_cmd(&zim, &query, budget, per_hit, max_hits, archives, json),
        Cmd::Archives { lib, filter, json } => archives_cmd(&lib, filter.as_deref(), json),
        Cmd::Mcp {
            lib,
            http,
            token,
            allowed_hosts,
            allowed_origins,
        } => {
            let opts = http.map(|bind| zimz_mcp::HttpOptions {
                bind,
                token,
                allowed_hosts,
                allowed_origins,
                ..zimz_mcp::HttpOptions::default()
            });
            mcp_cmd(&lib, opts.as_ref())
        }
        Cmd::Extract {
            zim,
            path,
            text,
            outline,
            section,
            link_prefix,
        } => extract_cmd(
            &zim,
            &path,
            text,
            outline,
            section.as_deref(),
            link_prefix.as_deref(),
        ),
        Cmd::DumpIndex { zim, kind, output } => dump_index(&zim, &kind, &output),
    }
}

fn open(path: &PathBuf) -> anyhow::Result<Archive> {
    Archive::open(path).with_context(|| format!("opening {}", path.display()))
}

#[allow(clippy::cast_precision_loss)]
fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.2} {}", UNITS[i])
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn info(path: &PathBuf) -> anyhow::Result<()> {
    let archive = open(path)?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    info_summary(&archive, &mut out)?;
    info_metadata(&archive, &mut out)
}

fn info_summary(archive: &Archive, out: &mut impl Write) -> anyhow::Result<()> {
    let header = archive.header();
    writeln!(out, "file:        {}", archive.describe())?;
    writeln!(
        out,
        "size:        {} ({} bytes)",
        human(archive.size()),
        archive.size()
    )?;
    writeln!(
        out,
        "zim version: {}.{}  ({} namespace scheme)",
        header.major,
        header.minor,
        if archive.uses_new_namespace_scheme() {
            "new C/M/W/X"
        } else {
            "old A/I/M/-"
        }
    )?;
    writeln!(out, "uuid:        {}", archive.uuid())?;
    writeln!(
        out,
        "entries:     {}   clusters: {}   mime types: {}",
        header.entry_count,
        header.cluster_count,
        archive.mime_list().len()
    )?;
    let namespaces: Vec<String> = archive
        .namespaces()?
        .iter()
        .map(|&n| (n as char).to_string())
        .collect();
    writeln!(out, "namespaces:  {}", namespaces.join(" "))?;
    let title_index = match archive.title_index() {
        TitleIndex::None => "none".to_string(),
        TitleIndex::Header { count, .. } => {
            format!("header title pointer list, {count} entries (all namespaces)")
        }
        TitleIndex::FrontArticles { count, .. } => {
            format!("X/listing/titleOrdered/v1, {count} front articles")
        }
    };
    writeln!(out, "title index: {title_index}")?;
    writeln!(out, "articles:    {}", archive.article_count()?)?;
    match archive.main_entry()? {
        Some(entry) => {
            let target = archive.resolve(&entry)?;
            writeln!(
                out,
                "main page:   {} -> {} [{}]",
                entry.full_path(),
                target.full_path(),
                target.title()
            )?;
        }
        None => writeln!(out, "main page:   none")?,
    }
    match archive.fulltext_index()? {
        Some(da) => writeln!(
            out,
            "fulltext idx: {} at offset {}",
            human(da.len),
            da.offset
        )?,
        None => writeln!(out, "fulltext idx: none")?,
    }
    match archive.title_xapian_index()? {
        Some(da) => writeln!(
            out,
            "title idx:    {} at offset {}",
            human(da.len),
            da.offset
        )?,
        None => writeln!(out, "title idx:    none")?,
    }
    match archive.stored_checksum() {
        Ok(sum) => writeln!(out, "checksum:    {}", hex(&sum))?,
        Err(_) => writeln!(out, "checksum:    none")?,
    }
    Ok(())
}

fn info_metadata(archive: &Archive, out: &mut impl Write) -> anyhow::Result<()> {
    writeln!(out, "metadata:")?;
    for key in archive.metadata_keys()? {
        let value = archive.metadata(&key)?.unwrap_or_default();
        if key.starts_with("Illustration") || value.iter().any(|&b| b < 9) {
            writeln!(out, "  {key:<18} <{} bytes binary>", value.len())?;
        } else {
            let text = String::from_utf8_lossy(&value).replace('\n', " ");
            let shown: String = text.chars().take(100).collect();
            let ellipsis = if text.chars().count() > 100 {
                "…"
            } else {
                ""
            };
            writeln!(out, "  {key:<18} {shown}{ellipsis}")?;
        }
    }
    let mut counter = archive.counter()?;
    if !counter.is_empty() {
        counter.sort_by_key(|entry| std::cmp::Reverse(entry.1));
        let top: Vec<String> = counter
            .iter()
            .take(6)
            .map(|(m, n)| format!("{m}={n}"))
            .collect();
        writeln!(out, "counter (top): {}", top.join("  "))?;
    }
    Ok(())
}

fn ls(
    path: &PathBuf,
    ns: Option<char>,
    by_title: bool,
    prefix: Option<&str>,
    limit: usize,
) -> anyhow::Result<()> {
    let a = open(path)?;
    let out = std::io::stdout();
    let mut w = std::io::BufWriter::new(out.lock());
    let max = if limit == 0 { usize::MAX } else { limit };
    let describe = |d: &zimz_core::Dirent| -> String {
        match d.kind {
            DirentKind::Item { cluster, blob } => {
                format!("{} c{cluster}/b{blob}", a.mime_type(d).unwrap_or("?"))
            }
            DirentKind::Redirect { target } => format!("-> #{target}"),
            DirentKind::LinkTarget => "linktarget".into(),
            DirentKind::Deleted => "deleted".into(),
        }
    };
    if by_title {
        let ns = ns.map_or(a.content_namespace(), |c| c as u8);
        let range = a.find_title_prefix(ns, prefix.unwrap_or(""))?;
        for pos in range.take(max) {
            let d = a.entry_by_title_position(pos)?;
            writeln!(w, "{}\t{}\t{}", d.full_path(), d.title(), describe(&d))?;
        }
        return Ok(());
    }
    let range = match ns {
        Some(c) => a.namespace_range(c as u8)?,
        None => 0..a.entry_count(),
    };
    let mut printed = 0;
    for d in a.entries_in(range) {
        let d = d?;
        if let Some(p) = prefix
            && !d.path.starts_with(p)
        {
            continue;
        }
        writeln!(w, "{}\t{}\t{}", d.full_path(), d.title(), describe(&d))?;
        printed += 1;
        if printed >= max {
            break;
        }
    }
    Ok(())
}

fn cat(path: &PathBuf, entry_path: &str, follow: bool) -> anyhow::Result<()> {
    let a = open(path)?;
    let d = match a.entry_by_long_path(entry_path)? {
        Some(d) => d,
        None => a
            .entry_by_path_compat(entry_path)?
            .with_context(|| format!("entry not found: {entry_path}"))?,
    };
    let d = if follow { a.resolve(&d)? } else { d };
    if !d.is_item() {
        bail!(
            "{} is a redirect to #{}",
            d.full_path(),
            d.redirect_target().unwrap_or(0)
        );
    }
    let data = a.item_data(&d)?;
    let out = std::io::stdout();
    let mut w = out.lock();
    match w.write_all(&data).and_then(|()| w.flush()) {
        Ok(()) => Ok(()),
        // `zimz cat … | head` closes the pipe early; that is not an error worth reporting.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn check(path: &PathBuf, checksum: bool, clusters: bool) -> anyhow::Result<()> {
    let a = open(path)?;
    let mut checks: Vec<Check> = Check::QUICK.to_vec();
    if clusters {
        checks.push(Check::Clusters);
    }
    if checksum {
        checks.push(Check::Checksum);
    }
    let problems = integrity::run(&a, &checks);
    if problems.is_empty() {
        println!("ok: {} ({} checks)", a.describe(), checks.len());
        return Ok(());
    }
    for p in &problems {
        println!("{:?}: {}", p.check, p.message);
    }
    bail!("{} problem(s) found", problems.len());
}

fn extract_cmd(
    path: &PathBuf,
    entry_path: &str,
    text: bool,
    outline: bool,
    section: Option<&str>,
    link_prefix: Option<&str>,
) -> anyhow::Result<()> {
    let archive = open(path)?;
    let entry = match archive.entry_by_long_path(entry_path)? {
        Some(e) => e,
        None => archive
            .entry_by_path_compat(entry_path)?
            .with_context(|| format!("entry not found: {entry_path}"))?,
    };
    let adapter = zimz_extract::detect_adapter(&archive);
    let doc = zimz_extract::extract(&archive, &entry, adapter, link_prefix)?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if outline {
        writeln!(
            out,
            "{} ({:?}, {} words, {} links, from {})",
            doc.title,
            doc.adapter,
            doc.word_count,
            doc.links.len(),
            doc.source_path
        )?;
        for o in doc.outline() {
            writeln!(
                out,
                "{:>3}. {}{} ({} chars)",
                o.index,
                "  ".repeat(usize::from(o.level.saturating_sub(1))),
                o.title,
                o.chars
            )?;
        }
        return Ok(());
    }
    let body = match section {
        Some(name) => {
            let idx = doc
                .find_section(name)
                .with_context(|| format!("no section matching {name:?}"))?;
            doc.section_markdown(idx).unwrap_or_default().to_string()
        }
        None if text => doc.text.clone(),
        None => doc.markdown.clone(),
    };
    match out.write_all(body.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn search(
    path: &PathBuf,
    query: &str,
    limit: usize,
    offset: usize,
    any: bool,
    time: bool,
) -> anyhow::Result<()> {
    use std::time::Instant;
    use zimz_glass::search::{Op, Query};
    let t0 = Instant::now();
    let archive = open(path)?;
    let da = archive
        .fulltext_index()?
        .with_context(|| "archive has no fulltext index")?;
    let bytes = archive.source().slice(da.offset, da.len as usize)?;
    let db = zimz_glass::GlassDb::open(&bytes)?;
    let language = db
        .metadata_string("language")?
        .or(archive.metadata_string("Language")?);
    let analyzer = zimz_glass::Analyzer::new(language.as_deref());
    let q = Query::parse(&analyzer, query, if any { Op::Or } else { Op::And });
    let t1 = Instant::now();
    let results = zimz_glass::search::search(&db, &q, offset, limit)?;
    let t2 = Instant::now();
    let title_slot = db.value_slot("title")?.unwrap_or(0);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let terms: Vec<&str> = q.terms.iter().map(|t| t.term.as_str()).collect();
    writeln!(
        out,
        "{} matches for {:?} (terms: {})",
        results.total,
        query,
        terms.join(" ")
    )?;
    for (i, hit) in results.hits.iter().enumerate() {
        let data = db.docdata_string(hit.docid)?.unwrap_or_default();
        let entry_path = if archive.uses_new_namespace_scheme() {
            data.strip_prefix("C/").unwrap_or(&data).to_string()
        } else {
            data.clone()
        };
        let title = archive
            .entry_by_path_compat(&entry_path)?
            .map(|e| e.title().to_string())
            .or(db.value_string(hit.docid, title_slot)?)
            .unwrap_or_default();
        writeln!(
            out,
            "{:>3}. {:>3}%  {}  [{}]",
            offset + i + 1,
            hit.percent,
            title,
            entry_path
        )?;
    }
    if time {
        eprintln!(
            "open+parse {:.1?}, search {:.1?}, total {:.1?}",
            t1 - t0,
            t2 - t1,
            t0.elapsed()
        );
    }
    Ok(())
}

fn suggest_cmd(path: &PathBuf, prefix: &str, limit: usize, time: bool) -> anyhow::Result<()> {
    use std::time::Instant;
    let t0 = Instant::now();
    let archive = open(path)?;
    let da = archive
        .title_xapian_index()?
        .with_context(|| "archive has no title index")?;
    let bytes = archive.source().slice(da.offset, da.len as usize)?;
    let db = zimz_glass::GlassDb::open(&bytes)?;
    let language = db
        .metadata_string("language")?
        .or(archive.metadata_string("Language")?);
    let analyzer = zimz_glass::Analyzer::new(language.as_deref());
    let t1 = Instant::now();
    let results = zimz_glass::suggest::suggest(&db, &analyzer, prefix, 0, limit)?;
    let t2 = Instant::now();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{} matching titles for {prefix:?}", results.total)?;
    for (i, s) in results.hits.iter().enumerate() {
        let p = if archive.uses_new_namespace_scheme() {
            s.path.strip_prefix("C/").unwrap_or(&s.path)
        } else {
            &s.path
        };
        let target = s
            .target_path
            .as_deref()
            .filter(|t| *t != p)
            .map(|t| format!(" -> {t}"))
            .unwrap_or_default();
        writeln!(out, "{:>3}. {}  [{p}]{target}", i + 1, s.title)?;
    }
    if time {
        eprintln!(
            "open+parse {:.1?}, suggest {:.1?}, total {:.1?}",
            t1 - t0,
            t2 - t1,
            t0.elapsed()
        );
    }
    Ok(())
}

fn dump_index(path: &PathBuf, kind: &str, output: &PathBuf) -> anyhow::Result<()> {
    let a = open(path)?;
    let da = match kind {
        "fulltext" => a.fulltext_index()?,
        "title" => a.title_xapian_index()?,
        other => bail!("unknown index kind {other:?} (use fulltext or title)"),
    }
    .with_context(|| format!("archive has no {kind} index"))?;
    let mut f = std::fs::File::create(output)?;
    let mut pos = 0u64;
    while pos < da.len {
        let n = (da.len - pos).min(8 << 20) as usize;
        let chunk = a.source().slice(da.offset + pos, n)?;
        f.write_all(&chunk)?;
        pos += n as u64;
    }
    println!("wrote {} bytes to {}", da.len, output.display());
    Ok(())
}

fn suggest_any(
    zim: &Path,
    prefix: &str,
    limit: usize,
    json: bool,
    time: bool,
) -> anyhow::Result<()> {
    if zim.is_dir() {
        library_suggest(zim, prefix, limit, json, time)
    } else {
        suggest_cmd(&zim.to_path_buf(), prefix, limit, time)
    }
}

fn mcp_cmd(lib: &LibraryArgs, http: Option<&zimz_mcp::HttpOptions>) -> anyhow::Result<()> {
    zimz_mcp::init_logging();
    let library = lib.open()?;
    match http {
        Some(opts) => zimz_mcp::run_http(library, opts),
        None => zimz_mcp::run_stdio(library),
    }
    .map_err(|e| anyhow::anyhow!("{e}"))
}

fn print_response<T: serde::Serialize>(
    value: &T,
    json: bool,
    text: impl FnOnce(&T) -> String,
) -> anyhow::Result<()> {
    let out = std::io::stdout();
    let mut w = out.lock();
    if json {
        serde_json::to_writer_pretty(&mut w, value)?;
        writeln!(w)?;
    } else {
        w.write_all(text(value).as_bytes())?;
    }
    Ok(())
}

struct DirSearch {
    limit: usize,
    archives: Vec<String>,
    cursor: Option<String>,
    snippet_chars: usize,
    json: bool,
    time: bool,
}

fn library_search(dir: &Path, query: &str, opts: DirSearch) -> anyhow::Result<()> {
    use std::time::Instant;
    let DirSearch {
        limit,
        archives,
        cursor,
        snippet_chars,
        json,
        time,
    } = opts;
    let t0 = Instant::now();
    let library = LibraryArgs::from_path(dir).open()?;
    let t1 = Instant::now();
    let mut req = zimz_search::SearchRequest::new(query);
    req.limit = limit;
    req.archives = archives;
    req.cursor = cursor;
    req.snippet_chars = snippet_chars;
    let res = library.search(&req)?;
    if time {
        eprintln!(
            "scan {} archives {:.1?}, search {:.1?}",
            library.len(),
            t1 - t0,
            t1.elapsed()
        );
    }
    print_response(&res, json, zimz_mcp::render::search)
}

fn library_suggest(
    dir: &Path,
    prefix: &str,
    limit: usize,
    json: bool,
    time: bool,
) -> anyhow::Result<()> {
    use std::time::Instant;
    let t0 = Instant::now();
    let library = LibraryArgs::from_path(dir).open()?;
    let t1 = Instant::now();
    let res = library.suggest(&zimz_search::SuggestRequest {
        prefix: prefix.to_string(),
        archives: Vec::new(),
        limit,
    })?;
    if time {
        eprintln!(
            "scan {} archives {:.1?}, suggest {:.1?}",
            library.len(),
            t1 - t0,
            t1.elapsed()
        );
    }
    print_response(&res, json, zimz_mcp::render::suggest)
}

fn context_cmd(
    path: &Path,
    query: &str,
    budget: usize,
    per_hit: usize,
    max_hits: usize,
    archives: Vec<String>,
    json: bool,
) -> anyhow::Result<()> {
    let library = LibraryArgs::from_path(path).open()?;
    let mut req = zimz_search::ContextRequest::new(query);
    req.budget_chars = budget;
    req.per_hit_chars = per_hit;
    req.max_hits = max_hits;
    req.archives = archives;
    let res = library.context(&req)?;
    print_response(&res, json, zimz_mcp::render::context)
}

fn archives_cmd(lib: &LibraryArgs, filter: Option<&str>, json: bool) -> anyhow::Result<()> {
    let library = lib.open()?;
    let archives: Vec<zimz_search::ArchiveInfo> =
        library.list(filter).into_iter().cloned().collect();
    let res = zimz_mcp::server::ListArchivesResponse {
        count: archives.len(),
        archives,
        failures: library.failures().to_vec(),
    };
    print_response(&res, json, zimz_mcp::render::archives)
}
