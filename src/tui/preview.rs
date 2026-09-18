//! Bounded, terminal-safe response presentation. Never used for pipeline bytes.
use ratatui::{
    style::{Color, Style},
    text::{Line, Span, Text},
};

use crate::output::{HISTORY_PREVIEW_LIMIT, escape_terminal_bytes};

const KEY: Color = Color::Cyan;
const STRING: Color = Color::Green;
const NUMBER: Color = Color::Yellow;
const LITERAL: Color = Color::Magenta;

#[derive(Debug, Clone)]
pub(super) struct Document {
    source: Text<'static>,
    wrapped: Text<'static>,
    width: u16,
}

impl Document {
    pub(super) fn new(source: Text<'static>) -> Self {
        Self {
            source,
            wrapped: Text::default(),
            width: 0,
        }
    }

    pub(super) fn at_width(&mut self, width: u16) -> &Text<'static> {
        let width = width.max(1);
        if self.width != width {
            self.width = width;
            let mut lines = Vec::new();
            for line in &self.source.lines {
                let mut current = Line::default().style(line.style);
                let mut used = 0;
                for span in &line.spans {
                    let mut start = 0;
                    for (index, ch) in span.content.char_indices() {
                        let mut bytes = [0; 4];
                        let cells = Span::raw(ch.encode_utf8(&mut bytes) as &str).width();
                        if used + cells > usize::from(width) && used > 0 {
                            if start < index {
                                current.spans.push(Span::styled(
                                    span.content[start..index].to_owned(),
                                    span.style,
                                ));
                            }
                            lines.push(current);
                            current = Line::default().style(line.style);
                            used = 0;
                            start = index;
                        }
                        used += cells;
                    }
                    if start < span.content.len() {
                        current
                            .spans
                            .push(Span::styled(span.content[start..].to_owned(), span.style));
                    }
                }
                lines.push(current);
            }
            self.wrapped = Text::from(lines).style(self.source.style);
        }
        &self.wrapped
    }
}

pub(super) fn headers(pairs: &[(String, String)]) -> Text<'static> {
    Text::from(
        pairs
            .iter()
            .map(|(name, value)| {
                Line::from(vec![
                    Span::styled(
                        escape_terminal_bytes(name.as_bytes()),
                        Style::default().fg(KEY),
                    ),
                    Span::raw(": "),
                    Span::raw(escape_terminal_bytes(value.as_bytes())),
                ])
            })
            .collect::<Vec<_>>(),
    )
}

pub(super) fn body(bytes: &[u8], headers: &[(String, String)], truncated: bool) -> Text<'static> {
    let clipped = &bytes[..bytes.len().min(HISTORY_PREVIEW_LIMIT)];
    let content_type = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .map_or("", |(_, value)| value.as_str())
        .to_ascii_lowercase();
    let binary = clipped.contains(&0)
        || (std::str::from_utf8(clipped).is_err_and(|err| err.error_len().is_some()));
    let mut text = if clipped.is_empty() {
        Text::styled("Empty body", Style::default().fg(Color::DarkGray))
    } else if binary {
        Text::styled(
            format!(
                "Binary body · {} preview bytes · use --output to save the full response",
                clipped.len()
            ),
            Style::default().fg(Color::Yellow),
        )
    } else if let Ok(json) = serde_json::from_slice::<serde_json::Value>(clipped) {
        let pretty = serde_json::to_string_pretty(&json).unwrap_or_default();
        json_source(&escape_terminal_bytes(pretty.as_bytes()))
    } else {
        let safe = escape_terminal_bytes(clipped);
        if content_type.contains("html")
            || content_type.contains("xml")
            || safe.trim_start().starts_with('<')
        {
            markup_text(&safe)
        } else if content_type.contains("json") {
            // An incomplete history preview can still have useful token colors.
            json_source(&safe)
        } else {
            Text::raw(safe)
        }
    };
    if truncated || bytes.len() > clipped.len() {
        text.lines.push(Line::styled(
            "… Preview truncated at 64 KiB …",
            Style::default().fg(Color::Yellow),
        ));
    }
    text
}

fn styled(token: &str, color: Color) -> Span<'static> {
    Span::styled(token.to_owned(), Style::default().fg(color))
}

fn quoted_end(text: &str, start: usize) -> usize {
    let quote = text.as_bytes()[start];
    let mut index = start + 1;
    while index < text.len() {
        match text.as_bytes()[index] {
            b'\\' => index = (index + 2).min(text.len()),
            ch if ch == quote => return index + 1,
            _ => index += 1,
        }
    }
    text.len()
}

pub(super) fn json_source(text: &str) -> Text<'static> {
    Text::from(
        text.lines()
            .map(|line| {
                let mut spans = Vec::new();
                let mut index = 0;
                while index < line.len() {
                    let start = index;
                    let ch = line.as_bytes()[index];
                    let color = if ch == b'"' {
                        index = quoted_end(line, index);
                        if line[index..].trim_start().starts_with(':') {
                            KEY
                        } else {
                            STRING
                        }
                    } else if ch.is_ascii_digit() || ch == b'-' {
                        index += 1;
                        while index < line.len()
                            && matches!(
                                line.as_bytes()[index],
                                b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'
                            )
                        {
                            index += 1;
                        }
                        NUMBER
                    } else if ch.is_ascii_alphabetic() {
                        index += 1;
                        while index < line.len() && line.as_bytes()[index].is_ascii_alphabetic() {
                            index += 1;
                        }
                        LITERAL
                    } else {
                        index += line[index..].chars().next().unwrap().len_utf8();
                        Color::Gray
                    };
                    spans.push(styled(&line[start..index], color));
                }
                Line::from(spans)
            })
            .collect::<Vec<_>>(),
    )
}

// Display-only tag layout. Quoted '>' characters, comments and raw text elements
// must not be split as tags. This deliberately does not interpret embedded JS/CSS.
fn markup_text(text: &str) -> Text<'static> {
    let lower = text.to_ascii_lowercase();
    let mut lines = Vec::new();
    let mut index = 0;
    let mut depth = 0usize;
    while index < text.len() {
        if text[index..].starts_with('<') {
            let start = index;
            if text[index..].starts_with("<!--") {
                index = text[index..]
                    .find("-->")
                    .map_or(text.len(), |end| index + end + 3);
                append_lines(&mut lines, &text[start..index], depth, true);
                continue;
            }
            index += 1;
            let mut quote = None;
            while index < text.len() {
                let ch = text.as_bytes()[index];
                index += 1;
                if let Some(q) = quote {
                    if ch == q {
                        quote = None;
                    }
                } else if ch == b'\'' || ch == b'"' {
                    quote = Some(ch);
                } else if ch == b'>' {
                    break;
                }
            }
            let tag = &text[start..index];
            let closing = tag.starts_with("</");
            if closing {
                depth = depth.saturating_sub(1);
            }
            append_lines(&mut lines, tag, depth, false);
            let name = tag
                .trim_start_matches('<')
                .trim_start_matches('/')
                .split(|ch: char| ch.is_whitespace() || ch == '>' || ch == '/')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            let void = matches!(
                name.as_str(),
                "area"
                    | "base"
                    | "br"
                    | "col"
                    | "embed"
                    | "hr"
                    | "img"
                    | "input"
                    | "link"
                    | "meta"
                    | "param"
                    | "source"
                    | "track"
                    | "wbr"
            );
            if !closing
                && !void
                && !tag.ends_with("/>")
                && !tag.starts_with("<!")
                && !tag.starts_with("<?")
            {
                depth = (depth + 1).min(16);
                if matches!(name.as_str(), "script" | "style" | "pre" | "textarea") {
                    let end = lower[index..]
                        .find(&format!("</{name}"))
                        .map_or(text.len(), |end| index + end);
                    for line in text[index..end].lines() {
                        lines.push(Line::raw(format!("{}{line}", "  ".repeat(depth))));
                    }
                    index = end;
                }
            }
        } else {
            let end = text[index..]
                .find('<')
                .map_or(text.len(), |end| index + end);
            for line in text[index..end]
                .lines()
                .filter(|line| !line.trim().is_empty())
            {
                lines.push(Line::raw(format!("{}{}", "  ".repeat(depth), line.trim())));
            }
            index = end;
        }
    }
    Text::from(lines)
}

fn append_lines(lines: &mut Vec<Line<'static>>, tag: &str, depth: usize, comment: bool) {
    for line in tag.lines() {
        let mut spans = vec![Span::raw("  ".repeat(depth))];
        let mut index = 0;
        while index < line.len() {
            let start = index;
            let ch = line.as_bytes()[index];
            let color = if comment {
                index = line.len();
                Color::DarkGray
            } else if ch == b'"' || ch == b'\'' {
                index = quoted_end(line, index);
                STRING
            } else if ch.is_ascii_alphanumeric() || ch == b'-' || ch == b':' {
                index += 1;
                while index < line.len()
                    && (line.as_bytes()[index].is_ascii_alphanumeric()
                        || matches!(line.as_bytes()[index], b'-' | b':' | b'_'))
                {
                    index += 1;
                }
                if line[..start].trim_matches(['<', '/', '!', '?']).is_empty() {
                    KEY
                } else {
                    NUMBER
                }
            } else {
                index += line[index..].chars().next().unwrap().len_utf8();
                Color::Gray
            };
            spans.push(styled(&line[start..index], color));
        }
        lines.push(Line::from(spans));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plain(text: &Text<'_>) -> String {
        text.lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn wrapping_preserves_unicode_colors_and_last_line_after_resize() {
        let source = Text::from(vec![Line::styled(
            "é日abcdefLAST",
            Style::default().fg(Color::Red),
        )]);
        let mut document = Document::new(source);
        for width in [4, 10, 3] {
            let text = document.at_width(width);
            let joined = plain(text).replace('\n', "");
            assert_eq!(joined, "é日abcdefLAST");
            assert!(
                text.lines
                    .iter()
                    .all(|line| line.width() <= usize::from(width))
            );
            assert!(
                text.lines
                    .iter()
                    .all(|line| line.style.fg == Some(Color::Red))
            );
        }
    }

    #[test]
    fn json_has_distinct_token_colors_and_preserves_values() {
        let text = body(
            br#"{"name":"a\"b","n":-1.2e3,"ok":true,"none":null}"#,
            &[],
            false,
        );
        for color in [KEY, STRING, NUMBER, LITERAL] {
            assert!(
                text.lines
                    .iter()
                    .flat_map(|line| &line.spans)
                    .any(|span| span.style.fg == Some(color))
            );
        }
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&plain(&text)).unwrap()["name"],
            "a\"b"
        );
    }
    #[test]
    fn html_tags_attributes_comments_and_scripts_are_readable() {
        let text = body(b"<!DOCTYPE html><html><meta name=\"x\" content=\"a>b\"><body><!-- a > b --><script>if(a<b){x='<div>';}</script><p>Hello</p></body></html>", &[], true);
        let plain = plain(&text);
        assert!(plain.contains("content=\"a>b\""));
        assert!(plain.contains("if(a<b){x='<div>';}"));
        assert!(plain.contains("Preview truncated"));
        assert!(text.lines.len() > 8);
        assert!(
            text.lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.style.fg == Some(STRING))
        );
    }
    #[test]
    fn malformed_unicode_controls_and_binary_are_safe() {
        for input in [
            "{\"é\":\"日",
            "<a x='日",
            "plain\u{1b}[31m",
            "<!--unfinished",
        ] {
            let text = body(
                input.as_bytes(),
                &[("Content-Type".into(), "application/json".into())],
                true,
            );
            assert!(!plain(&text).contains('\u{1b}'));
        }
        assert!(plain(&body(b"a\0b", &[], false)).contains("Binary body"));
    }
}
