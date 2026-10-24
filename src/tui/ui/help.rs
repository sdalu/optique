//! The help popup: its tabs, and the text of each.

use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use super::popups::{fitting, wrapped_height};
use super::{centered_rect, key_hint, scroll_indicator, wrap_text};

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
            Span::styled(
                label,
                Style::default().fg(Color::Black).bg(Color::LightBlue),
            )
        } else {
            Span::styled(label, Style::default().fg(Color::Gray))
        });
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
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

/// What the help's lines are written with: each a lead (a key, a marker, a
/// label) and a description wrapped under its own first column, `width`
/// columns of text wide.
struct HelpPen {
    width: usize,
}

impl HelpPen {
    /// A key, in its column, and what it does.
    fn keyline(&self, k: &str, txt: &str) -> Vec<Line<'static>> {
        let key = Span::styled(format!("{k:<12}"), Style::default().fg(Color::LightBlue));
        hang(vec![Span::raw("  "), key], txt, self.width)
    }

    /// A marker, then a name in a column of its own when there is one, the
    /// description wrapped under what follows it.
    fn mark(&self, m: &str, c: Color, name: &str, txt: &str) -> Vec<Line<'static>> {
        let mut lead = vec![Span::styled(format!("  {m:<3}"), Style::default().fg(c))];
        if !name.is_empty() {
            lead.push(Span::raw(format!("{name:<13}")));
        }
        hang(lead, txt, self.width)
    }

    /// Name-color legend: the label rendered in its actual style, aligned on '='.
    fn legend(&self, label: &str, style: Style, txt: &str) -> Vec<Line<'static>> {
        // The padding stays out of the style: an underline or a strike
        // marks the label, not the blanks before it.
        let pad = 14usize.saturating_sub(label.chars().count());
        let lead = vec![
            Span::raw(" ".repeat(2 + pad)),
            Span::styled(label.to_string(), style),
            Span::styled(" = ", Style::default().fg(Color::DarkGray)),
        ];
        hang(lead, txt, self.width)
    }

    fn head(&self, t: &str) -> Vec<Line<'static>> {
        vec![Line::from(Span::styled(
            format!(" {t}"),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ))]
    }

    fn blank(&self) -> Vec<Line<'static>> {
        vec![Line::default()]
    }
}

/// The help's text on tab `tab` (`HELP_TABS`), `width` columns wide.
fn help_lines(tab: usize, width: usize) -> Vec<Line<'static>> {
    let p = HelpPen { width };
    match tab {
        0 => help_markers(&p),
        1 => [help_option_row(&p), help_around_rows(&p)].concat(),
        2 => help_navigate(&p),
        _ => help_views(&p),
    }
}

/// Markers: the list's columns and the port name's colors.
fn help_markers(p: &HelpPen) -> Vec<Line<'static>> {
    [
        p.head("First column: the port's state"),
        p.mark(
            "✗",
            Color::Red,
            "conflict",
            "staged options violate PREVENTS or group rules",
        ),
        p.mark("?", Color::Yellow, "unconfigured", "no saved options file"),
        p.mark(
            "!",
            Color::LightRed,
            "stale",
            "option list changed since the file was written",
        ),
        p.mark(
            "≠",
            Color::Magenta,
            "mc-conflict",
            "contradicts make.conf policy (w view)",
        ),
        p.mark(
            "≈",
            Color::DarkGray,
            "mc-covered",
            "decided by make.conf, no attention (m view)",
        ),
        p.mark("✓", Color::DarkGray, "", "ok"),
        p.blank(),
        p.head("Second column"),
        p.mark(
            "*",
            Color::Cyan,
            "edited",
            "staged changes not yet applied (?*)",
        ),
        p.blank(),
        p.head("Between the groups"),
        hang(
            vec![Span::styled(
                "  ── N ok ── ",
                Style::default().fg(Color::DarkGray),
            )],
            "parts the N ok ports (≈ ✓) from the rest",
            p.width,
        ),
        p.blank(),
        p.head("After the port name"),
        p.mark(
            "⚠",
            Color::Red,
            "",
            "port is BROKEN/IGNORE with the current options",
        ),
        p.mark(
            "⊘",
            Color::DarkGray,
            "",
            "blacklisted for this jail/tree/set (never needs attention)",
        ),
        p.mark(
            "∞",
            Color::Red,
            "",
            "caught in a dependency loop — r lists it, Enter jumps to any port",
        ),
        p.mark(
            "+N",
            Color::Yellow,
            "",
            "N options added since its file was written (N in the pane), even when settled (≈)",
        ),
        p.blank(),
        p.head("Port name color"),
        p.legend(
            "red bold name",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            "will not build (BROKEN/IGNORE)",
        ),
        p.legend(
            "red name",
            Style::default().fg(Color::Red),
            "conflicting staged options",
        ),
        p.legend(
            "dim name",
            Style::default().fg(Color::DarkGray),
            "blacklisted",
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Option row: a sample row, then what each of its pieces says.
fn help_option_row(p: &HelpPen) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    // A realistic sample row, styled exactly like the editor renders it.
    lines.extend(p.head("A row, piece by piece"));
    lines.push(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(
            "N",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" [x] ", Style::default()),
        Span::styled("OPENSSL     ", Style::default().fg(Color::Yellow)),
        Span::styled(" off", Style::default().fg(Color::DarkGray)),
        Span::styled(" on ", Style::default().fg(Color::Green)),
        Span::styled("  Use OpenSSL", Style::default().fg(Color::DarkGray)),
    ]));
    lines.push(Line::default());
    for (label, style, txt) in [
        (
            "[x] / [ ]",
            Style::default(),
            "checkbox of a plain option (on / off)",
        ),
        (
            "(o) / ( )",
            Style::default(),
            "single/radio group member (pick one)",
        ),
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
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
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
        lines.extend(p.legend(label, style, txt));
    }
    lines
}

/// Option row, continued: the sections, titles and panels around the rows.
fn help_around_rows(p: &HelpPen) -> Vec<Line<'static>> {
    [
        p.blank(),
        p.head("Trailing sections"),
        p.legend(
            "struck-through",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::CROSSED_OUT),
            "obsolete option — dropped on apply",
        ),
        p.legend(
            "not in flavor",
            Style::default().fg(Color::DarkGray),
            "managed via the default flavor's view of the shared file",
        ),
        p.blank(),
        p.head("In the pane's title"),
        p.legend(
            "flavor*",
            Style::default(),
            "the flavor whose view owns the shared options file",
        ),
        p.legend("flavor‹", Style::default(), "the flavor shown in the pane"),
        p.legend(
            "flavor",
            Style::default().add_modifier(Modifier::UNDERLINED),
            "underlined: in the closure, f reaches it; plain: not held",
        ),
        p.legend(
            "[h] notes",
            key_hint(),
            "the port ships notes (pkg-help) — h reads them",
        ),
        p.blank(),
        p.head("On the pane's bottom border"),
        p.legend(
            "overlay: NAME",
            Style::default(),
            "the overlay the port is taken from",
        ),
        p.legend(
            "+A -B",
            Style::default().fg(Color::Cyan),
            "staged changes, not yet applied",
        ),
        p.legend(
            "3/12",
            Style::default().fg(Color::Cyan),
            "the cursor's option, while the pane has the focus",
        ),
        p.blank(),
        p.head("Above the rows"),
        p.legend(
            "port panel",
            Style::default().add_modifier(Modifier::BOLD),
            "the port's comment and pkg-descr, in five lines; shown at start on a \
                 terminal of 30 rows or more — V shows or hides it until you quit, v \
                 shows it all, from either pane",
        ),
        p.blank(),
        p.head("Below the rows"),
        p.legend(
            "option panel",
            Style::default().fg(Color::Gray),
            "the description of the option under the cursor, in 1–2 lines — i on \
                 it shows it all",
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Navigate/Edit: moving, editing and the popups.
fn help_navigate(p: &HelpPen) -> Vec<Line<'static>> {
    [
        p.keyline("j/k ↑/↓", "move · g/G first/last · the same keys scroll every popup"),
        p.keyline("PgUp/PgDn", "page up / down · also Ctrl-B / Ctrl-F"),
        p.keyline("Enter", "edit selected port · Esc/Tab/← back to the list"),
        p.keyline("Space", "toggle option (group rules enforced), in the option pane"),
        p.keyline("d / U", "reset port to defaults / revert to saved state"),
        p.keyline("u", "undo the last change on any port: Space, d, U, B, a restored draft"),
        p.keyline("B", "bulk: set an option on/off across the listed ports · Ctrl-U clears"),
        p.keyline("n / p", "next / previous port needing attention · Enter, and n/p in the pane, put the cursor on its first option needing it"),
        p.keyline("f", "jump to the next flavor of the same origin"),
        p.keyline("i", "option details (description, constraints, deps it adds)"),
        p.keyline("v", "the port's full description (comment, WWW, pkg-descr)"),
        p.keyline("V", "show / hide the port description above the options"),
        p.keyline("h", "port notes (pkg-help) — marked [h] notes in the pane title"),
        p.keyline("r", "why is this port here? — chain, loop, dependents; Enter jumps"),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Views/Actions: the views of the list, apply, quit.
fn help_views(p: &HelpPen) -> Vec<Line<'static>> {
    [
        p.keyline("t", "show only ports needing attention"),
        p.keyline("s", "toggle problems-first / stable alphabetical sort"),
        p.keyline("m", "make.conf-decided ports count as ok (≈)"),
        p.keyline("w", "flag make.conf contradictions (≠)"),
        p.keyline("L", "show only the ports caught in a dependency loop (∞)"),
        p.keyline(
            "/",
            "filter the port list · Enter or ↑/↓ keep it · Esc clears it",
        ),
        p.keyline(
            "a",
            "apply: preview, then y writes · conflicts: Y anyway, s skips them",
        ),
        p.keyline("? F1", "this help"),
        p.keyline("Ctrl-L", "force a full screen repaint"),
        p.keyline("q Ctrl-C", "quit — offers keeping staged edits as a draft"),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Draw the help on tab `tab`; returns how many rows of text it showed.
pub(super) fn draw_help(f: &mut Frame, tab: usize, scroll: &mut u16) -> u16 {
    let area = centered_rect(78, 72, f.area());
    f.render_widget(Clear, area);
    let lines = help_lines(tab, area.width.saturating_sub(2) as usize);

    // Clamp the scroll to what actually overflows the inner area, counted in
    // the rows the lines wrap to: a narrow terminal wraps many of them.
    let inner_height = area.height.saturating_sub(2) as usize;
    let rows = wrapped_height(&lines, area.width.saturating_sub(2));
    let max_scroll = rows.saturating_sub(inner_height) as u16;
    *scroll = (*scroll).min(max_scroll);
    // Fitted like every popup's hint: never cut mid-word on a narrow box.
    let room = area.width.saturating_sub(2);
    let hint = if max_scroll > 0 {
        fitting(
            room,
            &[
                " 1-4/Tab/←→ switch · ↑/↓ scroll · other key closes ",
                " 1-4/Tab switch · j/k scroll · other closes ",
                " 1-4 · j/k · other closes ",
            ],
        )
    } else {
        fitting(
            room,
            &[
                " 1-4/Tab/←→ switch · any other key closes ",
                " 1-4/Tab switch · other closes ",
            ],
        )
    };
    let border = Style::default().fg(Color::LightBlue);
    // The tabs head the box, out of the scrolled text: they stay in view.
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((*scroll, 0))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(help_tab_bar(tab, area.width.saturating_sub(2) as usize))
                .title_bottom(Line::from(Span::styled(
                    hint,
                    Style::default().fg(Color::DarkGray),
                )))
                .border_style(border),
        );
    f.render_widget(p, area);
    scroll_indicator(f, area, rows, *scroll as usize, border);
    inner_height as u16
}
