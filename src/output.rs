use std::{
    io::IsTerminal,
    path::PathBuf,
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::{error::CurlyError, executor::ExecutedResponse};

pub const TERMINAL_JSON_LIMIT: usize = 1024 * 1024;
pub const HISTORY_PREVIEW_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone)]
pub struct OutputOptions {
    pub output: Option<PathBuf>,
    pub raw: bool,
    pub headers: bool,
    pub quiet: bool,
    pub color: ColorMode,
}

#[derive(Debug, Clone)]
pub struct OutputResult {
    pub status: u16,
    pub response_headers: Vec<(String, String)>,
    pub response_preview: Vec<u8>,
    pub response_truncated: bool,
    pub response_bytes: u64,
    pub elapsed: Duration,
}

pub async fn write_response(
    response: ExecutedResponse,
    options: &OutputOptions,
    cancel: &CancellationToken,
) -> Result<OutputResult, CurlyError> {
    let started = Instant::now()
        .checked_sub(response.elapsed_to_headers)
        .unwrap_or_else(Instant::now);
    let stdout_terminal = std::io::stdout().is_terminal();
    let terminal_body = options.output.is_none() && stdout_terminal && !options.raw;

    if options.headers {
        eprintln!("HTTP {}", response.status);
        for (name, value) in &response.headers {
            eprintln!("{name}: {value}");
        }
        eprintln!();
    }

    let status = response.status;
    let response_headers = response.headers.clone();
    let content_type = response.content_type.clone();
    let mut stream = response.stream;
    let mut preview = Vec::with_capacity(HISTORY_PREVIEW_LIMIT);
    let mut total = 0_u64;

    if terminal_body {
        let binary_by_type = content_type
            .as_deref()
            .is_some_and(|value| !is_textual_content_type(value));
        if binary_by_type {
            while let Some(chunk) = next_chunk(&mut stream, cancel).await? {
                total += chunk.len() as u64;
                append_preview(&mut preview, &chunk);
            }
            write_stdout(binary_summary(total, content_type.as_deref()).as_bytes()).await?;
        } else {
            let mut candidate = Vec::with_capacity(TERMINAL_JSON_LIMIT + 1);
            let mut overflow = false;
            let mut binary_detected = false;
            let mut wrote_terminal_text = false;
            let mut escaper = TerminalEscaper::default();

            while let Some(chunk) = next_chunk(&mut stream, cancel).await? {
                total += chunk.len() as u64;
                append_preview(&mut preview, &chunk);
                if binary_detected {
                    continue;
                }
                if !overflow && candidate.len() + chunk.len() <= TERMINAL_JSON_LIMIT {
                    candidate.extend_from_slice(&chunk);
                    continue;
                }

                if !overflow {
                    overflow = true;
                    let candidate_contains_nul = candidate.contains(&0);
                    let prefix = escaper.push(&candidate, false);
                    candidate.clear();
                    let rendered = escaper.push(&chunk, false);
                    if candidate_contains_nul || chunk.contains(&0) || escaper.saw_invalid_utf8() {
                        binary_detected = true;
                        continue;
                    }
                    if !prefix.is_empty() {
                        write_stdout(prefix.as_bytes()).await?;
                        wrote_terminal_text = true;
                    }
                    if !rendered.is_empty() {
                        write_stdout(rendered.as_bytes()).await?;
                        wrote_terminal_text = true;
                    }
                    continue;
                }

                let rendered = escaper.push(&chunk, false);
                if chunk.contains(&0) || escaper.saw_invalid_utf8() {
                    binary_detected = true;
                    continue;
                }
                write_stdout(rendered.as_bytes()).await?;
                wrote_terminal_text |= !rendered.is_empty();
            }

            if binary_detected {
                if wrote_terminal_text {
                    write_stdout(b"\n").await?;
                }
                write_stdout(binary_summary(total, content_type.as_deref()).as_bytes()).await?;
            } else if overflow {
                let rendered = escaper.push(&[], true);
                if escaper.saw_invalid_utf8() {
                    if wrote_terminal_text {
                        write_stdout(b"\n").await?;
                    }
                    write_stdout(binary_summary(total, content_type.as_deref()).as_bytes()).await?;
                } else {
                    write_stdout(rendered.as_bytes()).await?;
                }
            } else {
                display_terminal_candidate(&candidate, options.color).await?;
            }
        }
    } else {
        let mut writer: Box<dyn AsyncWrite + Unpin + Send> = if let Some(path) = &options.output {
            Box::new(tokio::fs::File::create(path).await.map_err(|err| {
                CurlyError::Io(std::io::Error::new(
                    err.kind(),
                    format!("cannot create output file {}: {err}", path.display()),
                ))
            })?)
        } else {
            Box::new(tokio::io::stdout())
        };

        while let Some(chunk) = next_chunk(&mut stream, cancel).await? {
            total += chunk.len() as u64;
            append_preview(&mut preview, &chunk);
            if let Err(err) = writer.write_all(&chunk).await {
                if err.kind() == std::io::ErrorKind::BrokenPipe {
                    return Ok(OutputResult {
                        status,
                        response_headers,
                        response_preview: preview,
                        response_truncated: total > HISTORY_PREVIEW_LIMIT as u64,
                        response_bytes: total,
                        elapsed: started.elapsed(),
                    });
                }
                return Err(CurlyError::Io(err));
            }
        }
        if let Err(err) = writer.flush().await
            && err.kind() != std::io::ErrorKind::BrokenPipe
        {
            return Err(CurlyError::Io(err));
        }
    }

    let elapsed = started.elapsed();
    if stdout_terminal && !options.quiet {
        eprintln!("HTTP {status} · {total} bytes · {:.0?}", elapsed);
    }

    Ok(OutputResult {
        status,
        response_headers,
        response_preview: preview,
        response_truncated: total > HISTORY_PREVIEW_LIMIT as u64,
        response_bytes: total,
        elapsed,
    })
}

async fn next_chunk(
    stream: &mut crate::executor::ResponseStream,
    cancel: &CancellationToken,
) -> Result<Option<bytes::Bytes>, CurlyError> {
    tokio::select! {
        _ = cancel.cancelled() => Err(CurlyError::Cancelled),
        chunk = stream.next() => match chunk {
            Some(Ok(chunk)) => Ok(Some(chunk)),
            Some(Err(err)) => Err(CurlyError::Transport(err)),
            None => Ok(None),
        }
    }
}

fn append_preview(preview: &mut Vec<u8>, chunk: &[u8]) {
    if preview.len() >= HISTORY_PREVIEW_LIMIT {
        return;
    }
    let remaining = HISTORY_PREVIEW_LIMIT - preview.len();
    preview.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
}

fn is_textual_content_type(value: &str) -> bool {
    let value = value.to_ascii_lowercase();
    value.starts_with("text/")
        || value.contains("json")
        || value.contains("xml")
        || value.contains("javascript")
        || value.contains("x-www-form-urlencoded")
        || value.contains("yaml")
}

fn binary_summary(total: u64, content_type: Option<&str>) -> String {
    match content_type {
        Some(content_type) => {
            format!("[binary response: {total} bytes, content-type: {content_type}]\n")
        }
        None => format!("[binary response: {total} bytes]\n"),
    }
}

async fn display_terminal_candidate(bytes: &[u8], color: ColorMode) -> Result<(), CurlyError> {
    if bytes.is_empty() {
        return Ok(());
    }

    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
        let pretty = serde_json::to_string_pretty(&value)
            .map_err(|err| CurlyError::Invalid(format!("cannot format JSON response: {err}")))?;
        let use_color = match color {
            ColorMode::Always => true,
            ColorMode::Never => false,
            ColorMode::Auto => std::io::stdout().is_terminal(),
        };
        if use_color {
            write_stdout(format!("\x1b[36m{pretty}\x1b[0m\n").as_bytes()).await?;
        } else {
            write_stdout(format!("{pretty}\n").as_bytes()).await?;
        }
        return Ok(());
    }

    if bytes.contains(&0) || std::str::from_utf8(bytes).is_err() {
        println!("[binary response: {} bytes]", bytes.len());
        return Ok(());
    }
    let mut escaper = TerminalEscaper::default();
    let rendered = escaper.push(bytes, true);
    write_stdout(rendered.as_bytes()).await?;
    if !rendered.ends_with('\n') {
        write_stdout(b"\n").await?;
    }
    Ok(())
}

async fn write_stdout(bytes: &[u8]) -> Result<(), CurlyError> {
    let mut stdout = tokio::io::stdout();
    if let Err(err) = stdout.write_all(bytes).await {
        if err.kind() == std::io::ErrorKind::BrokenPipe {
            return Ok(());
        }
        return Err(CurlyError::Io(err));
    }
    stdout.flush().await.map_err(CurlyError::Io)
}

#[derive(Default)]
struct TerminalEscaper {
    pending: Vec<u8>,
    invalid_utf8: bool,
}

impl TerminalEscaper {
    fn push(&mut self, bytes: &[u8], final_chunk: bool) -> String {
        let mut data = std::mem::take(&mut self.pending);
        data.extend_from_slice(bytes);
        let mut out = String::new();
        let mut offset = 0;

        while offset < data.len() {
            match std::str::from_utf8(&data[offset..]) {
                Ok(text) => {
                    escape_text(text, &mut out);
                    offset = data.len();
                }
                Err(err) => {
                    let valid = err.valid_up_to();
                    if valid > 0 {
                        let text = std::str::from_utf8(&data[offset..offset + valid])
                            .expect("valid UTF-8 prefix");
                        escape_text(text, &mut out);
                        offset += valid;
                    }
                    match err.error_len() {
                        Some(length) => {
                            self.invalid_utf8 = true;
                            for byte in &data[offset..offset + length] {
                                out.push_str(&format!("\\x{byte:02X}"));
                            }
                            offset += length;
                        }
                        None if final_chunk => {
                            self.invalid_utf8 = true;
                            for byte in &data[offset..] {
                                out.push_str(&format!("\\x{byte:02X}"));
                            }
                            offset = data.len();
                        }
                        None => {
                            self.pending.extend_from_slice(&data[offset..]);
                            break;
                        }
                    }
                }
            }
        }
        out
    }

    fn saw_invalid_utf8(&self) -> bool {
        self.invalid_utf8
    }
}

fn escape_text(text: &str, out: &mut String) {
    for ch in text.chars() {
        match ch {
            '\n' => out.push(ch),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => {
                if (ch as u32) <= 0xff {
                    out.push_str(&format!("\\x{:02X}", ch as u32));
                } else {
                    out.push_str(&format!("\\u{{{:X}}}", ch as u32));
                }
            }
            _ => out.push(ch),
        }
    }
}

pub fn escape_terminal_bytes(bytes: &[u8]) -> String {
    let mut escaper = TerminalEscaper::default();
    escaper.push(bytes, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_escaper_handles_split_utf8_and_controls() {
        let mut escaper = TerminalEscaper::default();
        let euro = "€".as_bytes();
        assert_eq!(escaper.push(&euro[..1], false), "");
        assert_eq!(escaper.push(&euro[1..], false), "€");
        assert_eq!(escaper.push(b"\x1b[31m", true), "\\x1B[31m");
        assert!(!escaper.saw_invalid_utf8());
    }

    #[test]
    fn terminal_escaper_flags_binary_utf8_errors() {
        let mut escaper = TerminalEscaper::default();
        assert_eq!(escaper.push(b"text", false), "text");
        let _ = escaper.push(&[0xff, 0xfe], false);
        assert!(escaper.saw_invalid_utf8());
    }

    #[test]
    fn terminal_escaper_does_not_emit_carriage_return_or_tab() {
        assert_eq!(
            escape_terminal_bytes(b"hello\rworld\tend\n"),
            "hello\\rworld\\tend\n"
        );
    }
}
