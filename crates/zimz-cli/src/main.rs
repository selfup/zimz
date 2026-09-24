// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use zimz_core::integrity::{self, Check};
use zimz_core::{Archive, DirentKind, TitleIndex};

#[derive(Parser)]
#[command(
    name = "zimz",
    version,
    about = "Inspect, list, extract and check ZIM archives"
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
