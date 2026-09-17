use std::{pin::Pin, time::Duration};

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::{Body, Client, Response, header::HeaderValue, redirect::Policy};
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

    let (body, preview) = prepare_body(request).await?;
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
            let bytes = value.as_bytes();
            validate_json_if_needed(*kind, bytes)?;
            let preview = bytes[..bytes.len().min(PREVIEW_LIMIT)].to_vec();
            Ok((
                Some(Body::from(value.clone())),
                RequestPreview {
                    bytes: preview,
                    truncated: bytes.len() > PREVIEW_LIMIT,
                    total_bytes: bytes.len() as u64,
                },
            ))
        }
        BodySource::File { kind, path } => {
            if *kind == BodyKind::Json {
                let validation_file = std::fs::File::open(path).map_err(|err| {
                    CurlyError::Invalid(format!(
                        "cannot open JSON body file {}: {err}",
                        path.display()
                    ))
                })?;
                serde_json::from_reader::<_, serde_json::Value>(validation_file).map_err(
                    |err| CurlyError::Invalid(format!("invalid JSON in {}: {err}", path.display())),
                )?;
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
            tokio::io::stdin().read_to_end(&mut bytes).await?;
            validate_json_if_needed(*kind, &bytes)?;
            let preview = bytes[..bytes.len().min(PREVIEW_LIMIT)].to_vec();
            Ok((
                Some(Body::from(bytes.clone())),
                RequestPreview {
                    bytes: preview,
                    truncated: bytes.len() > PREVIEW_LIMIT,
                    total_bytes: bytes.len() as u64,
                },
            ))
        }
    }
}

fn validate_json_if_needed(kind: BodyKind, bytes: &[u8]) -> Result<(), CurlyError> {
    if kind == BodyKind::Json {
        serde_json::from_slice::<serde_json::Value>(bytes)
            .map_err(|err| CurlyError::Invalid(format!("invalid JSON request body: {err}")))?;
    }
    Ok(())
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
}
