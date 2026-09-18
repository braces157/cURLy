use super::*;

#[derive(Debug, Clone)]
pub(super) struct PairDraft {
    pub rows: Vec<(String, String)>,
    pub cell: usize,
}

impl PairDraft {
    pub fn new(mut rows: Vec<(String, String)>) -> Self {
        if rows.is_empty() {
            rows.push((String::new(), String::new()));
        }
        Self { rows, cell: 0 }
    }
}

impl Composer {
    pub(super) fn pair_values(&self) -> Option<Vec<(String, String)>> {
        let draft = self.pairs.as_ref()?;
        let mut rows = draft.rows.clone();
        let row = &mut rows[draft.cell / 2];
        if draft.cell.is_multiple_of(2) {
            row.0 = self.edit_buffer.clone();
        } else {
            row.1 = self.edit_buffer.clone();
        }
        Some(rows)
    }

    pub(super) fn select_pair_cell(&mut self, cell: usize) {
        // On initial entry the active text still contains the old multiline view.
        if let Some(draft) = &mut self.pairs {
            if self.editing && self.edit_cursor <= self.edit_buffer.len() && self.pair_active {
                let row = &mut draft.rows[draft.cell / 2];
                if draft.cell.is_multiple_of(2) {
                    row.0 = self.edit_buffer.clone();
                } else {
                    row.1 = self.edit_buffer.clone();
                }
            }
            let cell = cell.min(draft.rows.len() * 2);
            if cell / 2 >= draft.rows.len() {
                draft.rows.push((String::new(), String::new()));
            }
            draft.cell = cell;
            let row = &draft.rows[cell / 2];
            self.edit_buffer = if cell.is_multiple_of(2) {
                row.0.clone()
            } else {
                row.1.clone()
            };
            self.edit_cursor = self.edit_buffer.len();
            self.pair_active = true;
        }
    }

    pub(super) fn add_pair(&mut self) {
        if let Some(draft) = &self.pairs {
            self.select_pair_cell(draft.rows.len() * 2);
        }
    }

    pub(super) fn remove_pair(&mut self) {
        if let Some(draft) = &mut self.pairs {
            let row = draft.cell / 2;
            draft.rows.remove(row);
            if draft.rows.is_empty() {
                draft.rows.push((String::new(), String::new()));
            }
            let cell = row.min(draft.rows.len() - 1) * 2;
            self.pair_active = false;
            self.select_pair_cell(cell);
        }
    }

    pub(super) fn edit_error(&self) -> Option<String> {
        if let Some(rows) = self.pair_values() {
            for (index, (name, value)) in rows.iter().enumerate() {
                if name.is_empty() && value.is_empty() {
                    continue;
                }
                if name.is_empty() {
                    return Some(format!(
                        "Row {} needs a name. Empty values are allowed.",
                        index + 1
                    ));
                }
                if self.focus == ComposerField::Headers {
                    if reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err() {
                        return Some(format!(
                            "Row {}: header names cannot contain spaces or separators.",
                            index + 1
                        ));
                    }
                    if reqwest::header::HeaderValue::from_str(value).is_err() {
                        return Some(format!(
                            "Row {}: header value contains an invalid character or newline.",
                            index + 1
                        ));
                    }
                } else if name.contains(['\r', '\n']) || value.contains(['\r', '\n']) {
                    return Some(format!(
                        "Row {}: use a separate row for each parameter.",
                        index + 1
                    ));
                }
            }
            return None;
        }
        match self.focus {
            ComposerField::Url if !self.edit_buffer.trim().is_empty() => {
                crate::request::validate_url(self.edit_buffer.trim())
                    .err()
                    .map(|_| "Use a complete http:// or https:// URL with a host.".to_owned())
            }
            ComposerField::Body => validate_body(&self.edit_buffer, self.body_kind).err(),
            _ => None,
        }
    }

    pub(super) fn format_json(&mut self) {
        if self.focus != ComposerField::Body || self.body_kind != BodyKind::Json {
            return;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&self.edit_buffer)
            && let Ok(pretty) = serde_json::to_string_pretty(&value)
        {
            self.edit_buffer = pretty;
            self.edit_cursor = self.edit_buffer.len();
        }
    }

    pub(super) fn insert_json_template(&mut self, array: bool) {
        if self.edit_buffer.trim().is_empty() {
            self.edit_buffer = if array { "[\n  \n]" } else { "{\n  \n}" }.to_owned();
            self.edit_cursor = 4;
        }
    }

    pub(super) fn insert_smart_quote(&mut self) {
        if self.focus != ComposerField::Body
            || self.body_kind != BodyKind::Json
            || self.edit_buffer.starts_with('@')
        {
            self.insert_text("\"");
            return;
        }

        let mut in_string = false;
        let mut escaped = false;
        for ch in self.edit_buffer[..self.edit_cursor].chars() {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' if in_string => escaped = true,
                '"' => in_string = !in_string,
                _ => {}
            }
        }

        if in_string && self.edit_buffer[self.edit_cursor..].starts_with('"') {
            self.edit_cursor += 1;
        } else if in_string {
            self.insert_text("\"");
        } else {
            self.edit_buffer.insert_str(self.edit_cursor, "\"\"");
            self.edit_cursor += 1;
        }
    }

    pub(super) fn insert_smart_json_delimiter(&mut self, ch: char) {
        if self.focus != ComposerField::Body
            || self.body_kind != BodyKind::Json
            || self.edit_buffer.starts_with('@')
        {
            self.insert_text(&ch.to_string());
            return;
        }

        let mut in_string = false;
        let mut escaped = false;
        for current in self.edit_buffer[..self.edit_cursor].chars() {
            if escaped {
                escaped = false;
                continue;
            }
            match current {
                '\\' if in_string => escaped = true,
                '"' => in_string = !in_string,
                _ => {}
            }
        }

        if in_string {
            self.insert_text(&ch.to_string());
            return;
        }

        match ch {
            '{' => {
                self.edit_buffer.insert_str(self.edit_cursor, "{}");
                self.edit_cursor += 1;
            }
            '[' => {
                self.edit_buffer.insert_str(self.edit_cursor, "[]");
                self.edit_cursor += 1;
            }
            '}' | ']' if self.edit_buffer[self.edit_cursor..].starts_with(ch) => {
                self.edit_cursor += ch.len_utf8();
            }
            _ => self.insert_text(&ch.to_string()),
        }
    }

    pub(super) fn insert_newline(&mut self) {
        let prefix = &self.edit_buffer[..self.edit_cursor];
        let line = prefix.rsplit('\n').next().unwrap_or("");
        let indent = line.chars().take_while(|ch| *ch == ' ').count();
        let opener = line.trim_end().chars().next_back();
        let extra = if self.body_kind == BodyKind::Json && matches!(opener, Some('{' | '[')) {
            2
        } else {
            0
        };
        let inner_indent = (indent + extra).min(128);

        let matching_closer = if self.body_kind == BodyKind::Json {
            match opener {
                Some('{') => Some('}'),
                Some('[') => Some(']'),
                _ => None,
            }
        } else {
            None
        };

        if matching_closer
            .is_some_and(|closer| self.edit_buffer[self.edit_cursor..].starts_with(closer))
        {
            let insertion = format!(
                "\n{}\n{}",
                " ".repeat(inner_indent),
                " ".repeat(indent.min(128))
            );
            self.edit_buffer.insert_str(self.edit_cursor, &insertion);
            self.edit_cursor += 1 + inner_indent;
        } else {
            self.insert_text(&format!("\n{}", " ".repeat(inner_indent)));
        }
    }

    pub(super) fn move_edit_line(&mut self, down: bool) {
        let prefix = &self.edit_buffer[..self.edit_cursor];
        let start = prefix.rfind('\n').map_or(0, |i| i + 1);
        let column = self.edit_buffer[start..self.edit_cursor].chars().count();
        let target = if down {
            self.edit_buffer[self.edit_cursor..]
                .find('\n')
                .map(|i| self.edit_cursor + i + 1)
        } else if start > 0 {
            Some(
                self.edit_buffer[..start - 1]
                    .rfind('\n')
                    .map_or(0, |i| i + 1),
            )
        } else {
            None
        };
        if let Some(target) = target {
            let line = self.edit_buffer[target..].split('\n').next().unwrap_or("");
            self.edit_cursor = target
                + line
                    .char_indices()
                    .nth(column)
                    .map_or(line.len(), |(i, _)| i);
        }
    }
}

pub(super) fn validate_body(value: &str, kind: BodyKind) -> Result<(), String> {
    if value.trim().is_empty() {
        return Ok(());
    }
    if value == "-" {
        return Err(
            "Use a body file (@path) or paste text; stdin is not available in the TUI.".into(),
        );
    }
    if let Some(path) = value.strip_prefix('@') {
        return if path.trim().is_empty() {
            Err("Enter the file path after @.".into())
        } else {
            Ok(())
        };
    }
    if kind == BodyKind::Json {
        serde_json::from_str::<serde_json::Value>(value)
            .map_err(|error| format!("JSON: {error}"))?;
    }
    Ok(())
}

pub(super) fn draw(frame: &mut ratatui::Frame<'_>, composer: &Composer, hits: &mut Vec<HitTarget>) {
    let mut area = editor_area(frame.area(), composer.focus);
    if let Some(draft) = &composer.pairs {
        area.height = area
            .height
            .min(draft.rows.len().saturating_add(8).min(u16::MAX as usize) as u16);
        area.y = frame.area().y + frame.area().height.saturating_sub(area.height) / 2;
    }
    if composer.pairs.is_some() {
        draw_pairs(frame, area, composer, hits);
    } else {
        draw_text_editor(frame, composer);
    }
    if composer.focus == ComposerField::Query
        && let Some(rows) = composer.pair_values()
    {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in rows
            .iter()
            .filter(|(key, value)| !key.is_empty() || !value.is_empty())
        {
            query.append_pair(key, value);
        }
        let encoded = format!(" Encoded query: {}", query.finish());
        frame.render_widget(
            Paragraph::new(encoded).style(Style::default().fg(MUTED).bg(SURFACE)),
            Rect::new(
                area.x + 1,
                area.bottom().saturating_sub(5),
                area.width.saturating_sub(2),
                1,
            ),
        );
    }
    let error = composer.edit_error();
    let hint = if let Some(error) = &error {
        error.as_str()
    } else if composer.pairs.is_some() {
        "Tab / Enter next cell · Ctrl+N add · Ctrl+D remove · Ctrl+Enter save"
    } else if composer.focus == ComposerField::Url {
        "HTTP / HTTPS URL · Enter saves · Ctrl+S saves and sends"
    } else if composer.edit_buffer.starts_with('@') {
        "Body file · contents checked when you send"
    } else if composer.edit_buffer.trim().is_empty() && composer.body_kind == BodyKind::Json {
        "No body yet · choose Object / Array or paste JSON · Ctrl+Enter saves"
    } else if composer.body_kind == BodyKind::Json {
        "Valid JSON · Enter newline · Tab indent · Ctrl+F format · Ctrl+Enter save"
    } else {
        "Raw body · Enter newline · Ctrl+Enter save"
    };
    let hint_area = Rect::new(
        area.x + 1,
        area.bottom().saturating_sub(4),
        area.width.saturating_sub(2),
        2,
    );
    frame.render_widget(
        Paragraph::new(hint)
            .style(
                Style::default()
                    .fg(if error.is_some() {
                        Color::LightRed
                    } else {
                        MUTED
                    })
                    .bg(SURFACE),
            )
            .wrap(Wrap { trim: false }),
        hint_area,
    );
    let mut actions = vec![
        (
            " Save ",
            if error.is_some() {
                MouseAction::Disabled
            } else {
                MouseAction::SaveEdit
            },
        ),
        (" Cancel ", MouseAction::CancelEdit),
    ];
    if composer.pairs.is_some() {
        actions.extend([
            (" + Row ", MouseAction::AddPair),
            (" - Row ", MouseAction::RemovePair),
        ]);
    } else if composer.focus == ComposerField::Body && composer.body_kind == BodyKind::Json {
        actions.push((" Format ", MouseAction::FormatJson));
        if composer.edit_buffer.trim().is_empty() {
            actions.extend([
                (" {} Object ", MouseAction::JsonObject),
                (" [] Array ", MouseAction::JsonArray),
            ]);
        }
    }
    buttons(
        frame,
        Rect::new(
            area.x + 1,
            area.bottom().saturating_sub(2),
            area.width.saturating_sub(2),
            1,
        ),
        &actions,
        hits,
    );
}

fn draw_pairs(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    composer: &Composer,
    hits: &mut Vec<HitTarget>,
) {
    frame.render_widget(Clear, area);
    let title = if composer.focus == ComposerField::Headers {
        " Headers · name / value "
    } else {
        " Query parameters · encoded automatically "
    };
    let block = panel(title, true).title_bottom(" Duplicate names and empty values are supported ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let header = Rect::new(inner.x, inner.y, inner.width, 1);
    let columns =
        Layout::horizontal([Constraint::Percentage(36), Constraint::Percentage(64)]).split(header);
    frame.render_widget(
        Paragraph::new(" Name").style(Style::default().fg(MUTED)),
        columns[0],
    );
    frame.render_widget(
        Paragraph::new(" Value").style(Style::default().fg(MUTED)),
        columns[1],
    );
    let draft = composer.pairs.as_ref().unwrap();
    let rows = composer.pair_values().unwrap();
    let visible = usize::from(inner.height.saturating_sub(5)).max(1);
    let offset = (draft.cell / 2).saturating_sub(visible - 1);
    for (index, row) in rows.iter().enumerate().skip(offset).take(visible) {
        let y = inner.y + 1 + (index - offset) as u16;
        if y >= area.bottom().saturating_sub(4) {
            break;
        }
        for (column, value) in [&row.0, &row.1].into_iter().enumerate() {
            let rect = Rect::new(columns[column].x, y, columns[column].width, 1);
            let active = draft.cell == index * 2 + column;
            let shown = if active {
                format!(" {}", output::escape_terminal_bytes(value.as_bytes()))
            } else if value.is_empty() {
                if column == 0 {
                    " name".into()
                } else {
                    " value (optional)".into()
                }
            } else {
                format!(" {}", output::escape_terminal_bytes(value.as_bytes()))
            };
            let scroll = if active {
                Line::raw(format!(
                    " {}",
                    output::escape_terminal_bytes(&value.as_bytes()[..composer.edit_cursor])
                ))
                .width()
                .saturating_sub(usize::from(rect.width.max(1)).saturating_sub(1))
                .min(u16::MAX as usize) as u16
            } else {
                0
            };
            frame.render_widget(
                Paragraph::new(shown)
                    .style(
                        Style::default()
                            .fg(if active { FOREGROUND } else { MUTED })
                            .bg(if active { SELECTED } else { RAISED }),
                    )
                    .scroll((0, scroll)),
                rect,
            );
            if active && rect.width > 0 {
                let cursor_column = Line::raw(format!(
                    " {}",
                    output::escape_terminal_bytes(&value.as_bytes()[..composer.edit_cursor])
                ))
                .width();
                frame.set_cursor_position(Position::new(
                    rect.x + cursor_column.saturating_sub(usize::from(scroll)) as u16,
                    rect.y,
                ));
            }
            hits.push(HitTarget {
                area: rect,
                action: MouseAction::PairCell(index * 2 + column),
            });
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_cells_preserve_duplicates_empty_values_and_delimiters() {
        let mut composer = Composer {
            url: "https://example.test".into(),
            focus: ComposerField::Query,
            ..Composer::default()
        };
        composer.begin_edit();
        composer.insert_text("tag");
        composer.select_pair_cell(1);
        composer.insert_text("a&b=c || 日");
        composer.add_pair();
        composer.insert_text("tag");
        composer.select_pair_cell(3);
        assert!(composer.commit_edit());
        let expected = vec![
            ("tag".into(), "a&b=c || 日".into()),
            ("tag".into(), String::new()),
        ];
        assert_eq!(composer.build_request().unwrap().query, expected);
        composer.begin_edit();
        assert_eq!(composer.pair_values().unwrap(), expected);
        composer.select_pair_cell(1);
        composer.insert_text("changed");
        composer.cancel_edit();
        assert_eq!(composer.build_request().unwrap().query, expected);
    }

    #[test]
    fn invalid_cells_block_save_and_removal_recovers() {
        let mut composer = Composer {
            focus: ComposerField::Headers,
            ..Composer::default()
        };
        composer.begin_edit();
        composer.add_pair();
        composer.insert_text("Bad Name");
        composer.select_pair_cell(3);
        composer.insert_text("value");
        assert!(!composer.commit_edit());
        assert!(composer.editing);
        assert!(composer.edit_error().unwrap().contains("Row 2"));
        composer.remove_pair();
        assert!(composer.commit_edit());
        assert_eq!(
            composer.header_pairs.unwrap(),
            vec![("Accept".into(), "application/json".into())]
        );
    }

    #[test]
    fn json_validation_format_and_newline_keep_edits_safe() {
        let mut composer = Composer {
            focus: ComposerField::Body,
            url: "https://example.test".into(),
            ..Composer::default()
        };
        composer.begin_edit();
        composer.insert_text("{bad}");
        assert!(!composer.commit_edit());
        assert!(composer.edit_error().unwrap().contains("line 1"));
        assert!(composer.body.is_empty());
        composer.edit_buffer = "{\"a\":1,\"ok\":true}".into();
        composer.format_json();
        assert!(composer.edit_buffer.contains('\n'));
        assert!(composer.commit_edit());
        assert_eq!(composer.method, "POST");
        composer.begin_edit();
        composer.edit_buffer = "{".into();
        composer.edit_cursor = 1;
        composer.insert_newline();
        assert_eq!(composer.edit_buffer, "{\n  ");
        composer.insert_text("\"name\": \"日\"\n}");
        assert!(composer.commit_edit());
        assert!(composer.build_request().is_ok());
    }

    #[test]
    fn json_newline_expands_matching_braces_with_nested_indent() {
        let mut composer = Composer {
            focus: ComposerField::Body,
            ..Composer::default()
        };

        composer.edit_buffer = "{}".into();
        composer.edit_cursor = 1;
        composer.insert_newline();
        assert_eq!(composer.edit_buffer, "{\n  \n}");
        assert_eq!(composer.edit_cursor, 4);

        composer.edit_buffer = "  \"items\": []".into();
        composer.edit_cursor = composer.edit_buffer.len() - 1;
        composer.insert_newline();
        assert_eq!(composer.edit_buffer, "  \"items\": [\n    \n  ]");
        assert_eq!(composer.edit_cursor, "  \"items\": [\n    ".len());
    }

    #[test]
    fn json_quotes_auto_pair_and_skip_the_closing_quote() {
        let mut composer = Composer {
            focus: ComposerField::Body,
            ..Composer::default()
        };

        composer.insert_smart_quote();
        assert_eq!(composer.edit_buffer, "\"\"");
        assert_eq!(composer.edit_cursor, 1);

        composer.insert_text("name");
        composer.insert_smart_quote();
        assert_eq!(composer.edit_buffer, "\"name\"");
        assert_eq!(composer.edit_cursor, composer.edit_buffer.len());

        composer.edit_buffer = "{\n  \n}".into();
        composer.edit_cursor = 4;
        composer.insert_smart_quote();
        assert_eq!(composer.edit_buffer, "{\n  \"\"\n}");
        assert_eq!(composer.edit_cursor, 5);

        composer.body_kind = BodyKind::Raw;
        composer.edit_buffer.clear();
        composer.edit_cursor = 0;
        composer.insert_smart_quote();
        assert_eq!(composer.edit_buffer, "\"");
    }

    #[test]
    fn json_brackets_auto_pair_and_skip_existing_closers() {
        let mut composer = Composer {
            focus: ComposerField::Body,
            ..Composer::default()
        };

        composer.insert_smart_json_delimiter('{');
        assert_eq!(composer.edit_buffer, "{}");
        assert_eq!(composer.edit_cursor, 1);
        composer.insert_smart_json_delimiter('}');
        assert_eq!(composer.edit_buffer, "{}");
        assert_eq!(composer.edit_cursor, 2);

        composer.edit_buffer.clear();
        composer.edit_cursor = 0;
        composer.insert_smart_json_delimiter('[');
        assert_eq!(composer.edit_buffer, "[]");
        assert_eq!(composer.edit_cursor, 1);
        composer.insert_smart_json_delimiter(']');
        assert_eq!(composer.edit_buffer, "[]");
        assert_eq!(composer.edit_cursor, 2);

        composer.edit_buffer = "\"\"".into();
        composer.edit_cursor = 1;
        composer.insert_smart_json_delimiter('{');
        assert_eq!(composer.edit_buffer, "\"{\"");
    }

    #[test]
    fn invalid_url_and_json_cannot_be_committed_or_sent() {
        let mut composer = Composer::default();
        composer.begin_edit();
        composer.insert_text("ftp://example.test");
        assert!(!composer.commit_edit());
        composer.edit_buffer = " https://example.test/path ".into();
        assert!(composer.commit_edit());
        assert_eq!(composer.url, "https://example.test/path");
        composer.body = "{broken".into();
        assert!(composer.build_request().is_err());
        composer.body_kind = BodyKind::Raw;
        assert!(composer.build_request().is_ok());
    }

    #[test]
    fn explicit_get_survives_adding_body_and_templates_do_not_overwrite() {
        let mut composer = Composer {
            focus: ComposerField::Body,
            method_explicit: true,
            ..Composer::default()
        };
        composer.begin_edit();
        composer.insert_json_template(false);
        assert!(composer.commit_edit());
        assert_eq!(composer.method, "GET");
        composer.begin_edit();
        let original = composer.edit_buffer.clone();
        composer.insert_json_template(true);
        assert_eq!(composer.edit_buffer, original);
    }

    #[test]
    fn row_editor_renders_clickable_cells_and_scrolls_to_active_row() {
        for (width, height) in [(120, 32), (80, 24), (48, 18)] {
            let mut app = App::new(vec![]);
            app.composer.focus = ComposerField::Query;
            app.composer.begin_edit();
            for index in 0..30 {
                app.composer.insert_text(&format!("key{index}"));
                app.composer.select_pair_cell(index * 2 + 1);
                app.composer.insert_text("a&b");
                app.composer.add_pair();
            }
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| super::super::draw(frame, &mut app))
                .unwrap();
            let cell = app.composer.pairs.as_ref().unwrap().cell;
            let target = app
                .hits
                .iter()
                .find(|hit| hit.action == MouseAction::PairCell(cell))
                .unwrap();
            assert!(target.area.y < height);
            assert!(
                app.hits
                    .iter()
                    .any(|hit| hit.action == MouseAction::AddPair)
            );
            assert!(app.hits.iter().all(|hit| !matches!(
                hit.action,
                MouseAction::Key('s') | MouseAction::Workspace(_)
            )));
        }
    }
}
