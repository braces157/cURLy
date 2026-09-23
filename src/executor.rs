use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    pin::Pin,
    time::Duration,
};

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::{Body, Client, Response, header::HeaderValue, redirect::Policy};
use serde::Deserialize;
use tokio_util::{io::ReaderStream, sync::CancellationToken};

use crate::{
    error::CurlyError,
    request::{AuthDefinition, BodyKind, BodySource, RequestDefinition},
};

pub type ResponseStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

pub struct ExecutedResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub content_type: Option<String>,
    pub elapsed_to_headers: Duration,
    pub stream: ResponseStream,
}

#[derive(Debug, Clone)]
pub struct RequestPreview {
    pub bytes: Vec<u8>,
    pub truncated: bool,
    pub total_bytes: u64,
}

pub async fn execute(
    request: &RequestDefinition,
    cancel: &CancellationToken,
) -> Result<(ExecutedResponse, RequestPreview), CurlyError> {
    let client = build_client(request)?;
    let method = request.method()?;
    let mut builder = client.request(method, &request.url).query(&request.query);

    for (name, value) in &request.headers {
        let value = HeaderValue::from_str(value).map_err(|_| {
            CurlyError::Invalid(format!("invalid value for request header {name:?}"))
        })?;
        builder = builder.header(name, value);
    }

    match &request.auth {
        Some(AuthDefinition::BasicLiteral { username, password }) => {
            builder = builder.basic_auth(username, Some(password));
        }
        Some(AuthDefinition::BearerLiteral { token }) => {
            builder = builder.bearer_auth(token);
        }
        Some(AuthDefinition::BearerEnv { variable }) => {
            let token = std::env::var(variable).map_err(|_| {
                CurlyError::Invalid(format!(
                    "environment variable {variable:?} required for bearer authentication is not set"
                ))
            })?;
            builder = builder.bearer_auth(token);
        }
        None => {}
    }

    let (body, preview) = tokio::select! {
        _ = cancel.cancelled() => return Err(CurlyError::Cancelled),
        result = prepare_body(request, cancel) => result?,
    };
    if let Some(body) = body {
        if matches!(
            request.body.as_ref().map(BodySource::kind),
            Some(BodyKind::Json)
        ) && !request.has_header("content-type")
        {
            builder = builder.header("content-type", "application/json");
        }
        builder = builder.body(body);
    }

    let started = std::time::Instant::now();
    let response = tokio::select! {
        _ = cancel.cancelled() => return Err(CurlyError::Cancelled),
        result = builder.send() => result?,
    };
    let elapsed_to_headers = started.elapsed();
    Ok((response_parts(response, elapsed_to_headers), preview))
}

fn build_client(request: &RequestDefinition) -> Result<Client, CurlyError> {
    let redirect = if request.transport.follow {
        Policy::limited(10)
    } else {
        Policy::none()
    };
    let mut builder = Client::builder()
        .redirect(redirect)
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_millis(request.transport.connect_timeout_ms))
        .danger_accept_invalid_certs(request.transport.insecure)
        .user_agent(concat!("curly/", env!("CARGO_PKG_VERSION")));
    if let Some(ms) = request.transport.total_timeout_ms {
        builder = builder.timeout(Duration::from_millis(ms));
    }
    builder.build().map_err(CurlyError::Transport)
}

fn response_parts(response: Response, elapsed_to_headers: Duration) -> ExecutedResponse {
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or("<non-UTF8>").to_string(),
            )
        })
        .collect();
    ExecutedResponse {
        status,
        headers,
        content_type,
        elapsed_to_headers,
        stream: Box::pin(response.bytes_stream()),
    }
}

async fn prepare_body(
    request: &RequestDefinition,
    cancel: &CancellationToken,
) -> Result<(Option<Body>, RequestPreview), CurlyError> {
    const PREVIEW_LIMIT: usize = 64 * 1024;
    let Some(source) = &request.body else {
        return Ok((
            None,
            RequestPreview {
                bytes: vec![],
                truncated: false,
                total_bytes: 0,
            },
        ));
    };

    match source {
        BodySource::Inline { kind, value } => {
            let bytes = validate_json_bytes(*kind, value.as_bytes().to_vec(), cancel).await?;
            let total_bytes = bytes.len() as u64;
            let preview = bytes[..bytes.len().min(PREVIEW_LIMIT)].to_vec();
            Ok((
                Some(Body::from(bytes)),
                RequestPreview {
                    bytes: preview,
                    truncated: total_bytes > PREVIEW_LIMIT as u64,
                    total_bytes,
                },
            ))
        }
        BodySource::File { kind, path } => {
            if *kind == BodyKind::Json {
                validate_json_file(path, cancel).await?;
            }
            let metadata = tokio::fs::metadata(path).await.map_err(|err| {
                CurlyError::Invalid(format!("cannot read body file {}: {err}", path.display()))
            })?;
            let preview_file = tokio::fs::File::open(path).await?;
            let mut preview = Vec::with_capacity(PREVIEW_LIMIT.min(metadata.len() as usize));
            use tokio::io::AsyncReadExt;
            preview_file
                .take(PREVIEW_LIMIT as u64)
                .read_to_end(&mut preview)
                .await?;
            let file = tokio::fs::File::open(path).await?;
            let stream = ReaderStream::new(file);
            Ok((
                Some(Body::wrap_stream(stream)),
                RequestPreview {
                    bytes: preview,
                    truncated: metadata.len() > PREVIEW_LIMIT as u64,
                    total_bytes: metadata.len(),
                },
            ))
        }
        BodySource::Stdin { kind } => {
            let mut bytes = Vec::new();
            use tokio::io::AsyncReadExt;
            let mut stdin = tokio::io::stdin();
            tokio::select! {
                _ = cancel.cancelled() => return Err(CurlyError::Cancelled),
                result = stdin.read_to_end(&mut bytes) => result?,
            };
            let bytes = validate_json_bytes(*kind, bytes, cancel).await?;
            let total_bytes = bytes.len() as u64;
            let preview = bytes[..bytes.len().min(PREVIEW_LIMIT)].to_vec();
            Ok((
                Some(Body::from(bytes)),
                RequestPreview {
                    bytes: preview,
                    truncated: total_bytes > PREVIEW_LIMIT as u64,
                    total_bytes,
                },
            ))
        }
    }
}

async fn validate_json_bytes(
    kind: BodyKind,
    bytes: Vec<u8>,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, CurlyError> {
    if kind != BodyKind::Json {
        return Ok(bytes);
    }

    let worker_cancel = cancel.clone();
    let task = tokio::task::spawn_blocking(move || {
        let result = {
            let reader = CancellationReader::new(
                std::io::Cursor::new(bytes.as_slice()),
                worker_cancel.clone(),
            );
            let mut deserializer = serde_json::Deserializer::from_reader(reader);
            serde::de::IgnoredAny::deserialize(&mut deserializer).and_then(|_| deserializer.end())
        };
        if worker_cancel.is_cancelled() {
            return Err(CurlyError::Cancelled);
        }
        result.map_err(|err| CurlyError::Invalid(format!("invalid JSON request body: {err}")))?;
        Ok(bytes)
    });

    tokio::select! {
        _ = cancel.cancelled() => Err(CurlyError::Cancelled),
        result = task => result
            .map_err(|err| CurlyError::Io(io::Error::other(format!("JSON validation worker failed: {err}"))))?,
    }
}

async fn validate_json_file(path: &Path, cancel: &CancellationToken) -> Result<(), CurlyError> {
    let path = path.to_path_buf();
    let worker_cancel = cancel.clone();
    let task =
        tokio::task::spawn_blocking(move || validate_json_file_blocking(path, worker_cancel));
    tokio::select! {
        _ = cancel.cancelled() => Err(CurlyError::Cancelled),
        result = task => result
            .map_err(|err| CurlyError::Io(io::Error::other(format!("JSON validation worker failed: {err}"))))?,
    }
}

fn validate_json_file_blocking(path: PathBuf, cancel: CancellationToken) -> Result<(), CurlyError> {
    let file = std::fs::File::open(&path).map_err(|err| {
        CurlyError::Invalid(format!(
            "cannot open JSON body file {}: {err}",
            path.display()
        ))
    })?;
    let reader = CancellationReader::new(file, cancel.clone());
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let result =
        serde::de::IgnoredAny::deserialize(&mut deserializer).and_then(|_| deserializer.end());
    if cancel.is_cancelled() {
        return Err(CurlyError::Cancelled);
    }
    result.map_err(|err| CurlyError::Invalid(format!("invalid JSON in {}: {err}", path.display())))
}

struct CancellationReader<R> {
    inner: R,
    cancel: CancellationToken,
}

impl<R> CancellationReader<R> {
    fn new(inner: R, cancel: CancellationToken) -> Self {
        Self { inner, cancel }
    }
}

impl<R: Read> Read for CancellationReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("operation cancelled"));
        }
        self.inner.read(buffer)
    }
}

pub async fn collect_stream(
    mut stream: ResponseStream,
    cancel: &CancellationToken,
    preview_limit: usize,
) -> Result<(Vec<u8>, bool, u64), CurlyError> {
    let mut preview = Vec::with_capacity(preview_limit);
    let mut total = 0_u64;
    while let Some(chunk) = tokio::select! {
        _ = cancel.cancelled() => return Err(CurlyError::Cancelled),
        chunk = stream.next() => chunk,
    } {
        let chunk = chunk?;
        total = total.saturating_add(chunk.len() as u64);
        if preview.len() < preview_limit {
            let remaining = preview_limit - preview.len();
            preview.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        }
    }
    Ok((preview, total > preview_limit as u64, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::TransportOptions;

    #[tokio::test]
    async fn cancelled_request_stops_before_transport() {
        let request = RequestDefinition::new(
            "https://example.test/never-sent".into(),
            None,
            vec![],
            vec![],
            None,
            None,
            TransportOptions::default(),
        )
        .unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = match execute(&request, &cancellation).await {
            Ok(_) => panic!("cancelled request unexpectedly executed"),
            Err(error) => error,
        };
        assert!(matches!(error, CurlyError::Cancelled));
        assert_eq!(error.exit_code(), 130);
    }

    #[tokio::test]
    async fn json_validation_honors_cancellation() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let result = validate_json_bytes(
            BodyKind::Json,
            br#"{"large":[1,2,3]}"#.to_vec(),
            &cancellation,
        )
        .await;
        assert!(matches!(result, Err(CurlyError::Cancelled)));
    }

    #[tokio::test]
    async fn json_validation_rejects_trailing_data() {
        let cancellation = CancellationToken::new();
        let result = validate_json_bytes(
            BodyKind::Json,
            br#"{"ok":true} trailing"#.to_vec(),
            &cancellation,
        )
        .await;
        assert!(matches!(result, Err(CurlyError::Invalid(_))));
    }
}
