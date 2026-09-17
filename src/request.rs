use std::{path::PathBuf, str::FromStr, time::Duration};

use reqwest::{Method, header::HeaderName};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::CurlyError;

pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BodyKind {
    Json,
    Raw,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "source", rename_all = "lowercase")]
pub enum BodySource {
    Inline { kind: BodyKind, value: String },
    File { kind: BodyKind, path: PathBuf },
    Stdin { kind: BodyKind },
}

impl BodySource {
    pub fn kind(&self) -> BodyKind {
        match self {
            Self::Inline { kind, .. } | Self::File { kind, .. } | Self::Stdin { kind } => *kind,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthDefinition {
    BasicLiteral { username: String, password: String },
    BearerLiteral { token: String },
    BearerEnv { variable: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransportOptions {
    pub follow: bool,
    pub insecure: bool,
    pub connect_timeout_ms: u64,
    pub total_timeout_ms: Option<u64>,
}

impl Default for TransportOptions {
    fn default() -> Self {
        Self {
            follow: false,
            insecure: false,
            connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT.as_millis() as u64,
            total_timeout_ms: Some(DEFAULT_TOTAL_TIMEOUT.as_millis() as u64),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestDefinition {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub query: Vec<(String, String)>,
    pub body: Option<BodySource>,
    pub auth: Option<AuthDefinition>,
    pub transport: TransportOptions,
}

impl RequestDefinition {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        url: String,
        method: Option<String>,
        headers: Vec<(String, String)>,
        query: Vec<(String, String)>,
        body: Option<BodySource>,
        auth: Option<AuthDefinition>,
        transport: TransportOptions,
    ) -> Result<Self, CurlyError> {
        validate_url(&url)?;
        for (name, _) in &headers {
            HeaderName::from_str(name)
                .map_err(|_| CurlyError::Invalid(format!("invalid header name: {name}")))?;
        }

        let method = method.unwrap_or_else(|| {
            if body.is_some() {
                "POST".to_string()
            } else {
                "GET".to_string()
            }
        });
        Method::from_bytes(method.as_bytes())
            .map_err(|_| CurlyError::Invalid(format!("invalid HTTP method: {method}")))?;

        Ok(Self {
            method,
            url,
            headers,
            query,
            body,
            auth,
            transport,
        })
    }

    pub fn method(&self) -> Result<Method, CurlyError> {
        Method::from_bytes(self.method.as_bytes())
            .map_err(|_| CurlyError::Invalid(format!("invalid HTTP method: {}", self.method)))
    }

    pub fn has_header(&self, needle: &str) -> bool {
        self.headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(needle))
    }
}

pub fn validate_url(input: &str) -> Result<Url, CurlyError> {
    let url = Url::parse(input)
        .map_err(|err| CurlyError::Invalid(format!("invalid URL {input:?}: {err}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(CurlyError::Invalid(
            "URL scheme must be http or https".to_string(),
        ));
    }
    if url.host_str().is_none() {
        return Err(CurlyError::Invalid("URL must include a host".to_string()));
    }
    Ok(url)
}

pub fn parse_key_value(value: &str, label: &str) -> Result<(String, String), CurlyError> {
    let Some((key, value)) = value.split_once('=') else {
        return Err(CurlyError::Invalid(format!(
            "{label} must use KEY=VALUE syntax: {value:?}"
        )));
    };
    if key.is_empty() {
        return Err(CurlyError::Invalid(format!("{label} key cannot be empty")));
    }
    Ok((key.to_string(), value.to_string()))
}

pub fn parse_header(value: &str) -> Result<(String, String), CurlyError> {
    let Some((name, value)) = value.split_once(':') else {
        return Err(CurlyError::Invalid(format!(
            "header must use 'Name: value' syntax: {value:?}"
        )));
    };
    let name = name.trim();
    if name.is_empty() {
        return Err(CurlyError::Invalid(
            "header name cannot be empty".to_string(),
        ));
    }
    HeaderName::from_str(name)
        .map_err(|_| CurlyError::Invalid(format!("invalid header name: {name}")))?;
    Ok((name.to_string(), value.trim_start().to_string()))
}

pub fn parse_body_arg(value: &str, kind: BodyKind) -> BodySource {
    if value == "-" {
        BodySource::Stdin { kind }
    } else if let Some(path) = value.strip_prefix('@') {
        BodySource::File {
            kind,
            path: PathBuf::from(path),
        }
    } else {
        BodySource::Inline {
            kind,
            value: value.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_method_depends_on_body_presence() {
        let get = RequestDefinition::new(
            "https://example.com".into(),
            None,
            vec![],
            vec![],
            None,
            None,
            TransportOptions::default(),
        )
        .unwrap();
        assert_eq!(get.method, "GET");

        let post = RequestDefinition::new(
            "https://example.com".into(),
            None,
            vec![],
            vec![],
            Some(BodySource::Inline {
                kind: BodyKind::Raw,
                value: String::new(),
            }),
            None,
            TransportOptions::default(),
        )
        .unwrap();
        assert_eq!(post.method, "POST");
    }

    #[test]
    fn rejects_non_http_urls() {
        assert!(validate_url("file:///tmp/a").is_err());
    }
}
