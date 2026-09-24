// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! The zimz-core half of `scripts/bench_compare.py`: runs the shared workload and prints
//! timings as JSON. Not meant to be run by hand; use the script.

use std::hint::black_box;
use std::time::{Duration, Instant};

use serde::Deserialize;
use zimz_core::Archive;

#[derive(Deserialize)]
struct Workload {
    zim: String,
    index_lookups: Vec<u32>,
    path_lookups: Vec<String>,
    title_lookups: Vec<String>,
    cold_items: Vec<u32>,
    scan_entries: u32,
    scan_max_bytes: u64,
    #[serde(default)]
    checksum: bool,
}

#[allow(clippy::cast_precision_loss)]
fn per_op_ns(elapsed: Duration, n: usize) -> f64 {
    elapsed.as_nanos() as f64 / n.max(1) as f64
}

#[allow(clippy::cast_precision_loss)]
fn mb_per_s(bytes: u64, elapsed: Duration) -> f64 {
    bytes as f64 / 1e6 / elapsed.as_secs_f64().max(1e-9)
}

/// Read one byte per 4 KiB page so the data is really materialised (decompressed or
/// paged in) on both sides of the comparison, without adding hashing cost.
fn touch(data: &[u8]) -> u64 {
    data.iter()
        .step_by(4096)
        .map(|&b| u64::from(b))
        .sum::<u64>()
        + data.len() as u64
}

#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: bench_compare <workload.json>");
    let w: Workload = serde_json::from_slice(&std::fs::read(&path)?)?;
    let mut out = serde_json::Map::new();
    let mut sink = 0u64;

    let mut best = f64::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        black_box(Archive::open(&w.zim)?);
        best = best.min(t.elapsed().as_nanos() as f64 / 1e3);
    }
    out.insert("open_us".into(), best.into());
    let a = Archive::open(&w.zim)?;

    // Every random-access loop runs twice: "*_first" is the first pass (page cache in
    // whatever state it was), the headline number is the second, warm pass.
    for suffix in ["_first_ns", "_ns"] {
        let t = Instant::now();
        for &i in &w.index_lookups {
            sink += a.entry(i)?.path.len() as u64;
        }
        out.insert(
            format!("entry_by_index{suffix}"),
            per_op_ns(t.elapsed(), w.index_lookups.len()).into(),
        );
    }
    for suffix in ["_first_ns", "_ns"] {
        let t = Instant::now();
        for p in &w.path_lookups {
            sink += a.entry_by_path_compat(p)?.map_or(0, |e| u64::from(e.index));
        }
        out.insert(
            format!("path_lookup{suffix}"),
            per_op_ns(t.elapsed(), w.path_lookups.len()).into(),
        );
    }
    if !w.title_lookups.is_empty() {
        for suffix in ["_first_ns", "_ns"] {
            let t = Instant::now();
            for s in &w.title_lookups {
                sink += a.entry_by_title(s)?.map_or(0, |e| u64::from(e.index));
            }
            out.insert(
                format!("title_lookup{suffix}"),
                per_op_ns(t.elapsed(), w.title_lookups.len()).into(),
            );
        }
    }

    let mut a = Archive::open(&w.zim)?;
    for suffix in ["_first", ""] {
        a = Archive::open(&w.zim)?; // fresh reader caches; second pass has warm pages
        let t = Instant::now();
        let mut nbytes = 0u64;
        for &i in &w.cold_items {
            let e = a.entry(i)?;
            let data = a.item_data(&e)?;
            nbytes += data.len() as u64;
            sink += touch(&data);
        }
        let dt = t.elapsed();
        out.insert(
            format!("cold_read{suffix}_us"),
            (per_op_ns(dt, w.cold_items.len()) / 1e3).into(),
        );
        out.insert(
            format!("cold_read{suffix}_mb_s"),
            mb_per_s(nbytes, dt).into(),
        );
    }

    let warm: Vec<u32> = w.cold_items.iter().take(10).copied().collect();
    let t = Instant::now();
    for _ in 0..20 {
        for &i in &warm {
            let e = a.entry(i)?;
            sink += black_box(touch(&a.item_data(&e)?));
        }
    }
    out.insert(
        "warm_read_ns".into(),
        per_op_ns(t.elapsed(), 20 * warm.len()).into(),
    );

    for suffix in ["_first", ""] {
        let a = Archive::open(&w.zim)?;
        let t = Instant::now();
        let mut items = 0u64;
        let mut scanned = 0u64;
        for i in 0..w.scan_entries {
            let e = a.entry(i)?;
            if e.is_item() {
                let data = a.item_data(&e)?;
                scanned += data.len() as u64;
                sink += touch(&data);
                items += 1;
                if scanned >= w.scan_max_bytes {
                    break;
                }
            }
        }
        let dt = t.elapsed();
        out.insert(format!("scan{suffix}_items"), items.into());
        out.insert(format!("scan{suffix}_mb"), (scanned as f64 / 1e6).into());
        out.insert(format!("scan{suffix}_ms"), (dt.as_secs_f64() * 1e3).into());
        out.insert(format!("scan{suffix}_mb_s"), mb_per_s(scanned, dt).into());
        out.insert(
            format!("scan{suffix}_items_s"),
            (items as f64 / dt.as_secs_f64().max(1e-9)).into(),
        );
    }

    if w.checksum {
        let a = Archive::open(&w.zim)?;
        let t = Instant::now();
        let ok = a.verify_checksum()?;
        out.insert(
            "checksum_ms".into(),
            (t.elapsed().as_secs_f64() * 1e3).into(),
        );
        out.insert("checksum_ok".into(), ok.into());
    }
    out.insert("_sink".into(), sink.into());
    println!("{}", serde_json::Value::Object(out));
    Ok(())
}
