// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! A small in-memory ZIM writer for tests: enough of the spec to build valid archives
//! with either namespace scheme, redirects, metadata, title listings, and uncompressed,
//! zstd, xz or extended clusters. Its output has been checked against python-libzim.

use md5::{Digest, Md5};

#[derive(Clone, Debug)]
pub enum Content {
    Item { mime: String, data: Vec<u8> },
    Redirect { ns: u8, path: String },
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub ns: u8,
    pub path: String,
    pub title: String,
    pub content: Content,
    pub front: bool,
    pub uncompressed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    None,
    Zstd,
    Xz,
}

#[cfg(feature = "zstd-c")]
fn zstd_compress(payload: &[u8]) -> Vec<u8> {
    zstd::bulk::compress(payload, 3).unwrap()
}
#[cfg(not(feature = "zstd-c"))]
fn zstd_compress(_payload: &[u8]) -> Vec<u8> {
    panic!("Codec::Zstd in tests needs the zstd-c feature")
}
#[cfg(feature = "xz-c")]
fn xz_compress(payload: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = liblzma::write::XzEncoder::new(Vec::new(), 6);
    e.write_all(payload).unwrap();
    e.finish().unwrap()
}
#[cfg(not(feature = "xz-c"))]
fn xz_compress(_payload: &[u8]) -> Vec<u8> {
    panic!("Codec::Xz in tests needs the xz-c feature")
}

impl Codec {
    fn code(self) -> u8 {
        match self {
            Codec::None => 1,
            Codec::Zstd => 5,
            Codec::Xz => 4,
        }
    }

    fn compress(self, payload: &[u8]) -> Vec<u8> {
        match self {
            Codec::None => payload.to_vec(),
            Codec::Zstd => zstd_compress(payload),
            Codec::Xz => xz_compress(payload),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ZimBuilder {
    pub major: u16,
    pub minor: u16,
    pub codec: Codec,
    pub extended: bool,
    pub cluster_limit: usize,
    pub header_title_list: bool,
    pub v1_listing: bool,
    pub main: Option<(u8, String)>,
    pub uuid: [u8; 16],
    pub entries: Vec<Spec>,
}

/// Offsets of the pieces of a built archive, for corruption tests.
#[derive(Clone, Debug)]
pub struct Layout {
    pub bytes: Vec<u8>,
    pub dirent_offsets: Vec<u64>,
    pub cluster_offsets: Vec<u64>,
    pub path_ptr_pos: u64,
    pub title_ptr_pos: u64,
    pub cluster_ptr_pos: u64,
    pub checksum_pos: u64,
    /// `(namespace, path)` in path order.
    pub order: Vec<(u8, String)>,
}

impl Layout {
    pub fn index_of(&self, ns: u8, path: &str) -> u32 {
        self.order
            .iter()
            .position(|(n, p)| *n == ns && p == path)
            .unwrap() as u32
    }
}

struct Cluster {
    codec: Codec,
    blobs: Vec<Vec<u8>>,
    size: usize,
}

fn title_of(e: &Spec) -> &[u8] {
    if e.title.is_empty() {
        e.path.as_bytes()
    } else {
        e.title.as_bytes()
    }
}

impl ZimBuilder {
    pub fn new_scheme() -> Self {
        Self {
            major: 6,
            minor: 3,
            codec: Codec::None,
            extended: false,
            cluster_limit: 64 * 1024,
            header_title_list: false,
            v1_listing: true,
            main: None,
            uuid: *b"zimz-test-uuid-!",
            entries: Vec::new(),
        }
    }

    pub fn old_scheme() -> Self {
        Self {
            major: 5,
            minor: 0,
            header_title_list: true,
            v1_listing: false,
            ..Self::new_scheme()
        }
    }

    pub fn content_ns(&self) -> u8 {
        if self.minor >= 1 { b'C' } else { b'A' }
    }

    pub fn item(
        mut self,
        ns: u8,
        path: &str,
        title: &str,
        mime: &str,
        data: impl Into<Vec<u8>>,
    ) -> Self {
        self.entries.push(Spec {
            ns,
            path: path.into(),
            title: title.into(),
            content: Content::Item {
                mime: mime.into(),
                data: data.into(),
            },
            front: false,
            uncompressed: false,
        });
        self
    }

    /// A front article in the content namespace.
    pub fn html(self, path: &str, title: &str, body: &str) -> Self {
        let ns = self.content_ns();
        let html = format!("<html><head><title>{title}</title></head><body>{body}</body></html>");
        self.item(ns, path, title, "text/html", html).front(true)
    }

    pub fn redirect(mut self, ns: u8, path: &str, title: &str, to_ns: u8, to_path: &str) -> Self {
        self.entries.push(Spec {
            ns,
            path: path.into(),
            title: title.into(),
            content: Content::Redirect {
                ns: to_ns,
                path: to_path.into(),
            },
            front: true,
            uncompressed: false,
        });
        self
    }

    pub fn metadata(self, key: &str, value: &str) -> Self {
        self.item(b'M', key, "", "text/plain;charset=utf-8", value)
    }

    pub fn main(mut self, ns: u8, path: &str) -> Self {
        self.main = Some((ns, path.into()));
        self
    }

    /// Mark the last added entry as a front article (or not).
    pub fn front(mut self, front: bool) -> Self {
        self.entries.last_mut().unwrap().front = front;
        self
    }

    /// Store the last added item in an uncompressed cluster.
    pub fn uncompressed(mut self) -> Self {
        self.entries.last_mut().unwrap().uncompressed = true;
        self
    }

    pub fn build(&self) -> Vec<u8> {
        self.build_layout().bytes
    }

    #[allow(clippy::too_many_lines)]
    pub fn build_layout(&self) -> Layout {
        let mut all: Vec<Spec> = self.entries.clone();
        if let Some((ns, path)) = &self.main
            && self.minor >= 1
        {
            all.push(Spec {
                ns: b'W',
                path: "mainPage".into(),
                title: String::new(),
                content: Content::Redirect {
                    ns: *ns,
                    path: path.clone(),
                },
                front: false,
                uncompressed: false,
            });
        }
        if self.v1_listing {
            all.push(Spec {
                ns: b'X',
                path: "listing/titleOrdered/v1".into(),
                title: String::new(),
                content: Content::Item {
                    mime: "application/octet-stream+zimlisting".into(),
                    data: Vec::new(),
                },
                front: false,
                uncompressed: true,
            });
        }
        all.sort_by(|a, b| (a.ns, a.path.as_bytes()).cmp(&(b.ns, b.path.as_bytes())));
        for w in all.windows(2) {
            assert!(
                (w[0].ns, &w[0].path) != (w[1].ns, &w[1].path),
                "duplicate entry {}/{}",
                w[1].ns as char,
                w[1].path
            );
        }
        let index_of = |ns: u8, path: &str| -> u32 {
            all.iter()
                .position(|e| e.ns == ns && e.path == path)
                .unwrap_or_else(|| panic!("no entry {}/{path}", ns as char)) as u32
        };

        let mut mimes: Vec<String> = all
            .iter()
            .filter_map(|e| match &e.content {
                Content::Item { mime, .. } => Some(mime.clone()),
                Content::Redirect { .. } => None,
            })
            .collect();
        mimes.sort();
        mimes.dedup();
        let mime_index = |m: &str| mimes.iter().position(|x| x == m).unwrap() as u16;

        let mut front: Vec<u32> = all
            .iter()
            .enumerate()
            .filter(|(_, e)| e.front)
            .map(|(i, _)| i as u32)
            .collect();
        front.sort_by(|&a, &b| {
            title_of(&all[a as usize])
                .cmp(title_of(&all[b as usize]))
                .then(a.cmp(&b))
        });
        let listing: Vec<u8> = front.iter().flat_map(|i| i.to_le_bytes()).collect();

        let mut clusters: Vec<Cluster> = Vec::new();
        let mut location: Vec<Option<(u32, u32)>> = vec![None; all.len()];
        for (i, e) in all.iter().enumerate() {
            let Content::Item { data, .. } = &e.content else {
                continue;
            };
            let data = if e.ns == b'X' && e.path == "listing/titleOrdered/v1" {
                listing.clone()
            } else {
                data.clone()
            };
            let codec = if e.uncompressed || e.ns == b'X' {
                Codec::None
            } else {
                self.codec
            };
            let need_new = clusters
                .last()
                .is_none_or(|c| c.codec != codec || c.size + data.len() > self.cluster_limit);
            if need_new {
                clusters.push(Cluster {
                    codec,
                    blobs: Vec::new(),
                    size: 0,
                });
            }
            let ci = clusters.len() - 1;
            let c = &mut clusters[ci];
            c.size += data.len();
            c.blobs.push(data);
            location[i] = Some((ci as u32, (c.blobs.len() - 1) as u32));
        }

        let mut out = vec![0u8; 80];
        for m in &mimes {
            out.extend_from_slice(m.as_bytes());
            out.push(0);
        }
        out.push(0);

        let mut dirent_offsets = Vec::with_capacity(all.len());
        for (i, e) in all.iter().enumerate() {
            dirent_offsets.push(out.len() as u64);
            match &e.content {
                Content::Item { mime, .. } => {
                    out.extend_from_slice(&mime_index(mime).to_le_bytes());
                    out.push(0);
                    out.push(e.ns);
                    out.extend_from_slice(&0u32.to_le_bytes());
                    let (c, b) = location[i].unwrap();
                    out.extend_from_slice(&c.to_le_bytes());
                    out.extend_from_slice(&b.to_le_bytes());
                }
                Content::Redirect { ns, path } => {
                    out.extend_from_slice(&0xffffu16.to_le_bytes());
                    out.push(0);
                    out.push(e.ns);
                    out.extend_from_slice(&0u32.to_le_bytes());
                    out.extend_from_slice(&index_of(*ns, path).to_le_bytes());
                }
            }
            out.extend_from_slice(e.path.as_bytes());
            out.push(0);
            // like libzim, a title equal to the path is stored empty
            if e.title != e.path {
                out.extend_from_slice(e.title.as_bytes());
            }
            out.push(0);
        }

        let width = if self.extended { 8 } else { 4 };
        let push_off = |payload: &mut Vec<u8>, v: u64| {
            if width == 8 {
                payload.extend_from_slice(&v.to_le_bytes());
            } else {
                payload.extend_from_slice(&(v as u32).to_le_bytes());
            }
        };
        let mut cluster_offsets = Vec::new();
        for c in &clusters {
            cluster_offsets.push(out.len() as u64);
            let mut payload = Vec::new();
            let mut off = ((c.blobs.len() + 1) * width) as u64;
            push_off(&mut payload, off);
            for b in &c.blobs {
                off += b.len() as u64;
                push_off(&mut payload, off);
            }
            for b in &c.blobs {
                payload.extend_from_slice(b);
            }
            out.push(c.codec.code() | if self.extended { 0x10 } else { 0 });
            out.extend_from_slice(&c.codec.compress(&payload));
        }

        let path_ptr_pos = out.len() as u64;
        for o in &dirent_offsets {
            out.extend_from_slice(&o.to_le_bytes());
        }
        let title_ptr_pos = if self.header_title_list {
            let pos = out.len() as u64;
            let mut order: Vec<u32> = (0..all.len() as u32).collect();
            order.sort_by(|&a, &b| {
                (all[a as usize].ns, title_of(&all[a as usize]))
                    .cmp(&(all[b as usize].ns, title_of(&all[b as usize])))
                    .then(a.cmp(&b))
            });
            for i in order {
                out.extend_from_slice(&i.to_le_bytes());
            }
            pos
        } else {
            u64::MAX
        };
        let cluster_ptr_pos = out.len() as u64;
        for o in &cluster_offsets {
            out.extend_from_slice(&o.to_le_bytes());
        }
        let checksum_pos = out.len() as u64;

        let main_page = match &self.main {
            Some((ns, path)) => {
                if self.minor >= 1 {
                    index_of(b'W', "mainPage")
                } else {
                    index_of(*ns, path)
                }
            }
            None => u32::MAX,
        };
        out[0..4].copy_from_slice(&0x044D_495Au32.to_le_bytes());
        out[4..6].copy_from_slice(&self.major.to_le_bytes());
        out[6..8].copy_from_slice(&self.minor.to_le_bytes());
        out[8..24].copy_from_slice(&self.uuid);
        out[24..28].copy_from_slice(&(all.len() as u32).to_le_bytes());
        out[28..32].copy_from_slice(&(clusters.len() as u32).to_le_bytes());
        out[32..40].copy_from_slice(&path_ptr_pos.to_le_bytes());
        out[40..48].copy_from_slice(&title_ptr_pos.to_le_bytes());
        out[48..56].copy_from_slice(&cluster_ptr_pos.to_le_bytes());
        out[56..64].copy_from_slice(&80u64.to_le_bytes());
        out[64..68].copy_from_slice(&main_page.to_le_bytes());
        out[68..72].copy_from_slice(&u32::MAX.to_le_bytes());
        out[72..80].copy_from_slice(&checksum_pos.to_le_bytes());
        let digest: [u8; 16] = Md5::digest(&out).into();
        out.extend_from_slice(&digest);

        Layout {
            bytes: out,
            dirent_offsets,
            cluster_offsets,
            path_ptr_pos,
            title_ptr_pos,
            cluster_ptr_pos,
            checksum_pos,
            order: all.iter().map(|e| (e.ns, e.path.clone())).collect(),
        }
    }
}

/// A representative archive used by several tests.
pub fn sample(new_scheme: bool) -> ZimBuilder {
    let b = if new_scheme {
        ZimBuilder::new_scheme()
    } else {
        ZimBuilder::old_scheme()
    };
    let ns = b.content_ns();
    b.html("Home", "Home", "<p>welcome</p>")
        .html("Apple", "Apple", "<p>fruit</p>")
        .html("Apple_pie", "Apple pie", "<p>dessert</p>")
        .html("Banana", "Banana", "<p>yellow</p>")
        .html("zebra", "zebra", "<p>stripes</p>")
        .redirect(ns, "Apples", "Apples", ns, "Apple")
        .item(ns, "style.css", "", "text/css", "body{}")
        .item(
            if new_scheme { ns } else { b'I' },
            "logo.png",
            "logo.png",
            "image/png",
            vec![0x89, b'P', b'N', b'G'],
        )
        .metadata("Title", "Sample")
        .metadata("Language", "eng")
        .metadata("Counter", "text/html=5;text/css=1;image/png=1")
        .metadata("Tags", "sample;_ftindex:no;_category:test")
        .main(ns, "Home")
}
