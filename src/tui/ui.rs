use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Widget as _, Wrap,
};
use ratatui::Frame;

use crate::apply::PendingWrite;
use crate::model::options::{on_off, McLayer, Provenance};
use crate::model::port::PortInfo;
use crate::session::UiStatus;

use super::{clamp_u16, App, EditorRow, Focus, OverlayKind};

pub fn draw(f: &mut Frame, app: &mut App) {
    // The description panel's default goes by the terminal's height, taken
    // at every frame so a resize across it shows or hides the panel.
    app.note_screen(f.area().height);
    // The option pane draws the selected port's rows, never another's.
    app.sync_editor();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(f.area());
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(outer[0]);

    draw_port_list(f, app, panes[0]);
    app.page.options = draw_editor(f, app, panes[1]);
    draw_status_bar(f, app, outer[1]);
    match app.top_overlay() {
        Some(OverlayKind::QuitConfirm) => draw_quit_confirm(f),
        Some(OverlayKind::Apply) => draw_apply_modal(f, app),
        Some(OverlayKind::Bulk) => draw_bulk(f, app),
        Some(OverlayKind::Help(tab)) => app.page.help = draw_help(f, tab, &mut app.help_scroll),
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
        UiStatus::Unconfigured => ("?", Color::Yellow),
        UiStatus::Stale => ("!", Color::LightRed),
        UiStatus::McDeviation => ("≠", Color::Magenta),
        UiStatus::Ok => ("✓", Color::DarkGray),
    }
}

/// The border of a pane: double and light blue when it has the focus,
/// plain and dim otherwise.
fn pane_border(focused: bool) -> (Style, BorderType) {
    if focused {
        (Style::default().fg(Color::LightBlue), BorderType::Double)
    } else {
        (Style::default().fg(Color::DarkGray), BorderType::Plain)
    }
}

/// The row between the problem ports and the `ok` ok ones, `width` wide.
fn divider_item(ok: usize, width: usize) -> ListItem<'static> {
    let mut text = format!("── {ok} ok ");
    let fill = width.saturating_sub(text.chars().count());
    text.push_str(&"─".repeat(fill));
    ListItem::new(Line::from(Span::styled(text, Style::default().fg(Color::DarkGray))))
}

fn draw_port_list(f: &mut Frame, app: &mut App, area: Rect) {
    // The columns a row has for its name and badges, after its two markers
    // and the blank after them.
    let name_room = (area.width as usize).saturating_sub(2 + 3);
    let mut items: Vec<ListItem> = app
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
            // The second column: staged edits, whatever the state.
            let edited = if app.session.edited(info) { "*" } else { " " };
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
            let mut badges = Vec::new();
            if blacklisted {
                badges.push(Span::styled(" ⊘", Style::default().fg(Color::DarkGray)));
            }
            if blocked {
                badges.push(Span::styled(" ⚠", Style::default().fg(Color::Red)));
            }
            if app.loop_of(key).is_some() {
                badges.push(Span::styled(" ∞", Style::default().fg(Color::Red)));
            }
            // Options the port gained since its file was written, in the
            // style of their N in the option pane: shown whatever the
            // state, so a port -m or the m view settles still says it.
            if let Some((added, _)) = app.session.stale_delta(info).filter(|(a, _)| !a.is_empty()) {
                badges.push(Span::styled(
                    format!(" +{}", added.len()),
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ));
            }
            let badge_width: usize = badges.iter().map(Span::width).sum();
            let name = fit_port_name(key, name_room.saturating_sub(badge_width));
            let mut spans = vec![
                Span::styled(marker, Style::default().fg(color)),
                Span::styled(format!("{edited} "), Style::default().fg(Color::Cyan)),
                Span::styled(name, name_style),
            ];
            spans.extend(badges);
            ListItem::new(Line::from(spans))
        })
        .collect();
    if let Some(d) = app.divider {
        let width = area.width.saturating_sub(2) as usize;
        items.insert(d, divider_item(app.visible.len() - d, width));
    }

    let count = if app.hide_ok {
        format!("{}/{}", app.visible.len(), app.listable)
    } else {
        format!("{}", app.visible.len())
    };
    // The view the list is in, long and short: kept before the optionless
    // count, and shortened before it is cut.
    let (mut view, mut short) = (String::new(), String::new());
    if app.only_loops {
        view.push_str(" — in a dependency loop");
        short.push_str(" — in a loop");
    }
    if app.hide_ok {
        view.push_str(" — needs attention");
        short.push_str(" — attention");
    }
    let room = area.width.saturating_sub(2) as usize;
    let fits = |t: &str| Line::from(t).width() <= room;
    // The ports with no options are in the closure but never listed; their
    // count goes first when the title does not fit, then the view's long
    // wording, and only then is the rest cut.
    let full = format!(" Ports ({count} · {} optionless){view} ", app.hidden);
    let long = format!(" Ports ({count}){view} ");
    let brief = format!(" Ports ({count}){short} ");
    let title = if app.hidden > 0 && fits(&full) {
        full
    } else if fits(&long) {
        long
    } else {
        // A cut never ends on a view's dash: `in a loop…`, not `in a loop —…`.
        fit_title(&brief, room).replace(" —…", "…")
    };
    let (border_style, border_type) =
        pane_border(app.focus == Focus::List || app.focus == Focus::Filter);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .title(title)
        .border_style(border_style);
    // The filter on the bottom border, at its right: with the cursor only
    // while it is being typed, as text once left on the list. Too long, it
    // loses its start, so the end being typed stays in view.
    let typing = app.focus == Focus::Filter;
    if typing || !app.filter.is_empty() {
        let cursor = if typing { "▏" } else { "" };
        let label = fit_start(" filter: ", &format!("{}{cursor}", app.filter), " ", room);
        block = block.title_bottom(Line::from(label).right_aligned());
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    // The selection is an index among the ports; the widget counts rows,
    // the divider among them. Its scroll offset is in rows and kept.
    let mut rows = ListState::default()
        .with_offset(app.list_state.offset())
        .with_selected(app.list_state.selected().map(|i| app.list_row(i)));
    f.render_stateful_widget(list, area, &mut rows);
    *app.list_state.offset_mut() = rows.offset();
    app.page.list = area.height.saturating_sub(2);
}

/// A list row's `category/name[@flavor]` in `room` columns: whole when it
/// fits, else the category is cut first (`multim…/ffmpeg@default`, then
/// `…/ffmpeg@default`), and only then the end of the name and flavor.
pub(crate) fn fit_port_name(key: &crate::model::origin::PortKey, room: usize) -> String {
    let full = key.to_string();
    let width = |s: &str| Line::from(s).width();
    if width(&full) <= room {
        return full;
    }
    let (category, rest) = full.split_once('/').unwrap_or(("", full.as_str()));
    // `…/` and the rest, with what is left of the category before them.
    match room.checked_sub(width(rest) + 2) {
        Some(keep) => {
            let head: String = category.chars().take(keep).collect();
            format!("{head}…/{rest}")
        }
        None => format!("…/{}", fit_end(rest, room.saturating_sub(2))),
    }
}

/// Draw the option pane; returns how many option rows it showed.
fn draw_editor(f: &mut Frame, app: &App, area: Rect) -> u16 {
    let (border_style, border_type) = pane_border(app.focus == Focus::Editor);
    let Some(key) = app.selected_key() else {
        let p = Paragraph::new("no port selected").block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(border_type)
                .border_style(border_style),
        );
        f.render_widget(p, area);
        return 0;
    };
    let info = &app.session.ports[&key];
    // The versioned package names the port; its origin is the list's row.
    let mut title = vec![Span::raw(format!(" {} ", info.pkgname))];
    if let Some(flavors) = flavor_summary(app, info) {
        title = vec![Span::raw(format!(" {} · flavors: ", info.pkgname))];
        title.extend(flavors);
        title.push(Span::raw(" "));
    }

    // Text width of the pane: the rows are cut at it, so the row builder needs
    // it to decide whether a description still fits (the detail panel splits
    // the pane's height, never its width).
    let inner_width = area.width.saturating_sub(2) as usize;

    let mut lines: Vec<Line> = Vec::new();
    banner_lines(app, info, inner_width, &mut lines);
    // The banner's lines, and nothing else in `lines` yet.
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

    let state = app.session.state(info);
    let mut option_rows: Vec<usize> = Vec::new();
    for (i, row) in app.editor_rows.iter().enumerate() {
        let selected = app.focus == Focus::Editor && i == app.editor_idx;
        match row {
            EditorRow::GroupHeader(gi) => {
                let Some(g) = info.options.groups.get(*gi) else { continue };
                let mut text = format!("── {} — {}", g.name, g.kind.label());
                if !g.desc.is_empty() {
                    text.push_str(&format!(": {}", g.desc));
                }
                lines.push(Line::from(Span::styled(
                    text,
                    Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
                )));
            }
            // A row cuts itself: its columns stay, its description goes first.
            EditorRow::Option(opt) => {
                option_rows.push(lines.len());
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

    // A row wider than the pane ends in `…` at a word, not cut mid-word at
    // the border; option rows are laid out to the width already.
    for (i, line) in lines.iter_mut().enumerate() {
        if !option_rows.contains(&i) {
            ellipsize(line, inner_width);
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
    let shown = app.about_shown();
    let mut about = if shown {
        let descr = app.about_text.get(&key).map(String::as_str).unwrap_or("");
        about_lines(info, descr, inner_width)
    } else {
        Vec::new()
    };
    let (opt, mut detail) = option_detail(app, info, inner.width);
    let layout = editor_layout(inner, shown);

    // Keep the selected row in view: scroll so it sits inside the rows.
    let rows_height = layout.rows.height as usize;
    let selected_line = (banner + (app.editor_idx + 1).min(app.editor_rows.len())).saturating_sub(1);
    let mut scroll = selected_line.saturating_sub(rows_height.saturating_sub(1));
    // On the last option, the sections after it (not in this flavor,
    // obsolete), which the cursor never reaches, come into view as far as
    // the cursor stays in it.
    let last_option = app
        .editor_rows
        .iter()
        .skip(app.editor_idx + 1)
        .all(|r| !matches!(r, EditorRow::Option(_)));
    if last_option {
        scroll = scroll.max(lines.len().saturating_sub(rows_height).min(selected_line));
    }
    let scroll = scroll as u16;

    // The pane's title, and right of it the key that opens this port's
    // pkg-help notes, shown only for the ports that ship one. A long title
    // is cut short rather than drawn over the marker.
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(border_style);
    let mut room = (area.width as usize).saturating_sub(2);
    if info.pkg_help.is_some() {
        let marker = " [h] notes ";
        room = room.saturating_sub(marker.len() + 1);
        block = block.title_top(
            Line::from(Span::styled(marker, key_hint())).right_aligned(),
        );
    }
    block = block.title(fit_title_line(Line::from(title), room));
    let (left, right) = editor_footer(app, info, &key.origin, (area.width as usize).saturating_sub(2));
    if let Some(left) = left {
        block = block.title_bottom(Line::from(left));
    }
    if let Some(right) = right {
        block = block.title_bottom(
            Line::from(Span::styled(right, Style::default().fg(Color::Cyan))).right_aligned(),
        );
    }
    f.render_widget(block, area);
    if let Some(panel) = layout.about {
        // A cut description says where the rest is.
        let cut = mark_if_clipped(&mut about, panel.height as usize, inner_width);
        f.render_widget(Paragraph::new(about), panel);
        let hint = cut.then(|| RuleLabel::Hint(Span::styled(" [v] full text ", key_hint())));
        draw_rule(f, area, panel.y + panel.height, hint, border_style, border_type);
    }
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), layout.rows);
    if let Some(panel) = layout.detail {
        mark_if_clipped(&mut detail, panel.height as usize, inner_width);
        let label = opt.map(|o| {
            RuleLabel::Title(Span::styled(format!(" {o} "), Style::default().add_modifier(Modifier::BOLD)))
        });
        draw_rule(f, area, panel.y - 1, label, border_style, border_type);
        f.render_widget(Paragraph::new(detail), panel);
    }
    layout.rows.height
}

/// The option pane's bottom border in `room` columns: on the left the
/// overlay answering for the port, on the right its staged changes against
/// the saved baseline and, while the pane has the focus and the cursor is on
/// an option, its place among the port's options. When they do not fit, the
/// overlay is cut first, then the changes lose their start: the place stays.
fn editor_footer(
    app: &App,
    info: &PortInfo,
    origin: &str,
    room: usize,
) -> (Option<String>, Option<String>) {
    let label = crate::config::overlay_of(&app.overlays, origin).map(|o| o.label());
    let changes = app
        .session
        .state(info)
        .map(|s| crate::session::staged_diff(&s.baseline, &s.staged))
        .filter(|d| !d.is_empty());
    let options = || app.editor_rows.iter().filter(|r| matches!(r, EditorRow::Option(_)));
    let place = match app.editor_rows.get(app.editor_idx) {
        Some(EditorRow::Option(_)) if app.focus == Focus::Editor => {
            let at = app.editor_rows[..=app.editor_idx]
                .iter()
                .filter(|r| matches!(r, EditorRow::Option(_)))
                .count();
            Some(format!("{at}/{}", options().count()))
        }
        _ => None,
    };
    footer_parts(label.as_deref(), changes.as_deref(), place.as_deref(), room)
}

/// The parts of the option pane's bottom border, fitted to `room` columns
/// with at least one column of border between them: ` overlay: LABEL ` and
/// ` CHANGES · I/N `. The label is cut first, down to ` overlay: X… `; then
/// the changes lose their start to a `…`; the left part goes last, when not
/// even that leaves it room.
fn footer_parts(
    label: Option<&str>,
    changes: Option<&str>,
    place: Option<&str>,
    room: usize,
) -> (Option<String>, Option<String>) {
    let width = |s: &str| Line::from(s).width();
    let right = |changes: Option<&str>, cut: usize| -> Option<String> {
        match (changes, place) {
            (None, None) => None,
            (Some(c), None) => Some(fit_start(" ", c, " ", cut)),
            (None, Some(p)) => Some(format!(" {p} ")),
            (Some(c), Some(p)) => Some(fit_start(" ", c, &format!(" · {p} "), cut)),
        }
    };
    let full_right = right(changes, usize::MAX);
    let r = full_right.as_deref().map_or(0, width);
    let Some(label) = label else {
        return (None, right(changes, room));
    };
    let head = " overlay: ";
    let full_left = format!("{head}{label} ");
    let gap = usize::from(r > 0);
    if width(&full_left) + gap + r <= room {
        return (Some(full_left), full_right);
    }
    // The shortest the label is cut to: its first character and `…`.
    let first: String = label.chars().take(1).collect();
    let least = width(head) + width(&first) + 2;
    if least + gap + r <= room {
        let cut = fit_end(label, room - gap - r - width(head) - 1);
        return (Some(format!("{head}{cut} ")), full_right);
    }
    let left = format!("{head}{} ", fit_end(label, width(&first) + 1));
    let shortest = right(changes.map(|_| ""), usize::MAX).as_deref().map_or(0, width);
    if least + gap + shortest + usize::from(changes.is_some()) <= room {
        return (Some(left), right(changes, room - least - gap));
    }
    (None, right(changes, room))
}

/// How a key is advertised on the pane's borders (`[h] notes`, `[v] full text`).
fn key_hint() -> Style {
    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
}

/// Content rows the option rows always keep: a panel that would leave fewer
/// shrinks, or goes.
const MIN_ROWS: u16 = 4;

/// Height of the description panel, whatever the port says: a fixed size
/// keeps the option rows where they are from one port to the next.
const ABOUT_LINES: u16 = 5;

/// Width of the option pane's text from which the detail panel takes one line
/// rather than two: 97% of the ports' option descriptions fit one line there
/// (13683 surveyed in /usr/ports, 2026-10-23), against 86% at an 80-column
/// terminal's 44.
const DETAIL_ONE_LINE_WIDTH: u16 = 56;

/// Height of the option detail panel in a pane `width` columns wide,
/// whichever option the cursor is on (or none): one line on a wide pane, two
/// on a narrow one. It follows the terminal's size, never the option, so
/// nothing moves as the cursor does; a description longer than the panel is
/// cut, and `i` has it all.
fn detail_lines(width: u16) -> u16 {
    if width >= DETAIL_ONE_LINE_WIDTH { 1 } else { 2 }
}

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
/// scroll under whatever is left. The detail panel is `detail_lines`, and goes
/// when the rows would keep fewer than MIN_ROWS. The description panel, when
/// `about`, takes ABOUT_LINES out of what is then left above MIN_ROWS: it
/// shrinks only on a short pane, and goes when not even a line fits.
fn editor_layout(inner: Rect, about: bool) -> EditorLayout {
    let mut rows = inner;
    let lines = detail_lines(inner.width);
    let detail = (inner.height >= lines + 1 + MIN_ROWS).then(|| {
        rows.height -= lines + 1;
        Rect { y: inner.y + inner.height - lines, height: lines, ..inner }
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
/// panel under it describes), or a key hint on the right, as `[h] notes` sits
/// on the pane's title.
enum RuleLabel {
    Title(Span<'static>),
    Hint(Span<'static>),
}

/// A rule across the pane at row `y`, joining its side borders.
fn draw_rule(
    f: &mut Frame,
    pane: Rect,
    y: u16,
    label: Option<RuleLabel>,
    style: Style,
    border: BorderType,
) {
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
    // Its ends join the pane's sides, double ones while it has the focus.
    let (left, right) = if border == BorderType::Double { ("╟", "╢") } else { ("├", "┤") };
    let mut spans = vec![Span::styled(left, style)];
    spans.extend(middle);
    spans.push(Span::styled(right, style));
    f.render_widget(Paragraph::new(Line::from(spans)), Rect { y, height: 1, ..pane });
}

/// What the port is: its COMMENT, then its pkg-descr `text` re-wrapped to
/// `width`, one paragraph after the other with no blank line between them,
/// a bulleted line starting a line of its own
/// (the panel is short; the `v` overlay keeps the file's own layout). A
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
    shorten_for_marker(last, width);
    last.spans.push(Span::styled("…", Style::default().fg(Color::DarkGray)));
    true
}

/// Cut `line` to `width` columns when it is wider, at a word boundary, and
/// end it with `…` in the style of the text it follows; true when it was
/// cut. A pane cuts its rows at its right edge, mid-word and silently: cut
/// here, a row says there is more.
fn ellipsize(line: &mut Line<'static>, width: usize) -> bool {
    if line.width() <= width {
        return false;
    }
    shorten_for_marker(line, width);
    let style = line.spans.last().map_or_else(Style::default, |s| s.style);
    line.spans.push(Span::styled("…", style.remove_modifier(Modifier::REVERSED)));
    true
}

/// A border title (a blank at each end) fitted to `room` columns: cut at a
/// word with `…` when it is wider, its closing blank kept.
fn fit_title(title: &str, room: usize) -> String {
    fit_title_line(Line::from(title.to_string()), room).to_string()
}

/// The same for a title of styled spans: the `…` takes the style of the
/// text it follows, the closing blank is plain.
fn fit_title_line(mut line: Line<'static>, room: usize) -> Line<'static> {
    if line.width() <= room {
        return line;
    }
    // The closing blank goes, and a span it leaves with nothing.
    let mut n = line.spans.len();
    while let Some(span) = line.spans.last_mut() {
        let text = span.content.trim_end().to_string();
        if text.is_empty() && n > 1 {
            line.spans.pop();
            n -= 1;
            continue;
        }
        span.content = text.into();
        break;
    }
    ellipsize(&mut line, room.saturating_sub(1));
    line.spans.push(Span::raw(" "));
    line
}

/// `head`, `text` and `tail` in `room` columns: whole when they fit, else
/// `text` loses its start to a `…` (` filter: …ginx-module▏ `), so its end
/// stays in view. With not even room for `…`, the text goes.
fn fit_start(head: &str, text: &str, tail: &str, room: usize) -> String {
    let width = |s: &str| Line::from(s).width();
    let full = format!("{head}{text}{tail}");
    if width(&full) <= room {
        return full;
    }
    let budget = room.saturating_sub(width(head) + width(tail) + 1);
    // The longest end of `text` that fits the budget.
    let mut kept = String::new();
    for c in text.chars().rev() {
        let mut next = c.to_string();
        next.push_str(&kept);
        if width(&next) > budget {
            break;
        }
        kept = next;
    }
    if width(head) + width(tail) + 1 > room {
        return format!("{head}{tail}");
    }
    format!("{head}…{kept}{tail}")
}

/// `text` in `room` columns, its end cut to a `…` when it is wider.
fn fit_end(text: &str, room: usize) -> String {
    let width = |s: &str| Line::from(s).width();
    if width(text) <= room {
        return text.to_string();
    }
    let mut kept = String::new();
    for c in text.chars() {
        kept.push(c);
        if width(&kept) + 1 > room {
            kept.pop();
            break;
        }
    }
    format!("{}…", kept.trim_end())
}

/// Shorten `line` so a one-column marker still fits in `width`: back to the
/// last word boundary when that cuts a word, keeping a word cut in two only
/// when it is the line's first; no blank is left before the marker.
fn shorten_for_marker(line: &mut Line<'static>, width: usize) {
    // The character the shortening stopped before, if it cut into a span.
    let mut next: Option<char> = None;
    while line.width() + 1 > width {
        let Some(span) = line.spans.last_mut() else { break };
        let mut text = span.content.to_string();
        let Some(c) = text.pop() else {
            line.spans.pop();
            continue;
        };
        next = Some(c);
        span.content = text.into();
    }
    // A word cut in two goes whole, when the line has another before it.
    if next.is_some_and(|c| !c.is_whitespace()) {
        let n = line.spans.len();
        let before = line.spans[..n.saturating_sub(1)].iter().any(|s| !s.content.trim().is_empty());
        if let Some(span) = line.spans.last_mut() {
            let text = span.content.as_ref();
            if !text.ends_with(' ') {
                if let Some(end) =
                    text.rfind(' ').filter(|&e| before || !text[..e].trim().is_empty())
                {
                    span.content = text[..end].to_string().into();
                }
            }
        }
    }
    // The blanks the cut left at the end go, and a span left with nothing.
    let mut n = line.spans.len();
    while let Some(span) = line.spans.last_mut() {
        let text = span.content.trim_end();
        if text.is_empty() && n > 1 {
            line.spans.pop();
            n -= 1;
            continue;
        }
        if text.len() != span.content.len() {
            span.content = text.to_string().into();
        }
        break;
    }
}

/// The flavors of this port, marking the current one (`‹`) and the one whose
/// view owns the shared options file (`*`), blank-separated; a flavor the
/// closure holds — one `f` reaches — is underlined, an attribute rather than
/// a colour, so it reads in a pane without focus and without colour. None
/// when the port is unflavored.
fn flavor_summary(app: &App, info: &PortInfo) -> Option<Vec<Span<'static>>> {
    if info.flavors.is_empty() {
        return None;
    }
    let owner = app.session.owner_info(info).canonical.flavor.clone();
    let current = info.canonical.flavor.clone();
    let present: Vec<Option<String>> =
        app.session.siblings(&info.canonical).into_iter().map(|k| k.flavor).collect();
    let mut spans = Vec::new();
    for (i, f) in info.flavors.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        let style = if present.iter().any(|p| p.as_deref() == Some(f.as_str())) {
            Style::default().add_modifier(Modifier::UNDERLINED)
        } else {
            Style::default()
        };
        spans.push(Span::styled(f.clone(), style));
        // The marks stay out of the underline: they are not the name.
        let mut marks = String::new();
        if owner.as_deref() == Some(f.as_str()) {
            marks.push('*');
        }
        if current.as_deref() == Some(f.as_str()) {
            marks.push('‹');
        }
        if !marks.is_empty() {
            spans.push(Span::raw(marks));
        }
    }
    Some(spans)
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

/// The lines above a port's option rows saying what is wrong with it or
/// whose view it is, in a pane `width` columns wide. A CONFLICT, which names
/// what make.conf did to it, or a STALE too long for one line wraps to a
/// second, under its text; the rest is cut with `…`.
fn banner_lines(app: &App, info: &PortInfo, width: usize, lines: &mut Vec<Line<'static>>) {
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
    for c in app.session.conflicts(info) {
        let text = match &c.cause {
            Some(cause) => format!("{} — {cause}", c.text),
            None => c.text,
        };
        lines.extend(wrapped_banner("CONFLICT: ", &text, red, width));
    }
    if let Some(mut text) = stale_text(app, info) {
        // A port the list shows as done (≈ or ⊘) is no alarm here either: the
        // banner is dimmed and says what settles it.
        let style = if app.effective_status(info) == UiStatus::Ok {
            text.push_str(settled_by(app, info));
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD)
        };
        lines.extend(wrapped_banner("STALE: ", &text, style, width));
    }
}

/// Why a stale port needs nothing done, for its dimmed STALE banner.
fn settled_by(app: &App, info: &PortInfo) -> &'static str {
    if app.is_blacklisted(info) {
        " — blacklisted"
    } else if app.minimal {
        " — settled by the defaults (-m)"
    } else {
        " — settled by make.conf (m view)"
    }
}

/// What a stale port's file is behind on, `new A B · obsolete X`, each part
/// only when it has options; None when the port is not stale.
fn stale_text(app: &App, info: &PortInfo) -> Option<String> {
    let (added, removed) = app.session.stale_delta(info)?;
    let mut parts = Vec::new();
    if !added.is_empty() {
        parts.push(format!("new {}", added.join(" ")));
    }
    if !removed.is_empty() {
        parts.push(format!("obsolete {}", removed.join(" ")));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// A banner line `label` + `text` in `style`, wrapped under its text to a
/// second line when too long for one, and cut at a word with `…` past that.
fn wrapped_banner(label: &str, text: &str, style: Style, width: usize) -> Vec<Line<'static>> {
    let mut wrapped: Vec<Line<'static>> = wrap_text(&format!("{label}{text}"), width, label.len())
        .into_iter()
        .map(|t| Line::from(Span::styled(t, style)))
        .collect();
    mark_if_clipped(&mut wrapped, 2, width);
    if let Some(mark) = wrapped.last_mut().and_then(|l| l.spans.last_mut()) {
        // The marker in the banner's colour, not the panels' gray.
        if mark.content == "…" {
            mark.style = style;
        }
    }
    wrapped
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

    // 'N' in the first column: the saved file does not settle the option,
    // as the STALE line and the list's +N count it (Session::stale_delta).
    // A column of its own rather than a badge, so the eye finds every new
    // option down the left edge of the pane.
    let is_new = state.and_then(|s| s.saved.as_ref()).is_some_and(|saved| !saved.decides(opt));
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

    // make.conf column (fixed width): what make.conf says of the option,
    // whatever decides its value now — the options file included — as the
    // last layer naming it says, in that layer's colour: green for the
    // global knob, bright green for the per-port one, red bold for a *_FORCE
    // knob the options file cannot override. Blank only when no make.conf
    // layer names the option.
    let (mc, mc_style) = match opts.mc_value(opt) {
        Some((v, McLayer::Force)) => {
            (on_off(v), Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
        }
        Some((v, McLayer::Port)) => (on_off(v), Style::default().fg(Color::LightGreen)),
        Some((v, McLayer::Global)) => (on_off(v), Style::default().fg(Color::Green)),
        None => ("", Style::default()),
    };
    spans.push(Span::styled(format!(" {mc:<3}"), mc_style));

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
    }

    // The columns always stay: a row still wider than the pane cuts the
    // last one's own text at the edge, never leaving a `…` after a whole
    // column.
    let mut line = Line::from(spans);
    cut_last_column(&mut line, avail);

    // A description with only a few columns left would show as a word
    // fragment: it goes instead — the panel under the rows gives it in
    // full, wrapped — and one too long ends in `…` at a word, or goes when
    // not even its first two words fit: `Install…` says nothing the name
    // did not.
    let desc = opts.defs.get(opt).map_or("", |d| d.desc.as_str());
    let room = avail.saturating_sub(line.width() + 2);
    if !desc.is_empty() && room >= DESC_MIN_WIDTH {
        let mut text = Line::from(desc.to_string());
        let cut = ellipsize(&mut text, room);
        let kept = text.spans.first().map_or("", |s| s.content.as_ref()).to_string();
        // A cut that kept only part of a word, or a single word, drops the
        // description.
        let whole = !cut || (desc[kept.len()..].starts_with(' ') && kept.trim().contains(' '));
        if whole && !kept.is_empty() {
            let shown = if cut { format!("  {kept}…") } else { format!("  {kept}") };
            line.spans.push(Span::styled(shown, Style::default().fg(Color::DarkGray)));
        }
    }
    line
}

/// Fit an option row's columns in `width`: the blanks padding its end go,
/// then the column crossing the edge — the last one, unless those before it
/// are already wider than the pane — has its own text cut, ending in `…`.
/// The cut never leaves a column whole before the `…`: one cut down to its
/// leading blank goes, and the column before it loses a character instead.
fn cut_last_column(line: &mut Line<'static>, width: usize) {
    if line.width() <= width {
        return;
    }
    let mut n = line.spans.len();
    while let Some(span) = line.spans.last_mut() {
        let text = span.content.trim_end().to_string();
        if text.is_empty() && n > 1 {
            line.spans.pop();
            n -= 1;
            continue;
        }
        span.content = text.into();
        break;
    }
    if line.width() <= width || width == 0 {
        return;
    }
    // The span the edge falls in, with `…` kept inside the width.
    let mut before = 0;
    let mut at = 0;
    while at < line.spans.len() && before + line.spans[at].width() < width {
        before += line.spans[at].width();
        at += 1;
    }
    line.spans.truncate(at + 1);
    loop {
        let Some(span) = line.spans.last_mut() else { return };
        let own = span.content.trim_end().to_string();
        // Its own text up to the edge, one column for `…`, and always at
        // least a character short of whole.
        let keep = Line::from(own.as_str()).width().min(width - before).saturating_sub(1);
        let mut kept = String::new();
        for c in own.chars() {
            kept.push(c);
            if Line::from(kept.as_str()).width() > keep {
                kept.pop();
                break;
            }
        }
        if !kept.trim().is_empty() {
            span.content = format!("{}…", kept.trim_end()).into();
            return;
        }
        // Nothing of its own text would show: cut into the column before.
        line.spans.pop();
        let Some(prev) = line.spans.last() else { return };
        before -= prev.width();
    }
}

/// The keys the status bar names, whatever the pane.
const LEGEND_ALWAYS: [&str; 3] = ["?:help", "a:apply", "q:quit"];
/// The keys it names after them while the port list has the focus.
const LEGEND_LIST: [&str; 5] =
    ["Enter:edit", "n/p:next-problem", "t:attention", "/:filter", "v:description"];
/// The keys it names after them while the option pane has the focus.
const LEGEND_OPTIONS: [&str; 6] =
    ["Space:toggle", "i:info", "d:defaults", "U:revert", "u:undo", "n/p:next-problem"];

/// The legend for the focused pane in `width` columns, each token after a
/// space, cut at a whole token with `…` standing for the rest.
fn key_legend(focus: &Focus, width: usize) -> String {
    let pane: &[&str] = if *focus == Focus::Editor { &LEGEND_OPTIONS } else { &LEGEND_LIST };
    let tokens: Vec<&str> = LEGEND_ALWAYS.iter().chain(pane).copied().collect();
    let mut out = String::new();
    for (i, tok) in tokens.iter().enumerate() {
        // Room for " …" unless this is the last token.
        let room = if i + 1 == tokens.len() { 0 } else { 2 };
        if out.chars().count() + 1 + tok.chars().count() + room > width {
            out.push_str(" …");
            break;
        }
        out.push(' ');
        out.push_str(tok);
    }
    out
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    let line = if let Some((msg, is_err)) = &app.message {
        let style = if *is_err {
            Style::default().fg(Color::Black).bg(Color::LightRed)
        } else {
            Style::default().fg(Color::Black).bg(Color::LightGreen)
        };
        let msg = match &app.summary {
            Some(summary) => summary.fit(width.saturating_sub(2)),
            None => msg.clone(),
        };
        Line::from(Span::styled(format!(" {msg} "), style))
    } else {
        // Each port by its state, indexed in UiStatus declaration order
        // (Conflict first); the edited ones again, apart.
        let mut counts = [0usize; 5];
        let mut edited = 0;
        for key in &app.visible {
            let info = &app.session.ports[key];
            counts[app.effective_status(info) as usize] += 1;
            edited += usize::from(app.session.edited(info));
        }
        let mc = if app.warn_mc { format!(" {}≠", counts[3]) } else { String::new() };
        let mut spans = vec![Span::styled(
            format!(
                " {}✗ {edited}* {}? {}!{mc} {}✓ ",
                counts[0], counts[1], counts[2], counts[4]
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
        // The counts are the status and come whole; the legend has the rest.
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        spans.push(Span::styled(
            key_legend(&app.focus, width.saturating_sub(used)),
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

/// A scrollbar on the right border of a popup whose `rows` of content do not
/// fit in it, `scroll` rows down: `▲` and `▼` end it, and between them the
/// thumb says how much shows and where, so a cut text never looks whole; a
/// box under three rows high gets the arrows alone.
/// Nothing when it all fits. The thumb's length is the share in view,
/// rounded once, so it keeps its size as it moves; the border is the track.
fn scroll_indicator(f: &mut Frame, area: Rect, rows: usize, scroll: usize, border: Style) {
    let view = area.height.saturating_sub(2) as usize;
    if rows <= view || view == 0 || area.width == 0 {
        return;
    }
    let x = area.x + area.width - 1;
    let top = area.y + 1;
    let max = rows - view;
    let buf = f.buffer_mut();
    // Too short for a thumb between the arrows: the arrows alone, or on a
    // single row the way there is still to go.
    if view < 3 {
        if view == 2 {
            buf[(x, top)].set_symbol("▲").set_style(border);
            buf[(x, top + 1)].set_symbol("▼").set_style(border);
        } else {
            let arrow = if scroll < max { "▼" } else { "▲" };
            buf[(x, top)].set_symbol(arrow).set_style(border);
        }
        return;
    }
    buf[(x, top)].set_symbol("▲").set_style(border);
    buf[(x, top + view as u16 - 1)].set_symbol("▼").set_style(border);
    let (track_top, track) = (top + 1, view - 2);
    let thumb = ((track * view + rows / 2) / rows).clamp(1, track);
    // At the top when scrolled to the top, at the bottom when to the bottom.
    let start = (scroll.min(max) * (track - thumb) + max / 2) / max;
    for y in start..start + thumb {
        buf[(x, track_top + y as u16)].set_symbol("█").set_style(border);
    }
}

fn draw_apply_modal(f: &mut Frame, app: &mut App) {
    let area = centered_rect(80, 70, f.area());
    let inner_h = area.height.saturating_sub(2);
    app.page.apply = inner_h;
    let dir = &app.options_dir;
    let Some(modal) = &mut app.modal else { return };
    f.render_widget(Clear, area);
    let width = area.width.saturating_sub(2);

    let mut lines: Vec<Line> = Vec::new();
    // Rows of the entries scrolled past (j/k skip whole entries).
    let mut skipped = 0;
    if let Some(done) = modal.done.as_ref().filter(|d| !d.failed.is_empty()) {
        // Nothing written: the directory once, as the preview gives it,
        // then each port's reason, its paths cut to fit.
        let head = |dir: &str| format!("nothing written to {dir}");
        let dir = dir.display().to_string();
        let room = (width as usize).saturating_sub(head("").chars().count());
        lines.push(Line::from(head(&elide_dir(&dir, room))));
        for (key, msg) in &done.failed {
            lines.push(Line::from(elide_paths(&format!("{key}: {msg}"), width as usize)));
        }
    } else if let Some(done) = &modal.done {
        for l in done.lines(&dir.display().to_string()) {
            lines.push(Line::from(l));
        }
    } else {
        for w in &modal.warnings {
            lines.push(Line::from(Span::styled(
                format!("note: {w}"),
                Style::default().fg(Color::Magenta),
            )));
        }
        let removed = if modal.removals.is_empty() {
            String::new()
        } else {
            format!(", {} removed (--minimal)", modal.removals.len())
        };
        let head = |dir: &str| format!("{} options file(s) will be written to {dir}{removed}:", modal.writes.len());
        let dir = dir.display().to_string();
        // One row: the directory gives way, its last two components kept.
        let room = (width as usize).saturating_sub(head("").chars().count());
        lines.push(Line::from(head(&elide_dir(&dir, room))));
        lines.push(Line::default());

        let conflicting = |name: &str| modal.conflicting_files.contains(name);
        let write_line = |w: &PendingWrite| {
            Line::from(vec![
                Span::styled(format!("{:<40}", w.key.to_string()), Style::default().fg(Color::White)),
                Span::styled(w.describe(), Style::default().fg(Color::DarkGray)),
            ])
        };
        let removal_line = |r: &crate::clean::Removal| {
            Line::from(vec![
                Span::styled(format!("rm {:<37}", r.options_name), Style::default().fg(Color::Yellow)),
                Span::styled(r.reason.clone(), Style::default().fg(Color::DarkGray)),
            ])
        };
        let mut entries: Vec<Line> = Vec::new();
        if !modal.conflicting_files.is_empty() {
            entries.push(Line::from(Span::styled(
                "conflicting (Y writes them, s skips them):",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )));
            entries.extend(modal.removals.iter().filter(|r| conflicting(&r.options_name)).map(removal_line));
            entries.extend(modal.writes.iter().filter(|w| conflicting(&w.options_name)).map(write_line));
            entries.push(Line::default());
        }
        entries.extend(modal.removals.iter().filter(|r| !conflicting(&r.options_name)).map(removal_line));
        entries.extend(modal.writes.iter().filter(|w| !conflicting(&w.options_name)).map(write_line));
        // Scrolled no further than it takes to bring the last entry in:
        // past that only blank rows would come into view.
        let heights: Vec<usize> =
            entries.iter().map(|e| wrapped_height(std::slice::from_ref(e), width)).collect();
        let head_rows = wrapped_height(&lines, width);
        let mut rest: usize = heights.iter().sum();
        let mut last_page = 0;
        while last_page < entries.len() && head_rows + rest > inner_h as usize {
            rest -= heights[last_page];
            last_page += 1;
        }
        modal.scroll = modal.scroll.min(last_page);
        skipped = heights[..modal.scroll].iter().sum();
        lines.extend(entries.into_iter().skip(modal.scroll));
    }

    let failed = modal.done.as_ref().is_some_and(|d| !d.failed.is_empty());
    let (title, keys) = match (&modal.done, modal.conflicted.len()) {
        (Some(_), _) if failed => (" apply — FAILED ".to_string(), " any key closes ".to_string()),
        (Some(_), _) => (" apply — done ".to_string(), " any key closes ".to_string()),
        (None, 0) => (
            " apply? ".to_string(),
            fitting(width, &[" y: write · n: cancel · j/k scroll ", " y write · n cancel · j/k "]),
        ),
        (None, n) => (
            format!(" apply with {n} conflicting? "),
            fitting(
                width,
                &[
                    " Y: write anyway · s: skip conflicting · n: cancel · j/k scroll ",
                    " Y write · s skip · n cancel ",
                ],
            ),
        ),
    };
    // j/k scroll by whole entries, skipped from the list: count their rows in.
    let rows = skipped + wrapped_height(&lines, width);
    let border = Style::default().fg(if failed { Color::Red } else { Color::LightYellow });
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_bottom(Line::from(Span::styled(keys, Style::default().fg(Color::DarkGray))))
            .border_style(border),
    );
    f.render_widget(p, area);
    scroll_indicator(f, area, rows, skipped, border);
}

/// `dir` in at most `room` columns: whole when it fits, else its leading
/// components elided (`…/poudriere.d/server-options`, then
/// `…/server-options`), keeping as many as fit. The last component is kept
/// even when it alone does not fit.
fn elide_dir(dir: &str, room: usize) -> String {
    if dir.chars().count() <= room {
        return dir.to_string();
    }
    let parts: Vec<&str> = dir.trim_end_matches('/').split('/').collect();
    // `…/` and the components kept, from the most to the last alone.
    let elided = |keep: usize| format!("…/{}", parts[parts.len() - keep..].join("/"));
    let most = parts.len().saturating_sub(2);
    if most == 0 {
        return dir.to_string(); // nothing to elide
    }
    (1..=most)
        .rev()
        .map(elided)
        .find(|e| e.chars().count() <= room)
        .unwrap_or_else(|| elided(1))
}

/// `text` with each absolute path in it (a word starting with `/`) cut to
/// its last components by `elide_dir`, so the whole fits `room` columns as
/// far as the paths can make it; 0 keeps their last component alone.
pub(crate) fn elide_paths(text: &str, room: usize) -> String {
    let mut out = text.to_string();
    let width = |s: &str| s.chars().count();
    let mut from = 0;
    while let Some(at) = out[from..].find('/').map(|i| from + i) {
        // A path starts a word.
        if at > 0 && !out[..at].ends_with(char::is_whitespace) {
            from = at + 1;
            continue;
        }
        let end = out[at..].find(char::is_whitespace).map_or(out.len(), |i| at + i);
        // A colon or a comma after it is the text's, not the path's.
        let end = at + out[at..end].trim_end_matches([':', ',', ';', ')']).len();
        let path = &out[at..end];
        let others = width(&out) - width(path);
        let short = elide_dir(path, room.saturating_sub(others));
        out.replace_range(at..end, &short);
        from = at + short.len();
    }
    out
}

/// Help overlay tab titles; indices match `App.help_tab`.
pub const HELP_TABS: [&str; 4] = ["Markers", "Option row", "Navigate/Edit", "Views/Actions"];

/// The help's tabs with their number keys, the current one highlighted, for
/// a border `width` columns wide: every name when they fit, else the current
/// one named and the others by number.
fn help_tab_bar(tab: usize, width: usize) -> Line<'static> {
    let labels = |all: bool| -> Vec<String> {
        HELP_TABS
            .iter()
            .enumerate()
            .map(|(i, name)| match (i == tab, all) {
                // The highlight takes a blank each side of the current tab.
                (true, _) => format!(" {}:{name} ", i + 1),
                (false, true) => format!("{}:{name}", i + 1),
                (false, false) => format!("{}", i + 1),
            })
            .collect()
    };
    // A blank before each tab and after the last, against the corners.
    let fits = |l: &[String]| l.iter().map(|t| t.chars().count() + 1).sum::<usize>() < width;
    let full = labels(true);
    let shown = if fits(&full) { full } else { labels(false) };
    let mut spans = Vec::new();
    for (i, label) in shown.into_iter().enumerate() {
        spans.push(Span::raw(" "));
        spans.push(if i == tab {
            Span::styled(label, Style::default().fg(Color::Black).bg(Color::LightBlue))
        } else {
            Span::styled(label, Style::default().fg(Color::Gray))
        });
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

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

/// `lead` then `text` word-wrapped to `width` columns, each further row
/// indented to start under the text's first column, not under the lead.
fn hang(lead: Vec<Span<'static>>, text: &str, width: usize) -> Vec<Line<'static>> {
    let indent: usize = lead.iter().map(Span::width).sum();
    let rows = wrap_text(text, width.saturating_sub(indent).max(8), 0);
    let mut rows = rows.into_iter();
    let mut first = lead;
    first.push(Span::raw(rows.next().unwrap_or_default()));
    let mut lines = vec![Line::from(first)];
    lines.extend(rows.map(|r| Line::from(format!("{}{r}", " ".repeat(indent)))));
    lines
}

/// Draw the help on tab `tab`; returns how many rows of text it showed.
fn draw_help(f: &mut Frame, tab: usize, scroll: &mut u16) -> u16 {
    let area = centered_rect(78, 72, f.area());
    f.render_widget(Clear, area);
    let width = area.width.saturating_sub(2) as usize;

    // Every line is a lead (a key, a marker, a label) and a description
    // wrapped under its own first column.
    let key = |k: &str| Span::styled(format!("{k:<12}"), Style::default().fg(Color::LightBlue));
    let keyline = |k: &str, txt: &str| hang(vec![Span::raw("  "), key(k)], txt, width);
    // A marker, then a name in a column of its own when there is one, the
    // description wrapped under what follows it.
    let mark = |m: &str, c: Color, name: &str, txt: &str| {
        let mut lead = vec![Span::styled(format!("  {m:<3}"), Style::default().fg(c))];
        if !name.is_empty() {
            lead.push(Span::raw(format!("{name:<13}")));
        }
        hang(lead, txt, width)
    };
    // Name-color legend: the label rendered in its actual style, aligned on '='.
    let legend = |label: &str, style: Style, txt: &str| {
        // The padding stays out of the style: an underline or a strike
        // marks the label, not the blanks before it.
        let pad = 14usize.saturating_sub(label.chars().count());
        let lead = vec![
            Span::raw(" ".repeat(2 + pad)),
            Span::styled(label.to_string(), style),
            Span::styled(" = ", Style::default().fg(Color::DarkGray)),
        ];
        hang(lead, txt, width)
    };
    let head = |t: &str| {
        vec![Line::from(Span::styled(
            format!(" {t}"),
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        ))]
    };
    let blank = || vec![Line::default()];
    let mut lines: Vec<Line> = Vec::new();
    match tab {
        0 => lines.extend([
            head("First column: the port's state"),
            mark("✗", Color::Red, "conflict", "staged options violate PREVENTS or group rules"),
            mark("?", Color::Yellow, "unconfigured", "no saved options file"),
            mark("!", Color::LightRed, "stale", "option list changed since the file was written"),
            mark("≠", Color::Magenta, "mc-conflict", "contradicts make.conf policy (w view)"),
            mark("≈", Color::DarkGray, "mc-covered", "decided by make.conf, no attention (m view)"),
            mark("✓", Color::DarkGray, "", "ok"),
            blank(),
            head("Second column"),
            mark("*", Color::Cyan, "edited", "staged changes not yet applied (?*)"),
            blank(),
            head("Between the groups"),
            hang(
                vec![Span::styled("  ── N ok ── ", Style::default().fg(Color::DarkGray))],
                "parts the N ok ports (≈ ✓) from the rest",
                width,
            ),
            blank(),
            head("After the port name"),
            mark("⚠", Color::Red, "", "port is BROKEN/IGNORE with the current options"),
            mark("⊘", Color::DarkGray, "", "blacklisted for this jail/tree/set (never needs attention)"),
            mark("∞", Color::Red, "", "caught in a dependency loop — r lists it, Enter jumps to any port"),
            mark("+N", Color::Yellow, "", "N options added since its file was written (N in the pane), even when settled (≈)"),
            blank(),
            head("Port name color"),
            legend(
                "red bold name",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                "will not build (BROKEN/IGNORE)",
            ),
            legend("red name", Style::default().fg(Color::Red), "conflicting staged options"),
            legend("dim name", Style::default().fg(Color::DarkGray), "blacklisted"),
        ]
        .into_iter()
        .flatten()),
        1 => {
            // A realistic sample row, styled exactly like the editor renders it.
            lines.extend(head("A row, piece by piece"));
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
                    "2nd column: what make.conf says, whatever decides",
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
                lines.extend(legend(label, style, txt));
            }
            lines.extend([
                blank(),
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
                blank(),
                head("In the pane's title"),
                legend(
                    "flavor*",
                    Style::default(),
                    "the flavor whose view owns the shared options file",
                ),
                legend("flavor‹", Style::default(), "the flavor shown in the pane"),
                legend(
                    "flavor",
                    Style::default().add_modifier(Modifier::UNDERLINED),
                    "underlined: in the closure, f reaches it; plain: not held",
                ),
                legend("[h] notes", key_hint(), "the port ships notes (pkg-help) — h reads them"),
                blank(),
                head("On the pane's bottom border"),
                legend("overlay: NAME", Style::default(), "the overlay the port is taken from"),
                legend("+A -B", Style::default().fg(Color::Cyan), "staged changes, not yet applied"),
                legend("3/12", Style::default().fg(Color::Cyan), "the cursor's option, while the pane has the focus"),
                blank(),
                head("Above the rows"),
                legend(
                    "port panel",
                    Style::default().add_modifier(Modifier::BOLD),
                    "the port's comment and pkg-descr, in five lines; shown at start on a \
                     terminal of 30 rows or more — V shows or hides it until you quit, v \
                     shows it all, from either pane",
                ),
                blank(),
                head("Below the rows"),
                legend(
                    "option panel",
                    Style::default().fg(Color::Gray),
                    "the description of the option under the cursor, in 1–2 lines — i on \
                     it shows it all",
                ),
            ]
            .into_iter()
            .flatten());
        }
        2 => lines.extend([
            keyline("j/k ↑/↓", "move · g/G first/last · the same keys scroll every popup"),
            keyline("PgUp/PgDn", "page up / down · also Ctrl-B / Ctrl-F"),
            keyline("Enter", "edit selected port · Esc/Tab/← back to the list"),
            keyline("Space", "toggle option (group rules enforced), in the option pane"),
            keyline("d / U", "reset port to defaults / revert to saved state"),
            keyline("u", "undo the last change on any port: Space, d, U, B, a restored draft"),
            keyline("B", "bulk: set an option on/off across the listed ports · Ctrl-U clears"),
            keyline("n / p", "next / previous port needing attention · Enter, and n/p in the pane, put the cursor on its first option needing it"),
            keyline("f", "jump to the next flavor of the same origin"),
            keyline("i", "option details (description, constraints, deps it adds)"),
            keyline("v", "the port's full description (comment, WWW, pkg-descr)"),
            keyline("V", "show / hide the port description above the options"),
            keyline("h", "port notes (pkg-help) — marked [h] notes in the pane title"),
            keyline("r", "why is this port here? — chain, loop, dependents; Enter jumps"),
        ]
        .into_iter()
        .flatten()),
        _ => lines.extend([
            keyline("t", "show only ports needing attention"),
            keyline("s", "toggle problems-first / stable alphabetical sort"),
            keyline("m", "make.conf-decided ports count as ok (≈)"),
            keyline("w", "flag make.conf contradictions (≠)"),
            keyline("L", "show only the ports caught in a dependency loop (∞)"),
            keyline("/", "filter the port list · Enter or ↑/↓ keep it · Esc clears it"),
            keyline("a", "apply: preview, then y writes · conflicts: Y anyway, s skips them"),
            keyline("? F1", "this help"),
            keyline("Ctrl-L", "force a full screen repaint"),
            keyline("q Ctrl-C", "quit — offers keeping staged edits as a draft"),
        ]
        .into_iter()
        .flatten()),
    }

    // Clamp the scroll to what actually overflows the inner area, counted in
    // the rows the lines wrap to: a narrow terminal wraps many of them.
    let inner_height = area.height.saturating_sub(2) as usize;
    let rows = wrapped_height(&lines, area.width.saturating_sub(2));
    let max_scroll = rows.saturating_sub(inner_height) as u16;
    *scroll = (*scroll).min(max_scroll);
    // Fitted like every popup's hint: never cut mid-word on a narrow box.
    let room = area.width.saturating_sub(2);
    let hint = if max_scroll > 0 {
        fitting(room, &[
            " 1-4/Tab/←→ switch · ↑/↓ scroll · other key closes ",
            " 1-4/Tab switch · j/k scroll · other closes ",
            " 1-4 · j/k · other closes ",
        ])
    } else {
        fitting(room, &[" 1-4/Tab/←→ switch · any other key closes ", " 1-4/Tab switch · other closes "])
    };
    let border = Style::default().fg(Color::LightBlue);
    // The tabs head the box, out of the scrolled text: they stay in view.
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((*scroll, 0)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(help_tab_bar(tab, area.width.saturating_sub(2) as usize))
            .title_bottom(Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray))))
            .border_style(border),
    );
    f.render_widget(p, area);
    scroll_indicator(f, area, rows, *scroll as usize, border);
    inner_height as u16
}

/// Cap on the dependents listed before collapsing the rest into "+ N more".
pub(crate) const WHY_MAX_DEPENDENTS: usize = 15;

fn draw_why(f: &mut Frame, app: &mut App) {
    let area = centered_rect(70, 60, f.area());
    let inner_height = area.height.saturating_sub(2) as usize;
    app.page.why = inner_height as u16;
    let Some(why) = &mut app.why else { return };
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

    // A loop's tangle or the dependents can outrun the box: the view moves
    // only as far as it takes to keep the cursor's row in it, counting the
    // rows the lines above wrap to. The first entry shows the top, the last
    // one the end, so what heads and closes the text is reached too.
    let width = area.width.saturating_sub(2);
    let cursor_row = wrapped_height(&lines[..selected_line], width);
    let rows = wrapped_height(&lines, width);
    let last_page = rows.saturating_sub(inner_height);
    let mut scroll = why.scroll.min(last_page).min(cursor_row);
    if cursor_row >= scroll + inner_height {
        scroll = cursor_row + 1 - inner_height.max(1);
    }
    if why.selected == 0 && cursor_row < inner_height {
        scroll = 0;
    }
    if why.selected + 1 >= row && rows - cursor_row <= inner_height {
        scroll = last_page;
    }
    why.scroll = scroll;
    let hint = fitting(
        width,
        &[
            " Enter: jump · r: why of selection · j/k move · other key closes ",
            " Enter jump · r why · j/k move · other key closes ",
            " Enter jump · r why · j/k ",
        ],
    );
    let border = Style::default().fg(Color::LightBlue);
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((clamp_u16(scroll), 0)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" why {}? ", why.key))
            .title_bottom(Line::from(Span::styled(hint, dim)))
            .border_style(border),
    );
    f.render_widget(p, area);
    scroll_indicator(f, area, rows, scroll, border);
}

/// The first of `hints` that fits on a border `width` columns wide between
/// its corners, else the last (the shortest) cut at a whole token (`·`
/// parts them) to end in `…`. With not even its first token in, that token
/// is cut at a word.
fn fitting(width: u16, hints: &[&str]) -> String {
    let room = width as usize;
    let fits = |h: &str| Line::from(h).width() <= room;
    if let Some(hint) = hints.iter().find(|h| fits(h)) {
        return hint.to_string();
    }
    let Some(last) = hints.last() else { return String::new() };
    let tokens: Vec<&str> = last.trim().split(" · ").collect();
    let cut = (1..tokens.len())
        .rev()
        .map(|keep| format!(" {} … ", tokens[..keep].join(" · ")))
        .find(|h| fits(h));
    cut.unwrap_or_else(|| {
        let mut line = Line::from(format!(" {}", tokens[0]));
        ellipsize(&mut line, room.saturating_sub(1));
        format!("{line} ")
    })
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
fn draw_opt_info(f: &mut Frame, app: &mut App) {
    let area = centered_rect(72, 66, f.area());
    let inner_height = area.height.saturating_sub(2);
    app.page.opt_info = inner_height;
    let Some(shown) = &app.opt_info else { return };
    let (key, opt) = (&shown.key, &shown.opt);
    let Some(info) = app.session.ports.get(key) else { return };
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
            Span::styled(format!(" — {}", g.kind.label()), dim),
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

    let rows = wrapped_height(&lines, area.width.saturating_sub(2));
    let last_page = clamp_u16(rows.saturating_sub(inner_height as usize));
    let scroll = shown.scroll.min(last_page);
    let hint = if last_page > 0 { " j/k scroll · any other key closes " } else { " any key closes " };
    let border = Style::default().fg(Color::LightBlue);
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll, 0)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {opt} — {key} "))
            .title_bottom(Line::from(Span::styled(hint, dim)))
            .border_style(border),
    );
    f.render_widget(p, area);
    scroll_indicator(f, area, rows, scroll as usize, border);
    if let Some(shown) = &mut app.opt_info {
        shown.scroll = scroll;
    }
}

/// The bullets a line of a pkg-help or pkg-descr can start with.
const BULLETS: [&str; 3] = ["- ", "* ", "• "];

/// A pkg-help or the port's description as the popup lays it out, before
/// wrapping: consecutive plain lines join into one paragraph, which wraps to
/// the box, and blank lines separate paragraphs. A line that starts with
/// whitespace or a bullet, that looks preformatted (a run of two spaces or
/// more inside it, a table say), or a `WWW:` line keeps a line of its own.
/// Two spaces after a full stop, `!`, `?` or `:` are typing, not layout: a
/// tenth of the ports' pkg-descr files space their sentences so. Tabs are
/// expanded first, so a tab inside a line counts as a run.
fn reflow(src: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // Whether the last line out is a paragraph that takes the next one.
    let mut open = false;
    for line in src {
        let line = line.replace('\t', "        ");
        let line = line.trim_end();
        let kept = line.starts_with(char::is_whitespace)
            || BULLETS.iter().any(|b| line.starts_with(b))
            || line.starts_with("WWW:")
            || laid_out(line);
        match out.last_mut() {
            Some(para) if open && !kept && !line.is_empty() => {
                para.push(' ');
                para.push_str(line);
            }
            _ => out.push(line.to_string()),
        }
        open = !kept && !line.is_empty();
    }
    out
}

/// Whether a run of two spaces or more inside `line` lays it out (a table,
/// aligned columns), rather than ending a sentence.
fn laid_out(line: &str) -> bool {
    let mut prev = None;
    let mut run = 0;
    for c in line.chars() {
        if c == ' ' {
            run += 1;
            if run == 2 && !matches!(prev, Some('.' | '!' | '?' | ':')) {
                return true;
            }
        } else {
            run = 0;
            prev = Some(c);
        }
    }
    false
}

/// One line of the popup's text, ready for the box: tabs expanded, blank
/// lines preserved, its own indentation kept and its wrap hung under it
/// (under the text of a bullet). Both indents are capped at half the width —
/// a deeply indented line (a code sample, say) must still wrap inside the
/// box instead of running off it.
fn help_line(src: &str, width: usize) -> Vec<String> {
    let src = src.replace('\t', "        ");
    if src.trim().is_empty() {
        return vec![String::new()];
    }
    let text = src.trim_start();
    let indent = (src.len() - text.len()).min(width / 2);
    let bullet = if BULLETS.iter().any(|b| text.starts_with(b)) { 2 } else { 0 };
    let hang = (indent + bullet).min(width.saturating_sub(1));
    wrap_text(&format!("{}{}", " ".repeat(indent), text), width, hang)
}

/// The pkg-help and description popup: as tall as before, and wide enough
/// for an 80-column line where the screen allows — 76% of it, at least 84
/// columns, never closer than two columns to either edge.
fn port_help_rect(r: Rect) -> Rect {
    let width = ((r.width as u32 * 76 / 100) as u16).max(84).min(r.width.saturating_sub(4));
    Rect { x: r.x + (r.width - width) / 2, width, ..centered_rect(76, 70, r) }
}

/// The port's pkg-help file or description, scrollable. These are prose
/// files written for an 80-column terminal, hard-wrapped: their paragraphs
/// are joined and wrapped to the box, while whatever has a layout of its
/// own keeps its lines (see `reflow`).
fn draw_port_help(f: &mut Frame, app: &mut App) {
    let area = port_help_rect(f.area());
    let inner_w = area.width.saturating_sub(2) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;
    app.page.port_help = inner_h as u16;
    let Some(help) = &mut app.port_help else { return };
    f.render_widget(Clear, area);

    let lines: Vec<Line> =
        reflow(&help.lines).iter().flat_map(|src| help_line(src, inner_w)).map(Line::from).collect();

    let max_scroll = lines.len().saturating_sub(inner_h) as u16;
    help.scroll = help.scroll.min(max_scroll);
    let hint = if max_scroll > 0 {
        " j/k ↑/↓ PgUp/PgDn scroll · any other key closes "
    } else {
        " any key closes "
    };
    let rows = lines.len();
    let border = Style::default().fg(Color::LightBlue);
    let p = Paragraph::new(lines).scroll((help.scroll, 0)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} — {} ", help.title, help.key))
            .title_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .title_bottom(Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray))))
            .border_style(border),
    );
    f.render_widget(p, area);
    scroll_indicator(f, area, rows, help.scroll as usize, border);
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
    let room = area.width.saturating_sub(2) as usize;
    let title = fit_title(&format!(" {} ", app.bulk_scope()), room);
    // Under the line, kept current as it is typed: how many listed ports
    // carry the option named so far, then the keys.
    let (count, missing) = match app.bulk_carriers() {
        None => (String::new(), None),
        Some((opt, 0)) => {
            let none = format!("no listed port has {opt}");
            (format!("{none} · "), Some(none))
        }
        Some((opt, n)) => (format!("{n} carry {opt} · "), None),
    };
    let hint = fitting(
        clamp_u16(room),
        &[
            &format!(" {count}Enter apply · Esc cancel · Ctrl-U clear "),
            &format!(" {count}Enter · Esc · ^U clear "),
        ],
    );
    // An option no listed port carries is said in red, as far as it shows.
    let hint_line = match missing.and_then(|none| Some((hint.strip_prefix(&format!(" {none}"))?, none))) {
        Some((rest, none)) => Line::from(vec![
            Span::styled(format!(" {none}"), Style::default().fg(Color::Red)),
            Span::raw(rest.to_string()),
        ]),
        None => Line::from(hint),
    };
    let p = Paragraph::new(Line::from(format!(" {input}▏"))).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_bottom(hint_line)
            .border_style(Style::default().fg(Color::LightBlue)),
    );
    f.render_widget(p, area);
}

/// The keys of the quit prompt, on one line when it fits, else split so
/// that no answer is cut.
const QUIT_KEYS: [&str; 2] = ["y save & quit · n discard & quit", "Esc stay"];

/// The quit prompt: a box 52 columns wide at most, 4 less than the screen
/// at least, as high as its lines need, in the middle of the screen.
fn draw_quit_confirm(f: &mut Frame) {
    let full = f.area();
    let width = 52.min(full.width.saturating_sub(4));
    let inner = width.saturating_sub(2);
    let mut lines = vec![Line::from("Keep staged edits as a draft?")];
    let keys = QUIT_KEYS.join(" · ");
    if keys.chars().count() <= inner as usize {
        lines.push(Line::from(keys));
    } else {
        lines.extend(QUIT_KEYS.map(Line::from));
    }
    let active = crate::query::makerunner::active_make_count();
    if active > 0 {
        lines.push(Line::from(Span::styled(
            format!("{active} background make process(es) running — stopped on quit"),
            Style::default().fg(Color::DarkGray),
        )));
    }
    let height = (wrapped_height(&lines, inner) as u16 + 2).min(full.height);
    let area = Rect {
        x: full.x + (full.width - width) / 2,
        y: full.y + (full.height - height) / 2,
        width,
        height,
    };
    f.render_widget(Clear, area);
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title(" quit ").border_style(Style::default().fg(Color::Red)));
    f.render_widget(p, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory too long for its row keeps its last two components.
    #[test]
    fn a_long_options_dir_keeps_its_last_components() {
        let dir = "/usr/local/etc/poudriere.d/server-options";
        assert_eq!(elide_dir(dir, 80), dir, "it fits");
        assert_eq!(elide_dir(dir, dir.len()), dir);
        assert_eq!(elide_dir(dir, 30), "…/poudriere.d/server-options");
        assert_eq!(elide_dir(dir, 20), "…/server-options", "down to the last");
        assert_eq!(elide_dir(dir, 5), "…/server-options", "never shorter");
        assert_eq!(elide_dir("/var/db/ports/", 10), "…/db/ports");
        assert_eq!(elide_dir("/db/ports", 4), "…/ports");
        assert_eq!(elide_dir("/ports", 3), "/ports", "nothing to elide");
    }

    /// A path in a text gives way to fit, down to its last component; the
    /// colon after it stays the text's.
    #[test]
    fn paths_in_a_text_keep_their_last_components() {
        let text = "creating /var/db/ports/cat_alpha/.tmp: Is a directory";
        assert_eq!(elide_paths(text, 80), text, "it fits");
        assert_eq!(elide_paths(text, 45), "creating …/cat_alpha/.tmp: Is a directory");
        assert_eq!(elide_paths(text, 0), "creating …/.tmp: Is a directory");
        assert_eq!(elide_paths("a/b and/or c", 0), "a/b and/or c", "only words starting with /");
    }

    /// A hint is the first form that fits, else the last cut at a whole
    /// token, else its first token cut at a word.
    #[test]
    fn a_hint_is_fitted_at_a_token() {
        let forms = [" alpha: one · beta: two · gamma: three ", " a one · b two · c three "];
        assert_eq!(fitting(40, &forms), forms[0]);
        assert_eq!(fitting(25, &forms), forms[1]);
        assert_eq!(fitting(20, &forms), " a one · b two … ");
        assert_eq!(fitting(12, &forms), " a one … ");
        assert_eq!(fitting(6, &forms), " a… ");
    }

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
        assert_eq!(detail, Rect::new(1, 27, 38, 2), "two lines on a narrow pane");
        assert_eq!(l.rows.y, about.y + about.height + 1, "a rule between");
        assert_eq!(l.rows.y + l.rows.height + 1, detail.y, "a rule between");
        assert_eq!(l.rows.height, 28 - ABOUT_LINES - 1 - 2 - 1);
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

        // A wide pane gives the detail one line, whatever the option.
        let wide = Rect::new(1, 1, DETAIL_ONE_LINE_WIDTH, 28);
        assert_eq!(editor_layout(wide, false).detail.map(|d| d.height), Some(1));
        let narrower = Rect { width: DETAIL_ONE_LINE_WIDTH - 1, ..wide };
        assert_eq!(editor_layout(narrower, false).detail.map(|d| d.height), Some(2));
    }

    /// A box too short for a thumb between the arrows keeps the arrows.
    #[test]
    fn a_short_popup_keeps_the_arrows() {
        let bar = |height: u16, scroll: usize| -> String {
            let rows = rendered(10, height, |f| {
                scroll_indicator(f, Rect::new(0, 0, 10, height), 50, scroll, Style::default())
            });
            rows.iter().map(|r| r.chars().last().unwrap_or(' ')).collect()
        };
        assert_eq!(bar(4, 0).trim(), "▲▼");
        assert_eq!(bar(3, 0).trim(), "▼", "one row: more below");
        assert_eq!(bar(3, 49).trim(), "▲", "one row, at the bottom");
        assert!(bar(5, 0).contains('█'), "three rows: arrows and a thumb");
    }

    /// The help's tabs head its box, in view however far it is scrolled,
    /// and name only the current tab when all the names do not fit.
    #[test]
    fn the_help_tabs_stay_in_view() {
        let mut scroll = u16::MAX;
        let shown = rendered(80, 24, |f| { draw_help(f, 0, &mut scroll); });
        let top = shown.iter().find(|r| r.contains('┌')).expect("the box");
        assert!(top.contains("1:Markers") && top.contains("4:Views/Actions"), "{top}");
        assert!(scroll > 0, "scrolled");
        assert!(shown.iter().any(|r| r.contains("↑/↓ scroll")), "the keys at the bottom");

        let narrow = rendered(60, 24, |f| { draw_help(f, 2, &mut 0); });
        let top = narrow.iter().find(|r| r.contains('┌')).expect("the box");
        assert!(top.contains("3:Navigate/Edit") && !top.contains("Markers"), "{top}");
        assert!(top.contains(" 1 2 ") && top.contains(" 4 "), "{top}");
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
        // A normal line keeps its own indent and wraps under it.
        assert_eq!(help_line("  alpha beta gamma", 12), ["  alpha beta", "  gamma"]);
        // A bullet hangs its wrap under its text.
        assert_eq!(help_line("- alpha beta gamma", 12), ["- alpha beta", "  gamma"]);
    }

    /// A hard-wrapped paragraph is joined into one; a blank line ends it.
    #[test]
    fn reflow_joins_a_hard_wrapped_paragraph() {
        let src = |t: &str| t.lines().map(str::to_string).collect::<Vec<_>>();
        assert_eq!(
            reflow(&src("Dovecot uses its own daemon\nfor authentication, so\nno library.\n\nNext one.")),
            ["Dovecot uses its own daemon for authentication, so no library.", "", "Next one."]
        );
        // Two spaces between sentences are not a table (mail/postfix's
        // pkg-help); a run inside a sentence still is.
        assert_eq!(
            reflow(&src("for authentication.  Because there is
no library.")),
            ["for authentication.  Because there is no library."]
        );
        assert!(laid_out("name    value"));
        assert!(!laid_out("one.  Two?  Three:  four"));
    }

    /// A bulleted list, an indented block, a table and WWW: lines keep their
    /// lines, and a paragraph after them starts afresh.
    #[test]
    fn reflow_keeps_lists_blocks_and_tables() {
        let src: Vec<String> = [
            "Use it to:",
            "- avoid this",
            "* avoid that",
            "• and so on",
            "then a paragraph",
            "that goes on",
            "",
            "  % postconf -a",
            "  % postconf -A",
            "\tindented by a tab",
            "NAME    VALUE",
            "WWW: https://example.org/",
            "last",
        ]
        .map(String::from)
        .into();
        assert_eq!(
            reflow(&src),
            [
                "Use it to:",
                "- avoid this",
                "* avoid that",
                "• and so on",
                "then a paragraph that goes on",
                "",
                "  % postconf -a",
                "  % postconf -A",
                "        indented by a tab",
                "NAME    VALUE",
                "WWW: https://example.org/",
                "last",
            ]
        );
    }

    /// The pkg-help popup is 76% of the screen, at least 84 columns so an
    /// 80-column line fits, and two columns short of either edge at most.
    #[test]
    fn the_pkg_help_popup_fits_an_80_column_line() {
        let width = |w: u16| port_help_rect(Rect::new(0, 0, w, 40)).width;
        assert_eq!(width(100), 84, "wider than 76% of 100");
        assert_eq!(width(88), 84, "an 80-column line fits inside the borders and a blank");
        assert_eq!(width(80), 76, "two columns short of each edge");
        assert_eq!(width(200), 152, "76%");
        let r = port_help_rect(Rect::new(0, 0, 100, 40));
        assert_eq!((r.x, r.x + r.width), (8, 92), "centered");
        assert_eq!(r.height, centered_rect(76, 70, Rect::new(0, 0, 100, 40)).height, "height as before");
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
            .map(|r| r.trim().trim_matches(['│', '█', '▲', '▼']).trim().to_string())
            .rfind(|r| !r.is_empty() && !r.starts_with('└') && !r.starts_with('┌'))
            .unwrap_or_default()
    }

    /// Scrolled to its end, every help tab shows its last line on an 80x30
    /// terminal, where the box wraps lines the scroll limit must count.
    #[test]
    fn every_help_tab_scrolls_to_its_last_line_on_a_small_terminal() {
        for tab in 0..HELP_TABS.len() {
            // As tall as it takes to show it all, at the same width.
            let whole = rendered(80, 200, |f| { draw_help(f, tab, &mut 0); });
            let last = last_text_row(&whole);
            let mut scroll = u16::MAX;
            let small = rendered(80, 30, |f| { draw_help(f, tab, &mut scroll); });
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

    /// The legend starts with the keys that always hold and is cut at a
    /// whole token, `…` for the rest.
    #[test]
    fn the_key_legend_is_cut_at_a_whole_token() {
        assert_eq!(key_legend(&Focus::List, 22), " ?:help a:apply …");
        assert_eq!(key_legend(&Focus::List, 33), " ?:help a:apply q:quit …");
        assert_eq!(key_legend(&Focus::List, 35), " ?:help a:apply q:quit Enter:edit …");
        assert_eq!(
            key_legend(&Focus::Editor, 200),
            " ?:help a:apply q:quit Space:toggle i:info d:defaults U:revert u:undo n/p:next-problem"
        );
    }

    /// A line or a title too wide ends in `…` after a whole word, its first
    /// word kept, cut, when it is the only one; one that fits is untouched.
    #[test]
    fn fit_start_keeps_the_end() {
        assert_eq!(fit_start(" filter: ", "nginx▏", " ", 40), " filter: nginx▏ ");
        let cut = fit_start(" filter: ", "www/nginx-full-ginx-module▏", " ", 23);
        assert_eq!(cut, " filter: …ginx-module▏ ");
        assert_eq!(Line::from(cut.as_str()).width(), 23);
    }

    /// The overlay is cut first, then the changes lose their start; the
    /// place always shows.
    #[test]
    fn the_footer_cuts_the_overlay_first_then_the_changes() {
        let both = |room| footer_parts(Some("myoverlay"), Some("+AAA +BBB -CCC"), Some("3/12"), room);
        assert_eq!(
            both(60),
            (Some(" overlay: myoverlay ".into()), Some(" +AAA +BBB -CCC · 3/12 ".into()))
        );
        // 20 + 1 + 23 = 44 columns whole; at 40 the label gives way.
        assert_eq!(both(40), (Some(" overlay: myov… ".into()), Some(" +AAA +BBB -CCC · 3/12 ".into())));
        // Down to ` overlay: m… `, then the changes are cut at their start.
        assert_eq!(both(30), (Some(" overlay: m… ".into()), Some(" …B -CCC · 3/12 ".into())));
        // No room for the label at all: the right part alone.
        assert_eq!(both(18), (None, Some(" …BBB -CCC · 3/12 ".into())));
        assert_eq!(footer_parts(None, None, None, 40), (None, None));
        assert_eq!(footer_parts(Some("o"), None, None, 40), (Some(" overlay: o ".into()), None));
        assert_eq!(footer_parts(None, None, Some("1/2"), 40), (None, Some(" 1/2 ".into())));
        assert_eq!(footer_parts(None, Some("+A"), None, 40), (None, Some(" +A ".into())));
    }

    #[test]
    fn ellipsize_cuts_at_a_word() {
        let cut = |spans: Vec<Span<'static>>, width: usize| {
            let mut line = Line::from(spans);
            ellipsize(&mut line, width);
            line.to_string()
        };
        assert_eq!(cut(vec![Span::raw("one two three")], 13), "one two three");
        assert_eq!(cut(vec![Span::raw("one two three")], 12), "one two…");
        assert_eq!(cut(vec![Span::raw("one two three")], 9), "one two…", "no blank before the marker");
        assert_eq!(cut(vec![Span::raw("onetwothree")], 6), "onetw…", "the only word, cut");
        // Across spans: a description's first word goes whole too.
        assert_eq!(cut(vec![Span::raw("  [x] DOCS"), Span::raw("  Build docs")], 16), "  [x] DOCS…");
        assert_eq!(fit_title(" Ports (3) — filter: abc ", 25), " Ports (3) — filter: abc ");
        assert_eq!(fit_title(" Ports (3) — filter: abc ", 21), " Ports (3) —… ");
        assert_eq!(fit_title(" Ports (3) — filter: abc ", 22), " Ports (3) — filter:… ");
        assert!(Line::from(fit_title(" Ports (3) — filter: abc ", 20)).width() <= 20);
    }

    /// A legend's style marks its label alone: the blanks right-aligning it
    /// are neither underlined (`flavor`) nor struck through.
    #[test]
    fn a_help_legend_styles_its_label_not_its_padding() {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 80)).unwrap();
        terminal.draw(|f| { draw_help(f, 1, &mut 0); }).unwrap();
        let buffer = terminal.backend().buffer();
        let rows = screen_rows(buffer);
        for (label, modifier) in [
            (" flavor = underlined", Modifier::UNDERLINED),
            (" struck-through = ", Modifier::CROSSED_OUT),
        ] {
            let (y, row) = rows.iter().enumerate().find(|(_, r)| r.contains(label)).expect(label);
            let x = row[..row.find(label).unwrap()].chars().count() as u16;
            let styled = |dx: u16| buffer[(x + dx, y as u16)].modifier.contains(modifier);
            assert!(!styled(0), "the blank before {label:?} is plain: {row}");
            assert!(styled(1), "{label:?} itself is styled: {row}");
        }
    }

    /// The help names the pane title's flavor glyphs and the keys as they
    /// are: V for the panel, Ctrl-U in the bulk prompt, u and U, Ctrl-B/F.
    #[test]
    fn the_help_names_the_flavor_glyphs_and_the_current_keys() {
        let text = |tab: usize| rendered(200, 80, |f| { draw_help(f, tab, &mut 0); }).join("\n");
        let row = text(1);
        assert!(row.contains("flavor* = the flavor whose view owns the shared options file"), "{row}");
        assert!(row.contains("flavor‹ = the flavor shown in the pane"), "{row}");
        assert!(row.contains("flavor = underlined: in the closure, f reaches it"), "{row}");
        assert!(row.contains("[h] notes = the port ships notes (pkg-help)"), "{row}");
        assert!(row.contains("30 rows or more") && row.contains("V shows or hides it until you quit"), "{row}");
        let keys = text(2);
        assert!(keys.contains("toggle option (group rules enforced), in the option pane"), "{keys}");
        for want in ["Ctrl-B / Ctrl-F", "Ctrl-U clears", "  u ", "d / U", "  V ", "  v ", "  h "] {
            assert!(keys.contains(want), "{want:?} in\n{keys}");
        }
        assert!(!keys.contains("  I "), "I is no key");
    }
}
