use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::model::options::{GroupKind, Provenance};
use crate::model::port::PortInfo;
use crate::session::UiStatus;

use super::{App, EditorRow, Focus};

pub fn draw(f: &mut Frame, app: &mut App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(f.area());
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(outer[0]);

    draw_port_list(f, app, panes[0]);
    draw_editor(f, app, panes[1]);
    draw_status_bar(f, app, outer[1]);
    if app.quit_confirm {
        draw_quit_confirm(f);
    } else if app.modal.is_some() {
        draw_apply_modal(f, app);
    }
}

fn status_marker(status: &UiStatus) -> (&'static str, Color) {
    match status {
        UiStatus::Conflict => ("✗", Color::Red),
        UiStatus::Edited => ("*", Color::Cyan),
        UiStatus::Unconfigured => ("?", Color::Yellow),
        UiStatus::Stale => ("!", Color::LightRed),
        UiStatus::Ok => ("✓", Color::DarkGray),
    }
}

fn draw_port_list(f: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app
        .visible
        .iter()
        .map(|key| {
            let info = &app.session.ports[key];
            let status = app.session.status(info);
            let (marker, color) = status_marker(&status);
            let mut spans = vec![
                Span::styled(format!("{marker} "), Style::default().fg(color)),
                Span::raw(key.to_string()),
            ];
            if info.broken.is_some() || info.ignore.is_some() {
                spans.push(Span::styled(" ⚠", Style::default().fg(Color::Red)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let title = if app.filter.is_empty() && app.focus != Focus::Filter {
        format!(" Ports ({}) ", app.visible.len())
    } else {
        format!(" Ports ({}) — filter: {}▏", app.visible.len(), app.filter)
    };
    let border_style = if app.focus == Focus::List || app.focus == Focus::Filter {
        Style::default().fg(Color::LightBlue)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title).border_style(border_style))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn draw_editor(f: &mut Frame, app: &App, area: Rect) {
    let border_style = if app.focus == Focus::Editor {
        Style::default().fg(Color::LightBlue)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let Some(key) = app.selected_key() else {
        let p = Paragraph::new("no port selected")
            .block(Block::default().borders(Borders::ALL).border_style(border_style));
        f.render_widget(p, area);
        return;
    };
    let info = &app.session.ports[&key];
    let title = format!(" {} — {} ", key, info.pkgname);

    let mut lines: Vec<Line> = Vec::new();
    banner_lines(app, info, &mut lines);

    let state = app.session.state(info);
    for (i, row) in app.editor_rows.iter().enumerate() {
        let selected = app.focus == Focus::Editor && i == app.editor_idx;
        match row {
            EditorRow::GroupHeader(gi) => {
                let g = &info.options.groups[*gi];
                let mut text = format!("── {} — {}", g.name, g.kind.label());
                if !g.desc.is_empty() {
                    text.push_str(&format!(": {}", g.desc));
                }
                lines.push(Line::from(Span::styled(
                    text,
                    Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                )));
            }
            EditorRow::Option(opt) => {
                lines.push(option_line(app, info, opt, selected));
            }
            EditorRow::ObsoleteHeader => {
                lines.push(Line::from(Span::styled(
                    "── obsolete (no longer exist; dropped on apply)",
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
                )));
            }
            EditorRow::Obsolete(opt) => {
                let was_on = state
                    .and_then(|s| s.saved.as_ref())
                    .map(|s| s.set.contains(opt))
                    .unwrap_or(false);
                lines.push(Line::from(Span::styled(
                    format!("  [{}] {}", if was_on { "x" } else { " " }, opt),
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::CROSSED_OUT),
                )));
            }
        }
    }

    // Keep the selected row in view: scroll so it sits inside the pane.
    let inner_height = area.height.saturating_sub(2) as usize;
    let selected_line = app
        .editor_rows
        .iter()
        .take(app.editor_idx + 1)
        .count()
        .saturating_add(banner_len(app, info))
        .saturating_sub(1);
    let scroll = selected_line.saturating_sub(inner_height.saturating_sub(1)) as u16;

    let p = Paragraph::new(lines)
        .scroll((scroll, 0))
        .block(Block::default().borders(Borders::ALL).title(title).border_style(border_style));
    f.render_widget(p, area);
}

fn banner_len(app: &App, info: &PortInfo) -> usize {
    let mut n = 0;
    if info.broken.is_some() {
        n += 1;
    }
    if info.ignore.is_some() {
        n += 1;
    }
    if info.deprecated.is_some() {
        n += 1;
    }
    n + app.session.violations(info).len()
}

fn banner_lines(app: &App, info: &PortInfo, lines: &mut Vec<Line<'static>>) {
    let red = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
    if let Some(m) = &info.broken {
        lines.push(Line::from(Span::styled(format!("BROKEN: {m}"), red)));
    }
    if let Some(m) = &info.ignore {
        lines.push(Line::from(Span::styled(format!("IGNORE: {m}"), red)));
    }
    if let Some(m) = &info.deprecated {
        lines.push(Line::from(Span::styled(
            format!("DEPRECATED: {m}"),
            Style::default().fg(Color::Magenta),
        )));
    }
    for v in app.session.violations(info) {
        lines.push(Line::from(Span::styled(format!("CONFLICT: {v}"), red)));
    }
}

fn option_line(app: &App, info: &PortInfo, opt: &str, selected: bool) -> Line<'static> {
    let opts = &info.options;
    let state = app.session.state(info);
    let on = state.map(|s| s.staged.contains(opt)).unwrap_or(false);
    let is_default = opts.defaults.contains(opt);
    let in_choice_group = opts
        .groups
        .iter()
        .any(|g| matches!(g.kind, GroupKind::Single | GroupKind::Radio) && g.members.iter().any(|m| m == opt));

    let checkbox = match (in_choice_group, on) {
        (true, true) => "(o)",
        (true, false) => "( )",
        (false, true) => "[x]",
        (false, false) => "[ ]",
    };

    let mut spans: Vec<Span> = Vec::new();
    let name_style = if on != is_default {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    let base = format!("  {checkbox} {opt:<20}");
    spans.push(Span::styled(
        base,
        if selected { name_style.add_modifier(Modifier::REVERSED) } else { name_style },
    ));

    // default value column
    spans.push(Span::styled(
        format!(" (def: {})", if is_default { "on" } else { "off" }),
        Style::default().fg(Color::DarkGray),
    ));

    // NEW badge: option unknown to the saved file (only meaningful when a file exists)
    if let Some(saved) = state.and_then(|s| s.saved.as_ref()) {
        let known = saved.complete.iter().chain(saved.set.iter()).chain(saved.unset.iter());
        if !known.into_iter().any(|o| o == opt) {
            spans.push(Span::styled(
                " NEW",
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ));
        }
    }

    // provenance badge
    let file_state = state
        .and_then(|s| s.saved.as_ref())
        .and_then(|s| {
            if s.set.contains(opt) {
                Some(true)
            } else if s.unset.contains(opt) {
                Some(false)
            } else {
                None
            }
        });
    match opts.provenance(opt, file_state) {
        Provenance::Forced => spans.push(Span::styled(
            " FORCED",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Provenance::MakeConfPort => {
            spans.push(Span::styled(" mc:port", Style::default().fg(Color::Green)))
        }
        Provenance::MakeConfGlobal => {
            spans.push(Span::styled(" mc", Style::default().fg(Color::Green)))
        }
        _ => {}
    }

    if let Some(by) = app.session.implied_by(info, opt) {
        if on {
            spans.push(Span::styled(
                format!(" implied by {by}"),
                Style::default().fg(Color::Cyan),
            ));
        }
    }

    if let Some(def) = opts.defs.get(opt) {
        if def.broken.is_some() {
            spans.push(Span::styled(" ⚠broken", Style::default().fg(Color::Red)));
        }
        if def.ignore.is_some() {
            spans.push(Span::styled(" ⚠ignored", Style::default().fg(Color::Red)));
        }
        if !def.desc.is_empty() {
            spans.push(Span::styled(
                format!("  {}", def.desc),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    Line::from(spans)
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let line = if let Some((msg, is_err)) = &app.message {
        let style = if *is_err {
            Style::default().fg(Color::Black).bg(Color::LightRed)
        } else {
            Style::default().fg(Color::Black).bg(Color::LightGreen)
        };
        Line::from(Span::styled(format!(" {msg} "), style))
    } else {
        let mut counts = [0usize; 5];
        for key in &app.visible {
            let idx = match app.session.status(&app.session.ports[key]) {
                UiStatus::Conflict => 0,
                UiStatus::Edited => 1,
                UiStatus::Unconfigured => 2,
                UiStatus::Stale => 3,
                UiStatus::Ok => 4,
            };
            counts[idx] += 1;
        }
        Line::from(vec![
            Span::styled(
                format!(
                    " {}✗ {}* {}? {}! {}✓ (+{} optionless) ",
                    counts[0], counts[1], counts[2], counts[3], counts[4], app.hidden
                ),
                Style::default().fg(Color::White),
            ),
            Span::styled(
                " Space:toggle d:defaults u:revert n/p:next-problem /:filter a:apply q:quit",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    };
    f.render_widget(Paragraph::new(line), area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(v[1])[1]
}

fn draw_apply_modal(f: &mut Frame, app: &App) {
    let Some(modal) = &app.modal else { return };
    let area = centered_rect(80, 70, f.area());
    f.render_widget(Clear, area);

    let mut lines: Vec<Line> = Vec::new();
    if let Some(done) = &modal.done {
        for l in done.lines() {
            lines.push(Line::from(l.to_string()));
        }
        lines.push(Line::from(Span::styled(
            "press any key to close",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        if !modal.conflicted.is_empty() {
            lines.push(Line::from(Span::styled(
                format!(
                    "⚠ {} port(s) still have conflicting options: {}",
                    modal.conflicted.len(),
                    modal
                        .conflicted
                        .iter()
                        .take(5)
                        .map(|k| k.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )));
        }
        lines.push(Line::from(format!(
            "{} options file(s) will be written to {}:",
            modal.writes.len(),
            app.options_dir.display()
        )));
        lines.push(Line::default());
        for w in modal.writes.iter().skip(modal.scroll) {
            lines.push(Line::from(vec![
                Span::styled(format!("{:<40}", w.key.to_string()), Style::default().fg(Color::White)),
                Span::styled(w.describe(), Style::default().fg(Color::DarkGray)),
            ]));
        }
    }

    let title = if modal.done.is_some() { " apply — done " } else { " apply? y/n (j/k scroll) " };
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title(title).border_style(Style::default().fg(Color::LightYellow)));
    f.render_widget(p, area);
}

fn draw_quit_confirm(f: &mut Frame) {
    let area = centered_rect(50, 20, f.area());
    f.render_widget(Clear, area);
    let p = Paragraph::new("Unsaved staged changes — quit anyway? (y/N)")
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title(" quit ").border_style(Style::default().fg(Color::Red)));
    f.render_widget(p, area);
}
