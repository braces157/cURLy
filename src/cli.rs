use std::{io::IsTerminal, path::PathBuf, time::Instant};

use clap::{Args, Parser, Subcommand, ValueEnum};
use tokio_util::sync::CancellationToken;

use crate::{
    error::CurlyError,
    executor,
    output::{self, ColorMode, OutputOptions},
    request::{
        AuthDefinition, BodyKind, RequestDefinition, TransportOptions, parse_body_arg,
        parse_header, parse_key_value,
    },
    storage::{
        AppConfig, HistoryStore, NewHistoryEntry, StoragePaths, history::validate_replay, saved,
    },
    tui,
};

#[derive(Debug, Parser)]
#[command(
    name = "curly",
    version,
    about = "A JSON-focused HTTP client with reliable pipeline output",
    arg_required_else_help = true,
    subcommand_precedence_over_arg = true
)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    request: RequestArgs,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Execute a saved request.
    Run {
        name: String,
        #[command(flatten)]
        output: OutputArgs,
        #[command(flatten)]
        history: HistoryModeArgs,
    },
    /// Manage saved request definitions.
    Saved {
        #[command(subcommand)]
        command: SavedCommand,
    },
    /// Inspect and replay local request history.
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    /// Browse and replay history interactively.
    Tui,
}

#[derive(Debug, Subcommand)]
enum SavedCommand {
    List,
    Show { name: String },
    Delete { name: String },
}

#[derive(Debug, Subcommand)]
enum HistoryCommand {
    List,
    Show {
        id: i64,
    },
    Replay {
        id: i64,
        #[command(flatten)]
        output: OutputArgs,
        #[command(flatten)]
        history: HistoryModeArgs,
    },
    Clear,
}

#[derive(Debug, Args, Default)]
struct RequestArgs {
    /// HTTP or HTTPS URL.
    #[arg(value_name = "URL")]
    url: Option<String>,

    /// HTTP method. Defaults to GET, or POST when a body is supplied.
    #[arg(short = 'X', long = "method")]
    method: Option<String>,

    /// Request header in `Name: value` form. May be repeated.
    #[arg(short = 'H', long = "header")]
    headers: Vec<String>,

    /// Query parameter in KEY=VALUE form. May be repeated and preserves order.
    #[arg(long = "query")]
    query: Vec<String>,

    /// JSON body text, @file, or - for stdin.
    #[arg(long = "json", conflicts_with = "body")]
    json: Option<String>,

    /// Raw body text, @file, or - for stdin.
    #[arg(long = "body", conflicts_with = "json")]
    body: Option<String>,

    /// HTTP basic authentication as USER:PASSWORD.
    #[arg(long = "basic", conflicts_with_all = ["bearer", "bearer_env"])]
    basic: Option<String>,

    /// Literal bearer token. Prefer --bearer-env for saved requests.
    #[arg(long = "bearer", conflicts_with_all = ["basic", "bearer_env"])]
    bearer: Option<String>,

    /// Name of an environment variable containing a bearer token.
    #[arg(long = "bearer-env", conflicts_with_all = ["basic", "bearer"])]
    bearer_env: Option<String>,

    /// Follow redirects, up to ten hops.
    #[arg(long)]
    follow: bool,

    /// Disable TLS certificate verification.
    #[arg(long)]
    insecure: bool,

    /// Connection timeout in seconds.
    #[arg(long = "connect-timeout", default_value_t = 10.0, value_parser = positive_seconds)]
    connect_timeout: f64,

    /// Total timeout in seconds. Use 0 to disable it.
    #[arg(long = "timeout", default_value_t = 30.0, value_parser = nonnegative_seconds)]
    timeout: f64,

    /// Save this request definition before executing it.
    #[arg(long)]
    save: Option<String>,

    /// Replace an existing saved request with the same name.
    #[arg(long, requires = "save")]
    overwrite: bool,

    #[command(flatten)]
    output: OutputArgs,

    #[command(flatten)]
    history: HistoryModeArgs,
}

#[derive(Debug, Args, Clone)]
struct OutputArgs {
    /// Write response body bytes to a file.
    #[arg(short = 'o', long = "output")]
    output: Option<PathBuf>,

    /// Disable terminal formatting and write the response body as received.
    #[arg(long)]
    raw: bool,

    /// Print response headers to stderr.
    #[arg(long = "headers")]
    show_headers: bool,

    /// Suppress status/timing diagnostics.
    #[arg(short, long)]
    quiet: bool,

    /// Color terminal JSON output.
    #[arg(long, value_enum, default_value_t = ColorArg::Auto)]
    color: ColorArg,

    /// Return exit code 22 for HTTP 4xx/5xx while still writing the body.
    #[arg(long)]
    fail: bool,
}

impl Default for OutputArgs {
    fn default() -> Self {
        Self {
            output: None,
            raw: false,
            show_headers: false,
            quiet: false,
            color: ColorArg::Auto,
            fail: false,
        }
    }
}

#[derive(Debug, Args, Clone, Default)]
struct HistoryModeArgs {
    /// Record this request in local history.
    #[arg(long, conflicts_with = "no_history")]
    history: bool,

    /// Do not record this request in local history.
    #[arg(long = "no-history", conflicts_with = "history")]
    no_history: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ColorArg {
    Auto,
    Always,
    Never,
}

impl From<ColorArg> for ColorMode {
    fn from(value: ColorArg) -> Self {
        match value {
            ColorArg::Auto => Self::Auto,
            ColorArg::Always => Self::Always,
            ColorArg::Never => Self::Never,
        }
    }
}

pub async fn dispatch(cli: Cli) -> Result<u8, CurlyError> {
    let paths = StoragePaths::discover()?;
    let _config = AppConfig::load(&paths)?;

    match cli.command {
        Some(Command::Run {
            name,
            output,
            history,
        }) => {
            let request = saved::load(&paths, &name)?;
            execute_request(request, output, history, &paths).await
        }
        Some(Command::Saved { command }) => dispatch_saved(command, &paths),
        Some(Command::History { command }) => dispatch_history(command, &paths).await,
        Some(Command::Tui) => tui::run(paths).await,
        None => {
            let request = request_from_args(&cli.request)?;
            if let Some(name) = &cli.request.save {
                saved::save(&paths, name, &request, cli.request.overwrite)?;
            }
            execute_request(request, cli.request.output, cli.request.history, &paths).await
        }
    }
}

fn dispatch_saved(command: SavedCommand, paths: &StoragePaths) -> Result<u8, CurlyError> {
    match command {
        SavedCommand::List => {
            for name in saved::list(paths)? {
                println!("{name}");
            }
        }
        SavedCommand::Show { name } => print!("{}", saved::show(paths, &name)?),
        SavedCommand::Delete { name } => saved::delete(paths, &name)?,
    }
    Ok(0)
}

async fn dispatch_history(command: HistoryCommand, paths: &StoragePaths) -> Result<u8, CurlyError> {
    let store = HistoryStore::open(paths)?;
    match command {
        HistoryCommand::List => {
            for row in store.list_async().await? {
                let outcome = row
                    .status
                    .map(|status| status.to_string())
                    .or_else(|| row.error.as_ref().map(|_| "ERR".to_string()))
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "{}\t{}\t{}\t{}\t{}ms\t{}",
                    row.id, row.created_at, row.method, outcome, row.elapsed_ms, row.url
                );
            }
            Ok(0)
        }
        HistoryCommand::Show { id } => {
            let entry = store
                .get_async(id)
                .await?
                .ok_or_else(|| CurlyError::Invalid(format!("history entry {id} does not exist")))?;
            print_history_entry(&entry);
            Ok(0)
        }
        HistoryCommand::Replay {
            id,
            output,
            history,
        } => {
            let entry = store
                .get_async(id)
                .await?
                .ok_or_else(|| CurlyError::Invalid(format!("history entry {id} does not exist")))?;
            let request = validate_replay(&entry)?;
            execute_request(request, output, history, paths).await
        }
        HistoryCommand::Clear => {
            let deleted = store.clear_async().await?;
            println!(
                "cleared {deleted} history entr{}",
                if deleted == 1 { "y" } else { "ies" }
            );
            Ok(0)
        }
    }
}

fn print_history_entry(entry: &crate::storage::HistoryEntry) {
    println!("id: {}", entry.id);
    println!("time: {}", entry.created_at);
    println!("request: {} {}", entry.method, entry.url);
    println!(
        "status: {}",
        entry.status.map_or_else(|| "-".into(), |v| v.to_string())
    );
    println!("elapsed: {} ms", entry.elapsed_ms);
    println!("response bytes: {}", entry.response_bytes);
    if let Some(error) = &entry.error {
        println!("error: {error}");
    }
    if !entry.request_headers.is_empty() {
        println!("request headers:");
        for (name, value) in &entry.request_headers {
            println!("  {name}: {value}");
        }
    }
    if !entry.response_headers.is_empty() {
        println!("response headers:");
        for (name, value) in &entry.response_headers {
            println!("  {name}: {value}");
        }
    }
    if !entry.response_preview.is_empty() {
        println!("response preview:");
        print!("{}", output::escape_terminal_bytes(&entry.response_preview));
        if entry.response_truncated {
            println!("\n[… truncated …]");
        } else {
            println!();
        }
    }
    if !entry.replayable {
        println!(
            "replay: unavailable ({})",
            entry
                .replay_reason
                .as_deref()
                .unwrap_or("incomplete inputs")
        );
    }
}

fn request_from_args(args: &RequestArgs) -> Result<RequestDefinition, CurlyError> {
    let url = args
        .url
        .clone()
        .ok_or_else(|| CurlyError::Invalid("an HTTP/HTTPS URL is required".to_string()))?;
    let headers = args
        .headers
        .iter()
        .map(|header| parse_header(header))
        .collect::<Result<Vec<_>, _>>()?;
    let query = args
        .query
        .iter()
        .map(|item| parse_key_value(item, "query parameter"))
        .collect::<Result<Vec<_>, _>>()?;
    let body = args
        .json
        .as_deref()
        .map(|value| parse_body_arg(value, BodyKind::Json))
        .or_else(|| {
            args.body
                .as_deref()
                .map(|value| parse_body_arg(value, BodyKind::Raw))
        });
    let auth = parse_auth(args)?;
    let total_timeout_ms = if args.timeout == 0.0 {
        None
    } else {
        Some(seconds_to_millis(args.timeout))
    };
    let transport = TransportOptions {
        follow: args.follow,
        insecure: args.insecure,
        connect_timeout_ms: seconds_to_millis(args.connect_timeout),
        total_timeout_ms,
    };
    RequestDefinition::new(
        url,
        args.method.clone(),
        headers,
        query,
        body,
        auth,
        transport,
    )
}

fn parse_auth(args: &RequestArgs) -> Result<Option<AuthDefinition>, CurlyError> {
    if let Some(basic) = &args.basic {
        let Some((username, password)) = basic.split_once(':') else {
            return Err(CurlyError::Invalid(
                "--basic must use USER:PASSWORD syntax".to_string(),
            ));
        };
        return Ok(Some(AuthDefinition::BasicLiteral {
            username: username.to_string(),
            password: password.to_string(),
        }));
    }
    if let Some(token) = &args.bearer {
        return Ok(Some(AuthDefinition::BearerLiteral {
            token: token.clone(),
        }));
    }
    if let Some(variable) = &args.bearer_env {
        if variable.is_empty() {
            return Err(CurlyError::Invalid(
                "--bearer-env requires a non-empty variable name".to_string(),
            ));
        }
        return Ok(Some(AuthDefinition::BearerEnv {
            variable: variable.clone(),
        }));
    }
    Ok(None)
}

async fn execute_request(
    request: RequestDefinition,
    output_args: OutputArgs,
    history_args: HistoryModeArgs,
    paths: &StoragePaths,
) -> Result<u8, CurlyError> {
    let cancellation = CancellationToken::new();
    let signal_token = cancellation.clone();
    let signal_task = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_token.cancel();
        }
    });
    let started = Instant::now();
    let result = executor::execute(&request, &cancellation).await;
    let (response, request_preview) = match result {
        Ok(value) => value,
        Err(err) => {
            signal_task.abort();
            maybe_record_failure(paths, &request, &err, started.elapsed(), &history_args).await;
            return Err(err);
        }
    };

    let output_options = OutputOptions {
        output: output_args.output,
        raw: output_args.raw,
        headers: output_args.show_headers,
        quiet: output_args.quiet,
        color: output_args.color.into(),
    };
    let output_result = output::write_response(response, &output_options, &cancellation).await;
    signal_task.abort();
    let output_result = match output_result {
        Ok(value) => value,
        Err(err) => {
            maybe_record_failure(paths, &request, &err, started.elapsed(), &history_args).await;
            return Err(err);
        }
    };

    if history_enabled(&history_args)
        && let Err(err) = record_success(paths, &request, &request_preview, &output_result).await
    {
        eprintln!("curly: warning: could not write history: {err}");
    }

    if output_args.fail && (400..=599).contains(&output_result.status) {
        Ok(22)
    } else {
        Ok(0)
    }
}

fn history_enabled(args: &HistoryModeArgs) -> bool {
    if args.history {
        true
    } else if args.no_history {
        false
    } else {
        std::io::stdout().is_terminal()
    }
}

async fn record_success(
    paths: &StoragePaths,
    request: &RequestDefinition,
    request_preview: &executor::RequestPreview,
    output: &output::OutputResult,
) -> Result<(), CurlyError> {
    let store = HistoryStore::open(paths)?;
    store
        .record_async(NewHistoryEntry::success(request, request_preview, output))
        .await?;
    Ok(())
}

async fn maybe_record_failure(
    paths: &StoragePaths,
    request: &RequestDefinition,
    error: &CurlyError,
    elapsed: std::time::Duration,
    mode: &HistoryModeArgs,
) {
    if !history_enabled(mode) {
        return;
    }
    let result = match HistoryStore::open(paths) {
        Ok(store) => store
            .record_async(NewHistoryEntry::failure(request, error, elapsed))
            .await
            .map(|_| ()),
        Err(err) => Err(err),
    };
    if let Err(err) = result {
        eprintln!("curly: warning: could not write history: {err}");
    }
}

fn seconds_to_millis(seconds: f64) -> u64 {
    (seconds * 1000.0).round().min(u64::MAX as f64) as u64
}

fn positive_seconds(value: &str) -> Result<f64, String> {
    let value: f64 = value.parse().map_err(|_| "expected a number".to_string())?;
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err("expected a finite value greater than zero".to_string())
    }
}

fn nonnegative_seconds(value: &str) -> Result<f64, String> {
    let value: f64 = value.parse().map_err(|_| "expected a number".to_string())?;
    if value.is_finite() && value >= 0.0 {
        Ok(value)
    } else {
        Err("expected a finite value greater than or equal to zero".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn url_first_request_parses_duplicate_ordered_values() {
        let cli = Cli::try_parse_from([
            "curly",
            "https://example.test/items",
            "-H",
            "x-a: 1",
            "-H",
            "x-a: 2",
            "--query",
            "a=1",
            "--query",
            "a=2",
        ])
        .unwrap();
        let request = request_from_args(&cli.request).unwrap();
        assert_eq!(request.headers[0].1, "1");
        assert_eq!(request.headers[1].1, "2");
        assert_eq!(request.query[0].1, "1");
        assert_eq!(request.query[1].1, "2");
    }

    #[test]
    fn json_and_body_are_mutually_exclusive() {
        assert!(
            Cli::try_parse_from([
                "curly",
                "https://example.test",
                "--json",
                "{}",
                "--body",
                "x"
            ])
            .is_err()
        );
    }
}
