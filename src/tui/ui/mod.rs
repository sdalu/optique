//! Drawing the TUI: the two panes, the status bar and the popups over
//! them, with the fitting helpers they share.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{BorderType, Paragraph};

use super::{App, Focus, OverlayKind};

mod editor;
mod help;
mod list;
mod popups;

pub(crate) use help::HELP_TABS;
#[cfg(test)]
pub(crate) use list::fit_port_name;
pub(crate) use popups::{WHY_MAX_DEPENDENTS, elide_paths};

use editor::draw_editor;
use help::draw_help;
use list::draw_port_list;
use popups::{
    draw_apply_modal, draw_bulk, draw_opt_info, draw_port_help, draw_quit_confirm, draw_why,
};

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
        .map(|y| {
            (0..w)
                .map(|x| cells[y * w + x].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
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
    let shortest = right(changes.map(|_| ""), usize::MAX)
        .as_deref()
        .map_or(0, width);
    if least + gap + shortest + usize::from(changes.is_some()) <= room {
        return (Some(left), right(changes, room - least - gap));
    }
    (None, right(changes, room))
}

/// How a key is advertised on the pane's borders (`[h] notes`, `[v] full text`).
fn key_hint() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

/// Greedy word wrap; continuation lines are indented by `indent`. A word
/// longer than the line is hard-split rather than left to overflow.
fn wrap_text(text: &str, width: usize, indent: usize) -> Vec<String> {
    let width = width.max(indent + 1);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut first = true;
    for word in text.split_whitespace() {
        let lead = if first {
            text.len() - text.trim_start().len()
        } else {
            indent
        };
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
    let Some(last) = lines.last_mut() else {
        return true;
    };
    shorten_for_marker(last, width);
    last.spans
        .push(Span::styled("…", Style::default().fg(Color::DarkGray)));
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
    line.spans
        .push(Span::styled("…", style.remove_modifier(Modifier::REVERSED)));
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
        let Some(span) = line.spans.last_mut() else {
            break;
        };
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
        let before = line.spans[..n.saturating_sub(1)]
            .iter()
            .any(|s| !s.content.trim().is_empty());
        if let Some(span) = line.spans.last_mut() {
            let text = span.content.as_ref();
            if !text.ends_with(' ')
                && let Some(end) = text
                    .rfind(' ')
                    .filter(|&e| before || !text[..e].trim().is_empty())
            {
                span.content = text[..end].to_string().into();
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

/// The keys the status bar names, whatever the pane.
const LEGEND_ALWAYS: [&str; 3] = ["?:help", "a:apply", "q:quit"];
/// The keys it names after them while the port list has the focus.
const LEGEND_LIST: [&str; 5] = [
    "Enter:edit",
    "n/p:next-problem",
    "t:attention",
    "/:filter",
    "v:description",
];
/// The keys it names after them while the option pane has the focus.
const LEGEND_OPTIONS: [&str; 6] = [
    "Space:toggle",
    "i:info",
    "d:defaults",
    "U:revert",
    "u:undo",
    "n/p:next-problem",
];

/// The legend for the focused pane in `width` columns, each token after a
/// space, cut at a whole token with `…` standing for the rest.
fn key_legend(focus: &Focus, width: usize) -> String {
    let pane: &[&str] = if *focus == Focus::Editor {
        &LEGEND_OPTIONS
    } else {
        &LEGEND_LIST
    };
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
        let mc = if app.warn_mc {
            format!(" {}≠", counts[3])
        } else {
            String::new()
        };
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
            let tangled = app
                .visible
                .iter()
                .filter(|key| app.loop_of(key).is_some())
                .count();
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
    buf[(x, top + view as u16 - 1)]
        .set_symbol("▼")
        .set_style(border);
    let (track_top, track) = (top + 1, view - 2);
    let thumb = ((track * view + rows / 2) / rows).clamp(1, track);
    // At the top when scrolled to the top, at the bottom when to the bottom.
    let start = (scroll.min(max) * (track - thumb) + max / 2) / max;
    for y in start..start + thumb {
        buf[(x, track_top + y as u16)]
            .set_symbol("█")
            .set_style(border);
    }
}

#[cfg(test)]
mod tests;
