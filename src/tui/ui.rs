use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Widget as _, Wrap};
use ratatui::Frame;

use crate::model::options::{on_off, Provenance};
use crate::model::port::PortInfo;
use crate::session::UiStatus;

use super::{App, EditorRow, Focus, OverlayKind};

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
    match app.top_overlay() {
        Some(OverlayKind::QuitConfirm) => draw_quit_confirm(f),
        Some(OverlayKind::Apply) => draw_apply_modal(f, app),
        Some(OverlayKind::Bulk) => draw_bulk(f, app),
        Some(OverlayKind::Help(tab)) => draw_help(f, tab, &mut app.help_scroll),
        Some(OverlayKind::PortHelp) => draw_port_help(f, app),
        Some(OverlayKind::Why) => draw_why(f, app),
        Some(OverlayKind::OptInfo) => draw_opt_info(f, app),
        None => {}
    }
}

/// A drawn buffer as text, one string per row with trailing blanks stripped.
pub(crate) fn screen_rows(buffer: &Buffer) -> Vec<String> {
    let (w, h) = (buffer.area.width as usize, buffer.area.height as usize);
    let cells = buffer.content();
    (0..h)
        .map(|y| (0..w).map(|x| cells[y * w + x].symbol()).collect::<String>().trim_end().to_string())
        .collect()
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
            let blocked = crate::session::is_blocked(info);
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
            if app.loop_of(key).is_some() {
                spans.push(Span::styled(" ∞", Style::default().fg(Color::Red)));
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
    if app.only_loops {
        title.push_str(" — in a dependency loop");
    }
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
    let mut title = match flavor_summary(app, info) {
        Some(flavors) => format!(" {} — {} · flavors: {} ", key, info.pkgname, flavors),
        None => format!(" {} — {} ", key, info.pkgname),
    };
    // Only where an overlay, not the ports tree, answers for the port.
    if let Some(o) = crate::config::overlay_of(&app.overlays, &key.origin) {
        title.push_str(&format!("· overlay {} ", o.label()));
    }

    let mut lines: Vec<Line> = Vec::new();
    banner_lines(app, info, &mut lines);
    // One line per banner condition, and nothing else in `lines` yet.
    let banner = lines.len();

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

    // Text width of the pane: the rows are cut at it, so the row builder needs
    // it to decide whether a description still fits (the detail panel splits
    // the pane's height, never its width).
    let inner_width = area.width.saturating_sub(2) as usize;

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
                lines.push(option_line(app, info, opt, selected, name_width, inner_width));
            }
            EditorRow::ExcludedHeader => {
                lines.push(Line::from(Span::styled(
                    "── not in this flavor (managed via the default flavor)",
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD),
                )));
            }
            EditorRow::Excluded(opt) => {
                let on = app.session.is_staged(info, opt);
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
                    .and_then(|s| s.value(opt))
                    .unwrap_or(false);
                // The indent stays outside the struck span: a rule drawn over
                // the leading blanks reads as part of the pane, not the row.
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("[{}] {}", if was_on { "x" } else { " " }, opt),
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::CROSSED_OUT),
                    ),
                ]));
            }
        }
    }

    // One box for the pane; inside it, what the port is on top and the
    // option under the cursor at the bottom, each across a rule from the
    // option rows.
    let inner = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    };
    let mut about = if app.show_about {
        let descr = app.about_text.get(&key).map(String::as_str).unwrap_or("");
        about_lines(info, descr, inner_width)
    } else {
        Vec::new()
    };
    let (opt, mut detail) = option_detail(app, info, inner.width);
    let layout = editor_layout(inner, app.show_about);

    // Keep the selected row in view: scroll so it sits inside the rows.
    let rows_height = layout.rows.height as usize;
    let selected_line = (banner + (app.editor_idx + 1).min(app.editor_rows.len())).saturating_sub(1);
    let scroll = selected_line.saturating_sub(rows_height.saturating_sub(1)) as u16;

    // The pane's title, and right of it the key that opens this port's
    // pkg-help, shown only for the ports that ship one. A long title is cut
    // short rather than drawn over the marker.
    let mut block = Block::default().borders(Borders::ALL).border_style(border_style);
    if info.pkg_help.is_some() {
        let marker = " [h] help ";
        let room = (area.width as usize).saturating_sub(2 + marker.len() + 1);
        if title.chars().count() > room {
            title = format!("{}… ", title.chars().take(room.saturating_sub(2)).collect::<String>());
        }
        block = block.title_top(
            Line::from(Span::styled(marker, key_hint())).right_aligned(),
        );
    }
    f.render_widget(block.title(title), area);
    if let Some(panel) = layout.about {
        // A cut description says where the rest is.
        let cut = mark_if_clipped(&mut about, panel.height as usize, inner_width);
        f.render_widget(Paragraph::new(about), panel);
        let hint = cut.then(|| RuleLabel::Hint(Span::styled(" [v] full text ", key_hint())));
        draw_rule(f, area, panel.y + panel.height, hint, border_style);
    }
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), layout.rows);
    if let Some(panel) = layout.detail {
        mark_if_clipped(&mut detail, panel.height as usize, inner_width);
        let label = opt.map(|o| {
            RuleLabel::Title(Span::styled(format!(" {o} "), Style::default().add_modifier(Modifier::BOLD)))
        });
        draw_rule(f, area, panel.y - 1, label, border_style);
        f.render_widget(Paragraph::new(detail), panel);
    }
}

/// How a key is advertised on the pane's borders (`[h] help`, `[v] full text`).
fn key_hint() -> Style {
    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
}

/// Content rows the option rows always keep: a panel that would leave fewer
/// shrinks, or goes.
const MIN_ROWS: u16 = 4;

/// Height of the description panel, whatever the port says: a fixed size
/// keeps the option rows where they are from one port to the next.
const ABOUT_LINES: u16 = 5;

/// Height of the option detail panel, whichever option the cursor is on (or
/// none): a description longer than that is cut, and `i` has it all.
const DETAIL_LINES: u16 = 2;

/// Where the editor's parts go inside the pane's border: the description
/// panel on top, the option rows, the detail panel at the bottom. A panel is
/// set off from the rows by a rule, which costs it one row.
struct EditorLayout {
    about: Option<Rect>,
    rows: Rect,
    detail: Option<Rect>,
}

/// Lay the editor out in `inner`, the pane without its border, from its size
/// alone, so nothing moves as the cursor or the port changes: the option rows
/// scroll under whatever is left. The detail panel is DETAIL_LINES, and goes
/// when the rows would keep fewer than MIN_ROWS. The description panel, when
/// `about`, takes ABOUT_LINES out of what is then left above MIN_ROWS: it
/// shrinks only on a short pane, and goes when not even a line fits.
fn editor_layout(inner: Rect, about: bool) -> EditorLayout {
    let mut rows = inner;
    let detail = (inner.height >= DETAIL_LINES + 1 + MIN_ROWS).then(|| {
        rows.height -= DETAIL_LINES + 1;
        Rect { y: inner.y + inner.height - DETAIL_LINES, height: DETAIL_LINES, ..inner }
    });
    let room = rows.height.saturating_sub(1 + MIN_ROWS);
    let n = if about { room.min(ABOUT_LINES) } else { 0 };
    let about = (n > 0).then(|| {
        let panel = Rect { height: n, ..rows };
        rows.y += n + 1;
        rows.height -= n + 1;
        panel
    });
    EditorLayout { about, rows, detail }
}

/// What a rule across the pane carries: a title on the left (the option the
/// panel under it describes), or a key hint on the right, as `[h] help` sits
/// on the pane's title.
enum RuleLabel {
    Title(Span<'static>),
    Hint(Span<'static>),
}

/// A rule across the pane at row `y`, joining its side borders.
fn draw_rule(f: &mut Frame, pane: Rect, y: u16, label: Option<RuleLabel>, style: Style) {
    let dashes = |n: usize| Span::styled("─".repeat(n), style);
    let inner = (pane.width as usize).saturating_sub(2);
    let width = |s: &Span| s.content.chars().count();
    let middle = match label {
        None => vec![dashes(inner)],
        Some(RuleLabel::Title(t)) => {
            let fill = inner.saturating_sub(width(&t));
            vec![t, dashes(fill)]
        }
        // Two dashes after it, as the pane's title leaves its corner.
        Some(RuleLabel::Hint(h)) => {
            let fill = inner.saturating_sub(width(&h) + 2);
            vec![dashes(fill), h, dashes(2)]
        }
    };
    let mut spans = vec![Span::styled("├", style)];
    spans.extend(middle);
    spans.push(Span::styled("┤", style));
    f.render_widget(Paragraph::new(Line::from(spans)), Rect { y, height: 1, ..pane });
}

/// What the port is: its COMMENT, then its pkg-descr `text` re-wrapped to
/// `width`, one paragraph after the other with no blank line between them,
/// a bulleted line starting a line of its own
/// (the panel is short; the `i` overlay keeps the file's own layout). A
/// trailing `WWW:` line of an old-style pkg-descr is left to the overlay too.
/// A port that says nothing reads "No description available".
fn about_lines(info: &PortInfo, text: &str, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if !info.about.comment.is_empty() {
        lines.extend(wrap_text(&info.about.comment, width, 0).into_iter().map(|t| {
            Line::from(Span::styled(t, Style::default().add_modifier(Modifier::BOLD)))
        }));
    }
    let style = Style::default().fg(Color::Gray);
    for para in text.split("\n\n") {
        // A bullet starts an item of its own, wrapped under its text; any
        // other line joins the item before it.
        let mut items: Vec<(String, usize)> = Vec::new();
        for line in para.lines().map(str::trim).filter(|l| !l.starts_with("WWW:")) {
            let bullet = ["- ", "* ", "• "].iter().any(|b| line.starts_with(b));
            match items.last_mut() {
                Some((item, _)) if !bullet => {
                    item.push(' ');
                    item.push_str(line);
                }
                _ => items.push((line.to_string(), if bullet { 2 } else { 0 })),
            }
        }
        for (item, indent) in items {
            lines.extend(
                wrap_text(&item, width, indent)
                    .into_iter()
                    .map(|t| Line::from(Span::styled(t, style))),
            );
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No description available",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines
}

/// The option highlighted in the pane, if any, and what the detail panel
/// says: the description the ports framework gives it, wrapped to `width`
/// (an option it describes nowhere says so rather than leaving a blank box).
/// With no option highlighted — the port list has the focus, or the cursor
/// sits on a header — the panel keeps its place and says how to get one.
fn option_detail(app: &App, info: &PortInfo, width: u16) -> (Option<String>, Vec<Line<'static>>) {
    let dim = |t: &str| vec![Line::from(Span::styled(t.to_string(), Style::default().fg(Color::DarkGray)))];
    if app.focus != Focus::Editor {
        return (None, dim("  Enter: edit the options"));
    }
    let Some(EditorRow::Option(opt)) = app.editor_rows.get(app.editor_idx) else {
        return (None, dim("  no option selected"));
    };
    let desc = info.options.defs.get(opt).map(|d| d.desc.as_str()).unwrap_or("");
    let lines = if desc.is_empty() {
        dim("  (no description)")
    } else {
        let style = Style::default().fg(Color::Gray);
        wrap_text(&format!("  {desc}"), width as usize, 2)
            .into_iter()
            .map(|t| Line::from(Span::styled(t, style)))
            .collect()
    };
    (Some(opt.clone()), lines)
}

/// Greedy word wrap; continuation lines are indented by `indent`. A word
/// longer than the line is hard-split rather than left to overflow.
fn wrap_text(text: &str, width: usize, indent: usize) -> Vec<String> {
    let width = width.max(indent + 1);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut first = true;
    for word in text.split_whitespace() {
        let lead = if first { text.len() - text.trim_start().len() } else { indent };
        if cur.is_empty() {
            cur = " ".repeat(lead);
            first = false;
        }
        let sep = usize::from(!cur.trim_end().is_empty());
        if cur.chars().count() + sep + word.chars().count() > width && !cur.trim().is_empty() {
            out.push(std::mem::take(&mut cur));
            cur = " ".repeat(indent);
        }
        if !cur.trim_end().is_empty() {
            cur.push(' ');
        }
        // A word wider than the line gets broken across lines.
        for c in word.chars() {
            if cur.chars().count() >= width {
                out.push(std::mem::take(&mut cur));
                cur = " ".repeat(indent);
            }
            cur.push(c);
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Cut `lines` to `cap` rows and mark the cut with an ellipsis; true when
/// something was cut. The last row is shortened first when needed, back to a
/// word boundary: the box does not wrap, so a marker appended to a full line
/// would be clipped off the edge and the cut would look like the whole text.
fn mark_if_clipped(lines: &mut Vec<Line<'static>>, cap: usize, width: usize) -> bool {
    if lines.len() <= cap {
        return false;
    }
    lines.truncate(cap);
    let Some(last) = lines.last_mut() else { return true };
    // The character the shortening stopped before, if it cut into a span.
    let mut next: Option<char> = None;
    while last.width() + 1 > width {
        let Some(span) = last.spans.last_mut() else { break };
        let mut text = span.content.to_string();
        let Some(c) = text.pop() else {
            last.spans.pop();
            continue;
        };
        next = Some(c);
        span.content = text.into();
    }
    // A word cut in two goes whole, when the line has another before it.
    if next.is_some_and(|c| !c.is_whitespace()) {
        if let Some(span) = last.spans.last_mut() {
            let text = span.content.as_ref();
            if !text.ends_with(' ') {
                if let Some(end) = text.rfind(' ').filter(|&e| !text[..e].trim().is_empty()) {
                    span.content = text[..end].to_string().into();
                }
            }
        }
    }
    last.spans.push(Span::styled("…", Style::default().fg(Color::DarkGray)));
    true
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
    app.session.state(info)?.saved.as_ref()?.value(opt)
}

/// Room a row's trailing description needs to be worth printing: below this
/// the pane would cut it to a word fragment, and the detail panel under the
/// rows carries it in full anyway.
const DESC_MIN_WIDTH: usize = 16;

/// One option row, laid out for a pane `avail` columns wide.
fn option_line(
    app: &App,
    info: &PortInfo,
    opt: &str,
    selected: bool,
    name_width: usize,
    avail: usize,
) -> Line<'static> {
    let opts = &info.options;
    let state = app.session.state(info);
    let on = app.session.is_staged(info, opt);
    let is_default = opts.defaults.contains(opt);
    let in_choice_group = opts.exclusive_group_of(opt).is_some();

    let checkbox = match (in_choice_group, on) {
        (true, true) => "(o)",
        (true, false) => "( )",
        (false, true) => "[x]",
        (false, false) => "[ ]",
    };

    // 'N' in the first column: the option is unknown to the saved file, i.e.
    // the tree added it since. A column of its own rather than a badge, so the
    // eye finds every new option down the left edge of the pane.
    let is_new = state.and_then(|s| s.saved.as_ref()).is_some_and(|saved| !saved.mentions(opt));
    let mut spans: Vec<Span> = vec![Span::styled(
        if is_new { "N" } else { " " },
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    )];
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
    let base = format!(" {checkbox} {opt:<name_width$}");
    spans.push(Span::styled(
        base,
        if selected { name_style.add_modifier(Modifier::REVERSED) } else { name_style },
    ));

    // default value column (fixed width so following badges line up)
    spans.push(Span::styled(
        format!(" {:<3}", on_off(is_default)),
        Style::default().fg(Color::DarkGray),
    ));

    // make.conf column (fixed width): the value that layer dictates, in its
    // own colour — green for the global knob, bright green for the per-port
    // one, red bold for a *_FORCE knob the options file cannot override.
    // Blank when nothing but the port default or the file decides.
    let (prov, prov_style) = match opts.provenance(opt, file_state(app, info, opt)) {
        Provenance::Forced => (
            on_off(opts.forced_value(opt).unwrap_or(false)),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Provenance::MakeConfPort => (
            on_off(opts.port_set.contains(opt)),
            Style::default().fg(Color::LightGreen),
        ),
        Provenance::MakeConfGlobal => {
            (on_off(opts.mc_set.contains(opt)), Style::default().fg(Color::Green))
        }
        _ => ("", Style::default()),
    };
    spans.push(Span::styled(format!(" {prov:<3}"), prov_style));

    // No badge for a make.conf contradiction: the magenta option name above
    // already says it, and the row needs the columns for the description.

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
        // The pane cuts the row at its right edge, so a description with only
        // a few columns left would show as a word fragment. Drop it instead —
        // the panel under the rows gives it in full, wrapped.
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let room = avail.saturating_sub(used + 2);
        if !def.desc.is_empty() && room >= DESC_MIN_WIDTH {
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
        // Indexed in UiStatus declaration order, Conflict first.
        for key in &app.visible {
            counts[app.effective_status(&app.session.ports[key]) as usize] += 1;
        }
        let mc = if app.warn_mc { format!(" {}≠", counts[4]) } else { String::new() };
        let mut spans = vec![Span::styled(
            format!(
                " {}✗ {}* {}? {}!{mc} {}✓ (+{} optionless) ",
                counts[0], counts[1], counts[2], counts[3], counts[5], app.hidden
            ),
            Style::default().fg(Color::White),
        )];
        // Only when the closure actually holds one: the count of visible ports
        // caught in it, and the key that narrows the list to them.
        if !app.loops.is_empty() {
            let tangled =
                app.visible.iter().filter(|key| app.loop_of(key).is_some()).count();
            spans.push(Span::styled(
                format!(" {tangled}∞ L:loops "),
                Style::default().fg(Color::Red),
            ));
        }
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
            "{} options file(s) will be written to {}{}:",
            modal.writes.len(),
            app.options_dir.display(),
            if modal.removals.is_empty() {
                String::new()
            } else {
                format!(", {} removed (--minimal)", modal.removals.len())
            }
        )));
        lines.push(Line::default());
        for r in &modal.removals {
            lines.push(Line::from(vec![
                Span::styled(format!("rm {:<37}", r.options_name), Style::default().fg(Color::Yellow)),
                Span::styled(r.reason.clone(), Style::default().fg(Color::DarkGray)),
            ]));
        }
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

/// Rows `lines` take in a `width`-wide Paragraph wrapped with
/// `Wrap { trim: false }`. Measured by laying each long line out the way the
/// Paragraph will: ratatui keeps its own count (`line_count`) behind an
/// unstable feature.
fn wrapped_height(lines: &[Line], width: u16) -> usize {
    if width == 0 {
        return lines.len();
    }
    lines
        .iter()
        .map(|line| {
            let w = line.width();
            if w <= width as usize {
                return 1;
            }
            // Greedy wrapping: two rows in a row hold more than `width`
            // columns, so this many rows always hold the whole line.
            let bound = (2 * w.div_ceil(width as usize) + 1).min(u16::MAX as usize) as u16;
            let area = Rect::new(0, 0, width, bound);
            let mut buf = ratatui::buffer::Buffer::empty(area);
            Paragraph::new(line.clone()).wrap(Wrap { trim: false }).render(area, &mut buf);
            (0..bound)
                .rev()
                .find(|&y| (0..width).any(|x| buf[(x, y)].symbol() != " "))
                .map_or(1, |y| y as usize + 1)
        })
        .sum()
}

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
            mark("∞", Color::Red, "caught in a dependency loop — r lists it, Enter jumps to any port"),
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
                Span::styled("  ", Style::default()),
                Span::styled("N", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(" [x] ", Style::default()),
                Span::styled("OPENSSL     ", Style::default().fg(Color::Yellow)),
                Span::styled(" off", Style::default().fg(Color::DarkGray)),
                Span::styled(" on ", Style::default().fg(Color::Green)),
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
                    "contradicts make.conf policy",
                ),
                (
                    "N",
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    "first column: option added since the file was written",
                ),
                (
                    "on | off",
                    Style::default().fg(Color::DarkGray),
                    "1st column: the port's default value",
                ),
                (
                    "on | off",
                    Style::default().fg(Color::Green),
                    "2nd column: the value make.conf decides",
                ),
                (
                    "on | off",
                    Style::default().fg(Color::LightGreen),
                    "same, from the per-port knob (brighter)",
                ),
                (
                    "on | off",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
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
                Line::default(),
                head("Above the rows"),
                legend(
                    "port panel",
                    Style::default().add_modifier(Modifier::BOLD),
                    "with I, the port's comment and pkg-descr, in five",
                ),
                Line::from(Span::raw("                   lines — v shows it all, from either pane")),
                Line::default(),
                head("Below the rows"),
                legend(
                    "option panel",
                    Style::default().fg(Color::Gray),
                    "the description of the option under the cursor, in",
                ),
                // Aligned on the legend's text column (2 + 14 + " = ").
                Line::from(Span::raw("                   two lines — i on it shows it all")),
            ]);
        }
        2 => lines.extend([
            keyline("j/k ↑/↓", "move · g/G first/last · PgUp/PgDn page"),
            keyline("Enter/l", "edit selected port · Esc/Tab/← back to the list"),
            keyline("Space", "toggle option (group rules enforced)"),
            keyline("d / u", "reset port to defaults / revert to saved state"),
            keyline("U", "undo the last option change"),
            keyline("B", "bulk: set an option on/off across all visible ports"),
            keyline("n / p", "next / previous port needing attention"),
            keyline("f", "jump to the next flavor of the same origin"),
            keyline("i", "option details (description, constraints, deps it adds)"),
            keyline("", "on the list: what the port is (comment, WWW, pkg-descr)"),
            keyline("v", "the port's full description, from either pane"),
            keyline("I", "show / hide the port description above the options"),
            keyline("h", "port notes (pkg-help) — marked [h] in the pane title"),
            keyline("r", "why is this port here? — chain, loop, dependents; Enter jumps"),
        ]),
        _ => lines.extend([
            keyline("t", "show only ports needing attention"),
            keyline("s", "toggle problems-first / stable alphabetical sort"),
            keyline("m", "make.conf-decided ports count as ok (≈)"),
            keyline("w", "flag make.conf contradictions (≠)"),
            keyline("L", "show only the ports caught in a dependency loop (∞)"),
            keyline("/", "filter the port list"),
            keyline("a", "apply: preview every file diff, then write atomically"),
            keyline("? F1", "this help"),
            keyline("Ctrl-L", "force a full screen repaint"),
            keyline("q Ctrl-C", "quit — offers saving staged edits as a draft"),
        ]),
    }

    // Clamp the scroll to what actually overflows the inner area, counted in
    // the rows the lines wrap to: a narrow terminal wraps many of them.
    let inner_height = area.height.saturating_sub(2) as usize;
    let rows = wrapped_height(&lines, area.width.saturating_sub(2));
    let max_scroll = rows.saturating_sub(inner_height) as u16;
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
    // Navigable rows are numbered in WhyInfo::entries() order — chain, then
    // the loop's cycle, then the rest of its tangle, then the dependents.
    // `row` is the index of the next one, and the two orders must agree.
    let mut row = 0usize;
    // Line the cursor sits on, so the box can scroll it into view.
    let mut selected_line = 0usize;
    let mark = |lines: &Vec<Line>, row: usize, selected_line: &mut usize, style: Style| {
        if row == why.selected {
            *selected_line = lines.len();
            style.add_modifier(Modifier::REVERSED)
        } else {
            style
        }
    };

    match &why.chain {
        Some(chain) => {
            // Pad the first line so the "(root)" note clears the longest key.
            let width = chain.iter().map(|k| k.to_string().len()).max().unwrap_or(0);
            for (depth, key) in chain.iter().enumerate() {
                let last = depth + 1 == chain.len();
                let base = if last {
                    Style::default().fg(Color::LightBlue).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                let style = mark(&lines, row, &mut selected_line, base);
                row += 1;
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

    if let Some(dl) = &why.dep_loop {
        let total = dl.walk.len() + dl.tangled.len() + dl.hidden;
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            format!(
                "caught in a dependency loop ({total} port{}):",
                if total == 1 { "" } else { "s" }
            ),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )));
        // The cycle, rotated to start where the user is standing.
        let width = dl
            .walk
            .iter()
            .chain(dl.tangled.iter())
            .map(|k| k.to_string().len())
            .max()
            .unwrap_or(0);
        let self_loop = dl.walk.len() == 1;
        for (i, key) in dl.walk.iter().enumerate() {
            let style = mark(&lines, row, &mut selected_line, Style::default().fg(Color::Red));
            row += 1;
            let mut spans =
                vec![Span::raw("  "), Span::styled(format!("{key:<width$}"), style)];
            if self_loop {
                spans.push(Span::styled("  (this port — depends on itself)", dim));
            } else {
                if i == 0 {
                    spans.push(Span::styled("  (this port)", dim));
                }
                if i + 1 == dl.walk.len() {
                    spans.push(Span::styled(format!("  → back to {}", dl.walk[0]), dim));
                }
            }
            lines.push(Line::from(spans));
        }
        // Ports of the component the cycle does not pass through: each can
        // still reach every other, so none of them can be built first either.
        if !dl.tangled.is_empty() {
            lines.push(Line::from(Span::styled("  also in the tangle:", dim)));
            for key in &dl.tangled {
                let style =
                    mark(&lines, row, &mut selected_line, Style::default().fg(Color::Red));
                row += 1;
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(key.to_string(), style),
                ]));
            }
            if dl.hidden > 0 {
                lines.push(Line::from(Span::styled(format!("    + {} more", dl.hidden), dim)));
            }
        }
        lines.push(Line::from(Span::styled(
            "  poudriere cannot build these; an option may break the loop",
            dim,
        )));
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        format!("direct dependents ({}):", why.dependents.len()),
        Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
    )));
    for dep in why.dependents.iter().take(WHY_MAX_DEPENDENTS) {
        let style = mark(&lines, row, &mut selected_line, Style::default());
        row += 1;
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

    // A loop's tangle can outrun the box; keep the cursor's row in view,
    // counting the rows the lines above it wrap to.
    let inner_height = area.height.saturating_sub(2) as usize;
    let cursor_row = wrapped_height(&lines[..selected_line], area.width.saturating_sub(2));
    let scroll = cursor_row.saturating_sub(inner_height.saturating_sub(1)) as u16;
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll, 0)).block(
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

    let on = app.session.is_staged(info, opt);
    lines.push(Line::from(vec![
        Span::styled("value: ", dim),
        Span::styled(
            on_off(on),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                " · default: {} · source: {}",
                on_off(opts.defaults.contains(opt)),
                provenance_label(opts.provenance(opt, file_state(app, info, opt)))
            ),
            dim,
        ),
    ]));

    if let Some(g) = opts.group_of(opt) {
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

/// The port's pkg-help file, scrollable. These are prose files with their own
/// indentation and lines that outrun the popup, so each source line keeps its
/// indent and wraps under itself; tabs are expanded so the wrap counts what
/// the terminal will show.
/// One source line of a pkg-help, ready for the popup: tabs expanded, its own
/// indentation kept and wrapped under itself, blank lines preserved. Both
/// indents are capped at half the width — a deeply indented line (a code
/// sample, say) must still wrap inside the box instead of running off it.
fn help_line(src: &str, width: usize) -> Vec<String> {
    let src = src.replace('\t', "        ");
    if src.trim().is_empty() {
        return vec![String::new()];
    }
    let indent = (src.len() - src.trim_start().len()).min(width / 2);
    let hang = (indent + 2).min(width.saturating_sub(1));
    wrap_text(&format!("{}{}", " ".repeat(indent), src.trim_start()), width, hang)
}

fn draw_port_help(f: &mut Frame, app: &mut App) {
    let Some(help) = &mut app.port_help else { return };
    let area = centered_rect(76, 70, f.area());
    f.render_widget(Clear, area);
    let inner_w = area.width.saturating_sub(2) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;

    let lines: Vec<Line> =
        help.lines.iter().flat_map(|src| help_line(src, inner_w)).map(Line::from).collect();

    let max_scroll = lines.len().saturating_sub(inner_h) as u16;
    help.scroll = help.scroll.min(max_scroll);
    let hint = if max_scroll > 0 {
        " j/k ↑/↓ PgUp/PgDn scroll · any other key closes "
    } else {
        " any key closes "
    };
    let p = Paragraph::new(lines).scroll((help.scroll, 0)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} — {} ", help.title, help.key))
            .title_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .title_bottom(Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray))))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The panels are sized from the pane alone, never from the port, the
    /// option or how many options there are, so nothing moves as the cursor
    /// or the port changes.
    #[test]
    fn panels_are_sized_from_the_pane() {
        let inner = Rect::new(1, 1, 38, 28);
        let l = editor_layout(inner, true);
        let about = l.about.expect("room for the description");
        let detail = l.detail.expect("room for the detail");
        assert_eq!(about, Rect::new(1, 1, 38, ABOUT_LINES));
        assert_eq!(detail, Rect::new(1, 27, 38, DETAIL_LINES));
        assert_eq!(l.rows.y, about.y + about.height + 1, "a rule between");
        assert_eq!(l.rows.y + l.rows.height + 1, detail.y, "a rule between");
        assert_eq!(l.rows.height, 28 - ABOUT_LINES - 1 - DETAIL_LINES - 1);
        assert!(editor_layout(inner, false).about.is_none());

        // A short pane: the description shrinks to keep the rows MIN_ROWS,
        // and goes when not even a line fits; then the detail goes too.
        let l = editor_layout(Rect::new(1, 1, 38, 10), true);
        assert_eq!(l.about.expect("a shrunk description").height, 2, "10 - 3 - 1 - 4 rows");
        assert_eq!(l.rows.height, MIN_ROWS);
        let l = editor_layout(Rect::new(1, 1, 38, 8), true);
        assert!(l.about.is_none());
        assert!(l.detail.is_some(), "8 rows keep 4 beside 2 lines and a rule");
        assert!(editor_layout(Rect::new(1, 1, 38, 6), false).detail.is_none());
    }

    /// A cut line loses the word it would split, not half of it.
    #[test]
    fn a_clipped_line_ends_on_a_whole_word() {
        let mut lines: Vec<Line<'static>> =
            vec![Line::from("one two three"), Line::from("more")];
        assert!(mark_if_clipped(&mut lines, 1, 13));
        assert_eq!(lines[0].to_string(), "one two…");
        let mut lines: Vec<Line<'static>> = vec![Line::from("short"), Line::from("more")];
        assert!(mark_if_clipped(&mut lines, 1, 20));
        assert_eq!(lines[0].to_string(), "short…");
        let mut lines: Vec<Line<'static>> = vec![Line::from("fits")];
        assert!(!mark_if_clipped(&mut lines, 1, 20));
    }

    /// A bulleted pkg-descr keeps one bullet per line.
    #[test]
    fn about_lines_keeps_bullets_apart() {
        let mut info = PortInfo::stub("cat/port");
        info.about.comment = "A port".into();
        let text = "Use it to:\n- avoid this\n- avoid that, a long\n  continuation\n";
        let shown: Vec<String> = about_lines(&info, text, 40).iter().map(|l| l.to_string()).collect();
        assert_eq!(shown, ["A port", "Use it to:", "- avoid this", "- avoid that, a long continuation"]);
    }

    /// A port that says nothing about itself says so in the panel.
    #[test]
    fn about_lines_says_when_there_is_no_description() {
        let info = PortInfo::stub("cat/port");
        let lines = about_lines(&info, "", 40);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].to_string(), "No description available");
    }

    #[test]
    fn wrap_text_keeps_the_first_indent_and_indents_the_rest() {
        assert_eq!(
            wrap_text("  TLS-SRP (Secure Remote Password) support", 20, 2),
            ["  TLS-SRP (Secure", "  Remote Password)", "  support"]
        );
    }

    #[test]
    fn wrap_text_breaks_a_word_wider_than_the_line() {
        assert_eq!(wrap_text("  abcdefghij", 6, 2), ["  abcd", "  efgh", "  ij"]);
    }

    #[test]
    fn wrap_text_of_a_fitting_line_is_the_line() {
        assert_eq!(wrap_text("  short desc", 40, 2), ["  short desc"]);
    }

    #[test]
    fn wrap_text_hard_splits_a_word_longer_than_the_whole_line() {
        // An unbroken word must not overflow the panel it is wrapped for.
        assert!(wrap_text("  supercalifragilistic", 8, 2).iter().all(|l| l.chars().count() <= 8));
    }

    /// The popup does not wrap, so anything wider than it is lost off the
    /// right edge — including a deeply indented pkg-help line.
    #[test]
    fn help_line_stays_inside_the_popup_however_indented() {
        let deep = format!("{}code sample here", " ".repeat(40));
        for line in help_line(&deep, 20) {
            assert!(line.chars().count() <= 20, "{line:?} wider than the popup");
        }
        // Tabs count as the eight columns the terminal will show.
        assert_eq!(help_line("\tx", 40), ["        x"]);
        // Blank lines survive as blank lines.
        assert_eq!(help_line("   ", 40), [""]);
        // A normal line keeps its own indent and hangs the wrap under it.
        assert_eq!(
            help_line("  alpha beta gamma", 12),
            ["  alpha beta", "    gamma"]
        );
    }

    /// The rows `draw` leaves on a `width` x `height` screen, trimmed.
    fn rendered(width: u16, height: u16, draw: impl FnOnce(&mut Frame)) -> Vec<String> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(draw).unwrap();
        screen_rows(terminal.backend().buffer())
    }

    /// The last row of text inside the box, borders stripped.
    fn last_text_row(rows: &[String]) -> String {
        rows.iter()
            .map(|r| r.trim().trim_matches('│').trim().to_string())
            .rfind(|r| !r.is_empty() && !r.starts_with('└') && !r.starts_with('┌'))
            .unwrap_or_default()
    }

    /// Scrolled to its end, every help tab shows its last line on an 80x30
    /// terminal, where the box wraps lines the scroll limit must count.
    #[test]
    fn every_help_tab_scrolls_to_its_last_line_on_a_small_terminal() {
        for tab in 0..HELP_TABS.len() {
            // As tall as it takes to show it all, at the same width.
            let whole = rendered(80, 200, |f| draw_help(f, tab, &mut 0));
            let last = last_text_row(&whole);
            let mut scroll = u16::MAX;
            let small = rendered(80, 30, |f| draw_help(f, tab, &mut scroll));
            assert_eq!(last_text_row(&small), last, "tab {tab}:\n{}", small.join("\n"));
        }
    }

    /// Cutting the panel is announced, and the marker must be visible: a full
    /// last line has to give up a column for it, else it is clipped away and
    /// the cut looks like the whole text.
    #[test]
    fn mark_if_clipped_keeps_the_marker_inside_the_width() {
        let line = |t: &str| Line::from(Span::raw(t.to_string()));
        let mut lines = vec![line("0123456789"), line("second"), line("third")];
        mark_if_clipped(&mut lines, 2, 10);
        assert_eq!(lines.len(), 2);
        let first: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(first, "0123456789", "untouched: the cut is marked on the last row");
        let last: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(last, "second…");
        assert!(lines[1].width() <= 10);

        // Last row already full: it loses a column so the marker fits.
        let mut lines = vec![line("second"), line("0123456789"), line("third")];
        mark_if_clipped(&mut lines, 2, 10);
        let last: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(last, "012345678…");
        assert!(lines[1].width() <= 10);

        // Nothing to cut, nothing to mark.
        let mut lines = vec![line("a"), line("b")];
        mark_if_clipped(&mut lines, 2, 10);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].width(), 1);
    }
}
