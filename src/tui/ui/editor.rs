//! The option editor pane: its banner, rules, option rows and detail.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::model::options::{McLayer, on_off};
use crate::model::port::PortInfo;
use crate::session::UiStatus;
use crate::tui::{App, EditorRow, Focus};

use super::{
    ellipsize, fit_title_line, footer_parts, key_hint, mark_if_clipped, pane_border, wrap_text,
};

/// Draw the option pane; returns how many option rows it showed.
pub(super) fn draw_editor(f: &mut Frame, app: &App, area: Rect) -> u16 {
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

    let option_rows = row_lines(app, info, inner_width, &mut lines);

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
    let selected_line =
        (banner + (app.editor_idx + 1).min(app.editor_rows.len())).saturating_sub(1);
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
        block = block.title_top(Line::from(Span::styled(marker, key_hint())).right_aligned());
    }
    block = block.title(fit_title_line(Line::from(title), room));
    let (left, right) = editor_footer(
        app,
        info,
        &key.origin,
        (area.width as usize).saturating_sub(2),
    );
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
        draw_rule(
            f,
            area,
            panel.y + panel.height,
            hint,
            border_style,
            border_type,
        );
    }
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), layout.rows);
    if let Some(panel) = layout.detail {
        mark_if_clipped(&mut detail, panel.height as usize, inner_width);
        let label = opt.map(|o| {
            RuleLabel::Title(Span::styled(
                format!(" {o} "),
                Style::default().add_modifier(Modifier::BOLD),
            ))
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
    let label = crate::config::overlay_of(&app.overlays, origin).map(crate::config::Overlay::label);
    let changes = app
        .session
        .state(info)
        .map(|s| crate::session::staged_diff(&s.baseline, &s.staged))
        .filter(|d| !d.is_empty());
    let options = || {
        app.editor_rows
            .iter()
            .filter(|r| matches!(r, EditorRow::Option(_)))
    };
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

/// Content rows the option rows always keep: a panel that would leave fewer
/// shrinks, or goes.
pub(super) const MIN_ROWS: u16 = 4;

/// Height of the description panel, whatever the port says: a fixed size
/// keeps the option rows where they are from one port to the next.
pub(super) const ABOUT_LINES: u16 = 5;

/// Width of the option pane's text from which the detail panel takes one line
/// rather than two: 97% of the ports' option descriptions fit one line there
/// (13683 surveyed in /usr/ports, 2026-10-23), against 86% at an 80-column
/// terminal's 44.
pub(super) const DETAIL_ONE_LINE_WIDTH: u16 = 56;

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
pub(super) struct EditorLayout {
    pub(super) about: Option<Rect>,
    pub(super) rows: Rect,
    pub(super) detail: Option<Rect>,
}

/// Lay the editor out in `inner`, the pane without its border, from its size
/// alone, so nothing moves as the cursor or the port changes: the option rows
/// scroll under whatever is left. The detail panel is `detail_lines`, and goes
/// when the rows would keep fewer than MIN_ROWS. The description panel, when
/// `about`, takes ABOUT_LINES out of what is then left above MIN_ROWS: it
/// shrinks only on a short pane, and goes when not even a line fits.
pub(super) fn editor_layout(inner: Rect, about: bool) -> EditorLayout {
    let mut rows = inner;
    let lines = detail_lines(inner.width);
    let detail = (inner.height >= lines + 1 + MIN_ROWS).then(|| {
        rows.height -= lines + 1;
        Rect {
            y: inner.y + inner.height - lines,
            height: lines,
            ..inner
        }
    });
    let room = rows.height.saturating_sub(1 + MIN_ROWS);
    let n = if about { room.min(ABOUT_LINES) } else { 0 };
    let about = (n > 0).then(|| {
        let panel = Rect { height: n, ..rows };
        rows.y += n + 1;
        rows.height -= n + 1;
        panel
    });
    EditorLayout {
        about,
        rows,
        detail,
    }
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
    let width = |s: &Span| s.width();
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
    let (left, right) = if border == BorderType::Double {
        ("╟", "╢")
    } else {
        ("├", "┤")
    };
    let mut spans = vec![Span::styled(left, style)];
    spans.extend(middle);
    spans.push(Span::styled(right, style));
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect {
            y,
            height: 1,
            ..pane
        },
    );
}

/// What the port is: its COMMENT, then its pkg-descr `text` re-wrapped to
/// `width`, one paragraph after the other with no blank line between them,
/// a bulleted line starting a line of its own
/// (the panel is short; the `v` overlay keeps the file's own layout). A
/// trailing `WWW:` line of an old-style pkg-descr is left to the overlay too.
/// A port that says nothing reads "No description available".
pub(super) fn about_lines(info: &PortInfo, text: &str, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if !info.about.comment.is_empty() {
        lines.extend(
            wrap_text(&info.about.comment, width, 0)
                .into_iter()
                .map(|t| {
                    Line::from(Span::styled(
                        t,
                        Style::default().add_modifier(Modifier::BOLD),
                    ))
                }),
        );
    }
    let style = Style::default().fg(Color::Gray);
    for para in text.split("\n\n") {
        // A bullet starts an item of its own, wrapped under its text; any
        // other line joins the item before it.
        let mut items: Vec<(String, usize)> = Vec::new();
        for line in para
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with("WWW:"))
        {
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
    let dim = |t: &str| {
        vec![Line::from(Span::styled(
            t.to_string(),
            Style::default().fg(Color::DarkGray),
        ))]
    };
    if app.focus != Focus::Editor {
        return (None, dim("  Enter: edit the options"));
    }
    let Some(EditorRow::Option(opt)) = app.editor_rows.get(app.editor_idx) else {
        return (None, dim("  no option selected"));
    };
    let desc = info
        .options
        .defs
        .get(opt)
        .map(|d| d.desc.as_str())
        .unwrap_or("");
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
    let present: Vec<Option<String>> = app
        .session
        .siblings(&info.canonical)
        .into_iter()
        .map(|k| k.flavor)
        .collect();
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
/// The editor's rows after the banner, appended to `lines`: group headers,
/// option rows laid out `width` columns wide, the flavor-excluded and
/// obsolete sections. Returns the indices in `lines` of the option rows.
fn row_lines(
    app: &App,
    info: &PortInfo,
    width: usize,
    lines: &mut Vec<Line<'static>>,
) -> Vec<usize> {
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
                let Some(g) = info.options.groups.get(*gi) else {
                    continue;
                };
                let mut text = format!("── {} — {}", g.name, g.kind.label());
                if !g.desc.is_empty() {
                    text.push_str(&format!(": {}", g.desc));
                }
                lines.push(Line::from(Span::styled(
                    text,
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                )));
            }
            // A row cuts itself: its columns stay, its description goes first.
            EditorRow::Option(opt) => {
                option_rows.push(lines.len());
                lines.push(option_line(app, info, opt, selected, name_width, width));
            }
            EditorRow::ExcludedHeader => {
                lines.push(Line::from(Span::styled(
                    "── not in this flavor (managed via the flavor owning the file)",
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
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
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
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
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::CROSSED_OUT),
                    ),
                ]));
            }
        }
    }
    option_rows
}

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
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
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
            format!(
                "options file owned by {} ({})",
                owner.canonical,
                if crate::session::is_preferred(owner) {
                    "default flavor view"
                } else {
                    "widest view: the default flavor is not in the closure"
                }
            ),
            Style::default().fg(Color::DarkGray),
        )));
    }
    for m in app.session.moved_to(info) {
        let reason = if m.reason.is_empty() {
            String::new()
        } else {
            format!(" ({})", m.reason)
        };
        let text = if m.followed {
            format!("listed as {}{reason}; fix the list", m.listed)
        } else {
            format!(
                "listed as {}{reason}, not followed; fix the list{}",
                m.listed,
                m.or_follow("")
            )
        };
        lines.extend(wrapped_banner(
            "MOVED: ",
            &text,
            Style::default().fg(Color::Yellow),
            width,
        ));
    }
    if let Some(u) = app.session.unmanaged(info) {
        lines.extend(wrapped_banner(
            "UNMANAGED: ",
            &u.why,
            Style::default().fg(Color::Yellow),
            width,
        ));
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
            Style::default()
                .fg(Color::LightRed)
                .add_modifier(Modifier::BOLD)
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
pub(super) fn file_state(app: &App, info: &PortInfo, opt: &str) -> Option<bool> {
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
    let is_new = state
        .and_then(|s| s.saved.as_ref())
        .is_some_and(|saved| !saved.decides(opt));
    let mut spans: Vec<Span> = vec![Span::styled(
        if is_new { "N" } else { " " },
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
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
        if selected {
            name_style.add_modifier(Modifier::REVERSED)
        } else {
            name_style
        },
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
    // An OPTIONS_OVERRIDE decides the option as a FORCE knob would, below
    // FORCE and WITH themselves (bsd.options.mk:300-304): locked, red.
    let locked = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
    let overridden = opts.r#override.as_ref().map(|o| o.contains(opt));
    // Blank for a file optique cannot manage: the layers make dumped after
    // reading it may hold its own assignments. Not under an override, which
    // has make read no file at all.
    let unmanaged = overridden.is_none() && app.session.unmanaged(info).is_some();
    let (mc, mc_style) = match (
        opts.mc_value(opt).filter(|_| !unmanaged),
        overridden.filter(|_| !unmanaged),
    ) {
        (Some((v, McLayer::Force | McLayer::With)), _) => (on_off(v), locked),
        (_, Some(v)) => (on_off(v), locked),
        (value, None) => match value {
            Some((v, McLayer::Port)) => (on_off(v), Style::default().fg(Color::LightGreen)),
            Some((v, McLayer::Global)) => (on_off(v), Style::default().fg(Color::Green)),
            _ => ("", Style::default()),
        },
    };
    spans.push(Span::styled(format!(" {mc:<3}"), mc_style));

    // No badge for a make.conf contradiction: the magenta option name above
    // already says it, and the row needs the columns for the description.

    if let Some(by) = app.session.implied_by(info, opt)
        && on
    {
        spans.push(Span::styled(
            format!(" implied by {by}"),
            Style::default().fg(Color::Cyan),
        ));
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
        let kept = text
            .spans
            .first()
            .map_or("", |s| s.content.as_ref())
            .to_string();
        // A cut that kept only part of a word, or a single word, drops the
        // description.
        let whole = !cut || (desc[kept.len()..].starts_with(' ') && kept.trim().contains(' '));
        if whole && !kept.is_empty() {
            let shown = if cut {
                format!("  {kept}…")
            } else {
                format!("  {kept}")
            };
            line.spans
                .push(Span::styled(shown, Style::default().fg(Color::DarkGray)));
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
        let Some(span) = line.spans.last_mut() else {
            return;
        };
        let own = span.content.trim_end().to_string();
        // Its own text up to the edge, one column for `…`, and always at
        // least a character short of whole.
        let keep = Line::from(own.as_str())
            .width()
            .min(width - before)
            .saturating_sub(1);
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
        let Some(prev) = line.spans.last() else {
            return;
        };
        before -= prev.width();
    }
}
