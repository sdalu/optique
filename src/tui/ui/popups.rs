//! The popups: apply, help, why, option info, port help, bulk, quit.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget as _, Wrap};
use ratatui::Frame;

use crate::apply::PendingWrite;
use crate::model::options::{on_off, Provenance};
use crate::tui::{clamp_u16, App};

use super::editor::file_state;
use super::{centered_rect, ellipsize, fit_title, key_hint, scroll_indicator, wrap_text};

pub(super) fn draw_apply_modal(f: &mut Frame, app: &mut App) {
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
pub(super) fn elide_dir(dir: &str, room: usize) -> String {
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
pub(super) fn draw_help(f: &mut Frame, tab: usize, scroll: &mut u16) -> u16 {
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
                    "*_FORCE, WITH or OPTIONS_OVERRIDE — locked, the file cannot override",
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

pub(super) fn draw_why(f: &mut Frame, app: &mut App) {
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
pub(super) fn fitting(width: u16, hints: &[&str]) -> String {
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
        Provenance::Override => "make.conf OPTIONS_OVERRIDE (locked)",
        Provenance::Forced => "make.conf *_FORCE (locked)",
    }
}

/// Everything the framework says about one option, including the dependencies
/// enabling it pulls in. Looked up fresh on every frame.
pub(super) fn draw_opt_info(f: &mut Frame, app: &mut App) {
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
                // An override has make read no file: it, not the file, is
                // the source (bsd.options.mk:300-304).
                match app.session.unmanaged(info).filter(|_| opts.r#override.is_none()) {
                    Some(u) => format!("make, {} not managed (locked)", u.by),
                    None => provenance_label(opts.provenance(opt, file_state(app, info, opt))).to_string(),
                }
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
pub(super) fn reflow(src: &[String]) -> Vec<String> {
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
pub(super) fn laid_out(line: &str) -> bool {
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
pub(super) fn help_line(src: &str, width: usize) -> Vec<String> {
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
pub(super) fn port_help_rect(r: Rect) -> Rect {
    let width = ((r.width as u32 * 76 / 100) as u16).max(84).min(r.width.saturating_sub(4));
    Rect { x: r.x + (r.width - width) / 2, width, ..centered_rect(76, 70, r) }
}

/// The port's pkg-help file or description, scrollable. These are prose
/// files written for an 80-column terminal, hard-wrapped: their paragraphs
/// are joined and wrapped to the box, while whatever has a layout of its
/// own keeps its lines (see `reflow`).
pub(super) fn draw_port_help(f: &mut Frame, app: &mut App) {
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
pub(super) fn draw_bulk(f: &mut Frame, app: &App) {
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
pub(super) fn draw_quit_confirm(f: &mut Frame) {
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
