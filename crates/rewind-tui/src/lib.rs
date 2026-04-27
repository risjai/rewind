use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{
    prelude::*,
    widgets::*,
};
use rewind_replay::ReplayEngine;
use rewind_store::{Step, StepStatus, Store};
use std::io::stdout;

pub struct TuiApp {
    store: Store,
    session_id: String,
    #[allow(dead_code)]
    timeline_id: String,
    steps: Vec<Step>,
    selected_step: usize,
    request_scroll: u16,
    response_scroll: u16,
    panel: Panel,
}

#[derive(PartialEq)]
enum Panel {
    Timeline,
    Request,
    Response,
}

impl TuiApp {
    pub fn new(store: Store, session_id: &str, timeline_id: &str) -> Result<Self> {
        let engine = ReplayEngine::new(&store);
        let steps = engine.get_full_timeline_steps(timeline_id, session_id)?;

        Ok(TuiApp {
            store,
            session_id: session_id.to_string(),
            timeline_id: timeline_id.to_string(),
            steps,
            selected_step: 0,
            request_scroll: 0,
            response_scroll: 0,
            panel: Panel::Timeline,
        })
    }

    pub fn run(&mut self) -> Result<()> {
        enable_raw_mode()?;
        stdout().execute(EnterAlternateScreen)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

        loop {
            terminal.draw(|frame| self.draw(frame))?;

            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Up | KeyCode::Char('k') => match self.panel {
                        Panel::Timeline => {
                            if self.selected_step > 0 {
                                self.selected_step -= 1;
                                self.request_scroll = 0;
                                self.response_scroll = 0;
                            }
                        }
                        Panel::Request => {
                            self.request_scroll = self.request_scroll.saturating_sub(3);
                        }
                        Panel::Response => {
                            self.response_scroll = self.response_scroll.saturating_sub(3);
                        }
                    },
                    KeyCode::Down | KeyCode::Char('j') => match self.panel {
                        Panel::Timeline => {
                            if self.selected_step < self.steps.len().saturating_sub(1) {
                                self.selected_step += 1;
                                self.request_scroll = 0;
                                self.response_scroll = 0;
                            }
                        }
                        Panel::Request => {
                            self.request_scroll += 3;
                        }
                        Panel::Response => {
                            self.response_scroll += 3;
                        }
                    },
                    KeyCode::Tab => {
                        self.panel = match self.panel {
                            Panel::Timeline => Panel::Request,
                            Panel::Request => Panel::Response,
                            Panel::Response => Panel::Timeline,
                        };
                    }
                    KeyCode::Home => {
                        self.selected_step = 0;
                        self.request_scroll = 0;
                        self.response_scroll = 0;
                    }
                    KeyCode::End => {
                        self.selected_step = self.steps.len().saturating_sub(1);
                        self.request_scroll = 0;
                        self.response_scroll = 0;
                    }
                    _ => {}
                }
            }
        }

        disable_raw_mode()?;
        stdout().execute(LeaveAlternateScreen)?;
        Ok(())
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();

        // Main layout: header + body + footer
        let main_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // header
                Constraint::Min(10),   // body
                Constraint::Length(3), // footer / stats bar
            ])
            .split(area);

        self.draw_header(frame, main_layout[0]);
        self.draw_body(frame, main_layout[1]);
        self.draw_footer(frame, main_layout[2]);
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let session_name = self.store.get_session(&self.session_id)
            .ok()
            .flatten()
            .map(|s| s.name)
            .unwrap_or_else(|| "Unknown".into());

        let total_tokens: u64 = self.steps.iter().map(|s| s.tokens_in + s.tokens_out).sum();
        let error_count = self.steps.iter().filter(|s| s.status == StepStatus::Error).count();

        let header_text = vec![
            Line::from(vec![
                Span::styled("⏪ REWIND ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled("│ ", Style::default().fg(Color::DarkGray)),
                Span::styled(&session_name, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{} steps", self.steps.len()), Style::default().fg(Color::Yellow)),
                Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{} tokens", total_tokens), Style::default().fg(Color::Blue)),
                if error_count > 0 {
                    Span::styled(format!(" │ {} errors", error_count), Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
                } else {
                    Span::styled(" │ no errors", Style::default().fg(Color::Green))
                },
            ]),
        ];

        let header = Paragraph::new(header_text)
            .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(header, area);
    }

    fn draw_body(&self, frame: &mut Frame, area: Rect) {
        // Split: timeline (left 35%) | detail (right 65%)
        let body_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(35),
                Constraint::Percentage(65),
            ])
            .split(area);

        self.draw_timeline_panel(frame, body_layout[0]);
        self.draw_detail_panel(frame, body_layout[1]);
    }

    fn draw_timeline_panel(&self, frame: &mut Frame, area: Rect) {
        let border_style = if self.panel == Panel::Timeline {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let items: Vec<ListItem> = self.steps.iter().enumerate().map(|(i, step)| {
            let icon = step.step_type.icon();
            let status_icon = match step.status {
                StepStatus::Success => "✓",
                StepStatus::Error => "✗",
                StepStatus::Pending => "…",
            };
            let status_color = match step.status {
                StepStatus::Success => Color::Green,
                StepStatus::Error => Color::Red,
                StepStatus::Pending => Color::Yellow,
            };

            // Build the connector line for the timeline
            let connector = if i == 0 { "┌" } else if i == self.steps.len() - 1 { "└" } else { "├" };

            let line = Line::from(vec![
                Span::styled(format!(" {} ", connector), Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{}  ", icon), Style::default()),
                Span::styled(
                    format!("Step {:>2}", step.step_number),
                    if i == self.selected_step {
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    },
                ),
                Span::styled("  ", Style::default()),
                Span::styled(status_icon, Style::default().fg(status_color).add_modifier(Modifier::BOLD)),
                Span::styled("  ", Style::default()),
                Span::styled(
                    format!("{:.4}s", step.duration_ms as f64 / 1000.0),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled("  ", Style::default()),
                Span::styled(
                    format!("{}tok", step.tokens_in + step.tokens_out),
                    Style::default().fg(Color::Blue),
                ),
            ]);

            ListItem::new(line)
        }).collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title(Span::styled(" Timeline ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
                    .borders(Borders::ALL)
                    .border_style(border_style),
            )
            .highlight_style(Style::default().bg(Color::DarkGray))
            .highlight_symbol("▶ ");

        let mut list_state = ListState::default();
        list_state.select(Some(self.selected_step));
        frame.render_stateful_widget(list, area, &mut list_state);
    }

    fn draw_detail_panel(&self, frame: &mut Frame, area: Rect) {
        if self.steps.is_empty() {
            let empty = Paragraph::new("No steps recorded yet.")
                .block(Block::default().title(" Detail ").borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
            frame.render_widget(empty, area);
            return;
        }

        let step = &self.steps[self.selected_step];

        // Split detail area: info bar + request + response
        let detail_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),  // step info
                Constraint::Percentage(40), // request / context
                Constraint::Percentage(60), // response
            ])
            .split(area);

        // Step info bar
        self.draw_step_info(frame, detail_layout[0], step);

        // Request panel (the context window)
        self.draw_request_panel(frame, detail_layout[1], step);

        // Response panel
        self.draw_response_panel(frame, detail_layout[2], step);
    }

    fn draw_step_info(&self, frame: &mut Frame, area: Rect, step: &Step) {
        let status_color = match step.status {
            StepStatus::Success => Color::Green,
            StepStatus::Error => Color::Red,
            StepStatus::Pending => Color::Yellow,
        };

        let info = Paragraph::new(vec![
            Line::from(vec![
                Span::styled(format!(" {} ", step.step_type.icon()), Style::default()),
                Span::styled(step.step_type.label(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
                Span::styled(step.status.as_str(), Style::default().fg(status_color).add_modifier(Modifier::BOLD)),
                Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
                Span::styled(&step.model, Style::default().fg(Color::Magenta)),
            ]),
            Line::from(vec![
                Span::styled(
                    format!(" ↓{}tok ↑{}tok", step.tokens_in, step.tokens_out),
                    Style::default().fg(Color::Blue),
                ),
                Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:.0}ms", step.duration_ms), Style::default().fg(Color::Yellow)),
                if let Some(ref err) = step.error {
                    Span::styled(format!(" │ {}", &err[..err.len().min(60)]), Style::default().fg(Color::Red))
                } else {
                    Span::raw("")
                },
            ]),
        ])
        .block(
            Block::default()
                .title(Span::styled(
                    format!(" Step {} ", step.step_number),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        frame.render_widget(info, area);
    }

    fn draw_request_panel(&self, frame: &mut Frame, area: Rect, step: &Step) {
        let content = self.format_blob_for_display(&step.request_blob, true);
        let focused = self.panel == Panel::Request;
        let border_style = if focused {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let para = Paragraph::new(content)
            .block(
                Block::default()
                    .title(Span::styled(" Request / Context Window ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)))
                    .borders(Borders::ALL)
                    .border_style(border_style),
            )
            .wrap(Wrap { trim: false })
            .scroll((self.request_scroll, 0));
        frame.render_widget(para, area);
    }

    fn draw_response_panel(&self, frame: &mut Frame, area: Rect, step: &Step) {
        // Step 0.3 (Phase 0 follow-up): use the envelope-aware helper
        // so v0.13+ proxy-recorded steps unwrap to the inner model
        // response. Pre-migration format=0 round-trips unchanged.
        let content = self.format_response_blob_for_display(step);
        let focused = self.panel == Panel::Response;
        let border_style = if focused {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let para = Paragraph::new(content)
            .block(
                Block::default()
                    .title(Span::styled(" Response ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)))
                    .borders(Borders::ALL)
                    .border_style(border_style),
            )
            .wrap(Wrap { trim: false })
            .scroll((self.response_scroll, 0));
        frame.render_widget(para, area);
    }

    fn format_blob_for_display(&self, blob_hash: &str, is_request: bool) -> Vec<Line<'static>> {
        // Request path: blobs are always naked JSON (no envelope), so a
        // direct read is correct. Response path: callers MUST go through
        // `format_response_blob_for_display` for envelope-aware unwrap.
        // The `is_request: false` branch is preserved for backward compat
        // with any caller that hasn't been migrated, but it would break on
        // v0.13+ envelope blobs — explicit assertion documents the expectation.
        debug_assert!(
            is_request,
            "format_blob_for_display(is_request=false) is unsafe for v0.13+ \
             envelope blobs; use format_response_blob_for_display(step) instead",
        );
        self.format_blob_bytes_for_display(blob_hash.is_empty(), || {
            self.store.blobs.get(blob_hash).ok().map(|d| d.to_vec())
        }, is_request)
    }

    /// Step 0.3 (Phase 0 follow-up): envelope-aware response display.
    ///
    /// Routes through `Store::read_step_response_body` so the inner
    /// model response is what gets formatted, not the envelope wrapper
    /// JSON. Pre-migration format=0 blobs read identically to today
    /// via the legacy fallback in `ResponseEnvelope::from_blob_bytes`.
    fn format_response_blob_for_display(&self, step: &Step) -> Vec<Line<'static>> {
        self.format_blob_bytes_for_display(
            step.response_blob.is_empty(),
            || self.store.read_step_response_body(step),
            false,
        )
    }

    fn format_blob_bytes_for_display<F>(
        &self,
        is_empty: bool,
        fetch: F,
        is_request: bool,
    ) -> Vec<Line<'static>>
    where
        F: FnOnce() -> Option<Vec<u8>>,
    {
        if is_empty {
            return vec![Line::from(Span::styled("(empty)", Style::default().fg(Color::DarkGray)))];
        }
        let Some(data) = fetch() else {
            return vec![Line::from(Span::styled("(blob not found)", Style::default().fg(Color::Red)))];
        };
        let json_str = match String::from_utf8(data) {
            Ok(s) => s,
            Err(_) => return vec![Line::from(Span::styled("(binary data)", Style::default().fg(Color::DarkGray)))],
        };
        let val: serde_json::Value = match serde_json::from_str(&json_str) {
            Ok(v) => v,
            Err(_) => return vec![Line::from(Span::raw(json_str))],
        };
        if is_request {
            self.format_request_json(&val)
        } else {
            self.format_response_json(&val)
        }
    }

    fn format_request_json(&self, val: &serde_json::Value) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        // Show model
        if let Some(model) = val.get("model").and_then(|m| m.as_str()) {
            lines.push(Line::from(vec![
                Span::styled("model: ", Style::default().fg(Color::DarkGray)),
                Span::styled(model.to_string(), Style::default().fg(Color::Magenta)),
            ]));
        }

        // Show messages (the context window)
        if let Some(messages) = val.get("messages").and_then(|m| m.as_array()) {
            lines.push(Line::from(Span::styled(
                format!("─── Messages ({}) ───", messages.len()),
                Style::default().fg(Color::DarkGray),
            )));

            for msg in messages {
                let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("?");
                let role_color = match role {
                    "system" => Color::Magenta,
                    "user" => Color::Cyan,
                    "assistant" => Color::Green,
                    "tool" => Color::Yellow,
                    _ => Color::White,
                };

                lines.push(Line::from(vec![
                    Span::styled(format!("[{}]", role), Style::default().fg(role_color).add_modifier(Modifier::BOLD)),
                ]));

                // Handle string content
                if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                    for text_line in content.lines() {
                        lines.push(Line::from(style_content_line(text_line)));
                    }
                }

                // Handle array content (Anthropic format)
                if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                    for block in content {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            for text_line in text.lines() {
                                lines.push(Line::from(Span::styled(
                                    format!("  {}", text_line),
                                    Style::default().fg(Color::White),
                                )));
                            }
                        }
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                            let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                            lines.push(Line::from(Span::styled(
                                format!("  🔧 tool_use: {}", name),
                                Style::default().fg(Color::Yellow),
                            )));
                        }
                    }
                }

                // Handle tool_calls (OpenAI format)
                if let Some(tool_calls) = msg.get("tool_calls").and_then(|t| t.as_array()) {
                    for tc in tool_calls {
                        let name = tc.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str()).unwrap_or("?");
                        lines.push(Line::from(Span::styled(
                            format!("  🔧 call: {}", name),
                            Style::default().fg(Color::Yellow),
                        )));
                    }
                }

                lines.push(Line::from(""));
            }
        }

        // Show tools if present
        if let Some(tools) = val.get("tools").and_then(|t| t.as_array()) {
            lines.push(Line::from(Span::styled(
                format!("─── Tools ({}) ───", tools.len()),
                Style::default().fg(Color::DarkGray),
            )));
            for tool in tools {
                let name = tool.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str())
                    .or_else(|| tool.get("name").and_then(|n| n.as_str()))
                    .unwrap_or("?");
                lines.push(Line::from(Span::styled(
                    format!("  🔧 {}", name),
                    Style::default().fg(Color::Yellow),
                )));
            }
        }

        if lines.is_empty() {
            // Fallback: pretty print JSON
            let pretty = serde_json::to_string_pretty(val).unwrap_or_default();
            for line in pretty.lines() {
                lines.push(Line::from(Span::styled(line.to_string(), Style::default().fg(Color::White))));
            }
        }

        lines
    }

    fn format_response_json(&self, val: &serde_json::Value) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        // OpenAI format
        if let Some(choices) = val.get("choices").and_then(|c| c.as_array()) {
            for choice in choices {
                if let Some(msg) = choice.get("message") {
                    let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("assistant");
                    lines.push(Line::from(Span::styled(
                        format!("[{}]", role),
                        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                    )));

                    if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                        for text_line in content.lines() {
                            lines.push(Line::from(style_content_line(text_line)));
                        }
                    }

                    if let Some(tool_calls) = msg.get("tool_calls").and_then(|t| t.as_array()) {
                        lines.push(Line::from(Span::styled(
                            "  ── Tool Calls ──",
                            Style::default().fg(Color::Yellow),
                        )));
                        for tc in tool_calls {
                            let name = tc.get("function").and_then(|f| f.get("name")).and_then(|n| n.as_str()).unwrap_or("?");
                            let args = tc.get("function").and_then(|f| f.get("arguments")).and_then(|a| a.as_str()).unwrap_or("{}");
                            lines.push(Line::from(Span::styled(
                                format!("  🔧 {}({})", name, &args[..args.len().min(80)]),
                                Style::default().fg(Color::Yellow),
                            )));
                        }
                    }
                }

                let finish = choice.get("finish_reason").and_then(|f| f.as_str()).unwrap_or("?");
                lines.push(Line::from(Span::styled(
                    format!("  finish_reason: {}", finish),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }

        // Anthropic format
        if let Some(content) = val.get("content").and_then(|c| c.as_array()) {
            for block in content {
                let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("?");
                match block_type {
                    "text" => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            lines.push(Line::from(Span::styled(
                                "[assistant]",
                                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                            )));
                            for text_line in text.lines() {
                                lines.push(Line::from(style_content_line(text_line)));
                            }
                        }
                    }
                    "tool_use" => {
                        let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                        lines.push(Line::from(Span::styled(
                            format!("  🔧 {}", name),
                            Style::default().fg(Color::Yellow),
                        )));
                    }
                    _ => {}
                }
            }
        }

        // Usage info
        if let Some(usage) = val.get("usage") {
            lines.push(Line::from(""));
            let prompt = usage.get("prompt_tokens").or(usage.get("input_tokens"))
                .and_then(|v| v.as_u64()).unwrap_or(0);
            let completion = usage.get("completion_tokens").or(usage.get("output_tokens"))
                .and_then(|v| v.as_u64()).unwrap_or(0);
            lines.push(Line::from(Span::styled(
                format!("  tokens: {}↓ {}↑ = {} total", prompt, completion, prompt + completion),
                Style::default().fg(Color::Blue),
            )));
        }

        if lines.is_empty() {
            let pretty = serde_json::to_string_pretty(val).unwrap_or_default();
            for line in pretty.lines() {
                lines.push(Line::from(Span::styled(line.to_string(), Style::default().fg(Color::White))));
            }
        }

        lines
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        let help = Paragraph::new(Line::from(vec![
            Span::styled(" ↑↓/jk ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled("Navigate  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Tab ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled("Switch panel  ", Style::default().fg(Color::DarkGray)),
            Span::styled("Home/End ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled("First/Last  ", Style::default().fg(Color::DarkGray)),
            Span::styled("q ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::styled("Quit  ", Style::default().fg(Color::DarkGray)),
            Span::styled("│ ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("Step {}/{}", self.selected_step + 1, self.steps.len()),
                Style::default().fg(Color::Yellow),
            ),
        ]))
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::DarkGray)));
        frame.render_widget(help, area);
    }
}

/// Style a content line — highlight error keywords in red.
fn style_content_line(text: &str) -> Vec<Span<'static>> {
    let error_keywords = ["ERROR", "HALLUCINATION", "FAIL", "FATAL", "WARN"];
    let line = format!("  {}", text);

    // Check if line contains any error keyword (case-insensitive for some)
    let upper = line.to_uppercase();
    let has_error = error_keywords.iter().any(|kw| upper.contains(kw));

    if has_error {
        vec![Span::styled(line, Style::default().fg(Color::Red))]
    } else {
        vec![Span::styled(line, Style::default().fg(Color::White))]
    }
}
