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
    } else if app.bulk.is_some() {
        draw_bulk(f, app);
    } else if let Some(tab) = app.help_tab {
        draw_help(f, tab, &mut app.help_scroll);
    } else if app.why.is_some() {
        draw_why(f, app);
    } else if app.opt_info.is_some() {
        draw_opt_info(f, app);
    }
}

fn status_marker(status: &UiStatus) -> (&'static str, Color) {
    match status {
        UiStatus::Conflict => ("✗", Color::Red),
        UiStatus::Edited => ("*", Color::Cyan),
        UiStatus::Unconfigured => ("?", Color::Yellow),
        UiStatus::Stale => ("!", Color::LightRed),
        UiStatus::McDeviation => ("≠", Color::Magenta),
        UiStatus::Ok => ("✓", Color::DarkGray),
    }
}

fn draw_port_list(f: &mut Frame, app: &mut App, area: Rect) {
    let items: Vec<ListItem> = app
        .visible
        .iter()
        .map(|key| {
            let info = &app.session.ports[key];
            let raw = app.session.status(info);
            let status = app.effective_status(info);
            let blacklisted = app.is_blacklisted(info);
            // A port silenced by the mc_relax view keeps a hint marker — but
            // a blacklisted one is silenced for its own reason (the ⊘ below).
            let (marker, color) = if status == UiStatus::Ok && raw != UiStatus::Ok && !blacklisted {
                ("≈", Color::DarkGray)
            } else {
                status_marker(&status)
            };
            // The port name itself carries the severity: red bold for a port
            // that will not build (BROKEN/IGNORE), red for conflicting staged
            // options, dim for blacklisted ports.
            let blocked = info.broken.is_some() || info.ignore.is_some();
            let name_style = if blacklisted {
                Style::default().fg(Color::DarkGray)
            } else if blocked {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else if status == UiStatus::Conflict {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            let mut spans = vec![
                Span::styled(format!("{marker} "), Style::default().fg(color)),
                Span::styled(key.to_string(), name_style),
            ];
            if blacklisted {
                spans.push(Span::styled(" ⊘", Style::default().fg(Color::DarkGray)));
            }
            if blocked {
                spans.push(Span::styled(" ⚠", Style::default().fg(Color::Red)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let count = if app.hide_ok {
        format!("{}/{}", app.visible.len(), app.listable)
    } else {
        format!("{}", app.visible.len())
    };
    let mut title = format!(" Ports ({count})");
    if app.hide_ok {
        title.push_str(" — needs attention");
    }
    if !app.filter.is_empty() || app.focus == Focus::Filter {
        title.push_str(&format!(" — filter: {}▏", app.filter));
    }
    title.push(' ');
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
    let title = match flavor_summary(app, info) {
        Some(flavors) => format!(" {} — {} · flavors: {} ", key, info.pkgname, flavors),
        None => format!(" {} — {} ", key, info.pkgname),
    };

    let mut lines: Vec<Line> = Vec::new();
    banner_lines(app, info, &mut lines);

    // Align the name column to the longest option of this port.
    let name_width = app
        .editor_rows
        .iter()
        .filter_map(|r| match r {
            EditorRow::Option(o) => Some(o.len()),
            _ => None,
        })
        .max()
        .unwrap_or(12)
        .clamp(12, 28);

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
                lines.push(option_line(app, info, opt, selected, name_width));
            }
            EditorRow::ExcludedHeader => {
                lines.push(Line::from(Span::styled(
                    "── not in this flavor (managed via the default flavor)",
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
                )));
            }
            EditorRow::Excluded(opt) => {
                let on = state.map(|s| s.staged.contains(opt)).unwrap_or(false);
                lines.push(Line::from(Span::styled(
                    format!("  [{}] {}", if on { "x" } else { " " }, opt),
                    Style::default().fg(Color::DarkGray),
                )));
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

/// The flavors of this port, marking the current one (`‹`) and the one whose
/// view owns the shared options file (`*`). None when the port is unflavored.
fn flavor_summary(app: &App, info: &PortInfo) -> Option<String> {
    if info.flavors.is_empty() {
        return None;
    }
    let owner = app.session.owner_info(info).canonical.flavor.clone();
    let current = info.canonical.flavor.clone();
    let marked: Vec<String> = info
        .flavors
        .iter()
        .map(|f| {
            let mut s = f.clone();
            if owner.as_deref() == Some(f.as_str()) {
                s.push('*');
            }
            if current.as_deref() == Some(f.as_str()) {
                s.push('‹');
            }
            s
        })
        .collect();
    Some(marked.join(" "))
}

/// The port owning this one's options file, when that is a *different* port —
/// i.e. the options are edited through another flavor's view.
fn foreign_owner<'a>(app: &'a App, info: &'a PortInfo) -> Option<&'a PortInfo> {
    let owner = app.session.owner_info(info);
    (owner.canonical != info.canonical).then_some(owner)
}

/// The option changes blamed for this port's BROKEN/IGNORE state, when it has
/// one and a background refresh attributed it.
fn blame_of<'a>(app: &'a App, info: &PortInfo) -> Option<&'a String> {
    if !crate::session::is_blocked(info) {
        return None;
    }
    app.blame.get(&info.options_name)
}

fn banner_len(app: &App, info: &PortInfo) -> usize {
    let mut n = 0;
    if info.broken.is_some() {
        n += 1;
    }
    if info.ignore.is_some() {
        n += 1;
    }
    if blame_of(app, info).is_some() {
        n += 1;
    }
    if info.deprecated.is_some() {
        n += 1;
    }
    if foreign_owner(app, info).is_some() {
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
    if let Some(blame) = blame_of(app, info) {
        lines.push(Line::from(Span::styled(
            format!("likely cause: {blame}"),
            Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
        )));
    }
    if let Some(m) = &info.deprecated {
        lines.push(Line::from(Span::styled(
            format!("DEPRECATED: {m}"),
            Style::default().fg(Color::Magenta),
        )));
    }
    if let Some(owner) = foreign_owner(app, info) {
        lines.push(Line::from(Span::styled(
            format!("options file owned by {} (default flavor view)", owner.canonical),
            Style::default().fg(Color::DarkGray),
        )));
    }
    for v in app.session.violations(info) {
        lines.push(Line::from(Span::styled(format!("CONFLICT: {v}"), red)));
    }
}

/// The option's value as recorded in the saved options file, if it mentions it
/// at all — the `file_state` argument `PortOptions::provenance` expects.
fn file_state(app: &App, info: &PortInfo, opt: &str) -> Option<bool> {
    let saved = app.session.state(info)?.saved.as_ref()?;
    if saved.set.contains(opt) {
        Some(true)
    } else if saved.unset.contains(opt) {
        Some(false)
    } else {
        None
    }
}

fn option_line(
    app: &App,
    info: &PortInfo,
    opt: &str,
    selected: bool,
    name_width: usize,
) -> Line<'static> {
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
    // Magenta: contradicts make.conf policy; yellow: deviates from the
    // port default; plain: matches the port default.
    let deviates_mc = app.session.mc_deviates(info, opt);
    let name_style = if deviates_mc {
        Style::default().fg(Color::Magenta)
    } else if on != is_default {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    let base = format!("  {checkbox} {opt:<name_width$}");
    spans.push(Span::styled(
        base,
        if selected { name_style.add_modifier(Modifier::REVERSED) } else { name_style },
    ));

    // default value column (fixed width so following badges line up)
    spans.push(Span::styled(
        format!(" def:{:<3}", if is_default { "on" } else { "off" }),
        Style::default().fg(Color::DarkGray),
    ));

    // NEW badge column: option unknown to the saved file
    let is_new = state
        .and_then(|s| s.saved.as_ref())
        .map(|saved| {
            !saved
                .complete
                .iter()
                .chain(saved.set.iter())
                .chain(saved.unset.iter())
                .any(|o| o == opt)
        })
        .unwrap_or(false);
    spans.push(Span::styled(
        if is_new { " NEW" } else { "    " },
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    ));

    // provenance column (fixed width)
    let (prov, prov_style) = match opts.provenance(opt, file_state(app, info, opt)) {
        Provenance::Forced => ("FORCED ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        Provenance::MakeConfPort => ("mc:port", Style::default().fg(Color::Green)),
        Provenance::MakeConfGlobal => ("mc     ", Style::default().fg(Color::Green)),
        _ => ("       ", Style::default()),
    };
    spans.push(Span::styled(format!(" {prov}"), prov_style));

    if deviates_mc {
        spans.push(Span::styled(
            " ≠mc",
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        ));
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
        let mut counts = [0usize; 6];
        for key in &app.visible {
            let idx = match app.effective_status(&app.session.ports[key]) {
                UiStatus::Conflict => 0,
                UiStatus::Edited => 1,
                UiStatus::Unconfigured => 2,
                UiStatus::Stale => 3,
                UiStatus::McDeviation => 4,
                UiStatus::Ok => 5,
            };
            counts[idx] += 1;
        }
        let mc = if app.warn_mc { format!(" {}≠", counts[4]) } else { String::new() };
        let mut spans = vec![Span::styled(
            format!(
                " {}✗ {}* {}? {}!{mc} {}✓ (+{} optionless) ",
                counts[0], counts[1], counts[2], counts[3], counts[5], app.hidden
            ),
            Style::default().fg(Color::White),
        )];
        if app.refreshing > 0 {
            let progress = app
                .refresh_progress
                .map(|(d, t)| format!(" ⟳ refreshing deps {d}/{t} "))
                .unwrap_or_else(|| " ⟳ refreshing deps… ".to_string());
            spans.push(Span::styled(
                progress,
                Style::default().fg(Color::Black).bg(Color::Yellow),
            ));
        }
        spans.push(Span::styled(
            " Space:toggle n/p:problems t:attention m:mc-ok w:mc-warn /:filter a:apply ?:help q:quit",
            Style::default().fg(Color::DarkGray),
        ));
        Line::from(spans)
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
        for w in &modal.warnings {
            lines.push(Line::from(Span::styled(
                format!("note: {w}"),
                Style::default().fg(Color::Magenta),
            )));
        }
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

/// Help overlay tab titles; indices match `App.help_tab`.
pub const HELP_TABS: [&str; 4] = ["Markers", "Option row", "Navigate/Edit", "Views/Actions"];

fn draw_help(f: &mut Frame, tab: usize, scroll: &mut u16) {
    let area = centered_rect(78, 72, f.area());
    f.render_widget(Clear, area);

    let key = |k: &str| Span::styled(format!("{k:<12}"), Style::default().fg(Color::LightBlue));
    let keyline = |k: &str, txt: &str| {
        Line::from(vec![Span::raw("  "), key(k), Span::raw(txt.to_string())])
    };
    let mark = |m: &str, c: Color, txt: &str| {
        Line::from(vec![
            Span::styled(format!("  {m:<3}"), Style::default().fg(c)),
            Span::raw(txt.to_string()),
        ])
    };
    // Name-color legend: the label rendered in its actual style, aligned on '='.
    let legend = |label: &str, style: Style, txt: &str| {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{label:>14}"), style),
            Span::styled(" = ", Style::default().fg(Color::DarkGray)),
            Span::raw(txt.to_string()),
        ])
    };

    // Tab bar with the number hotkeys.
    let mut bar: Vec<Span> = vec![Span::raw(" ")];
    for (i, name) in HELP_TABS.iter().enumerate() {
        let label = format!(" {}:{} ", i + 1, name);
        bar.push(if i == tab {
            Span::styled(label, Style::default().fg(Color::Black).bg(Color::LightBlue))
        } else {
            Span::styled(label, Style::default().fg(Color::DarkGray))
        });
        bar.push(Span::raw(" "));
    }

    let head = |t: &str| {
        Line::from(Span::styled(
            format!(" {t}"),
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        ))
    };
    let mut lines: Vec<Line> = vec![Line::from(bar), Line::default()];
    match tab {
        0 => lines.extend([
            head("Status column"),
            mark("✗", Color::Red, "conflict     staged options violate PREVENTS or group rules"),
            mark("*", Color::Cyan, "edited       staged changes not yet applied"),
            mark("?", Color::Yellow, "unconfigured no saved options file"),
            mark("!", Color::LightRed, "stale        option list changed since the file was written"),
            mark("≠", Color::Magenta, "mc-conflict  contradicts make.conf policy (w view)"),
            mark("≈", Color::DarkGray, "mc-covered   decided by make.conf, no attention (m view)"),
            mark("✓", Color::DarkGray, "ok"),
            Line::default(),
            head("After the port name"),
            mark("⚠", Color::Red, "port is BROKEN/IGNORE with the current options"),
            mark("⊘", Color::DarkGray, "blacklisted for this jail/tree/set (never needs attention)"),
            Line::default(),
            head("Port name color"),
            legend(
                "red bold name",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                "will not build (BROKEN/IGNORE)",
            ),
            legend("red name", Style::default().fg(Color::Red), "conflicting staged options"),
            legend("dim name", Style::default().fg(Color::DarkGray), "blacklisted"),
        ]),
        1 => {
            // A realistic sample row, styled exactly like the editor renders it.
            lines.push(head("A row, piece by piece"));
            lines.push(Line::from(vec![
                Span::styled("   [x] ", Style::default()),
                Span::styled("OPENSSL     ", Style::default().fg(Color::Yellow)),
                Span::styled(" def:off", Style::default().fg(Color::DarkGray)),
                Span::styled(" NEW", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(" mc     ", Style::default().fg(Color::Green)),
                Span::styled("  Use OpenSSL", Style::default().fg(Color::DarkGray)),
            ]));
            lines.push(Line::default());
            for (label, style, txt) in [
                ("[x] / [ ]", Style::default(), "checkbox of a plain option (on / off)"),
                ("(o) / ( )", Style::default(), "single/radio group member (pick one)"),
                (
                    "yellow name",
                    Style::default().fg(Color::Yellow),
                    "differs from the port default",
                ),
                (
                    "magenta name",
                    Style::default().fg(Color::Magenta),
                    "contradicts make.conf (≠mc badge)",
                ),
                ("def:on|off", Style::default().fg(Color::DarkGray), "the port's default value"),
                (
                    "NEW",
                    Style::default().fg(Color::Yellow),
                    "option added since the file was written",
                ),
                (
                    "mc · mc:port",
                    Style::default().fg(Color::Green),
                    "value decided by make.conf (per-port knob = mc:port)",
                ),
                (
                    "FORCED",
                    Style::default().fg(Color::Red),
                    "*_FORCE knob — locked, the file cannot override",
                ),
                (
                    "implied by X",
                    Style::default().fg(Color::Cyan),
                    "auto-enabled through IMPLIES — locked while X is on",
                ),
                (
                    "⚠broken",
                    Style::default().fg(Color::Red),
                    "enabling marks the port BROKEN (⚠ignored → IGNORE)",
                ),
            ] {
                lines.push(legend(label, style, txt));
            }
            lines.extend([
                Line::default(),
                head("Trailing sections"),
                legend(
                    "struck-through",
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::CROSSED_OUT),
                    "obsolete option — dropped on apply",
                ),
                legend(
                    "not in flavor",
                    Style::default().fg(Color::DarkGray),
                    "managed via the default flavor's view of the shared file",
                ),
            ]);
        }
        2 => lines.extend([
            keyline("j/k ↑/↓", "move · g/G first/last · PgUp/PgDn page"),
            keyline("Enter/l", "edit selected port · h/Esc back to the list"),
            keyline("Space", "toggle option (group rules enforced)"),
            keyline("d / u", "reset port to defaults / revert to saved state"),
            keyline("U", "undo the last option change"),
            keyline("B", "bulk: set an option on/off across all visible ports"),
            keyline("n / p", "next / previous port needing attention"),
            keyline("f", "jump to the next flavor of the same origin"),
            keyline("i", "option details (description, constraints, deps it adds)"),
            keyline("r", "why is this port here? — navigable chain, Enter jumps"),
        ]),
        _ => lines.extend([
            keyline("t", "show only ports needing attention"),
            keyline("s", "toggle problems-first / stable alphabetical sort"),
            keyline("m", "make.conf-decided ports count as ok (≈)"),
            keyline("w", "flag make.conf contradictions (≠)"),
            keyline("/", "filter the port list"),
            keyline("a", "apply: preview every file diff, then write atomically"),
            keyline("? h F1", "this help"),
            keyline("Ctrl-L", "force a full screen repaint"),
            keyline("q Ctrl-C", "quit — offers saving staged edits as a draft"),
        ]),
    }

    // Clamp the scroll to what actually overflows the inner area.
    let inner_height = area.height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(inner_height) as u16;
    *scroll = (*scroll).min(max_scroll);
    let title = if max_scroll > 0 {
        " help — 1-4/Tab/←→ switch · ↑/↓ scroll · other key closes "
    } else {
        " help — 1-4/Tab/←→ switch · any other key closes "
    };
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((*scroll, 0)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(Color::LightBlue)),
    );
    f.render_widget(p, area);
}

/// Cap on the dependents listed before collapsing the rest into "+ N more".
pub(crate) const WHY_MAX_DEPENDENTS: usize = 15;

fn draw_why(f: &mut Frame, app: &App) {
    let Some(why) = &app.why else { return };
    let area = centered_rect(70, 60, f.area());
    f.render_widget(Clear, area);
    let dim = Style::default().fg(Color::DarkGray);

    let mut lines: Vec<Line> = Vec::new();
    let chain_len = why.chain.as_ref().map(Vec::len).unwrap_or(0);
    match &why.chain {
        Some(chain) => {
            // Pad the first line so the "(root)" note clears the longest key.
            let width = chain.iter().map(|k| k.to_string().len()).max().unwrap_or(0);
            for (depth, key) in chain.iter().enumerate() {
                let last = depth + 1 == chain.len();
                let mut style = if last {
                    Style::default().fg(Color::LightBlue).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                if depth == why.selected {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                let mut spans = Vec::new();
                if depth > 0 {
                    spans.push(Span::raw(format!("{}└─ ", " ".repeat(depth))));
                }
                spans.push(Span::styled(format!("{key:<width$}"), style));
                if depth == 0 {
                    spans.push(Span::styled("  (root)", dim));
                }
                lines.push(Line::from(spans));
            }
        }
        None => lines.push(Line::from(Span::styled(
            "not reachable from the request roots (kept by fallback)",
            Style::default().fg(Color::Yellow),
        ))),
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        format!("direct dependents ({}):", why.dependents.len()),
        Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
    )));
    for (i, dep) in why.dependents.iter().take(WHY_MAX_DEPENDENTS).enumerate() {
        let style = if chain_len + i == why.selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(format!("  {dep}"), style)));
    }
    match why.dependents.len().checked_sub(WHY_MAX_DEPENDENTS) {
        Some(more) if more > 0 => {
            lines.push(Line::from(Span::styled(format!("  + {more} more"), dim)))
        }
        _ => {}
    }
    if why.dependents.is_empty() {
        lines.push(Line::from(Span::styled("  none in this closure", dim)));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "↑/↓ move · Enter jump to port · r why of selection · other key closes",
        dim,
    )));

    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" why {}? ", why.key))
            .border_style(Style::default().fg(Color::LightBlue)),
    );
    f.render_widget(p, area);
}

/// Human-readable source of an option's current value.
fn provenance_label(p: Provenance) -> &'static str {
    match p {
        Provenance::Default => "port default",
        Provenance::MakeConfGlobal => "make.conf (mc)",
        Provenance::MakeConfPort => "make.conf (mc:port)",
        Provenance::File => "options file",
        Provenance::Forced => "make.conf *_FORCE (locked)",
    }
}

/// Everything the framework says about one option, including the dependencies
/// enabling it pulls in. Looked up fresh on every frame.
fn draw_opt_info(f: &mut Frame, app: &App) {
    let Some((key, opt)) = &app.opt_info else { return };
    let Some(info) = app.session.ports.get(key) else { return };
    let area = centered_rect(72, 66, f.area());
    f.render_widget(Clear, area);
    let dim = Style::default().fg(Color::DarkGray);
    let red = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
    let head = Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD);
    let opts = &info.options;
    let def = opts.defs.get(opt);

    let mut lines: Vec<Line> = Vec::new();
    if let Some(desc) = def.map(|d| d.desc.as_str()).filter(|d| !d.is_empty()) {
        lines.push(Line::from(desc.to_string()));
        lines.push(Line::default());
    }

    let on = app.session.state(info).map(|s| s.staged.contains(opt)).unwrap_or(false);
    lines.push(Line::from(vec![
        Span::styled("value: ", dim),
        Span::styled(
            if on { "on" } else { "off" },
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                " · default: {} · source: {}",
                if opts.defaults.contains(opt) { "on" } else { "off" },
                provenance_label(opts.provenance(opt, file_state(app, info, opt)))
            ),
            dim,
        ),
    ]));

    if let Some(g) = opts.groups.iter().find(|g| g.members.iter().any(|m| m == opt)) {
        lines.push(Line::from(vec![
            Span::styled("group: ", dim),
            Span::raw(g.name.clone()),
            Span::styled(format!(" ({})", g.kind.label()), dim),
        ]));
    }
    if let Some(by) = app.session.implied_by(info, opt) {
        lines.push(Line::from(vec![
            Span::styled("implied by: ", dim),
            Span::styled(by, Style::default().fg(Color::Cyan)),
        ]));
    }

    if let Some(d) = def {
        if !d.implies.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("implies: ", dim),
                Span::raw(d.implies.join(" ")),
            ]));
        }
        if !d.prevents.is_empty() {
            let mut spans = vec![
                Span::styled("prevents: ", dim),
                Span::raw(d.prevents.join(" ")),
            ];
            if let Some(msg) = &d.prevents_msg {
                spans.push(Span::styled(format!(" ({msg})"), dim));
            }
            lines.push(Line::from(spans));
        }
        if let Some(msg) = &d.broken {
            lines.push(Line::from(Span::styled(
                format!("⚠ BROKEN when enabled: {msg}"),
                red,
            )));
        }
        if let Some(msg) = &d.ignore {
            lines.push(Line::from(Span::styled(
                format!("⚠ IGNORE when enabled: {msg}"),
                red,
            )));
        }
        if !d.deps.is_empty() || !d.uses.is_empty() {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled("adds when enabled", head)));
            for (class, origins) in &d.deps {
                lines.push(Line::from(vec![
                    Span::styled(format!("{class} deps: "), dim),
                    Span::raw(origins.join(" ")),
                ]));
            }
            if !d.uses.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("uses: ", dim),
                    Span::raw(d.uses.join(" ")),
                ]));
            }
        }
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled("press any key to close", dim)));

    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {opt} — {key} "))
            .border_style(Style::default().fg(Color::LightBlue)),
    );
    f.render_widget(p, area);
}

/// One-line input prompt for the bulk decision, centered on the screen.
fn draw_bulk(f: &mut Frame, app: &App) {
    let Some(input) = &app.bulk else { return };
    let full = f.area();
    let height = 3.min(full.height);
    let area = Rect {
        y: full.y + full.height.saturating_sub(height) / 2,
        height,
        ..centered_rect(70, 20, full)
    };
    f.render_widget(Clear, area);
    let p = Paragraph::new(Line::from(format!(" {input}▏"))).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" bulk: OPTION=on|off — Enter apply, Esc cancel ")
            .border_style(Style::default().fg(Color::LightBlue)),
    );
    f.render_widget(p, area);
}

fn draw_quit_confirm(f: &mut Frame) {
    let area = centered_rect(50, 20, f.area());
    f.render_widget(Clear, area);
    let mut lines = vec![Line::from(
        "Unsaved staged changes — [s]ave draft & quit · [d]iscard & quit · [Esc] stay",
    )];
    let active = crate::query::makerunner::active_make_count();
    if active > 0 {
        lines.push(Line::from(Span::styled(
            format!("{active} background make process(es) running — stopped on quit"),
            Style::default().fg(Color::DarkGray),
        )));
    }
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title(" quit ").border_style(Style::default().fg(Color::Red)));
    f.render_widget(p, area);
}
