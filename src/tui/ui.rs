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
    } else if app.show_help {
        draw_help(f);
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
            // A port silenced by the mc_relax view keeps a hint marker.
            let (marker, color) = if status == UiStatus::Ok && raw != UiStatus::Ok {
                ("≈", Color::DarkGray)
            } else {
                status_marker(&status)
            };
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

fn draw_help(f: &mut Frame) {
    let area = centered_rect(78, 88, f.area());
    f.render_widget(Clear, area);

    let key = |k: &str| Span::styled(format!("{k:<12}"), Style::default().fg(Color::LightBlue));
    let head = |t: &str| {
        Line::from(Span::styled(
            t.to_string(),
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        ))
    };
    let mark = |m: &str, c: Color, txt: &str| {
        Line::from(vec![
            Span::styled(format!("  {m:<3}"), Style::default().fg(c)),
            Span::raw(txt.to_string()),
        ])
    };

    let mut lines: Vec<Line> = vec![
        head("Port markers"),
        mark("✗", Color::Red, "conflict — staged options violate PREVENTS/group rules"),
        mark("*", Color::Cyan, "edited — staged changes not yet applied"),
        mark("?", Color::Yellow, "unconfigured — no saved options file"),
        mark("!", Color::LightRed, "stale — option list changed since the file was written"),
        mark("≠", Color::Magenta, "contradicts make.conf OPTIONS_SET/UNSET (w view)"),
        mark("≈", Color::DarkGray, "needs no attention: decided by make.conf (m view)"),
        mark("✓", Color::DarkGray, "ok · ⚠ port BROKEN/IGNORE with current options"),
        Line::default(),
        head("Option row"),
        mark("[x]", Color::White, "checkbox · (o) single/radio group member"),
        mark("", Color::Yellow, "yellow name = deviates from the port default"),
        mark("", Color::Magenta, "magenta name = contradicts make.conf policy (≠mc)"),
        mark("", Color::DarkGray, "def:on|off — the port's default value"),
        mark("NEW", Color::Yellow, "added since the options file was written"),
        mark("mc", Color::Green, "value from make.conf (mc:port = per-port knob)"),
        mark("", Color::Red, "FORCED — *_FORCE knob, file cannot override (locked)"),
        mark("", Color::Cyan, "implied by X — auto-enabled through IMPLIES (locked)"),
        mark("⚠", Color::Red, "broken/ignored — enabling marks the port BROKEN/IGNORE"),
        mark("≠mc", Color::Magenta, "value contradicts the global make.conf policy"),
        Line::default(),
        head("Keys"),
    ];
    for (k, txt) in [
        ("j/k ↑/↓", "move · g/G first/last · PgUp/PgDn page"),
        ("Enter/l", "edit selected port · h/Esc back to the list"),
        ("Space", "toggle option (group rules enforced)"),
        ("d / u", "reset port to defaults / revert to saved state"),
        ("B", "bulk: set an option on/off across all visible ports"),
        ("n / p", "next / previous port needing attention"),
        ("f", "jump to the next flavor of the same origin"),
        ("t", "show only ports needing attention"),
        ("s", "toggle problems-first / stable alphabetical sort"),
        ("m", "make.conf-decided ports count as ok (≈)"),
        ("w", "flag make.conf contradictions (≠)"),
        ("/", "filter the port list"),
        ("a", "apply: preview every file diff, then write atomically"),
        ("i", "option details (description, constraints, deps it adds)"),
        ("r", "why is this port here? (dependency chain + dependents)"),
        ("? h F1", "this help"),
        ("Ctrl-L", "force a full screen repaint"),
        ("q Ctrl-C", "quit (confirms when staged changes exist)"),
    ] {
        lines.push(Line::from(vec![Span::raw("  "), key(k), Span::raw(txt.to_string())]));
    }

    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" help — any key to close ")
            .border_style(Style::default().fg(Color::LightBlue)),
    );
    f.render_widget(p, area);
}

/// Cap on the dependents listed before collapsing the rest into "+ N more".
const WHY_MAX_DEPENDENTS: usize = 15;

fn draw_why(f: &mut Frame, app: &App) {
    let Some(why) = &app.why else { return };
    let area = centered_rect(70, 60, f.area());
    f.render_widget(Clear, area);
    let dim = Style::default().fg(Color::DarkGray);

    let mut lines: Vec<Line> = Vec::new();
    match &why.chain {
        Some(chain) => {
            // Pad the first line so the "(root)" note clears the longest key.
            let width = chain.iter().map(|k| k.to_string().len()).max().unwrap_or(0);
            for (depth, key) in chain.iter().enumerate() {
                let last = depth + 1 == chain.len();
                let style = if last {
                    Style::default().fg(Color::LightBlue).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
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
    for dep in why.dependents.iter().take(WHY_MAX_DEPENDENTS) {
        lines.push(Line::from(format!("  {dep}")));
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
    lines.push(Line::from(Span::styled("press any key to close", dim)));

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
    let p = Paragraph::new("Unsaved staged changes — quit anyway? (y/N)")
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title(" quit ").border_style(Style::default().fg(Color::Red)));
    f.render_widget(p, area);
}
