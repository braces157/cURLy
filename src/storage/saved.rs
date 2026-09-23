use std::{path::Path, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    error::CurlyError,
    request::{AuthDefinition, BodySource, RequestDefinition},
};

use super::StoragePaths;

const SAVED_REQUEST_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SavedRequestFile {
    version: u32,
    request: RequestDefinition,
}

pub fn validate_name(name: &str) -> Result<(), CurlyError> {
    let valid_len = !name.is_empty() && name.len() <= 64;
    let valid_chars = name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'));
    if !valid_len
        || !valid_chars
        || name == "."
        || name == ".."
        || name.starts_with('.')
        || name.contains("..")
    {
        return Err(CurlyError::Invalid(
            "saved request names must be 1-64 ASCII letters, digits, '-' or '_' and cannot contain '..'"
                .to_string(),
        ));
    }
    Ok(())
}

pub fn save(
    paths: &StoragePaths,
    name: &str,
    request: &RequestDefinition,
    overwrite: bool,
) -> Result<PathBuf, CurlyError> {
    validate_name(name)?;
    reject_unsafe_saved_inputs(request)?;
    std::fs::create_dir_all(&paths.requests_dir)?;
    let destination = paths.requests_dir.join(format!("{name}.toml"));
    if destination.exists() && !overwrite {
        return Err(CurlyError::Invalid(format!(
            "saved request {name:?} already exists; use --overwrite to replace it"
        )));
    }

    let mut stored = request.clone();
    if let Some(BodySource::File { path, .. }) = &mut stored.body {
        let absolute = canonical_or_absolute(path)?;
        *path = pathdiff::diff_paths(&absolute, &paths.requests_dir).unwrap_or(absolute);
    }

    let file = SavedRequestFile {
        version: SAVED_REQUEST_VERSION,
        request: stored,
    };
    let text = toml::to_string_pretty(&file)
        .map_err(|err| CurlyError::Config(format!("cannot serialize saved request: {err}")))?;
    let temporary = paths
        .requests_dir
        .join(format!(".{name}.toml.tmp-{}", std::process::id()));
    std::fs::write(&temporary, text)?;
    if overwrite && destination.exists() {
        replace_existing(&temporary, &destination, name)?;
    } else {
        std::fs::rename(&temporary, &destination)?;
    }
    Ok(destination)
}

fn replace_existing(temporary: &Path, destination: &Path, name: &str) -> Result<(), CurlyError> {
    // On platforms where rename replaces an existing file, this is atomic.
    if std::fs::rename(temporary, destination).is_ok() {
        return Ok(());
    }

    // Windows std::fs::rename does not replace existing files. Preserve the old
    // definition as a backup until the new complete file is in place so a failed
    // second rename cannot destroy the user's saved request.
    let backup = destination.with_file_name(format!(".{name}.toml.backup-{}", std::process::id()));
    if backup.exists() {
        std::fs::remove_file(&backup)?;
    }
    std::fs::rename(destination, &backup)?;
    match std::fs::rename(temporary, destination) {
        Ok(()) => {
            let _ = std::fs::remove_file(&backup);
            Ok(())
        }
        Err(err) => {
            let restore = std::fs::rename(&backup, destination);
            if let Err(restore_err) = restore {
                return Err(CurlyError::Io(std::io::Error::new(
                    err.kind(),
                    format!(
                        "could not replace saved request and could not restore the previous file: {err}; restore failed: {restore_err}"
                    ),
                )));
            }
            Err(CurlyError::Io(err))
        }
    }
}

pub fn load(paths: &StoragePaths, name: &str) -> Result<RequestDefinition, CurlyError> {
    validate_name(name)?;
    let path = paths.requests_dir.join(format!("{name}.toml"));
    let text = std::fs::read_to_string(&path)
        .map_err(|err| CurlyError::Invalid(format!("cannot read saved request {name:?}: {err}")))?;
    let mut file: SavedRequestFile = toml::from_str(&text).map_err(|err| {
        CurlyError::Invalid(format!("invalid saved request {}: {err}", path.display()))
    })?;
    if file.version != SAVED_REQUEST_VERSION {
        return Err(CurlyError::Invalid(format!(
            "saved request {name:?} uses unsupported version {}",
            file.version
        )));
    }
    if let Some(BodySource::File {
        path: body_path, ..
    }) = &mut file.request.body
        && body_path.is_relative()
    {
        *body_path = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(&*body_path);
    }
    Ok(file.request)
}

pub fn list(paths: &StoragePaths) -> Result<Vec<String>, CurlyError> {
    if !paths.requests_dir.exists() {
        return Ok(vec![]);
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&paths.requests_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("toml") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
            names.push(stem.to_string());
        }
    }
    names.sort();
    Ok(names)
}

pub fn show(paths: &StoragePaths, name: &str) -> Result<String, CurlyError> {
    validate_name(name)?;
    let path = paths.requests_dir.join(format!("{name}.toml"));
    std::fs::read_to_string(&path)
        .map_err(|err| CurlyError::Invalid(format!("cannot read saved request {name:?}: {err}")))
}

pub fn delete(paths: &StoragePaths, name: &str) -> Result<(), CurlyError> {
    validate_name(name)?;
    let path = paths.requests_dir.join(format!("{name}.toml"));
    std::fs::remove_file(&path)
        .map_err(|err| CurlyError::Invalid(format!("cannot delete saved request {name:?}: {err}")))
}

fn reject_unsafe_saved_inputs(request: &RequestDefinition) -> Result<(), CurlyError> {
    if matches!(request.body, Some(BodySource::Stdin { .. })) {
        return Err(CurlyError::Invalid(
            "cannot save a request whose body comes from stdin; use @path so it can be replayed"
                .to_string(),
        ));
    }
    if matches!(
        request.auth,
        Some(AuthDefinition::BasicLiteral { .. } | AuthDefinition::BearerLiteral { .. })
    ) {
        return Err(CurlyError::Invalid(
            "refusing to save literal authentication credentials; use --bearer-env for replayable saved credentials"
                .to_string(),
        ));
    }
    if request.headers.iter().any(|(name, _)| is_sensitive(name)) {
        return Err(CurlyError::Invalid(
            "refusing to save a request containing a credential-bearing header; use an environment-backed authentication option"
                .to_string(),
        ));
    }
    Ok(())
}

fn is_sensitive(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
    )
}

fn canonical_or_absolute(path: &Path) -> Result<PathBuf, CurlyError> {
    if path.exists() {
        return std::fs::canonicalize(path).map_err(CurlyError::Io);
    }
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{BodyKind, TransportOptions};
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

    #[test]
    fn rejects_path_traversal_names() {
        assert!(validate_name("../secret").is_err());
        assert!(validate_name("good-name_2").is_ok());
    }

    #[test]
    fn overwrite_replaces_existing_request() {
        let temp = TempDir::new().unwrap();
        let paths = paths(&temp);
        let first = RequestDefinition::new(
            "https://example.test/first".into(),
            None,
            vec![],
            vec![],
            None,
            None,
            TransportOptions::default(),
        )
        .unwrap();
        let second = RequestDefinition::new(
            "https://example.test/second".into(),
            None,
            vec![],
            vec![],
            None,
            None,
            TransportOptions::default(),
        )
        .unwrap();

        save(&paths, "replace-me", &first, false).unwrap();
        save(&paths, "replace-me", &second, true).unwrap();

        assert_eq!(load(&paths, "replace-me").unwrap().url, second.url);
    }

    #[test]
    fn saved_request_round_trip_preserves_env_reference_and_body_file() {
        let temp = TempDir::new().unwrap();
        let paths = paths(&temp);
        let body_path = temp.path().join("payload.json");
        std::fs::write(&body_path, br#"{"ok":true}"#).unwrap();
        let request = RequestDefinition::new(
            "https://example.test/items".into(),
            Some("PUT".into()),
            vec![("accept".into(), "application/json".into())],
            vec![("a".into(), "1".into()), ("a".into(), "2".into())],
            Some(BodySource::File {
                kind: BodyKind::Json,
                path: body_path.clone(),
            }),
            Some(AuthDefinition::BearerEnv {
                variable: "TEST_API_TOKEN".into(),
            }),
            TransportOptions::default(),
        )
        .unwrap();

        let saved = save(&paths, "round-trip", &request, false).unwrap();
        let text = std::fs::read_to_string(&saved).unwrap();
        assert!(text.contains("TEST_API_TOKEN"));
        assert!(!text.contains("BearerLiteral"));

        let loaded = load(&paths, "round-trip").unwrap();
        assert_eq!(loaded.method, "PUT");
        assert_eq!(loaded.query.len(), 2);
        assert!(matches!(
            loaded.auth,
            Some(AuthDefinition::BearerEnv { ref variable }) if variable == "TEST_API_TOKEN"
        ));
        let Some(BodySource::File { path, .. }) = loaded.body else {
            panic!("expected file body");
        };
        assert_eq!(
            std::fs::canonicalize(path).unwrap(),
            std::fs::canonicalize(body_path).unwrap()
        );
    }

    #[test]
    fn saved_request_rejects_stdin_literal_credentials_and_accidental_overwrite() {
        let temp = TempDir::new().unwrap();
        let paths = paths(&temp);
        let base = RequestDefinition::new(
            "https://example.test/items".into(),
            None,
            vec![],
            vec![],
            None,
            None,
            TransportOptions::default(),
        )
        .unwrap();
        save(&paths, "safe", &base, false).unwrap();
        assert!(save(&paths, "safe", &base, false).is_err());
        assert!(save(&paths, "safe", &base, true).is_ok());

        let mut stdin_request = base.clone();
        stdin_request.body = Some(BodySource::Stdin {
            kind: BodyKind::Raw,
        });
        assert!(save(&paths, "stdin", &stdin_request, false).is_err());

        let mut literal_auth = base;
        literal_auth.auth = Some(AuthDefinition::BearerLiteral {
            token: "do-not-save".into(),
        });
        assert!(save(&paths, "secret", &literal_auth, false).is_err());
    }
}
