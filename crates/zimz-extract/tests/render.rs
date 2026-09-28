// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot

//! Renderer behaviour on small HTML snippets shaped like each scraper's output.

use zimz_extract::render::RenderOptions;
use zimz_extract::{Adapter, LinkTarget, extract_html};

fn opts() -> RenderOptions<'static> {
    RenderOptions {
        base_namespace: b'C',
        base_path: "Page",
        new_scheme: true,
        link_prefix: Some("zim://wiki/"),
        image_sources: false,
    }
}

#[test]
fn headings_paragraphs_and_sections() {
    let html = "<html><head><title>T</title></head><body><h1>Title</h1><p>Intro   text\n  here.</p><h2>Alpha</h2><p>A1</p><h3>Sub</h3><p>S1</p><h2>Beta</h2><p>B1</p></body></html>";
    let d = extract_html(html, Adapter::Generic, None, &opts());
    assert_eq!(d.title, "T");
    assert_eq!(
        d.markdown,
        "# Title\n\nIntro text here.\n\n## Alpha\n\nA1\n\n### Sub\n\nS1\n\n## Beta\n\nB1\n"
    );
    let titles: Vec<(u8, &str)> = d
        .sections
        .iter()
        .map(|s| (s.level, s.title.as_str()))
        .collect();
    assert_eq!(
        titles,
        vec![(1, "Title"), (2, "Alpha"), (3, "Sub"), (2, "Beta")]
    );
    assert_eq!(
        d.section_markdown(1),
        Some("## Alpha\n\nA1\n\n### Sub\n\nS1\n\n"),
        "a section runs until the next heading of its level or higher"
    );
    assert_eq!(d.section_markdown(2), Some("### Sub\n\nS1\n\n"));
    assert_eq!(d.section_markdown(3), Some("## Beta\n\nB1\n"));
    assert_eq!(d.find_section("beta"), Some(3));
    assert_eq!(d.find_section("2"), Some(2));
    assert_eq!(d.find_section("nope"), None);
    assert_eq!(
        d.text,
        "Title\n\nIntro text here.\n\nAlpha\n\nA1\n\nSub\n\nS1\n\nBeta\n\nB1\n"
    );
    assert_eq!(d.word_count, 10);
    let outline = d.outline();
    assert_eq!(
        outline[1].chars,
        d.section_markdown(1).unwrap().chars().count()
    );
}

#[test]
fn inline_markup_links_and_images() {
    let html = r##"<body><p>See <a href="Other_Page">the <b>other</b> page</a>, <a href="https://x.org/">x.org</a>, <a href="#Sec">below</a> and <em>this</em> <code>code</code> <s>gone</s>.</p><p><img src="a.png" alt="An image"> <img src="b.png" alt=""></p><p><a href="javascript:void(0)">js</a> <a href="Enc%C3%B3ded#frag">enc</a></p></body>"##;
    let d = extract_html(html, Adapter::Generic, None, &opts());
    assert_eq!(
        d.markdown,
        "See [the **other** page](zim://wiki/Other_Page), [x.org](https://x.org/), [below](#Sec) and *this* `code` ~~gone~~.\n\n![An image]\n\njs [enc](zim://wiki/Encóded#frag)\n"
    );
    assert_eq!(
        d.text,
        "See the other page, x.org, below and this code gone.\n\nAn image\n\njs enc\n"
    );
    assert_eq!(d.links.len(), 4);
    assert_eq!(
        d.links[0].target,
        LinkTarget::Internal {
            namespace: b'C',
            path: "Other_Page".into(),
            fragment: None
        }
    );
    assert_eq!(
        d.links[1].target,
        LinkTarget::External("https://x.org/".into())
    );
    assert_eq!(d.links[2].target, LinkTarget::Anchor("Sec".into()));
    assert_eq!(
        d.internal_links(true),
        vec![
            ("the other page".to_string(), "Other_Page".to_string()),
            ("enc".to_string(), "Encóded".to_string())
        ]
    );
}

#[test]
fn lists_nested_and_ordered() {
    let html = "<body><ul><li>one</li><li>two<ul><li>two.a</li><li>two.b</li></ul></li></ul><ol start=\"3\"><li>three</li><li>four</li></ol></body>";
    let d = extract_html(html, Adapter::Generic, None, &opts());
    assert_eq!(
        d.markdown,
        "- one\n- two\n  - two.a\n  - two.b\n\n3. three\n4. four\n"
    );
}

#[test]
fn tables_code_quotes_and_definitions() {
    let html = r#"<body><table><tr><th>Name</th><th>Value|x</th></tr><tr><td>a</td><td>1</td></tr><tr><td>b</td><td>2</td></tr></table>
<pre><code class="language-rust">fn main() {
    println!("hi");
}</code></pre><blockquote><p>quoted <b>bold</b></p><p>second</p></blockquote><dl><dt>Term</dt><dd>Definition one</dd><dd>Definition two</dd></dl><hr><p>after</p></body>"#;
    let d = extract_html(html, Adapter::Generic, None, &opts());
    assert_eq!(
        d.markdown,
        "| Name | Value\\|x |\n|---|---|\n| a | 1 |\n| b | 2 |\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\n> quoted **bold**\n\n> second\n\n**Term**\n  Definition one\n  Definition two\n\n---\n\nafter\n"
    );
    assert!(
        d.text.contains("Name | Value\\|x")
            && d.text.contains("fn main() {")
            && !d.text.contains("```")
    );
}

#[test]
fn skips_scripts_styles_and_hidden_and_collapses_whitespace() {
    let html = "<body><script>var x = 1;</script><style>p{}</style><div hidden>secret</div><noscript>ns</noscript><p>  visible \n\n text  </p><p></p><p>   </p><div><div><div>deep</div></div></div></body>";
    let d = extract_html(html, Adapter::Generic, None, &opts());
    assert_eq!(d.markdown, "visible text\n\ndeep\n");
}

#[test]
fn mwoffliner_boilerplate_is_pruned_and_infobox_becomes_a_list() {
    let html = r##"<html><head><title>Zstd</title></head><body><div id="mw-content-text"><div class="mw-parser-output">
<div class="hatnote">Not to be confused with Z.</div>
<table class="infobox"><tbody><tr><th>Developer</th><td>Yann Collet</td></tr><tr><th>Initial release</th><td>2015</td></tr></tbody></table>
<p><b>Zstandard</b> is a compression algorithm.<sup class="reference" id="cite_ref-1"><a href="#cite_note-1">[1]</a></sup> See <a href="LZ4">LZ4</a>.</p>
<div class="mw-heading mw-heading2"><h2 id="Features">Features<span class="mw-editsection">[edit]</span></h2></div>
<p>Fast.</p>
<div class="mw-heading mw-heading2"><h2 id="References">References</h2></div>
<div class="reflist"><ol class="references"><li id="cite_note-1">Ref one</li></ol></div>
<div class="navbox"><table><tr><td><a href="Other">navigation</a></td></tr></table></div>
</div></div><div class="zim-footer">footer</div></body></html>"##;
    let d = extract_html(html, Adapter::MwOffliner, None, &opts());
    assert_eq!(d.title, "Zstd");
    assert!(!d.markdown.contains("Not to be confused"), "hatnote pruned");
    assert!(
        !d.markdown.contains("[1]") && !d.markdown.contains("Ref one"),
        "references pruned"
    );
    assert!(
        !d.markdown.contains("navigation")
            && !d.markdown.contains("footer")
            && !d.markdown.contains("[edit]")
    );
    assert_eq!(
        d.markdown,
        "- **Developer:** Yann Collet\n- **Initial release:** 2015\n\n**Zstandard** is a compression algorithm. See [LZ4](zim://wiki/LZ4).\n\n## Features\n\nFast.\n",
        "the empty References section goes with its reflist"
    );
    assert_eq!(d.links.len(), 1);
    assert_eq!(
        d.sections
            .iter()
            .map(|s| s.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Features"]
    );
}

#[test]
fn old_scheme_links_resolve_namespaces() {
    let html = r#"<body><div id="mw-content-text"><p><a href="STEMI">STEMI</a> <a href="../I/m/x.png">img</a></p></div></body>"#;
    let o = RenderOptions {
        base_namespace: b'A',
        base_path: "Acute_chest_pain",
        new_scheme: false,
        link_prefix: Some("zim://wikem/"),
        image_sources: false,
    };
    let d = extract_html(html, Adapter::MwOffliner, None, &o);
    assert_eq!(
        d.markdown,
        "[STEMI](zim://wikem/A/STEMI) [img](zim://wikem/I/m/x.png)\n"
    );
}

#[test]
fn zimit_and_devdocs_content_roots() {
    let html = r#"<body><nav id="menu">menu</nav><header>hdr</header><main><h2>Synopsis</h2><p>cat [OPTION]</p></main><footer>ftr</footer><script src="../_zim_static/wombat.js"></script></body>"#;
    let d = extract_html(html, Adapter::Zimit, None, &opts());
    assert_eq!(d.markdown, "## Synopsis\n\ncat [OPTION]\n");
    let html = r#"<body><div class="_app"><devdocs-navbar>nav</devdocs-navbar><div class="_page _git"><h1>git rebase</h1><p>Reapply commits</p><div class="_attribution">© license</div></div></div></body>"#;
    let d = extract_html(html, Adapter::DevDocs, None, &opts());
    assert_eq!(d.markdown, "# git rebase\n\nReapply commits\n");
    assert_eq!(d.title, "git rebase");
}

#[test]
fn math_and_summary() {
    let html = r#"<body><p>Let <math alttext="a^2+b^2=c^2"><mi>a</mi></math> hold.</p><details><summary>More</summary><p>hidden text</p></details></body>"#;
    let d = extract_html(html, Adapter::Generic, None, &opts());
    assert_eq!(
        d.markdown,
        "Let $a^2+b^2=c^2$ hold.\n\n**More**\n\nhidden text\n"
    );
}

#[test]
fn headings_inside_links_and_summaries_keep_their_structure() {
    let html = r##"<body><a href="#Synopsis"><h2 id="Synopsis">Synopsis</h2></a><p>text</p><details><summary><h2 class="section-heading">Clinical Features</h2></summary><p>body</p></details><b><p>bold block</p></b></body>"##;
    let d = extract_html(html, Adapter::Zimit, None, &opts());
    assert_eq!(
        d.markdown,
        "## Synopsis\n\ntext\n\n## Clinical Features\n\nbody\n\nbold block\n"
    );
    assert_eq!(
        d.sections
            .iter()
            .map(|s| s.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Synopsis", "Clinical Features"]
    );
    assert_eq!(d.links.len(), 1, "the block link is still recorded");
}

#[test]
fn old_mwoffliner_reference_markers_are_pruned() {
    let html = r##"<body><div id="mw-content-text"><p>Chest pain<sup class="mw-ref" id="cite_ref-1"><a href="#cite_note-1"><span class="mw-reflink-text">[1]</span></a></sup> radiating.</p><div class="mw-references-wrap"><ol class="mw-references references"><li id="cite_note-1">Ref</li></ol></div></div></body>"##;
    let d = extract_html(html, Adapter::MwOffliner, None, &opts());
    assert_eq!(d.markdown, "Chest pain radiating.\n");
}
