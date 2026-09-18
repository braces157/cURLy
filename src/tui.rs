mod editor;
mod preview;

use std::io::{self, IsTerminal};

use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Margin, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use tokio_util::sync::CancellationToken;

use crate::{
    error::CurlyError,
    executor,
    output::{self, HISTORY_PREVIEW_LIMIT, OutputResult},
    request::{
        AuthDefinition, BodyKind, BodySource, RequestDefinition, TransportOptions, parse_header,
        parse_key_value,
    },
    storage::{
        HistoryEntry, HistoryStore, NewHistoryEntry, StoragePaths, history::validate_replay,
    },
};

const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
const ACCENT: Color = Color::Rgb(119, 210, 183);
const MUTED: Color = Color::Rgb(139, 153, 173);
const BACKGROUND: Color = Color::Rgb(16, 21, 29);
const SURFACE: Color = Color::Rgb(23, 30, 41);
const RAISED: Color = Color::Rgb(30, 40, 53);
const FOREGROUND: Color = Color::Rgb(218, 225, 235);
const BORDER: Color = Color::Rgb(47, 60, 77);
const SELECTED: Color = Color::Rgb(35, 62, 61);

const REQUEST_HELP: &str = "Tab field · Enter edit · Ctrl+S send · 2 history · ? help · q quit";
const HISTORY_HELP: &str = "↑/↓ j/k move/scroll · Tab focus · / filter · e edit request · r replay · h/b view · 1 request · q quit";

pub async fn run(paths: StoragePaths) -> Result<u8, CurlyError> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(CurlyError::Invalid(
            "curly tui requires interactive stdin and stdout".to_string(),
        ));
    }

    let store = HistoryStore::open(&paths)?;
    let mut app = App::new(store.list_async().await?);
    app.refresh_selection(&store).await?;
    let mut session = TerminalSession::enter()?;

    loop {
        session.terminal.draw(|frame| draw(frame, &mut app))?;
        // Wait for input without rebuilding and formatting the entire screen while idle.
        let input = loop {
            let input = event::read()?;
            if matches!(&input, Event::Key(key) if key.kind == KeyEventKind::Release)
                || matches!(&input, Event::Mouse(mouse) if matches!(mouse.kind, MouseEventKind::Moved | MouseEventKind::Up(_) | MouseEventKind::Drag(_)))
            {
                continue;
            }
            break input;
        };
        if matches!(&input, Event::Key(key) if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            break;
        }
        if let Event::Mouse(mouse) = input {
            let previous = app.selected_id();
            let key = handle_mouse(&mut app, mouse);
            if previous != app.selected_id() {
                app.refresh_selection(&store).await?;
            }
            if let Some(ch) = key {
                let key = KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE);
                match app.workspace {
                    Workspace::Request => {
                        handle_request_key(&mut session, &mut app, &store, &paths, key).await?
                    }
                    Workspace::History => {
                        handle_history_key(&mut session, &mut app, &store, &paths, key).await?
                    }
                }
            }
            continue;
        }
        if let Some(selected) = app.method_menu {
            if let Event::Key(key) = input {
                match key.code {
                    KeyCode::Esc => app.method_menu = None,
                    KeyCode::Up => {
                        app.method_menu = Some((selected + METHODS.len() - 1) % METHODS.len())
                    }
                    KeyCode::Down => app.method_menu = Some((selected + 1) % METHODS.len()),
                    KeyCode::Enter => {
                        app.composer.method = METHODS[selected].into();
                        app.composer.method_explicit = true;
                        app.method_menu = None;
                    }
                    _ => {}
                }
            }
            continue;
        }
        if app.help {
            if matches!(&input, Event::Key(key) if matches!(key.code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::F(1)))
            {
                app.help = false;
            }
            continue;
        }
        if matches!(&input, Event::Key(key) if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL))
            && app.workspace == Workspace::Request
        {
            if app.composer.editing && !app.composer.commit_edit() {
                continue;
            }
            handle_request_key(
                &mut session,
                &mut app,
                &store,
                &paths,
                KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
            )
            .await?;
            continue;
        }
        if app.composer.editing {
            handle_composer_edit(&mut app, input);
            continue;
        }
        if app.workspace == Workspace::History && app.filtering {
            handle_history_filter(&mut app, &store, input).await?;
            continue;
        }

        let Event::Key(key) = input else {
            continue;
        };
        match key.code {
            KeyCode::Char('?') => app.help = true,
            KeyCode::Char('q') => break,
            KeyCode::F(1) | KeyCode::Char('1') => app.switch_workspace(Workspace::Request),
            KeyCode::F(2) | KeyCode::Char('2') => app.switch_workspace(Workspace::History),
            _ => match app.workspace {
                Workspace::Request => {
                    handle_request_key(&mut session, &mut app, &store, &paths, key).await?
                }
                Workspace::History => {
                    handle_history_key(&mut session, &mut app, &store, &paths, key).await?
                }
            },
        }
    }

    Ok(0)
}

async fn handle_request_key(
    session: &mut TerminalSession,
    app: &mut App,
    store: &HistoryStore,
    paths: &StoragePaths,
    key: KeyEvent,
) -> Result<(), CurlyError> {
    match key.code {
        KeyCode::Tab | KeyCode::Down | KeyCode::Char('j') => app.composer.move_focus(1),
        KeyCode::BackTab | KeyCode::Up | KeyCode::Char('k') => app.composer.move_focus(-1),
        KeyCode::Left if app.composer.focus == ComposerField::Method => {
            app.composer.cycle_method(-1)
        }
        KeyCode::Right if app.composer.focus == ComposerField::Method => {
            app.composer.cycle_method(1)
        }
        KeyCode::Enter => {
            if app.composer.focus == ComposerField::Method {
                app.method_menu = Some(
                    METHODS
                        .iter()
                        .position(|method| *method == app.composer.method)
                        .unwrap_or(0),
                );
            } else {
                app.composer.begin_edit();
                app.message = "Ctrl+Enter save · Esc cancel · Ctrl+S save and send".to_string();
            }
        }
        KeyCode::Char('m') => app.composer.cycle_method(1),
        KeyCode::Char('t') => {
            app.composer.body_kind = match app.composer.body_kind {
                BodyKind::Json => BodyKind::Raw,
                BodyKind::Raw => BodyKind::Json,
            };
            app.message = format!("Body type: {}", app.composer.body_kind_label());
        }
        KeyCode::Char('f') => {
            app.composer.follow = !app.composer.follow;
            app.message = format!(
                "Redirect following {}",
                if app.composer.follow {
                    "enabled"
                } else {
                    "disabled"
                }
            );
        }
        KeyCode::Char('I') => {
            app.composer.insecure = !app.composer.insecure;
            app.message = if app.composer.insecure {
                "TLS certificate verification disabled for this request".to_string()
            } else {
                "TLS certificate verification enabled".to_string()
            };
        }
        KeyCode::Char('n') => {
            app.composer = Composer::default();
            app.message = "New request".to_string();
        }
        KeyCode::Char('h') | KeyCode::Char('b') => {
            app.composer.show_response_headers = key.code == KeyCode::Char('h');
            app.composer.response_scroll = 0;
        }
        KeyCode::PageDown => {
            app.composer.response_scroll = app.composer.response_scroll.saturating_add(10)
        }
        KeyCode::PageUp => {
            app.composer.response_scroll = app.composer.response_scroll.saturating_sub(10)
        }
        KeyCode::Home => app.composer.response_scroll = 0,
        KeyCode::Char('s') => {
            let request = match app.composer.build_request() {
                Ok(request) => request,
                Err(err) => {
                    app.message = err.to_string();
                    return Ok(());
                }
            };
            let result =
                execute_interactive(session, app, paths.clone(), request, "Sending").await?;
            match result {
                Ok(response) => {
                    app.message = format!(
                        "HTTP {} · {} bytes · {} ms",
                        response.status, response.response_bytes, response.elapsed_ms
                    );
                    app.composer.response_scroll = 0;
                    app.composer.display_cache = None;
                    app.composer.response = Some(response);
                }
                Err(CurlyError::Cancelled) => app.message = "Request cancelled".to_string(),
                Err(err) => app.message = format!("Request failed: {err}"),
            }
            app.refresh_history(store).await?;
        }
        _ => {}
    }
    Ok(())
}

async fn handle_history_key(
    session: &mut TerminalSession,
    app: &mut App,
    store: &HistoryStore,
    paths: &StoragePaths,
    key: KeyEvent,
) -> Result<(), CurlyError> {
    match key.code {
        KeyCode::Char('/') => app.filtering = true,
        KeyCode::Down | KeyCode::Char('j') => match app.history_focus {
            HistoryFocus::List => {
                app.move_selection(1);
                app.refresh_selection(store).await?;
            }
            HistoryFocus::Details => app.history_scroll = app.history_scroll.saturating_add(1),
        },
        KeyCode::Up | KeyCode::Char('k') => match app.history_focus {
            HistoryFocus::List => {
                app.move_selection(-1);
                app.refresh_selection(store).await?;
            }
            HistoryFocus::Details => app.history_scroll = app.history_scroll.saturating_sub(1),
        },
        KeyCode::Tab => app.history_focus = app.history_focus.toggle(),
        KeyCode::Char('h') => {
            app.show_history_headers = true;
            app.history_focus = HistoryFocus::Details;
            app.history_scroll = 0;
        }
        KeyCode::Char('b') => {
            app.show_history_headers = false;
            app.history_focus = HistoryFocus::Details;
            app.history_scroll = 0;
        }
        KeyCode::PageDown => app.history_scroll = app.history_scroll.saturating_add(10),
        KeyCode::PageUp => app.history_scroll = app.history_scroll.saturating_sub(10),
        KeyCode::Home => app.history_scroll = 0,
        KeyCode::Char('e') => {
            let Some(entry) = app.selected.clone() else {
                app.message = "No history request selected".to_string();
                return Ok(());
            };
            match validate_replay(&entry) {
                Ok(request) => {
                    app.composer.load_request(request);
                    app.switch_workspace(Workspace::Request);
                    app.message = "Loaded history request into the composer".to_string();
                }
                Err(err) => app.message = err.to_string(),
            }
        }
        KeyCode::Char('r') => {
            let Some(entry) = app.selected.clone() else {
                return Ok(());
            };
            if requires_confirmation(&entry) {
                app.message = "Replay this non-safe method? press y to confirm".to_string();
                session.terminal.draw(|frame| draw(frame, app))?;
                if !confirm_replay(session, app)? {
                    app.message = "Replay cancelled".to_string();
                    return Ok(());
                }
            }
            let request = match validate_replay(&entry) {
                Ok(request) => request,
                Err(err) => {
                    app.message = err.to_string();
                    return Ok(());
                }
            };
            let result =
                execute_interactive(session, app, paths.clone(), request, "Replaying").await?;
            app.message = match result {
                Ok(response) => format!("Replay finished: HTTP {}", response.status),
                Err(CurlyError::Cancelled) => "Replay cancelled".to_string(),
                Err(err) => format!("Replay failed: {err}"),
            };
            app.refresh_history(store).await?;
        }
        _ => {}
    }
    Ok(())
}

fn handle_composer_edit(app: &mut App, input: Event) {
    let composer = &mut app.composer;
    match input {
        Event::Paste(text) => composer.insert_text(&text),
        Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
            KeyCode::Esc => {
                composer.cancel_edit();
                app.message = REQUEST_HELP.to_string();
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                composer.commit_edit();
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                composer.format_json()
            }
            KeyCode::Char('n')
                if key.modifiers.contains(KeyModifiers::CONTROL) && composer.pairs.is_some() =>
            {
                composer.add_pair()
            }
            KeyCode::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL) && composer.pairs.is_some() =>
            {
                composer.remove_pair()
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Enter if composer.pairs.is_some() => {
                let cell = composer.pairs.as_ref().unwrap().cell;
                composer.select_pair_cell(if key.code == KeyCode::BackTab {
                    cell.saturating_sub(1)
                } else {
                    cell + 1
                });
            }
            KeyCode::Enter if composer.focus == ComposerField::Body => composer.insert_newline(),
            KeyCode::Tab if composer.focus == ComposerField::Body => composer.insert_text("  "),
            KeyCode::Enter | KeyCode::Tab | KeyCode::BackTab => {
                if !composer.commit_edit() {
                    return;
                }
                {
                    if key.code == KeyCode::Tab {
                        composer.move_focus(1);
                    }
                    if key.code == KeyCode::BackTab {
                        composer.move_focus(-1);
                    }
                    app.message = REQUEST_HELP.to_string();
                }
            }
            KeyCode::Up | KeyCode::Down => {
                if let Some(pairs) = &composer.pairs {
                    let cell = if key.code == KeyCode::Up {
                        pairs.cell.saturating_sub(2)
                    } else {
                        pairs.cell + 2
                    };
                    composer.select_pair_cell(cell);
                } else {
                    composer.move_edit_line(key.code == KeyCode::Down);
                }
            }
            KeyCode::Left => composer.edit_cursor = composer.previous_boundary(),
            KeyCode::Right => composer.edit_cursor = composer.next_boundary(),
            KeyCode::Home => composer.edit_cursor = 0,
            KeyCode::End => composer.edit_cursor = composer.edit_buffer.len(),
            KeyCode::Backspace => {
                let previous = composer.previous_boundary();
                composer.edit_buffer.drain(previous..composer.edit_cursor);
                composer.edit_cursor = previous;
            }
            KeyCode::Delete => {
                let next = composer.next_boundary();
                composer.edit_buffer.drain(composer.edit_cursor..next);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                composer.edit_buffer.clear();
                composer.edit_cursor = 0;
            }
            KeyCode::Char('"') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                composer.insert_smart_quote()
            }
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(ch, '{' | '}' | '[' | ']') =>
            {
                composer.insert_smart_json_delimiter(ch)
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                composer.insert_text(&ch.to_string())
            }
            _ => {}
        },
        _ => {}
    }
}

async fn handle_history_filter(
    app: &mut App,
    store: &HistoryStore,
    input: Event,
) -> Result<(), CurlyError> {
    if let Event::Paste(text) = &input {
        app.filter.push_str(text.trim());
        app.rebuild_visible();
        app.refresh_selection(store).await?;
    }
    if let Event::Key(key) = input {
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.filter.clear();
                app.rebuild_visible();
                app.refresh_selection(store).await?;
            }
            KeyCode::Esc | KeyCode::Enter => app.filtering = false,
            KeyCode::Backspace => {
                app.filter.pop();
                app.rebuild_visible();
                app.refresh_selection(store).await?;
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.filter.push(ch);
                app.rebuild_visible();
                app.refresh_selection(store).await?;
            }
            _ => {}
        }
    }
    Ok(())
}

async fn execute_interactive(
    session: &mut TerminalSession,
    app: &mut App,
    paths: StoragePaths,
    request: RequestDefinition,
    action: &str,
) -> Result<Result<TuiResponse, CurlyError>, CurlyError> {
    let cancellation = CancellationToken::new();
    let task_cancel = cancellation.clone();
    let task = tokio::spawn(async move { execute_tui_request(&paths, request, task_cancel).await });
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let mut tick = 0usize;
    app.busy = true;

    while !task.is_finished() {
        app.message = format!("{} {action}… Esc cancels", spinner[tick % spinner.len()]);
        tick = tick.wrapping_add(1);
        session.terminal.draw(|frame| draw(frame, app))?;
        while event::poll(std::time::Duration::ZERO)? {
            let input = event::read()?;
            let cancel = match input {
                Event::Key(event) => {
                    event.kind != KeyEventKind::Release
                        && (event.code == KeyCode::Esc
                            || (event.code == KeyCode::Char('c')
                                && event.modifiers.contains(KeyModifiers::CONTROL)))
                }
                Event::Mouse(mouse) => {
                    mouse.kind == MouseEventKind::Down(MouseButton::Left)
                        && app.hits.iter().any(|hit| {
                            hit.action == MouseAction::CancelRequest
                                && hit.area.contains(Position::new(mouse.column, mouse.row))
                        })
                }
                _ => false,
            };
            if cancel {
                cancellation.cancel();
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    }

    app.busy = false;
    task.await
        .map_err(|err| CurlyError::History(format!("TUI request worker failed: {err}")))
}

async fn execute_tui_request(
    paths: &StoragePaths,
    request: RequestDefinition,
    cancellation: CancellationToken,
) -> Result<TuiResponse, CurlyError> {
    let started = std::time::Instant::now();
    let store = HistoryStore::open(paths)?;
    let (response, request_preview) = match executor::execute(&request, &cancellation).await {
        Ok(value) => value,
        Err(err) => {
            record_tui_failure(&store, &request, &err, started.elapsed()).await;
            return Err(err);
        }
    };
    let status = response.status;
    let response_headers = response.headers.clone();
    let (response_preview, response_truncated, response_bytes) =
        match executor::collect_stream(response.stream, &cancellation, HISTORY_PREVIEW_LIMIT).await
        {
            Ok(value) => value,
            Err(err) => {
                record_tui_failure(&store, &request, &err, started.elapsed()).await;
                return Err(err);
            }
        };
    let elapsed = started.elapsed();
    let output = OutputResult {
        status,
        response_headers: response_headers.clone(),
        response_preview: response_preview.clone(),
        response_truncated,
        response_bytes,
        elapsed,
    };
    if let Err(err) = store
        .record_async(NewHistoryEntry::success(
            &request,
            &request_preview,
            &output,
        ))
        .await
    {
        eprintln!("curly: warning: could not write TUI history: {err}");
    }

    Ok(TuiResponse {
        status,
        response_headers,
        response_preview,
        response_truncated,
        response_bytes,
        elapsed_ms: elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
    })
}

async fn record_tui_failure(
    store: &HistoryStore,
    request: &RequestDefinition,
    err: &CurlyError,
    elapsed: std::time::Duration,
) {
    if let Err(history_err) = store
        .record_async(NewHistoryEntry::failure(request, err, elapsed))
        .await
    {
        eprintln!("curly: warning: could not write TUI history: {history_err}");
    }
}

fn confirm_replay(session: &mut TerminalSession, app: &mut App) -> Result<bool, CurlyError> {
    loop {
        session.terminal.draw(|frame| {
            draw(frame, app);
            app.hits.clear();
            let area = overlay_area(frame.area());
            frame.render_widget(Clear, area);
            let request = app.selected.as_ref().map_or(String::new(), |entry| format!("{} {}", entry.method, compact_value(&entry.url)));
            frame.render_widget(Paragraph::new(format!("Replay this request?\n\n{request}\n\nThis method may change data.\n\ny confirm · n / Esc cancel")).block(panel(" Confirm replay ", true)).wrap(Wrap { trim: false }), area);
            buttons(frame, Rect::new(area.x + 1, area.bottom().saturating_sub(2), area.width.saturating_sub(2), 1), &[(" Replay ", MouseAction::ConfirmReplay), (" Cancel ", MouseAction::RejectReplay)], &mut app.hits);
        })?;
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => return Ok(true),
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => return Ok(false),
                _ => {}
            },
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => {
                let position = Position::new(mouse.column, mouse.row);
                if let Some(hit) = app.hits.iter().find(|hit| hit.area.contains(position)) {
                    match hit.action {
                        MouseAction::ConfirmReplay => return Ok(true),
                        MouseAction::RejectReplay => return Ok(false),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

fn requires_confirmation(entry: &HistoryEntry) -> bool {
    !matches!(entry.method.as_str(), "GET" | "HEAD" | "OPTIONS")
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    active: bool,
}

impl TerminalSession {
    fn enter() -> Result<Self, CurlyError> {
        let backend = CrosstermBackend::new(io::stdout());
        let terminal = Terminal::new(backend)?;
        let mut session = Self {
            terminal,
            active: false,
        };
        enable_raw_mode()?;
        session.active = true;
        execute!(
            session.terminal.backend_mut(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture
        )?;
        Ok(session)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
            let _ = execute!(
                self.terminal.backend_mut(),
                DisableBracketedPaste,
                DisableMouseCapture,
                LeaveAlternateScreen
            );
            let _ = self.terminal.show_cursor();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Workspace {
    Request,
    History,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComposerField {
    Method,
    Url,
    Headers,
    Query,
    Body,
}

impl ComposerField {
    const ALL: [Self; 5] = [
        Self::Method,
        Self::Url,
        Self::Headers,
        Self::Query,
        Self::Body,
    ];

    fn moved(self, delta: isize) -> Self {
        let current = Self::ALL
            .iter()
            .position(|field| *field == self)
            .unwrap_or(0) as isize;
        Self::ALL[(current + delta).rem_euclid(Self::ALL.len() as isize) as usize]
    }
}

#[derive(Debug, Clone)]
struct Composer {
    method: String,
    method_explicit: bool,
    url: String,
    headers: String,
    query: String,
    body: String,
    body_kind: BodyKind,
    auth: Option<AuthDefinition>,
    follow: bool,
    insecure: bool,
    focus: ComposerField,
    editing: bool,
    edit_buffer: String,
    edit_cursor: usize,
    pairs: Option<editor::PairDraft>,
    pair_active: bool,
    header_pairs: Option<Vec<(String, String)>>,
    query_pairs: Option<Vec<(String, String)>>,
    response: Option<TuiResponse>,
    response_scroll: u16,
    show_response_headers: bool,
    display_cache: Option<(bool, preview::Document)>,
}

impl Default for Composer {
    fn default() -> Self {
        Self {
            method: "GET".to_string(),
            method_explicit: false,
            url: String::new(),
            headers: "Accept: application/json".to_string(),
            query: String::new(),
            body: String::new(),
            body_kind: BodyKind::Json,
            auth: None,
            follow: false,
            insecure: false,
            focus: ComposerField::Url,
            editing: false,
            edit_buffer: String::new(),
            edit_cursor: 0,
            pairs: None,
            pair_active: false,
            header_pairs: None,
            query_pairs: None,
            response: None,
            response_scroll: 0,
            show_response_headers: false,
            display_cache: None,
        }
    }
}

impl Composer {
    fn previous_boundary(&self) -> usize {
        self.edit_buffer[..self.edit_cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next_boundary(&self) -> usize {
        self.edit_buffer[self.edit_cursor..]
            .chars()
            .next()
            .map_or(self.edit_cursor, |ch| self.edit_cursor + ch.len_utf8())
    }

    fn insert_text(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.edit_buffer.insert_str(self.edit_cursor, &text);
        self.edit_cursor += text.len();
    }

    fn cycle_method(&mut self, delta: isize) {
        self.method_explicit = true;
        let current = METHODS
            .iter()
            .position(|method| *method == self.method.as_str())
            .unwrap_or(0) as isize;
        self.method =
            METHODS[(current + delta).rem_euclid(METHODS.len() as isize) as usize].to_string();
    }

    fn move_focus(&mut self, delta: isize) {
        self.focus = self.focus.moved(delta);
    }

    fn begin_edit(&mut self) {
        self.editing = true;
        self.edit_buffer = match self.focus {
            ComposerField::Method => self.method.clone(),
            ComposerField::Url => self.url.clone(),
            ComposerField::Headers => self.headers.clone(),
            ComposerField::Query => self.query.clone(),
            ComposerField::Body => self.body.clone(),
        };
        self.edit_cursor = self.edit_buffer.len();
        self.pairs = None;
        self.pair_active = false;
        if matches!(self.focus, ComposerField::Headers | ComposerField::Query) {
            let rows = if self.focus == ComposerField::Headers {
                self.header_pairs
                    .clone()
                    .unwrap_or_else(|| parse_headers_input(&self.headers).unwrap_or_default())
            } else {
                self.query_pairs
                    .clone()
                    .unwrap_or_else(|| parse_query_input(&self.query).unwrap_or_default())
            };
            self.pairs = Some(editor::PairDraft::new(rows));
            self.select_pair_cell(0);
        }
    }

    fn cancel_edit(&mut self) {
        self.editing = false;
        self.edit_buffer.clear();
    }

    fn commit_edit(&mut self) -> bool {
        if self.edit_error().is_some() {
            return false;
        }
        if let Some(rows) = self.pair_values() {
            let rows = rows
                .into_iter()
                .filter(|(key, value)| !key.is_empty() || !value.is_empty())
                .collect::<Vec<_>>();
            if self.focus == ComposerField::Headers {
                self.headers = format_pairs(&rows, ": ");
                self.header_pairs = Some(rows);
            } else {
                self.query = format_pairs(&rows, "=");
                self.query_pairs = Some(rows);
            }
            self.editing = false;
            self.pairs = None;
            return true;
        }
        let value = std::mem::take(&mut self.edit_buffer);
        match self.focus {
            ComposerField::Method => {}
            ComposerField::Url => self.url = value.trim().to_owned(),
            ComposerField::Headers => self.headers = value,
            ComposerField::Query => self.query = value,
            ComposerField::Body => {
                let body_was_empty = self.body.trim().is_empty();
                self.body = value;
                if body_was_empty
                    && !self.body.trim().is_empty()
                    && self.method == "GET"
                    && !self.method_explicit
                {
                    self.method = "POST".to_string();
                }
            }
        }
        self.editing = false;
        true
    }

    fn body_kind_label(&self) -> &'static str {
        match self.body_kind {
            BodyKind::Json => "JSON",
            BodyKind::Raw => "raw",
        }
    }

    fn build_request(&self) -> Result<RequestDefinition, CurlyError> {
        if self.url.trim().is_empty() {
            return Err(CurlyError::Invalid(
                "enter an HTTP/HTTPS URL before sending".to_string(),
            ));
        }
        editor::validate_body(&self.body, self.body_kind).map_err(CurlyError::Invalid)?;
        RequestDefinition::new(
            self.url.trim().to_string(),
            Some(self.method.clone()),
            self.header_pairs
                .clone()
                .map_or_else(|| parse_headers_input(&self.headers), Ok)?,
            self.query_pairs
                .clone()
                .map_or_else(|| parse_query_input(&self.query), Ok)?,
            body_from_input(&self.body, self.body_kind)?,
            self.auth.clone(),
            TransportOptions {
                follow: self.follow,
                insecure: self.insecure,
                ..TransportOptions::default()
            },
        )
    }

    fn load_request(&mut self, request: RequestDefinition) {
        self.method = request.method;
        self.method_explicit = true;
        self.url = request.url;
        self.header_pairs = Some(request.headers.clone());
        self.query_pairs = Some(request.query.clone());
        self.headers = format_pairs(&request.headers, ": ");
        self.query = format_pairs(&request.query, "=");
        self.auth = request.auth;
        self.follow = request.transport.follow;
        self.insecure = request.transport.insecure;
        self.body.clear();
        if let Some(body) = request.body {
            self.body_kind = body.kind();
            self.body = match body {
                BodySource::Inline { value, .. } => value,
                BodySource::File { path, .. } => format!("@{}", path.display()),
                BodySource::Stdin { .. } => "-".to_string(),
            };
        }
        self.focus = ComposerField::Url;
        self.editing = false;
        self.edit_buffer.clear();
        self.response = None;
        self.display_cache = None;
        self.response_scroll = 0;
        self.show_response_headers = false;
    }

    fn auth_label(&self) -> String {
        match &self.auth {
            Some(AuthDefinition::BasicLiteral { username, .. }) => format!("basic ({username})"),
            Some(AuthDefinition::BearerLiteral { .. }) => "bearer".to_string(),
            Some(AuthDefinition::BearerEnv { variable }) => format!("bearer env {variable}"),
            None => "none".to_string(),
        }
    }
}

fn parse_headers_input(input: &str) -> Result<Vec<(String, String)>, CurlyError> {
    split_editor_items(input)
        .map(parse_header)
        .collect::<Result<Vec<_>, _>>()
}

fn parse_query_input(input: &str) -> Result<Vec<(String, String)>, CurlyError> {
    split_editor_items(input)
        .map(|item| parse_key_value(item, "query parameter"))
        .collect::<Result<Vec<_>, _>>()
}

fn split_editor_items(input: &str) -> impl Iterator<Item = &str> {
    input
        .lines()
        .flat_map(|line| line.split("||"))
        .map(str::trim)
        .filter(|item| !item.is_empty())
}

fn format_pairs(pairs: &[(String, String)], separator: &str) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{key}{separator}{value}"))
        .collect::<Vec<_>>()
        .join(
            "
",
        )
}

fn body_from_input(input: &str, kind: BodyKind) -> Result<Option<BodySource>, CurlyError> {
    if input.trim().is_empty() {
        return Ok(None);
    }
    if input.trim() == "-" {
        return Err(CurlyError::Invalid(
            "the TUI cannot use stdin as a request body; paste the body or use @path".to_string(),
        ));
    }
    if let Some(path) = input.strip_prefix('@') {
        if path.trim().is_empty() {
            return Err(CurlyError::Invalid(
                "body file syntax requires a path after @".to_string(),
            ));
        }
        return Ok(Some(BodySource::File {
            kind,
            path: path.trim().into(),
        }));
    }
    Ok(Some(BodySource::Inline {
        kind,
        value: input.to_string(),
    }))
}

#[derive(Debug, Clone)]
struct TuiResponse {
    status: u16,
    response_headers: Vec<(String, String)>,
    response_preview: Vec<u8>,
    response_truncated: bool,
    response_bytes: u64,
    elapsed_ms: u64,
}

#[derive(Debug)]
struct App {
    workspace: Workspace,
    composer: Composer,
    summaries: Vec<crate::storage::history::HistorySummary>,
    visible: Vec<usize>,
    list_state: ListState,
    selected: Option<HistoryEntry>,
    filter: String,
    filtering: bool,
    history_focus: HistoryFocus,
    show_history_headers: bool,
    history_scroll: u16,
    message: String,
    help: bool,
    busy: bool,
    method_menu: Option<usize>,
    hits: Vec<HitTarget>,
    history_cache: Option<(i64, bool, preview::Document)>,
}

impl App {
    fn new(summaries: Vec<crate::storage::history::HistorySummary>) -> Self {
        let mut app = Self {
            workspace: Workspace::Request,
            composer: Composer::default(),
            summaries,
            visible: Vec::new(),
            list_state: ListState::default(),
            selected: None,
            filter: String::new(),
            filtering: false,
            history_focus: HistoryFocus::List,
            show_history_headers: false,
            history_scroll: 0,
            message: "Ready · Enter a URL, then Ctrl+S to send".to_string(),
            help: false,
            busy: false,
            method_menu: None,
            hits: Vec::new(),
            history_cache: None,
        };
        app.rebuild_visible();
        app
    }

    fn switch_workspace(&mut self, workspace: Workspace) {
        self.workspace = workspace;
        self.filtering = false;
        self.composer.editing = false;
        self.message = match workspace {
            Workspace::Request => REQUEST_HELP,
            Workspace::History => HISTORY_HELP,
        }
        .to_string();
    }

    async fn refresh_history(&mut self, store: &HistoryStore) -> Result<(), CurlyError> {
        self.summaries = store.list_async().await?;
        self.rebuild_visible();
        self.refresh_selection(store).await
    }

    fn rebuild_visible(&mut self) {
        let needle = self.filter.to_ascii_lowercase();
        self.visible = self
            .summaries
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let haystack = format!(
                    "{} {} {}",
                    item.method,
                    item.url,
                    item.status
                        .map_or(String::new(), |status| status.to_string())
                )
                .to_ascii_lowercase();
                (needle.is_empty() || haystack.contains(&needle)).then_some(index)
            })
            .collect();
        if self.visible.is_empty() {
            self.list_state.select(None);
            self.selected = None;
        } else {
            let current = self
                .list_state
                .selected()
                .unwrap_or(0)
                .min(self.visible.len() - 1);
            self.list_state.select(Some(current));
        }
        self.history_scroll = 0;
    }

    fn selected_id(&self) -> Option<i64> {
        let position = self.list_state.selected()?;
        let summary_index = *self.visible.get(position)?;
        Some(self.summaries.get(summary_index)?.id)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, self.visible.len() as isize - 1) as usize;
        self.list_state.select(Some(next));
        self.history_scroll = 0;
    }

    async fn refresh_selection(&mut self, store: &HistoryStore) -> Result<(), CurlyError> {
        self.selected = match self.selected_id() {
            Some(id) => store.get_async(id).await?,
            None => None,
        };
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryFocus {
    List,
    Details,
}

impl HistoryFocus {
    fn toggle(self) -> Self {
        match self {
            Self::List => Self::Details,
            Self::Details => Self::List,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseAction {
    Workspace(Workspace),
    Field(ComposerField),
    Key(char),
    HistoryRow(usize),
    HistoryList,
    HistoryDetails,
    Response,
    SaveEdit,
    Disabled,
    PairCell(usize),
    MethodPick(usize),
    AddPair,
    RemovePair,
    FormatJson,
    JsonObject,
    JsonArray,
    CancelEdit,
    Help,
    CancelRequest,
    ConfirmReplay,
    RejectReplay,
}

#[derive(Debug)]
struct HitTarget {
    area: Rect,
    action: MouseAction,
}

fn buttons(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    items: &[(&str, MouseAction)],
    hits: &mut Vec<HitTarget>,
) {
    if area.height == 0 {
        return;
    }
    frame.render_widget(Block::default().style(Style::default().bg(SURFACE)), area);
    let mut x = area.x;
    for (label, action) in items {
        let width = Line::raw(*label).width() as u16;
        if x.saturating_add(width) > area.right() {
            break;
        }
        let target = Rect::new(x, area.y, width, 1);
        frame.render_widget(
            Paragraph::new(label.replace(['[', ']'], " ")).style(
                if matches!(
                    action,
                    MouseAction::Key('s') | MouseAction::SaveEdit | MouseAction::ConfirmReplay
                ) {
                    Style::default()
                        .fg(BACKGROUND)
                        .bg(ACCENT)
                        .add_modifier(Modifier::BOLD)
                } else if label.starts_with('[') {
                    Style::default()
                        .fg(ACCENT)
                        .bg(SELECTED)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(MUTED).bg(SURFACE)
                },
            ),
            target,
        );
        if *action != MouseAction::Disabled {
            hits.push(HitTarget {
                area: target,
                action: *action,
            });
        }
        x = x.saturating_add(width + 1);
    }
}

fn handle_mouse(app: &mut App, mouse: MouseEvent) -> Option<char> {
    let position = Position::new(mouse.column, mouse.row);
    if matches!(
        mouse.kind,
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
    ) {
        let action = app
            .hits
            .iter()
            .rev()
            .find(|hit| {
                hit.area.contains(position)
                    && matches!(
                        hit.action,
                        MouseAction::Response
                            | MouseAction::HistoryList
                            | MouseAction::HistoryDetails
                    )
            })
            .map(|hit| hit.action);
        let down = mouse.kind == MouseEventKind::ScrollDown;
        match action {
            Some(MouseAction::Response) => {
                app.composer.response_scroll = if down {
                    app.composer.response_scroll.saturating_add(3)
                } else {
                    app.composer.response_scroll.saturating_sub(3)
                }
            }
            Some(MouseAction::HistoryDetails) => {
                app.history_focus = HistoryFocus::Details;
                app.history_scroll = if down {
                    app.history_scroll.saturating_add(3)
                } else {
                    app.history_scroll.saturating_sub(3)
                };
            }
            Some(MouseAction::HistoryList) => {
                app.history_focus = HistoryFocus::List;
                app.move_selection(if down { 3 } else { -3 });
            }
            _ => {}
        }
        return None;
    }
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return None;
    }
    let action = app
        .hits
        .iter()
        .rev()
        .find(|hit| hit.area.contains(position))
        .map(|hit| hit.action)?;
    match action {
        MouseAction::Workspace(workspace) => app.switch_workspace(workspace),
        MouseAction::Field(field) => {
            app.composer.focus = field;
            if field == ComposerField::Method {
                app.method_menu = Some(
                    METHODS
                        .iter()
                        .position(|method| *method == app.composer.method)
                        .unwrap_or(0),
                );
            } else {
                app.composer.begin_edit();
            }
        }
        MouseAction::Key(key) => {
            app.filtering = false;
            return Some(key);
        }
        MouseAction::HistoryRow(row) => {
            app.list_state.select(Some(row));
            app.history_focus = HistoryFocus::List;
            app.history_scroll = 0;
        }
        MouseAction::HistoryDetails => app.history_focus = HistoryFocus::Details,
        MouseAction::HistoryList => app.history_focus = HistoryFocus::List,
        MouseAction::SaveEdit => {
            app.composer.commit_edit();
        }
        MouseAction::CancelEdit => app.composer.cancel_edit(),
        MouseAction::MethodPick(index) => {
            app.composer.method = METHODS[index].into();
            app.composer.method_explicit = true;
            app.method_menu = None;
        }
        MouseAction::PairCell(cell) => app.composer.select_pair_cell(cell),
        MouseAction::AddPair => app.composer.add_pair(),
        MouseAction::RemovePair => app.composer.remove_pair(),
        MouseAction::FormatJson => app.composer.format_json(),
        MouseAction::JsonObject => app.composer.insert_json_template(false),
        MouseAction::JsonArray => app.composer.insert_json_template(true),
        MouseAction::Help => app.help = !app.help,
        MouseAction::Disabled
        | MouseAction::Response
        | MouseAction::CancelRequest
        | MouseAction::ConfirmReplay
        | MouseAction::RejectReplay => {}
    }
    None
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &mut App) {
    app.hits.clear();
    frame.render_widget(
        Block::default().style(Style::default().fg(FOREGROUND).bg(BACKGROUND)),
        frame.area(),
    );
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(2),
        ])
        .split(frame.area());
    draw_navigation(frame, outer[0], app);
    let workspace_area = outer[1].inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    match app.workspace {
        Workspace::Request => draw_request_workspace(frame, workspace_area, app),
        Workspace::History => draw_history_workspace(frame, workspace_area, app),
    }
    draw_status(frame, outer[2], app);
    if app.composer.editing {
        app.hits.clear();
        editor::draw(frame, &app.composer, &mut app.hits);
    }
    if let Some(selected) = app.method_menu {
        app.hits.clear();
        let bounds = overlay_area(frame.area());
        let area = Rect::new(
            bounds.x,
            bounds.y,
            bounds.width.min(42),
            bounds.height.min(11),
        );
        frame.render_widget(Clear, area);
        frame.render_widget(panel(" Choose method · Esc cancel ", true), area);
        for (index, method) in METHODS.iter().enumerate() {
            if index as u16 + 2 >= area.height {
                break;
            }
            let row = Rect::new(
                area.x + 1,
                area.y + 1 + index as u16,
                area.width.saturating_sub(2),
                1,
            );
            frame.render_widget(
                Paragraph::new(format!(
                    " {method:<8} {}",
                    [
                        "Read a resource",
                        "Create / submit",
                        "Replace",
                        "Update",
                        "Delete",
                        "Headers only",
                        "Allowed methods"
                    ][index]
                ))
                .style(
                    Style::default()
                        .fg(if index == selected {
                            ACCENT
                        } else {
                            FOREGROUND
                        })
                        .bg(if index == selected { SELECTED } else { SURFACE }),
                ),
                row,
            );
            app.hits.push(HitTarget {
                area: row,
                action: MouseAction::MethodPick(index),
            });
        }
    }
    if app.help {
        app.hits.clear();
        draw_help(frame);
        let area = overlay_area(frame.area());
        buttons(
            frame,
            Rect::new(
                area.x + 1,
                area.bottom().saturating_sub(2),
                area.width.saturating_sub(2),
                1,
            ),
            &[(" Close ", MouseAction::Help)],
            &mut app.hits,
        );
    }
    if app.busy {
        app.hits.clear();
        let area = outer[2];
        buttons(
            frame,
            Rect::new(
                area.right().saturating_sub(10),
                area.y + 1,
                10.min(area.width),
                area.height.saturating_sub(1).min(1),
            ),
            &[(" Cancel ", MouseAction::CancelRequest)],
            &mut app.hits,
        );
    }
}

fn draw_navigation(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut App) {
    frame.render_widget(Block::default().style(Style::default().bg(SURFACE)), area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "  curly",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  /", Style::default().fg(BORDER)),
        ])),
        Rect::new(
            area.x,
            area.y + 1.min(area.height.saturating_sub(1)),
            area.width.min(12),
            1.min(area.height),
        ),
    );
    let start = area.x + 13.min(area.width);
    buttons(
        frame,
        Rect::new(
            start,
            area.y + 1.min(area.height.saturating_sub(1)),
            area.right().saturating_sub(start),
            area.height.min(1),
        ),
        &[
            (
                if app.workspace == Workspace::Request {
                    "[1  Request]"
                } else {
                    " 1  Request "
                },
                MouseAction::Workspace(Workspace::Request),
            ),
            (
                if app.workspace == Workspace::History {
                    "[2  History]"
                } else {
                    " 2  History "
                },
                MouseAction::Workspace(Workspace::History),
            ),
            (" ? Help ", MouseAction::Help),
        ],
        &mut app.hits,
    );
    if area.width >= 90 {
        frame.render_widget(
            Paragraph::new("Local workspace  ")
                .alignment(Alignment::Right)
                .style(Style::default().fg(MUTED)),
            Rect::new(area.right() - 22, area.y + 1, 22, 1),
        );
    }
}

fn draw_request_workspace(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut App) {
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .split(area);
    draw_endpoint(frame, rows[1], app);
    let horizontal = area.width >= 92;
    let panes = Layout::default()
        .direction(if horizontal {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints(if horizontal {
            [
                Constraint::Length((area.width / 3).clamp(36, 56)),
                Constraint::Min(1),
            ]
        } else {
            [Constraint::Length(8), Constraint::Min(1)]
        })
        .split(rows[3]);
    let setup = if horizontal {
        Rect::new(
            panes[0].x,
            panes[0].y,
            panes[0].width.saturating_sub(1),
            panes[0].height,
        )
    } else {
        panes[0]
    };
    draw_composer(frame, setup, app);
    draw_response(frame, panes[1], app);
}

fn draw_endpoint(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut App) {
    let columns = Layout::horizontal([
        Constraint::Length(11),
        Constraint::Min(5),
        Constraint::Length(if area.width >= 70 { 16 } else { 9 }),
    ])
    .split(area);
    let method = columns[0];
    frame.render_widget(
        Paragraph::new(format!(" {}  ▾", app.composer.method))
            .style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            .block(panel("", app.composer.focus == ComposerField::Method)),
        method,
    );
    app.hits.push(HitTarget {
        area: method,
        action: MouseAction::Field(ComposerField::Method),
    });
    let url = if app.composer.url.is_empty() {
        " Enter request URL…".to_owned()
    } else {
        format!(" {}", compact_value(&app.composer.url))
    };
    frame.render_widget(
        Paragraph::new(url)
            .style(Style::default().fg(if app.composer.url.is_empty() {
                MUTED
            } else {
                FOREGROUND
            }))
            .block(panel("", app.composer.focus == ComposerField::Url)),
        columns[1],
    );
    app.hits.push(HitTarget {
        area: columns[1],
        action: MouseAction::Field(ComposerField::Url),
    });
    let send = columns[2].inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    frame.render_widget(
        Block::default().style(Style::default().bg(SURFACE)),
        columns[2],
    );
    buttons(
        frame,
        send,
        &[(
            if area.width >= 70 {
                " Send  Ctrl+S "
            } else {
                " Send "
            },
            MouseAction::Key('s'),
        )],
        &mut app.hits,
    );
}

fn draw_composer(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut App) {
    if area.height < 18 {
        // Compact layouts keep all request fields one click away.
        frame.render_widget(panel(" Request setup ", false), area);
        let inner = panel("", false).inner(area);
        for (offset, field, label, value) in [
            (
                0,
                ComposerField::Headers,
                "Headers",
                compact_value(&app.composer.headers),
            ),
            (
                2,
                ComposerField::Query,
                "Query",
                compact_value(&app.composer.query),
            ),
            (
                3,
                ComposerField::Body,
                "Body",
                compact_value(&app.composer.body),
            ),
        ] {
            if offset >= inner.height {
                continue;
            }
            let row = Rect::new(
                inner.x + 1,
                inner.y + offset,
                inner.width.saturating_sub(2),
                if field == ComposerField::Headers {
                    2.min(inner.height)
                } else {
                    1
                },
            );
            let text = if field == ComposerField::Headers {
                format!("{label}\n{value}")
            } else {
                format!(
                    "{label:<9} {}",
                    if value.is_empty() {
                        "Click to add"
                    } else {
                        &value
                    }
                )
            };
            frame.render_widget(
                Paragraph::new(text).style(Style::default().fg(if app.composer.focus == field {
                    ACCENT
                } else {
                    FOREGROUND
                })),
                row,
            );
            app.hits.push(HitTarget {
                area: row,
                action: MouseAction::Field(field),
            });
        }
        if inner.height > 4 {
            draw_options(
                frame,
                Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1),
                app,
            );
        }
        return;
    }
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(6),
        Constraint::Length(5),
        Constraint::Min(5),
        Constraint::Length(4),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(" Request setup")
            .style(Style::default().fg(FOREGROUND).add_modifier(Modifier::BOLD)),
        rows[0],
    );
    let header_title = format!(
        " Headers · {} ",
        app.composer.header_pairs.as_ref().map_or_else(
            || split_editor_items(&app.composer.headers).count(),
            Vec::len
        )
    );
    draw_request_section(
        frame,
        rows[1],
        ComposerField::Headers,
        &header_title,
        &app.composer.headers,
        "No headers yet. Click to add one.",
        app.composer.focus,
        &mut app.hits,
    );
    let query_title = format!(
        " Query parameters · {} ",
        app.composer
            .query_pairs
            .as_ref()
            .map_or_else(|| split_editor_items(&app.composer.query).count(), Vec::len)
    );
    draw_request_section(
        frame,
        rows[2],
        ComposerField::Query,
        &query_title,
        &app.composer.query,
        "Add key=value parameters",
        app.composer.focus,
        &mut app.hits,
    );
    let body_title = format!(" Body · {} ", app.composer.body_kind_label());
    draw_request_section(
        frame,
        rows[3],
        ComposerField::Body,
        &body_title,
        &app.composer.body,
        "No request body\n\nClick to write JSON, paste text,\nor use @path for a body file.",
        app.composer.focus,
        &mut app.hits,
    );
    frame.render_widget(
        Paragraph::new(" Request options").style(Style::default().fg(MUTED)),
        Rect::new(rows[4].x, rows[4].y, rows[4].width, 1),
    );
    draw_options(
        frame,
        Rect::new(rows[4].x, rows[4].y + 1, rows[4].width, 1),
        app,
    );
    frame.render_widget(
        Paragraph::new(format!(" Auth: {}", app.composer.auth_label()))
            .style(Style::default().fg(MUTED)),
        Rect::new(rows[4].x, rows[4].y + 2, rows[4].width, 1),
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_request_section(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    field: ComposerField,
    title: &str,
    value: &str,
    empty: &str,
    focus: ComposerField,
    hits: &mut Vec<HitTarget>,
) {
    let area = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1));
    let block = panel(title, focus == field);
    let inner = block.inner(area).inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    frame.render_widget(block, area);
    if value.trim().is_empty() {
        frame.render_widget(
            Paragraph::new(empty)
                .style(Style::default().fg(MUTED))
                .wrap(Wrap { trim: false }),
            inner,
        );
    } else {
        let text = if field == ComposerField::Body {
            preview::body(value.as_bytes(), &[], false)
        } else {
            Text::from(
                value
                    .lines()
                    .map(|item| Line::raw(output::escape_terminal_bytes(item.as_bytes())))
                    .collect::<Vec<_>>(),
            )
        };
        frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
    }
    hits.push(HitTarget {
        area,
        action: MouseAction::Field(field),
    });
}

fn draw_options(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut App) {
    buttons(
        frame,
        area,
        &[
            (
                if app.composer.follow {
                    "[Follow]"
                } else {
                    " Follow "
                },
                MouseAction::Key('f'),
            ),
            (
                if app.composer.insecure {
                    "[Insecure]"
                } else {
                    " TLS on "
                },
                MouseAction::Key('I'),
            ),
            (
                if app.composer.body_kind == BodyKind::Json {
                    " JSON "
                } else {
                    " Raw "
                },
                MouseAction::Key('t'),
            ),
            (" New ", MouseAction::Key('n')),
        ],
        &mut app.hits,
    );
}

fn draw_response(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut App) {
    let block = panel(" Response ", false);
    let inner = block.inner(area).inner(Margin {
        horizontal: 1,
        vertical: 0,
    });
    frame.render_widget(block, area);
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(inner);
    buttons(
        frame,
        rows[0],
        &[
            (
                if app.composer.show_response_headers {
                    " Body "
                } else {
                    "[Body]"
                },
                MouseAction::Key('b'),
            ),
            (
                if app.composer.show_response_headers {
                    "[Headers]"
                } else {
                    " Headers "
                },
                MouseAction::Key('h'),
            ),
        ],
        &mut app.hits,
    );
    app.hits.push(HitTarget {
        area: rows[3],
        action: MouseAction::Response,
    });
    let composer = &mut app.composer;
    let Some(response) = &composer.response else {
        let height = rows[3].height.min(8);
        let intro = Rect::new(
            rows[3].x,
            rows[3].y + rows[3].height.saturating_sub(height) / 3,
            rows[3].width,
            height,
        );
        let text = Text::from(vec![
            Line::styled(
                "{  }",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            Line::styled(
                if app.busy {
                    "Sending your request"
                } else {
                    "Your response starts here"
                },
                Style::default().fg(FOREGROUND).add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            Line::styled(
                "Enter a URL above, then send the request.",
                Style::default().fg(MUTED),
            ),
            Line::styled(
                "Status, headers and formatted content appear here.",
                Style::default().fg(MUTED),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(text)
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: false }),
            intro,
        );
        frame.render_widget(
            Paragraph::new(" Local history keeps your recent requests")
                .style(Style::default().fg(MUTED)),
            rows[4],
        );
        return;
    };
    let metrics = Line::from(vec![
        Span::styled(
            format!(" {} ", response.status),
            Style::default()
                .fg(if response.status >= 400 {
                    Color::LightRed
                } else {
                    ACCENT
                })
                .bg(SELECTED)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "   {} ms   /   {}",
                response.elapsed_ms,
                human_bytes(response.response_bytes)
            ),
            Style::default().fg(MUTED),
        ),
        Span::styled(
            if response.response_truncated {
                "   TRUNCATED"
            } else {
                ""
            },
            Style::default().fg(Color::Yellow),
        ),
    ]);
    frame.render_widget(Paragraph::new(metrics), rows[2]);
    if composer
        .display_cache
        .as_ref()
        .is_none_or(|(headers, _)| *headers != composer.show_response_headers)
    {
        let text = if composer.show_response_headers {
            preview::headers(&response.response_headers)
        } else {
            preview::body(
                &response.response_preview,
                &response.response_headers,
                response.response_truncated,
            )
        };
        composer.display_cache =
            Some((composer.show_response_headers, preview::Document::new(text)));
    }
    let (_, text) = composer.display_cache.as_mut().unwrap();
    render_viewer(frame, rows[3], text, &mut composer.response_scroll);
    frame.render_widget(
        Paragraph::new(" Wheel / PgUp PgDn  scroll    h / b  switch view")
            .style(Style::default().fg(MUTED)),
        rows[4],
    );
}

fn human_bytes(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MiB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn viewer_areas(area: Rect) -> (Rect, Rect) {
    let toolbar = Rect::new(area.x, area.y, area.width, area.height.min(1));
    let content = Rect::new(
        area.x,
        area.y + area.height.min(2),
        area.width,
        area.height.saturating_sub(2),
    );
    (toolbar, content)
}

fn render_viewer(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    document: &mut preview::Document,
    scroll: &mut u16,
) {
    let gutter = if area.width >= 32 { 5 } else { 0 };
    let text = document.at_width(area.width.saturating_sub(gutter + 1));
    let max_scroll = text
        .lines
        .len()
        .saturating_sub(usize::from(area.height))
        .min(u16::MAX as usize) as u16;
    *scroll = (*scroll).min(max_scroll);
    let visible = Text::from(
        text.lines
            .iter()
            .skip(usize::from(*scroll))
            .take(usize::from(area.height))
            .cloned()
            .collect::<Vec<_>>(),
    )
    .style(text.style);
    let content = Rect::new(
        area.x + gutter,
        area.y,
        area.width.saturating_sub(gutter + 1),
        area.height,
    );
    frame.render_widget(Paragraph::new(visible), content);
    if gutter > 0 {
        let numbers = (0..area.height)
            .map(|row| {
                let number = usize::from(*scroll) + usize::from(row) + 1;
                Line::styled(
                    if number <= text.lines.len() {
                        format!("{number:>3} │")
                    } else {
                        String::new()
                    },
                    Style::default().fg(MUTED),
                )
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(numbers),
            Rect::new(area.x, area.y, gutter, area.height),
        );
    }
    if max_scroll > 0 && area.width > 0 && area.height > 0 {
        let position = u32::from(*scroll) * u32::from(area.height - 1) / u32::from(max_scroll);
        frame.render_widget(
            Paragraph::new("┃").style(Style::default().fg(ACCENT)),
            Rect::new(area.right() - 1, area.y + position as u16, 1, 1),
        );
    }
}

fn draw_history_workspace(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut App) {
    let horizontal = area.width >= 90;
    let panes = Layout::default()
        .direction(if horizontal {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints(if horizontal {
            [Constraint::Percentage(38), Constraint::Percentage(62)]
        } else {
            [Constraint::Percentage(42), Constraint::Percentage(58)]
        })
        .split(area);
    draw_history(frame, panes[0], app);
    draw_history_details(frame, panes[1], app);
}

fn draw_history(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut App) {
    let (toolbar, list_area) = viewer_areas(area);
    buttons(
        frame,
        toolbar,
        &[
            (" Search / ", MouseAction::Key('/')),
            (" Edit ", MouseAction::Key('e')),
            (" Replay ", MouseAction::Key('r')),
        ],
        &mut app.hits,
    );
    let area = list_area;
    let items = app
        .visible
        .iter()
        .filter_map(|index| app.summaries.get(*index))
        .map(|item| {
            let outcome = item
                .status
                .map_or_else(|| "ERR".to_string(), |status| status.to_string());
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<7}", item.method), Style::default().fg(ACCENT)),
                Span::styled(
                    format!("{outcome:>3}  "),
                    Style::default().fg(if item.status.is_some_and(|status| status < 400) {
                        MUTED
                    } else {
                        Color::LightRed
                    }),
                ),
                Span::raw(compact_value(&item.url)),
            ]))
        });
    let title = if app.filter.is_empty() {
        format!(" History · {} requests ", app.visible.len())
    } else {
        format!(" History · /{} ", compact_value(&app.filter))
    };
    let list = List::new(items)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .style(Style::default().bg(SURFACE))
                .title_style(Style::default().fg(MUTED))
                .border_style(focus_style(app.history_focus == HistoryFocus::List)),
        )
        .style(Style::default().fg(FOREGROUND).bg(SURFACE))
        .highlight_style(Style::default().fg(ACCENT).bg(SELECTED))
        .highlight_symbol("› ");
    frame.render_stateful_widget(list, area, &mut app.list_state);
    let inner = panel("", false).inner(area);
    app.hits.push(HitTarget {
        area: inner,
        action: MouseAction::HistoryList,
    });
    for row in 0..inner.height {
        let position = app.list_state.offset() + usize::from(row);
        if position >= app.visible.len() {
            break;
        }
        app.hits.push(HitTarget {
            area: Rect::new(inner.x, inner.y + row, inner.width, 1),
            action: MouseAction::HistoryRow(position),
        });
    }
}

fn draw_history_details(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut App) {
    let title = if app.show_history_headers {
        " Headers "
    } else {
        " Request / response "
    };
    let title = format!(
        "{title}{}",
        if app
            .selected
            .as_ref()
            .is_some_and(|entry| entry.response_truncated || entry.request_truncated)
        {
            "· TRUNCATED "
        } else {
            ""
        }
    );
    let block = panel(&title, app.history_focus == HistoryFocus::Details);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (toolbar, content_area) = viewer_areas(inner);
    buttons(
        frame,
        toolbar,
        &[
            (
                if app.show_history_headers {
                    " Body "
                } else {
                    "[Body]"
                },
                MouseAction::Key('b'),
            ),
            (
                if app.show_history_headers {
                    "[Headers]"
                } else {
                    " Headers "
                },
                MouseAction::Key('h'),
            ),
        ],
        &mut app.hits,
    );
    app.hits.push(HitTarget {
        area: content_area,
        action: MouseAction::HistoryDetails,
    });
    let Some(entry) = &app.selected else {
        frame.render_widget(
            Paragraph::new("No matching requests. Send a request or change your search.")
                .wrap(Wrap { trim: false }),
            content_area,
        );
        return;
    };
    if app
        .history_cache
        .as_ref()
        .is_none_or(|(id, headers, _)| *id != entry.id || *headers != app.show_history_headers)
    {
        let mut text = Text::from(vec![
            Line::styled(
                format!("{} {}", entry.method, compact_value(&entry.url)),
                Style::default().fg(ACCENT),
            ),
            Line::styled(
                format!(
                    "HTTP {} · {} ms · {} bytes{}",
                    entry
                        .status
                        .map_or_else(|| "ERR".to_string(), |status| status.to_string()),
                    entry.elapsed_ms,
                    entry.response_bytes,
                    if entry.response_truncated {
                        " · TRUNCATED"
                    } else {
                        ""
                    }
                ),
                Style::default().fg(if entry.status.is_some_and(|status| status < 400) {
                    Color::Green
                } else {
                    Color::Red
                }),
            ),
            Line::raw(""),
        ]);
        if let Some(error) = &entry.error {
            text.lines.push(Line::styled(
                output::escape_terminal_bytes(error.as_bytes()),
                Style::default().fg(Color::Red),
            ));
        }
        if app.show_history_headers {
            text.lines
                .push(Line::styled("REQUEST HEADERS", Style::default().fg(MUTED)));
            text.lines
                .extend(preview::headers(&entry.request_headers).lines);
            text.lines.push(Line::raw(""));
            text.lines
                .push(Line::styled("RESPONSE HEADERS", Style::default().fg(MUTED)));
            text.lines
                .extend(preview::headers(&entry.response_headers).lines);
        } else {
            if !entry.request_preview.is_empty() {
                text.lines
                    .push(Line::styled("REQUEST BODY", Style::default().fg(MUTED)));
                text.lines.extend(
                    preview::body(
                        &entry.request_preview,
                        &entry.request_headers,
                        entry.request_truncated,
                    )
                    .lines,
                );
                text.lines.push(Line::raw(""));
            }
            text.lines
                .push(Line::styled("RESPONSE BODY", Style::default().fg(MUTED)));
            text.lines.extend(
                preview::body(
                    &entry.response_preview,
                    &entry.response_headers,
                    entry.response_truncated,
                )
                .lines,
            );
        }
        app.history_cache = Some((
            entry.id,
            app.show_history_headers,
            preview::Document::new(text),
        ));
    }
    render_viewer(
        frame,
        content_area,
        &mut app.history_cache.as_mut().unwrap().2,
        &mut app.history_scroll,
    );
}

fn draw_status(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let text = if app.workspace == Workspace::History && app.filtering {
        format!("  Search: {}▏   Enter apply · Esc close", app.filter)
    } else {
        format!("  {}", app.message)
    };
    frame.render_widget(Block::default().style(Style::default().bg(RAISED)), area);
    frame.render_widget(
        Paragraph::new(output::escape_terminal_bytes(text.as_bytes()))
            .style(Style::default().fg(MUTED)),
        Rect::new(area.x, area.y, area.width, area.height.min(1)),
    );
    if area.height > 1 {
        frame.render_widget(
            Paragraph::new("  Tab navigate   Enter edit   Ctrl+S send   ? shortcuts   q quit")
                .style(Style::default().fg(MUTED)),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
    }
}

fn panel(title: &str, focused: bool) -> Block<'_> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focus_style(focused))
        .style(Style::default().fg(FOREGROUND).bg(SURFACE))
        .title_style(Style::default().fg(if focused { ACCENT } else { MUTED }))
}

fn overlay_area(area: Rect) -> Rect {
    let width = area.width.saturating_sub(4).min(92);
    let height = area.height.saturating_sub(2).min(22);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn editor_area(area: Rect, field: ComposerField) -> Rect {
    let mut editor = overlay_area(area);
    if field == ComposerField::Url {
        editor.height = editor.height.min(7);
        editor.y = area.y + area.height.saturating_sub(editor.height) / 2;
    }
    editor
}

fn draw_text_editor(frame: &mut ratatui::Frame<'_>, composer: &Composer) {
    let area = editor_area(frame.area(), composer.focus);
    frame.render_widget(Clear, area);
    let label = match composer.focus {
        ComposerField::Method => "Method",
        ComposerField::Url => "URL",
        ComposerField::Headers => "Headers · one per line",
        ComposerField::Query => "Query · key=value per line",
        ComposerField::Body => {
            if composer.body_kind == BodyKind::Json {
                "JSON body · validated before saving"
            } else {
                "Raw body · text or @file"
            }
        }
    };
    let block = panel(label, true).title_bottom(" Ctrl+Enter save · Esc cancel ");
    let mut inner = block.inner(area);
    inner.height = inner.height.saturating_sub(3);
    frame.render_widget(block, area);
    let source = output::escape_terminal_bytes(composer.edit_buffer.as_bytes());
    let text = if composer.focus == ComposerField::Body
        && composer.body_kind == BodyKind::Json
        && !composer.edit_buffer.starts_with('@')
    {
        preview::json_source(&source)
    } else {
        Text::raw(source)
    };
    // Keep the real terminal cursor visible without inserting a glyph into the text. An inline
    // cursor shifts every character after it by one cell as Left/Right moves through a line.
    let prefix =
        output::escape_terminal_bytes(&composer.edit_buffer.as_bytes()[..composer.edit_cursor]);
    let cursor_row = prefix.matches('\n').count();
    let cursor_column = Line::raw(prefix.rsplit('\n').next().unwrap_or("")).width();
    let scroll_y = cursor_row
        .saturating_sub(usize::from(inner.height.max(1)).saturating_sub(1))
        .min(u16::MAX as usize) as u16;
    let scroll_x = cursor_column
        .saturating_sub(usize::from(inner.width.max(1)).saturating_sub(1))
        .min(u16::MAX as usize) as u16;
    frame.render_widget(Paragraph::new(text).scroll((scroll_y, scroll_x)), inner);
    if inner.width > 0 && inner.height > 0 {
        frame.set_cursor_position(Position::new(
            inner.x + cursor_column.saturating_sub(usize::from(scroll_x)) as u16,
            inner.y + cursor_row.saturating_sub(usize::from(scroll_y)) as u16,
        ));
    }
}

fn draw_help(frame: &mut ratatui::Frame<'_>) {
    let area = overlay_area(frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(
        "MOUSE  Click tabs, fields, buttons and rows · wheel scroll\nREQUEST\nTab / Shift+Tab   Next / previous field\nEnter             Edit selected field\ns / Ctrl+S        Send (Ctrl+S saves your edit first)\nm / left / right  Cycle method\nt                 JSON / raw body\nf / I             Follow redirects / insecure TLS\nh / b             Response headers / body\nPgUp / PgDn       Scroll response\nn                 New request\n\nEDITOR\nLeft / Right      Move cursor · Home / End jump\nEnter             Body newline · Ctrl+U clear\nCtrl+Enter / Esc   Save / discard edit\n\nHISTORY  2 / F2    / search · e edit · r replay\nGLOBAL   1 / F1 request · q quit · Esc cancel send"
    ).block(panel(" Keyboard guide · ? / Esc close ", true)).wrap(Wrap { trim: false }), area);
}

fn compact_value(value: &str) -> String {
    output::escape_terminal_bytes(value.as_bytes())
        .chars()
        .take(160)
        .collect::<String>()
        .replace('\r', "")
        .replace('\n', " ↵ ")
}

fn focus_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(BORDER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(
        id: i64,
        method: &str,
        url: &str,
        status: u16,
    ) -> crate::storage::history::HistorySummary {
        crate::storage::history::HistorySummary {
            id,
            created_at: "now".into(),
            method: method.into(),
            url: url.into(),
            status: Some(status),
            error: None,
            elapsed_ms: 1,
            response_bytes: 0,
        }
    }

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn click(app: &mut App, action: MouseAction) -> Option<char> {
        let target = app
            .hits
            .iter()
            .find(|hit| hit.action == action)
            .unwrap()
            .area;
        handle_mouse(
            app,
            mouse(MouseEventKind::Down(MouseButton::Left), target.x, target.y),
        )
    }

    #[test]
    fn mouse_targets_follow_responsive_layout_and_modal_blocks_background() {
        for (width, height) in [(120, 32), (80, 24), (48, 18)] {
            let mut app = App::new(vec![]);
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert_eq!(click(&mut app, MouseAction::Key('s')), Some('s'));
            for field in ComposerField::ALL {
                let target = app
                    .hits
                    .iter()
                    .find(|hit| hit.action == MouseAction::Field(field))
                    .unwrap()
                    .area;
                assert!(target.width > 0 && target.height > 0);
                assert!(target.right() <= width && target.bottom() <= height);
            }
            let url_area = app
                .hits
                .iter()
                .find(|hit| hit.action == MouseAction::Field(ComposerField::Url))
                .unwrap()
                .area;
            let send_area = app
                .hits
                .iter()
                .find(|hit| hit.action == MouseAction::Key('s'))
                .unwrap()
                .area;
            assert!(send_area.x >= url_area.right());
            assert!(send_area.y >= url_area.y && send_area.y < url_area.bottom());
            click(&mut app, MouseAction::Field(ComposerField::Url));
            assert!(app.composer.editing);
            app.composer.insert_text("https://example.test");
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert!(
                app.hits.iter().all(|hit| matches!(
                    hit.action,
                    MouseAction::SaveEdit | MouseAction::CancelEdit
                ))
            );
            click(&mut app, MouseAction::SaveEdit);
            assert_eq!(app.composer.url, "https://example.test");
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            click(&mut app, MouseAction::Workspace(Workspace::History));
            assert_eq!(app.workspace, Workspace::History);
        }
    }

    #[test]
    fn mouse_selects_scrolled_history_rows_and_scrolls_only_hovered_pane() {
        let mut app = App::new(
            (0..40)
                .map(|id| summary(id, "GET", "https://example.test", 200))
                .collect(),
        );
        app.workspace = Workspace::History;
        app.list_state.select(Some(30));
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let offset = app.list_state.offset();
        assert!(offset > 0);
        click(&mut app, MouseAction::HistoryRow(offset));
        assert_eq!(app.selected_id(), Some(offset as i64));
        let area = app
            .hits
            .iter()
            .find(|hit| hit.action == MouseAction::HistoryDetails)
            .unwrap()
            .area;
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, area.x, area.y));
        assert_eq!(app.history_scroll, 3);
        assert_eq!(app.selected_id(), Some(offset as i64));
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollUp, area.x, area.y));
        assert_eq!(app.history_scroll, 0);
        assert_eq!(click(&mut app, MouseAction::Key('r')), Some('r'));
    }

    #[test]
    fn highlighted_response_keeps_truncation_visible_and_clamps_scroll() {
        let mut app = App::new(vec![]);
        app.composer.response = Some(TuiResponse {
            status: 200,
            response_headers: vec![("Content-Type".into(), "text/html".into())],
            response_preview:
                b"<!DOCTYPE html><html><body><p class=\"intro\">Hello</p></body></html>".to_vec(),
            response_truncated: true,
            response_bytes: 200000,
            elapsed_ms: 42,
        });
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("TRUNCATED"));
        assert!(rendered.contains("Hello"));
        assert!(
            buffer
                .content()
                .iter()
                .any(|cell| cell.fg == Color::Green && cell.symbol() == "i")
        );
        app.composer.response_scroll = u16::MAX;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(app.composer.response_scroll < 100);
    }

    #[test]
    fn editor_inserts_deletes_and_cancels_unicode() {
        let mut app = App::new(vec![]);
        app.composer.url = "aé日z".into();
        app.composer.begin_edit();
        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        handle_composer_edit(&mut app, key(KeyCode::Left));
        handle_composer_edit(&mut app, key(KeyCode::Backspace));
        handle_composer_edit(&mut app, Event::Paste("中".into()));
        assert_eq!(app.composer.edit_buffer, "aé中z");
        handle_composer_edit(&mut app, key(KeyCode::Home));
        handle_composer_edit(&mut app, key(KeyCode::Delete));
        assert_eq!(app.composer.edit_buffer, "é中z");
        handle_composer_edit(&mut app, key(KeyCode::Esc));
        assert_eq!(app.composer.url, "aé日z");
    }

    #[test]
    fn editor_newlines_tab_and_release_events() {
        let mut app = App::new(vec![]);
        app.composer.focus = ComposerField::Headers;
        app.composer.begin_edit();
        app.composer.add_pair();
        handle_composer_edit(&mut app, Event::Paste("X-Test".into()));
        handle_composer_edit(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        );
        handle_composer_edit(&mut app, Event::Paste("yes".into()));
        let mut release = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        handle_composer_edit(&mut app, Event::Key(release));
        handle_composer_edit(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)),
        );
        assert_eq!(
            app.composer.header_pairs.as_ref().unwrap()[1],
            ("X-Test".into(), "yes".into())
        );
        assert!(!app.composer.editing);
    }

    #[test]
    fn responsive_editor_keeps_cursor_visible_and_help_renders() {
        for (width, height) in [(120, 32), (80, 24), (48, 16), (20, 8)] {
            let mut app = App::new(vec![]);
            app.composer.url = format!("https://example.test/{}END", "x".repeat(200));
            app.composer.begin_edit();
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let rendered = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(
                rendered.contains("END"),
                "text around cursor hidden at {width}x{height}"
            );
            assert!(!rendered.contains('▏'));
            let cursor = terminal.backend().cursor_position();
            assert!(cursor.x < width && cursor.y < height);
            app.composer.cancel_edit();
            app.help = true;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        }
    }

    #[test]
    fn moving_editor_cursor_does_not_shift_json_text() {
        let mut app = App::new(vec![]);
        app.composer.focus = ComposerField::Body;
        app.composer.body = "{\n  \"test\": \"ok\"\n}".into();
        app.composer.begin_edit();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let after_closing_brace = terminal.backend().buffer().clone();

        app.composer.edit_cursor = app.composer.previous_boundary();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let before_closing_brace = terminal.backend().buffer().clone();

        assert_eq!(after_closing_brace, before_closing_brace);
        assert!(
            !before_closing_brace
                .content()
                .iter()
                .any(|cell| cell.symbol() == "▏")
        );
    }

    #[test]
    fn composer_builds_ordered_duplicate_headers_and_query() {
        let composer = Composer {
            method: "POST".into(),
            url: "https://example.test/items".into(),
            headers: "X-Test: one || X-Test: two".into(),
            query: "tag=one || tag=two".into(),
            body: r#"{"ok":true}"#.into(),
            ..Composer::default()
        };
        let request = composer.build_request().unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.headers,
            vec![
                ("X-Test".into(), "one".into()),
                ("X-Test".into(), "two".into())
            ]
        );
        assert_eq!(
            request.query,
            vec![("tag".into(), "one".into()), ("tag".into(), "two".into())]
        );
        assert!(matches!(
            request.body,
            Some(BodySource::Inline {
                kind: BodyKind::Json,
                ..
            })
        ));
    }

    #[test]
    fn adding_body_to_default_get_switches_to_post() {
        let mut composer = Composer {
            focus: ComposerField::Body,
            ..Composer::default()
        };
        composer.begin_edit();
        composer.edit_buffer = "{}".into();
        composer.commit_edit();
        assert_eq!(composer.method, "POST");
    }

    #[test]
    fn tui_rejects_stdin_body() {
        let error = body_from_input("-", BodyKind::Raw).unwrap_err().to_string();
        assert!(error.contains("cannot use stdin"));
    }

    #[test]
    fn filter_changes_visible_rows() {
        let summaries = vec![
            summary(1, "GET", "https://example.test/alpha", 200),
            summary(2, "POST", "https://example.test/beta", 201),
        ];
        let mut app = App::new(summaries);
        app.filter = "beta".into();
        app.rebuild_visible();
        assert_eq!(app.visible, vec![1]);
    }

    #[test]
    fn test_backend_renders_request_workspace_by_default() {
        let mut app = App::new(vec![]);
        let backend = ratatui::backend::TestBackend::new(110, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Request"));
        assert!(rendered.contains("Response"));
        assert!(rendered.contains("Accept: application/json"));
    }

    #[test]
    fn test_backend_renders_history_and_details() {
        let mut app = App::new(vec![summary(1, "GET", "https://example.test/items", 200)]);
        app.workspace = Workspace::History;
        app.selected = Some(HistoryEntry {
            id: 1,
            created_at: "now".into(),
            method: "GET".into(),
            url: "https://example.test/items".into(),
            request_headers: vec![],
            request_preview: vec![],
            request_truncated: false,
            request_bytes: 0,
            status: Some(200),
            error: None,
            elapsed_ms: 4,
            response_headers: vec![],
            response_preview: b"{}".to_vec(),
            response_truncated: false,
            response_bytes: 2,
            replay: None,
            replayable: false,
            replay_reason: Some("test".into()),
        });
        let backend = ratatui::backend::TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("History"));
        assert!(rendered.contains("Request / response"));
        assert!(rendered.contains("https://example.test/items"));
    }
}
