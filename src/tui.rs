use std::io::{self, IsTerminal};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    error::CurlyError,
    executor,
    output::{self, HISTORY_PREVIEW_LIMIT, OutputResult},
    storage::{
        HistoryEntry, HistoryStore, NewHistoryEntry, StoragePaths, history::validate_replay,
    },
};

pub async fn run(paths: StoragePaths) -> Result<u8, CurlyError> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(CurlyError::Invalid(
            "curly tui requires interactive stdin and stdout".to_string(),
        ));
    }
    let store = HistoryStore::open(&paths)?;
    let summaries = store.list_async().await?;
    let mut app = App::new(summaries);
    if let Some(id) = app.selected_id() {
        app.selected = store.get_async(id).await?;
    }

    let mut session = TerminalSession::enter()?;
    loop {
        session.terminal.draw(|frame| draw(frame, &mut app))?;
        if !event::poll(std::time::Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if app.filtering {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => app.filtering = false,
                KeyCode::Backspace => {
                    app.filter.pop();
                    app.rebuild_visible();
                    app.refresh_selection(&store).await?;
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    app.filter.push(ch);
                    app.rebuild_visible();
                    app.refresh_selection(&store).await?;
                }
                _ => {}
            }
            continue;
        }

        match key.code {
            KeyCode::Char('q') => break,
            KeyCode::Char('/') => app.filtering = true,
            KeyCode::Down | KeyCode::Char('j') => match app.focus {
                FocusPane::History => {
                    app.move_selection(1);
                    app.refresh_selection(&store).await?;
                }
                FocusPane::Details => app.scroll = app.scroll.saturating_add(1),
            },
            KeyCode::Up | KeyCode::Char('k') => match app.focus {
                FocusPane::History => {
                    app.move_selection(-1);
                    app.refresh_selection(&store).await?;
                }
                FocusPane::Details => app.scroll = app.scroll.saturating_sub(1),
            },
            KeyCode::Tab => app.focus = app.focus.toggle(),
            KeyCode::Char('h') => {
                app.show_headers = true;
                app.focus = FocusPane::Details;
                app.scroll = 0;
            }
            KeyCode::Char('b') => {
                app.show_headers = false;
                app.focus = FocusPane::Details;
                app.scroll = 0;
            }
            KeyCode::PageDown => app.scroll = app.scroll.saturating_add(10),
            KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(10),
            KeyCode::Home => app.scroll = 0,
            KeyCode::Char('r') => {
                if let Some(entry) = app.selected.clone() {
                    if requires_confirmation(&entry) {
                        app.message = "Replay this non-safe method? press y to confirm".to_string();
                        session.terminal.draw(|frame| draw(frame, &mut app))?;
                        if !confirm_replay()? {
                            app.message = "Replay cancelled".to_string();
                            continue;
                        }
                    }
                    let request = match validate_replay(&entry) {
                        Ok(request) => request,
                        Err(err) => {
                            app.message = err.to_string();
                            continue;
                        }
                    };
                    let cancellation = tokio_util::sync::CancellationToken::new();
                    let task_cancel = cancellation.clone();
                    let replay_paths = paths.clone();
                    let replay_task = tokio::spawn(async move {
                        replay_request(&replay_paths, request, task_cancel).await
                    });
                    let mut tick = 0usize;
                    while !replay_task.is_finished() {
                        let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                        app.message =
                            format!("{} Replaying… Esc cancels", spinner[tick % spinner.len()]);
                        tick = tick.wrapping_add(1);
                        session.terminal.draw(|frame| draw(frame, &mut app))?;
                        if event::poll(std::time::Duration::ZERO)?
                            && let Event::Key(event) = event::read()?
                            && event.code == KeyCode::Esc
                        {
                            cancellation.cancel();
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
                    }
                    let result = replay_task.await.map_err(|err| {
                        CurlyError::History(format!("replay worker failed: {err}"))
                    })?;
                    app.message = match result {
                        Ok(status) => format!("Replay finished: HTTP {status}"),
                        Err(CurlyError::Cancelled) => "Replay cancelled".to_string(),
                        Err(err) => format!("Replay failed: {err}"),
                    };
                    app.summaries = store.list_async().await?;
                    app.rebuild_visible();
                    app.refresh_selection(&store).await?;
                }
            }
            _ => {}
        }
    }
    Ok(0)
}

async fn replay_request(
    paths: &StoragePaths,
    request: crate::request::RequestDefinition,
    cancellation: tokio_util::sync::CancellationToken,
) -> Result<u16, CurlyError> {
    let started = std::time::Instant::now();
    let store = HistoryStore::open(paths)?;
    let executed = executor::execute(&request, &cancellation).await;
    let (response, request_preview) = match executed {
        Ok(value) => value,
        Err(err) => {
            if let Err(history_err) = store
                .record_async(NewHistoryEntry::failure(&request, &err, started.elapsed()))
                .await
            {
                eprintln!("curly: warning: could not write replay history: {history_err}");
            }
            return Err(err);
        }
    };
    let status = response.status;
    let response_headers = response.headers.clone();
    let collected =
        executor::collect_stream(response.stream, &cancellation, HISTORY_PREVIEW_LIMIT).await;
    let (response_preview, response_truncated, response_bytes) = match collected {
        Ok(value) => value,
        Err(err) => {
            if let Err(history_err) = store
                .record_async(NewHistoryEntry::failure(&request, &err, started.elapsed()))
                .await
            {
                eprintln!("curly: warning: could not write replay history: {history_err}");
            }
            return Err(err);
        }
    };
    let result = OutputResult {
        status,
        response_headers,
        response_preview,
        response_truncated,
        response_bytes,
        elapsed: started.elapsed(),
    };
    if let Err(err) = store
        .record_async(NewHistoryEntry::success(
            &request,
            &request_preview,
            &result,
        ))
        .await
    {
        eprintln!("curly: warning: could not write replay history: {err}");
    }
    Ok(status)
}

fn confirm_replay() -> Result<bool, CurlyError> {
    loop {
        let Event::Key(KeyEvent { code, .. }) = event::read()? else {
            continue;
        };
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => return Ok(true),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => return Ok(false),
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
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self {
            terminal,
            active: true,
        })
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
            let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
            let _ = self.terminal.show_cursor();
        }
    }
}

#[derive(Debug)]
struct App {
    summaries: Vec<crate::storage::history::HistorySummary>,
    visible: Vec<usize>,
    list_state: ListState,
    selected: Option<HistoryEntry>,
    filter: String,
    filtering: bool,
    focus: FocusPane,
    show_headers: bool,
    scroll: u16,
    message: String,
}

impl App {
    fn new(summaries: Vec<crate::storage::history::HistorySummary>) -> Self {
        let mut app = Self {
            summaries,
            visible: Vec::new(),
            list_state: ListState::default(),
            selected: None,
            filter: String::new(),
            filtering: false,
            focus: FocusPane::History,
            show_headers: false,
            scroll: 0,
            message: "↑/↓ j/k move/scroll · Tab focus · h headers · b body · / filter · r replay · q quit"
                .to_string(),
        };
        app.rebuild_visible();
        app
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
                    item.status.map_or(String::new(), |s| s.to_string())
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
        self.scroll = 0;
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
        self.scroll = 0;
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
enum FocusPane {
    History,
    Details,
}

impl FocusPane {
    fn toggle(self) -> Self {
        match self {
            Self::History => Self::Details,
            Self::Details => Self::History,
        }
    }
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &mut App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(2)])
        .split(frame.area());
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(outer[0]);
    draw_history(frame, panes[0], app);
    draw_details(frame, panes[1], app);
    let filter = if app.filtering {
        format!("Filter: {}_", app.filter)
    } else if !app.filter.is_empty() {
        format!("Filter: {} · {}", app.filter, app.message)
    } else {
        app.message.clone()
    };
    frame.render_widget(
        Paragraph::new(filter).block(Block::default().borders(Borders::TOP)),
        outer[1],
    );
}

fn draw_history(frame: &mut ratatui::Frame<'_>, area: Rect, app: &mut App) {
    let items = app
        .visible
        .iter()
        .filter_map(|index| app.summaries.get(*index))
        .map(|item| {
            let outcome = item
                .status
                .map_or_else(|| "ERR".to_string(), |s| s.to_string());
            ListItem::new(Line::from(format!(
                "{} {:>3} {}",
                item.method, outcome, item.url
            )))
        });
    let border_style = if app.focus == FocusPane::History {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let list = List::new(items)
        .block(
            Block::default()
                .title("History")
                .borders(Borders::ALL)
                .border_style(border_style),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("› ");
    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn draw_details(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let Some(entry) = &app.selected else {
        frame.render_widget(
            Paragraph::new("No history entries")
                .block(Block::default().title("Details").borders(Borders::ALL)),
            area,
        );
        return;
    };
    let (content, content_style) = if app.show_headers {
        let mut text = String::new();
        text.push_str("Request headers\n");
        for (name, value) in &entry.request_headers {
            text.push_str(&format!("{name}: {value}\n"));
        }
        text.push_str("\nResponse headers\n");
        for (name, value) in &entry.response_headers {
            text.push_str(&format!("{name}: {value}\n"));
        }
        (text, Style::default())
    } else {
        let mut text = format!(
            "{} {}\nStatus: {} · {} ms · {} bytes\n\n",
            entry.method,
            entry.url,
            entry
                .status
                .map_or_else(|| "ERR".to_string(), |s| s.to_string()),
            entry.elapsed_ms,
            entry.response_bytes
        );
        if let Some(error) = &entry.error {
            text.push_str(&format!("Error: {error}\n\n"));
        }
        if !entry.request_preview.is_empty() {
            text.push_str("Request body\n");
            text.push_str(&format_preview(&entry.request_preview));
            if entry.request_truncated {
                text.push_str("\n[… request preview truncated at 64 KiB …]");
            }
            text.push_str("\n\nResponse body\n");
        }
        text.push_str(&format_preview(&entry.response_preview));
        if entry.response_truncated {
            text.push_str("\n[… response preview truncated at 64 KiB …]");
        }
        let style = if serde_json::from_slice::<serde_json::Value>(&entry.response_preview).is_ok()
        {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        (text, style)
    };
    let title = if app.show_headers {
        "Headers"
    } else {
        "Response"
    };
    let border_style = if app.focus == FocusPane::Details {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    frame.render_widget(
        Paragraph::new(Text::raw(content))
            .style(content_style)
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(border_style),
            )
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0)),
        area,
    );
}

fn format_preview(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "<empty>".to_string();
    }
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(value) => serde_json::to_string_pretty(&value)
            .unwrap_or_else(|_| output::escape_terminal_bytes(bytes)),
        Err(_) => output::escape_terminal_bytes(bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_changes_visible_rows() {
        let summaries = vec![
            crate::storage::history::HistorySummary {
                id: 1,
                created_at: "now".into(),
                method: "GET".into(),
                url: "https://example.test/alpha".into(),
                status: Some(200),
                error: None,
                elapsed_ms: 1,
                response_bytes: 0,
            },
            crate::storage::history::HistorySummary {
                id: 2,
                created_at: "now".into(),
                method: "POST".into(),
                url: "https://example.test/beta".into(),
                status: Some(201),
                error: None,
                elapsed_ms: 1,
                response_bytes: 0,
            },
        ];
        let mut app = App::new(summaries);
        app.filter = "beta".into();
        app.rebuild_visible();
        assert_eq!(app.visible, vec![1]);
    }

    #[test]
    fn test_backend_renders_history_and_details() {
        let summaries = vec![crate::storage::history::HistorySummary {
            id: 1,
            created_at: "now".into(),
            method: "GET".into(),
            url: "https://example.test/items".into(),
            status: Some(200),
            error: None,
            elapsed_ms: 4,
            response_bytes: 2,
        }];
        let mut app = App::new(summaries);
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
        assert!(rendered.contains("Response"));
        assert!(rendered.contains("https://example.test/items"));
    }
}
