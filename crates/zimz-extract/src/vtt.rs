// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Regis Boudinot

//! WebVTT subtitles → plain transcript text (youtube2zim stores `video.<lang>.vtt`).

/// Cue text lines without timestamps, settings, tags or duplicates of the previous cue.
pub fn transcript(vtt: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_cue = false;
    for raw in vtt.lines() {
        let line = raw.trim();
        if line.is_empty() {
            in_cue = false;
            continue;
        }
        if line.starts_with("WEBVTT")
            || line.starts_with("NOTE")
            || line.starts_with("STYLE")
            || line.starts_with("REGION")
            || line.starts_with("Kind:")
            || line.starts_with("Language:")
        {
            continue;
        }
        if line.contains("-->") {
            in_cue = true;
            continue;
        }
        if !in_cue {
            // cue identifier line
            continue;
        }
        let text = strip_tags(line);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if out.last().is_some_and(|prev| prev == text) {
            continue;
        }
        out.push(text.to_string());
    }
    out.join("\n")
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cues() {
        let vtt = "WEBVTT\nKind: captions\nLanguage: en\n\n1\n00:00:00.000 --> 00:00:02.000 align:start\nHello <b>world</b>\n\n2\n00:00:02.000 --> 00:00:04.000\nHello world\n\n00:00:04.000 --> 00:00:06.000\n<c.colorE5E5E5>Second</c> line &amp; more\n";
        assert_eq!(transcript(vtt), "Hello world\nSecond line & more");
        assert_eq!(transcript(""), "");
    }
}
