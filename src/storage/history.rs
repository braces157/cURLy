use std::{path::PathBuf, time::Duration};

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{
    error::CurlyError,
    executor::RequestPreview,
    output::OutputResult,
    request::{AuthDefinition, BodySource, RequestDefinition},
};

use super::StoragePaths;

const HISTORY_SCHEMA_VERSION: i64 = 1;
const HISTORY_LIMIT: i64 = 1_000;

#[derive(Debug, Clone)]
pub struct HistoryStore {
    path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistorySummary {
    pub id: i64,
    pub created_at: String,
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    pub error: Option<String>,
    pub elapsed_ms: u64,
    pub response_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub created_at: String,
    pub method: String,
    pub url: String,
    pub request_headers: Vec<(String, String)>,
    pub request_preview: Vec<u8>,
    pub request_truncated: bool,
    pub request_bytes: u64,
    pub status: Option<u16>,
    pub error: Option<String>,
    pub elapsed_ms: u64,
    pub response_headers: Vec<(String, String)>,
    pub response_preview: Vec<u8>,
    pub response_truncated: bool,
    pub response_bytes: u64,
    pub replay: Option<RequestDefinition>,
    pub replayable: bool,
    pub replay_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewHistoryEntry {
    pub method: String,
    pub url: String,
    pub request_headers: Vec<(String, String)>,
    pub request_preview: Vec<u8>,
    pub request_truncated: bool,
    pub request_bytes: u64,
    pub status: Option<u16>,
    pub error: Option<String>,
    pub elapsed_ms: u64,
    pub response_headers: Vec<(String, String)>,
    pub response_preview: Vec<u8>,
    pub response_truncated: bool,
    pub response_bytes: u64,
    pub replay: Option<RequestDefinition>,
    pub replayable: bool,
    pub replay_reason: Option<String>,
}

impl NewHistoryEntry {
    pub fn success(
        request: &RequestDefinition,
        request_preview: &RequestPreview,
        output: &OutputResult,
    ) -> Self {
        let (replay, replayable, replay_reason) = replay_snapshot(request, request_preview);
        Self {
            method: request.method.clone(),
            url: request.url.clone(),
            request_headers: redact_headers(&request.headers),
            request_preview: request_preview.bytes.clone(),
            request_truncated: request_preview.truncated,
            request_bytes: request_preview.total_bytes,
            status: Some(output.status),
            error: None,
            elapsed_ms: output.elapsed.as_millis().min(u64::MAX as u128) as u64,
            response_headers: redact_headers(&output.response_headers),
            response_preview: output.response_preview.clone(),
            response_truncated: output.response_truncated,
            response_bytes: output.response_bytes,
            replay,
            replayable,
            replay_reason,
        }
    }

    pub fn failure(request: &RequestDefinition, error: &CurlyError, elapsed: Duration) -> Self {
        let preview = RequestPreview {
            bytes: Vec::new(),
            truncated: request.body.is_some(),
            total_bytes: 0,
        };
        let (replay, replayable, replay_reason) = replay_snapshot(request, &preview);
        Self {
            method: request.method.clone(),
            url: request.url.clone(),
            request_headers: redact_headers(&request.headers),
            request_preview: Vec::new(),
            request_truncated: request.body.is_some(),
            request_bytes: 0,
            status: None,
            error: Some(error.to_string()),
            elapsed_ms: elapsed.as_millis().min(u64::MAX as u128) as u64,
            response_headers: Vec::new(),
            response_preview: Vec::new(),
            response_truncated: false,
            response_bytes: 0,
            replay,
            replayable,
            replay_reason,
        }
    }
}

impl HistoryStore {
    pub fn open(paths: &StoragePaths) -> Result<Self, CurlyError> {
        if let Some(parent) = paths.history_db.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let store = Self {
            path: paths.history_db.clone(),
        };
        store.migrate()?;
        Ok(store)
    }

    fn connection(&self) -> Result<Connection, CurlyError> {
        let conn = Connection::open(&self.path)
            .map_err(|err| CurlyError::History(format!("cannot open history database: {err}")))?;
        conn.busy_timeout(Duration::from_millis(750))
            .map_err(|err| {
                CurlyError::History(format!("cannot configure history database: {err}"))
            })?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")
            .map_err(|err| {
                CurlyError::History(format!("cannot configure history database: {err}"))
            })?;
        Ok(conn)
    }

    fn migrate(&self) -> Result<(), CurlyError> {
        let mut conn = self.connection()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL
            );",
        )
        .map_err(history_err)?;
        let version: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .map_err(history_err)?;
        if version > HISTORY_SCHEMA_VERSION {
            return Err(CurlyError::History(format!(
                "history database schema version {version} is newer than this curly build supports"
            )));
        }
        if version < 1 {
            let tx = conn.transaction().map_err(history_err)?;
            tx.execute_batch(
                "CREATE TABLE history (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    created_at TEXT NOT NULL,
                    method TEXT NOT NULL,
                    url TEXT NOT NULL,
                    request_headers TEXT NOT NULL,
                    request_preview BLOB NOT NULL,
                    request_truncated INTEGER NOT NULL,
                    request_bytes INTEGER NOT NULL,
                    status INTEGER,
                    error TEXT,
                    elapsed_ms INTEGER NOT NULL,
                    response_headers TEXT NOT NULL,
                    response_preview BLOB NOT NULL,
                    response_truncated INTEGER NOT NULL,
                    response_bytes INTEGER NOT NULL,
                    replay_json TEXT,
                    replayable INTEGER NOT NULL,
                    replay_reason TEXT
                );
                CREATE INDEX history_created_at_idx ON history(created_at DESC);",
            )
            .map_err(history_err)?;
            tx.execute(
                "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
                params![1_i64, Utc::now().to_rfc3339()],
            )
            .map_err(history_err)?;
            tx.commit().map_err(history_err)?;
        }
        Ok(())
    }

    pub fn record(&self, entry: &NewHistoryEntry) -> Result<i64, CurlyError> {
        let request_headers = serde_json::to_string(&entry.request_headers)
            .map_err(|err| CurlyError::History(format!("cannot encode request headers: {err}")))?;
        let response_headers = serde_json::to_string(&entry.response_headers)
            .map_err(|err| CurlyError::History(format!("cannot encode response headers: {err}")))?;
        let replay_json = entry
            .replay
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|err| CurlyError::History(format!("cannot encode replay metadata: {err}")))?;

        let mut conn = self.connection()?;
        let tx = conn.transaction().map_err(history_err)?;
        tx.execute(
            "INSERT INTO history(
                created_at, method, url, request_headers, request_preview, request_truncated,
                request_bytes, status, error, elapsed_ms, response_headers, response_preview,
                response_truncated, response_bytes, replay_json, replayable, replay_reason
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                Utc::now().to_rfc3339(),
                entry.method,
                entry.url,
                request_headers,
                entry.request_preview,
                bool_i64(entry.request_truncated),
                to_i64(entry.request_bytes),
                entry.status.map(i64::from),
                entry.error,
                to_i64(entry.elapsed_ms),
                response_headers,
                entry.response_preview,
                bool_i64(entry.response_truncated),
                to_i64(entry.response_bytes),
                replay_json,
                bool_i64(entry.replayable),
                entry.replay_reason,
            ],
        )
        .map_err(history_err)?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "DELETE FROM history WHERE id NOT IN (SELECT id FROM history ORDER BY id DESC LIMIT ?1)",
            params![HISTORY_LIMIT],
        )
        .map_err(history_err)?;
        tx.commit().map_err(history_err)?;
        Ok(id)
    }

    pub fn list(&self) -> Result<Vec<HistorySummary>, CurlyError> {
        let conn = self.connection()?;
        let mut statement = conn
            .prepare(
                "SELECT id, created_at, method, url, status, error, elapsed_ms, response_bytes
                 FROM history ORDER BY id DESC LIMIT 1000",
            )
            .map_err(history_err)?;
        let rows = statement
            .query_map([], |row| {
                Ok(HistorySummary {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                    method: row.get(2)?,
                    url: row.get(3)?,
                    status: row.get::<_, Option<i64>>(4)?.map(|value| value as u16),
                    error: row.get(5)?,
                    elapsed_ms: row.get::<_, i64>(6)? as u64,
                    response_bytes: row.get::<_, i64>(7)? as u64,
                })
            })
            .map_err(history_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(history_err)
    }

    pub fn get(&self, id: i64) -> Result<Option<HistoryEntry>, CurlyError> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT id, created_at, method, url, request_headers, request_preview,
                    request_truncated, request_bytes, status, error, elapsed_ms,
                    response_headers, response_preview, response_truncated, response_bytes,
                    replay_json, replayable, replay_reason
             FROM history WHERE id = ?1",
            params![id],
            decode_entry,
        )
        .optional()
        .map_err(history_err)
    }

    pub fn clear(&self) -> Result<usize, CurlyError> {
        let conn = self.connection()?;
        conn.execute("DELETE FROM history", []).map_err(history_err)
    }

    pub async fn record_async(&self, entry: NewHistoryEntry) -> Result<i64, CurlyError> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.record(&entry))
            .await
            .map_err(|err| CurlyError::History(format!("history worker failed: {err}")))?
    }

    pub async fn list_async(&self) -> Result<Vec<HistorySummary>, CurlyError> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.list())
            .await
            .map_err(|err| CurlyError::History(format!("history worker failed: {err}")))?
    }

    pub async fn get_async(&self, id: i64) -> Result<Option<HistoryEntry>, CurlyError> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.get(id))
            .await
            .map_err(|err| CurlyError::History(format!("history worker failed: {err}")))?
    }

    pub async fn clear_async(&self) -> Result<usize, CurlyError> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.clear())
            .await
            .map_err(|err| CurlyError::History(format!("history worker failed: {err}")))?
    }
}

pub fn validate_replay(entry: &HistoryEntry) -> Result<RequestDefinition, CurlyError> {
    if !entry.replayable {
        return Err(CurlyError::Invalid(
            entry
                .replay_reason
                .clone()
                .unwrap_or_else(|| "history entry is not replayable".to_string()),
        ));
    }
    let request = entry.replay.clone().ok_or_else(|| {
        CurlyError::Invalid("history entry does not contain complete replay metadata".to_string())
    })?;
    if let Some(BodySource::File { path, .. }) = &request.body
        && !path.is_file()
    {
        return Err(CurlyError::Invalid(format!(
            "cannot replay history entry: body file {} is missing",
            path.display()
        )));
    }
    if let Some(AuthDefinition::BearerEnv { variable }) = &request.auth
        && std::env::var_os(variable).is_none()
    {
        return Err(CurlyError::Invalid(format!(
            "cannot replay history entry: environment variable {variable:?} is not set"
        )));
    }
    Ok(request)
}

pub fn redact_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            if sensitive_header(name) {
                (name.clone(), "<redacted>".to_string())
            } else {
                (name.clone(), value.clone())
            }
        })
        .collect()
}

fn replay_snapshot(
    request: &RequestDefinition,
    request_preview: &RequestPreview,
) -> (Option<RequestDefinition>, bool, Option<String>) {
    let mut replay = request.clone();
    let mut reasons = Vec::new();

    if replay
        .headers
        .iter()
        .any(|(name, _)| sensitive_header(name))
    {
        replay.headers.retain(|(name, _)| !sensitive_header(name));
        reasons.push("credential-bearing request headers were redacted".to_string());
    }
    match &replay.auth {
        Some(AuthDefinition::BasicLiteral { .. } | AuthDefinition::BearerLiteral { .. }) => {
            replay.auth = None;
            reasons.push("literal authentication credentials were not persisted".to_string());
        }
        Some(AuthDefinition::BearerEnv { .. }) | None => {}
    }
    match &replay.body {
        Some(BodySource::Stdin { .. }) => {
            replay.body = None;
            reasons.push("stdin request bodies are not persisted for replay".to_string());
        }
        Some(BodySource::Inline { .. }) if request_preview.truncated => {
            replay.body = None;
            reasons.push("request body exceeded the 64 KiB history preview limit".to_string());
        }
        _ => {}
    }
    if let Some(BodySource::File { path, .. }) = &mut replay.body
        && path.is_relative()
        && let Ok(cwd) = std::env::current_dir()
    {
        *path = cwd.join(&*path);
    }

    let replayable = reasons.is_empty();
    let reason = if reasons.is_empty() {
        None
    } else {
        Some(reasons.join("; "))
    };
    (Some(replay), replayable, reason)
}

fn decode_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
    let request_headers_json: String = row.get(4)?;
    let response_headers_json: String = row.get(11)?;
    let replay_json: Option<String> = row.get(15)?;
    Ok(HistoryEntry {
        id: row.get(0)?,
        created_at: row.get(1)?,
        method: row.get(2)?,
        url: row.get(3)?,
        request_headers: serde_json::from_str(&request_headers_json).unwrap_or_default(),
        request_preview: row.get(5)?,
        request_truncated: row.get::<_, i64>(6)? != 0,
        request_bytes: row.get::<_, i64>(7)? as u64,
        status: row.get::<_, Option<i64>>(8)?.map(|value| value as u16),
        error: row.get(9)?,
        elapsed_ms: row.get::<_, i64>(10)? as u64,
        response_headers: serde_json::from_str(&response_headers_json).unwrap_or_default(),
        response_preview: row.get(12)?,
        response_truncated: row.get::<_, i64>(13)? != 0,
        response_bytes: row.get::<_, i64>(14)? as u64,
        replay: replay_json.and_then(|json| serde_json::from_str(&json).ok()),
        replayable: row.get::<_, i64>(16)? != 0,
        replay_reason: row.get(17)?,
    })
}

fn sensitive_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
    )
}

fn bool_i64(value: bool) -> i64 {
    i64::from(value)
}

fn to_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

fn history_err(error: rusqlite::Error) -> CurlyError {
    CurlyError::History(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        output::OutputResult,
        request::{BodyKind, TransportOptions},
    };
    use tempfile::TempDir;

    fn paths(temp: &TempDir) -> StoragePaths {
        let config_root = temp.path().join("config");
        let data_root = temp.path().join("data");
        StoragePaths {
            config_file: config_root.join("config.toml"),
            requests_dir: config_root.join("requests"),
            history_db: data_root.join("history.sqlite3"),
            config_root,
            data_root,
        }
    }

    fn request() -> RequestDefinition {
        RequestDefinition::new(
            "https://example.com".into(),
            None,
            vec![("Authorization".into(), "secret".into())],
            vec![],
            None,
            None,
            TransportOptions::default(),
        )
        .unwrap()
    }

    #[test]
    fn redacts_sensitive_headers_case_insensitively() {
        let redacted = redact_headers(&request().headers);
        assert_eq!(redacted[0].1, "<redacted>");
    }

    #[test]
    fn history_round_trip_redacts_headers_and_preserves_env_reference() {
        let temp = TempDir::new().unwrap();
        let paths = paths(&temp);
        let store = HistoryStore::open(&paths).unwrap();
        let request = RequestDefinition::new(
            "https://example.test/items".into(),
            Some("POST".into()),
            vec![
                ("Authorization".into(), "Bearer literal-header".into()),
                ("x-visible".into(), "yes".into()),
            ],
            vec![],
            Some(BodySource::Inline {
                kind: BodyKind::Json,
                value: "{}".into(),
            }),
            Some(AuthDefinition::BearerEnv {
                variable: "MISSING_TEST_TOKEN".into(),
            }),
            TransportOptions::default(),
        )
        .unwrap();
        let preview = RequestPreview {
            bytes: b"{}".to_vec(),
            truncated: false,
            total_bytes: 2,
        };
        let output = OutputResult {
            status: 200,
            response_headers: vec![("Set-Cookie".into(), "session=secret".into())],
            response_preview: br#"{"ok":true}"#.to_vec(),
            response_truncated: false,
            response_bytes: 11,
            elapsed: Duration::from_millis(5),
        };
        let id = store
            .record(&NewHistoryEntry::success(&request, &preview, &output))
            .unwrap();
        let entry = store.get(id).unwrap().unwrap();

        assert_eq!(entry.request_headers[0].1, "<redacted>");
        assert_eq!(entry.response_headers[0].1, "<redacted>");
        assert!(!entry.replayable);
        assert!(
            entry
                .replay_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("credential-bearing request headers"))
        );
        let replay = entry.replay.unwrap();
        assert!(
            replay
                .headers
                .iter()
                .all(|(name, _)| !sensitive_header(name))
        );
        assert!(matches!(
            replay.auth,
            Some(AuthDefinition::BearerEnv { ref variable }) if variable == "MISSING_TEST_TOKEN"
        ));
    }

    #[test]
    fn replay_rejects_missing_environment_and_truncated_inline_body() {
        let request = RequestDefinition::new(
            "https://example.test/items".into(),
            None,
            vec![],
            vec![],
            Some(BodySource::Inline {
                kind: BodyKind::Raw,
                value: "x".repeat(70_000),
            }),
            Some(AuthDefinition::BearerEnv {
                variable: "CURLY_TEST_VARIABLE_THAT_DOES_NOT_EXIST".into(),
            }),
            TransportOptions::default(),
        )
        .unwrap();
        let preview = RequestPreview {
            bytes: vec![b'x'; 64 * 1024],
            truncated: true,
            total_bytes: 70_000,
        };
        let output = OutputResult {
            status: 200,
            response_headers: vec![],
            response_preview: vec![],
            response_truncated: false,
            response_bytes: 0,
            elapsed: Duration::from_millis(1),
        };
        let new_entry = NewHistoryEntry::success(&request, &preview, &output);
        assert!(!new_entry.replayable);
        assert!(
            new_entry
                .replay_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("64 KiB"))
        );

        let entry = HistoryEntry {
            id: 1,
            created_at: "now".into(),
            method: request.method.clone(),
            url: request.url.clone(),
            request_headers: vec![],
            request_preview: vec![],
            request_truncated: false,
            request_bytes: 0,
            status: Some(200),
            error: None,
            elapsed_ms: 1,
            response_headers: vec![],
            response_preview: vec![],
            response_truncated: false,
            response_bytes: 0,
            replay: Some(RequestDefinition {
                body: None,
                ..request
            }),
            replayable: true,
            replay_reason: None,
        };
        let error = validate_replay(&entry).unwrap_err().to_string();
        assert!(error.contains("environment variable"));
    }

    #[test]
    fn retention_keeps_only_newest_thousand_entries() {
        let temp = TempDir::new().unwrap();
        let paths = paths(&temp);
        let store = HistoryStore::open(&paths).unwrap();
        for index in 0..1_005_u64 {
            store
                .record(&NewHistoryEntry {
                    method: "GET".into(),
                    url: format!("https://example.test/{index}"),
                    request_headers: vec![],
                    request_preview: vec![],
                    request_truncated: false,
                    request_bytes: 0,
                    status: Some(200),
                    error: None,
                    elapsed_ms: 1,
                    response_headers: vec![],
                    response_preview: vec![],
                    response_truncated: false,
                    response_bytes: 0,
                    replay: None,
                    replayable: false,
                    replay_reason: Some("test".into()),
                })
                .unwrap();
        }
        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 1_000);
        assert!(entries.first().unwrap().url.ends_with("/1004"));
        assert!(entries.last().unwrap().url.ends_with("/5"));
    }

    #[test]
    fn concurrent_history_writes_succeed() {
        let temp = TempDir::new().unwrap();
        let paths = paths(&temp);
        let store = HistoryStore::open(&paths).unwrap();
        let mut workers = Vec::new();
        for worker in 0..4 {
            let store = store.clone();
            workers.push(std::thread::spawn(move || {
                for index in 0..10 {
                    store
                        .record(&NewHistoryEntry {
                            method: "GET".into(),
                            url: format!("https://example.test/{worker}/{index}"),
                            request_headers: vec![],
                            request_preview: vec![],
                            request_truncated: false,
                            request_bytes: 0,
                            status: Some(200),
                            error: None,
                            elapsed_ms: 1,
                            response_headers: vec![],
                            response_preview: vec![],
                            response_truncated: false,
                            response_bytes: 0,
                            replay: None,
                            replayable: false,
                            replay_reason: Some("test".into()),
                        })
                        .unwrap();
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(store.list().unwrap().len(), 40);
    }

    #[test]
    fn rejects_newer_history_schema() {
        let temp = TempDir::new().unwrap();
        let paths = paths(&temp);
        std::fs::create_dir_all(&paths.data_root).unwrap();
        let conn = Connection::open(&paths.history_db).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);\n\
             INSERT INTO schema_migrations(version, applied_at) VALUES (99, 'future');",
        )
        .unwrap();
        drop(conn);
        let error = HistoryStore::open(&paths).unwrap_err().to_string();
        assert!(error.contains("newer"));
    }
}
