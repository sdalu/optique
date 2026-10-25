use super::editor::{ABOUT_LINES, DETAIL_ONE_LINE_WIDTH, MIN_ROWS, about_lines, editor_layout};
use super::popups::{elide_dir, fitting, help_line, laid_out, port_help_rect, reflow};
use super::*;
use crate::model::port::PortInfo;

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

/// A directory's room is counted in columns: wide characters take two.
#[test]
fn a_wide_options_dir_is_elided_by_columns() {
    let dir = "/home/漢字漢字/opts";
    assert_eq!(elide_dir(dir, 20), dir, "19 columns fit");
    assert_eq!(elide_dir(dir, 18), "…/漢字漢字/opts");
    assert_eq!(elide_dir(dir, 14), "…/opts");
}

/// A path in a text gives way to fit, down to its last component; the
/// colon after it stays the text's.
#[test]
fn paths_in_a_text_keep_their_last_components() {
    let text = "creating /var/db/ports/cat_alpha/.tmp: Is a directory";
    assert_eq!(elide_paths(text, 80), text, "it fits");
    assert_eq!(
        elide_paths(text, 45),
        "creating …/cat_alpha/.tmp: Is a directory"
    );
    assert_eq!(elide_paths(text, 0), "creating …/.tmp: Is a directory");
    assert_eq!(
        elide_paths("a/b and/or c", 0),
        "a/b and/or c",
        "only words starting with /"
    );
}

/// A hint is the first form that fits, else the last cut at a whole
/// token, else its first token cut at a word.
#[test]
fn a_hint_is_fitted_at_a_token() {
    let forms = [
        " alpha: one · beta: two · gamma: three ",
        " a one · b two · c three ",
    ];
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
    assert_eq!(
        detail,
        Rect::new(1, 27, 38, 2),
        "two lines on a narrow pane"
    );
    assert_eq!(l.rows.y, about.y + about.height + 1, "a rule between");
    assert_eq!(l.rows.y + l.rows.height + 1, detail.y, "a rule between");
    assert_eq!(l.rows.height, 28 - ABOUT_LINES - 1 - 2 - 1);
    assert!(editor_layout(inner, false).about.is_none());

    // A short pane: the description shrinks to keep the rows MIN_ROWS,
    // and goes when not even a line fits; then the detail goes too.
    let l = editor_layout(Rect::new(1, 1, 38, 10), true);
    assert_eq!(
        l.about.expect("a shrunk description").height,
        2,
        "10 - 3 - 1 - 4 rows"
    );
    assert_eq!(l.rows.height, MIN_ROWS);
    let l = editor_layout(Rect::new(1, 1, 38, 8), true);
    assert!(l.about.is_none());
    assert!(
        l.detail.is_some(),
        "8 rows keep 4 beside 2 lines and a rule"
    );
    assert!(
        editor_layout(Rect::new(1, 1, 38, 6), false)
            .detail
            .is_none()
    );

    // A wide pane gives the detail one line, whatever the option.
    let wide = Rect::new(1, 1, DETAIL_ONE_LINE_WIDTH, 28);
    assert_eq!(editor_layout(wide, false).detail.map(|d| d.height), Some(1));
    let narrower = Rect {
        width: DETAIL_ONE_LINE_WIDTH - 1,
        ..wide
    };
    assert_eq!(
        editor_layout(narrower, false).detail.map(|d| d.height),
        Some(2)
    );
}

/// A box too short for a thumb between the arrows keeps the arrows.
#[test]
fn a_short_popup_keeps_the_arrows() {
    let bar = |height: u16, scroll: usize| -> String {
        let rows = rendered(10, height, |f| {
            scroll_indicator(f, Rect::new(0, 0, 10, height), 50, scroll, Style::default())
        });
        rows.iter()
            .map(|r| r.chars().last().unwrap_or(' '))
            .collect()
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
    let shown = rendered(80, 24, |f| {
        draw_help(f, 0, &mut scroll);
    });
    let top = shown.iter().find(|r| r.contains('┌')).expect("the box");
    assert!(
        top.contains("1:Markers") && top.contains("4:Views/Actions"),
        "{top}"
    );
    assert!(scroll > 0, "scrolled");
    assert!(
        shown.iter().any(|r| r.contains("↑/↓ scroll")),
        "the keys at the bottom"
    );

    let narrow = rendered(60, 24, |f| {
        draw_help(f, 2, &mut 0);
    });
    let top = narrow.iter().find(|r| r.contains('┌')).expect("the box");
    assert!(
        top.contains("3:Navigate/Edit") && !top.contains("Markers"),
        "{top}"
    );
    assert!(top.contains(" 1 2 ") && top.contains(" 4 "), "{top}");
}

/// A cut line loses the word it would split, not half of it.
#[test]
fn a_clipped_line_ends_on_a_whole_word() {
    let mut lines: Vec<Line<'static>> = vec![Line::from("one two three"), Line::from("more")];
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
    let shown: Vec<String> = about_lines(&info, text, 40)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        shown,
        [
            "A port",
            "Use it to:",
            "- avoid this",
            "- avoid that, a long continuation"
        ]
    );
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
    assert_eq!(
        wrap_text("  abcdefghij", 6, 2),
        ["  abcd", "  efgh", "  ij"]
    );
}

#[test]
fn wrap_text_of_a_fitting_line_is_the_line() {
    assert_eq!(wrap_text("  short desc", 40, 2), ["  short desc"]);
}

#[test]
fn wrap_text_hard_splits_a_word_longer_than_the_whole_line() {
    // An unbroken word must not overflow the panel it is wrapped for.
    assert!(
        wrap_text("  supercalifragilistic", 8, 2)
            .iter()
            .all(|l| l.chars().count() <= 8)
    );
}

/// Lines are measured in terminal columns: a wide character takes two, a
/// combining mark none, so neither text overflows nor wraps early.
#[test]
fn wrap_text_measures_columns_not_characters() {
    use ratatui::text::Line;
    let wide = wrap_text("漢字 漢字 漢字", 10, 0);
    assert!(
        wide.iter().all(|l| Line::raw(l.as_str()).width() <= 10),
        "{wide:?}"
    );
    assert_eq!(wide, ["漢字 漢字", "漢字"]);
    assert_eq!(wrap_text("漢字漢字漢字", 5, 0), ["漢字", "漢字", "漢字"]);
    // e + U+0301 is one column: the line fits as it is.
    assert_eq!(
        wrap_text("cafe\u{301} cafe\u{301}", 9, 0),
        ["cafe\u{301} cafe\u{301}"]
    );
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
    assert_eq!(
        help_line("  alpha beta gamma", 12),
        ["  alpha beta", "  gamma"]
    );
    // A bullet hangs its wrap under its text.
    assert_eq!(
        help_line("- alpha beta gamma", 12),
        ["- alpha beta", "  gamma"]
    );
}

/// A hard-wrapped paragraph is joined into one; a blank line ends it.
#[test]
fn reflow_joins_a_hard_wrapped_paragraph() {
    let src = |t: &str| t.lines().map(str::to_string).collect::<Vec<_>>();
    assert_eq!(
        reflow(&src(
            "Dovecot uses its own daemon\nfor authentication, so\nno library.\n\nNext one."
        )),
        [
            "Dovecot uses its own daemon for authentication, so no library.",
            "",
            "Next one."
        ]
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
    assert_eq!(
        width(88),
        84,
        "an 80-column line fits inside the borders and a blank"
    );
    assert_eq!(width(80), 76, "two columns short of each edge");
    assert_eq!(width(200), 152, "76%");
    let r = port_help_rect(Rect::new(0, 0, 100, 40));
    assert_eq!((r.x, r.x + r.width), (8, 92), "centered");
    assert_eq!(
        r.height,
        centered_rect(76, 70, Rect::new(0, 0, 100, 40)).height,
        "height as before"
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
        .map(|r| {
            r.trim()
                .trim_matches(['│', '█', '▲', '▼'])
                .trim()
                .to_string()
        })
        .rfind(|r| !r.is_empty() && !r.starts_with('└') && !r.starts_with('┌'))
        .unwrap_or_default()
}

/// Scrolled to its end, every help tab shows its last line on an 80x30
/// terminal, where the box wraps lines the scroll limit must count.
#[test]
fn every_help_tab_scrolls_to_its_last_line_on_a_small_terminal() {
    for tab in 0..HELP_TABS.len() {
        // As tall as it takes to show it all, at the same width.
        let whole = rendered(80, 200, |f| {
            draw_help(f, tab, &mut 0);
        });
        let last = last_text_row(&whole);
        let mut scroll = u16::MAX;
        let small = rendered(80, 30, |f| {
            draw_help(f, tab, &mut scroll);
        });
        assert_eq!(
            last_text_row(&small),
            last,
            "tab {tab}:\n{}",
            small.join("\n")
        );
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
    assert_eq!(
        first, "0123456789",
        "untouched: the cut is marked on the last row"
    );
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
    assert_eq!(
        key_legend(&Focus::List, 35),
        " ?:help a:apply q:quit Enter:edit …"
    );
    assert_eq!(
        key_legend(&Focus::Editor, 200),
        " ?:help a:apply q:quit Space:toggle i:info d:defaults U:revert u:undo n/p:next-problem"
    );
}

/// A line or a title too wide ends in `…` after a whole word, its first
/// word kept, cut, when it is the only one; one that fits is untouched.
#[test]
fn fit_start_keeps_the_end() {
    assert_eq!(
        fit_start(" filter: ", "nginx▏", " ", 40),
        " filter: nginx▏ "
    );
    let cut = fit_start(" filter: ", "www/nginx-full-ginx-module▏", " ", 23);
    assert_eq!(cut, " filter: …ginx-module▏ ");
    assert_eq!(Line::from(cut.as_str()).width(), 23);
}

/// The overlay is cut first, then the changes lose their start; the
/// place always shows.
#[test]
fn the_footer_cuts_the_overlay_first_then_the_changes() {
    let both = |room| {
        footer_parts(
            Some("myoverlay"),
            Some("+AAA +BBB -CCC"),
            Some("3/12"),
            room,
        )
    };
    assert_eq!(
        both(60),
        (
            Some(" overlay: myoverlay ".into()),
            Some(" +AAA +BBB -CCC · 3/12 ".into())
        )
    );
    // 20 + 1 + 23 = 44 columns whole; at 40 the label gives way.
    assert_eq!(
        both(40),
        (
            Some(" overlay: myov… ".into()),
            Some(" +AAA +BBB -CCC · 3/12 ".into())
        )
    );
    // Down to ` overlay: m… `, then the changes are cut at their start.
    assert_eq!(
        both(30),
        (
            Some(" overlay: m… ".into()),
            Some(" …B -CCC · 3/12 ".into())
        )
    );
    // No room for the label at all: the right part alone.
    assert_eq!(both(18), (None, Some(" …BBB -CCC · 3/12 ".into())));
    assert_eq!(footer_parts(None, None, None, 40), (None, None));
    assert_eq!(
        footer_parts(Some("o"), None, None, 40),
        (Some(" overlay: o ".into()), None)
    );
    assert_eq!(
        footer_parts(None, None, Some("1/2"), 40),
        (None, Some(" 1/2 ".into()))
    );
    assert_eq!(
        footer_parts(None, Some("+A"), None, 40),
        (None, Some(" +A ".into()))
    );
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
    assert_eq!(
        cut(vec![Span::raw("one two three")], 9),
        "one two…",
        "no blank before the marker"
    );
    assert_eq!(
        cut(vec![Span::raw("onetwothree")], 6),
        "onetw…",
        "the only word, cut"
    );
    // Across spans: a description's first word goes whole too.
    assert_eq!(
        cut(vec![Span::raw("  [x] DOCS"), Span::raw("  Build docs")], 16),
        "  [x] DOCS…"
    );
    assert_eq!(
        fit_title(" Ports (3) — filter: abc ", 25),
        " Ports (3) — filter: abc "
    );
    assert_eq!(fit_title(" Ports (3) — filter: abc ", 21), " Ports (3) —… ");
    assert_eq!(
        fit_title(" Ports (3) — filter: abc ", 22),
        " Ports (3) — filter:… "
    );
    assert!(Line::from(fit_title(" Ports (3) — filter: abc ", 20)).width() <= 20);
}

/// A legend's style marks its label alone: the blanks right-aligning it
/// are neither underlined (`flavor`) nor struck through.
#[test]
fn a_help_legend_styles_its_label_not_its_padding() {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 80)).unwrap();
    terminal
        .draw(|f| {
            draw_help(f, 1, &mut 0);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let rows = screen_rows(buffer);
    for (label, modifier) in [
        (" flavor = underlined", Modifier::UNDERLINED),
        (" struck-through = ", Modifier::CROSSED_OUT),
    ] {
        let (y, row) = rows
            .iter()
            .enumerate()
            .find(|(_, r)| r.contains(label))
            .expect(label);
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
    let text = |tab: usize| {
        rendered(200, 80, |f| {
            draw_help(f, tab, &mut 0);
        })
        .join("\n")
    };
    let row = text(1);
    assert!(
        row.contains("flavor* = the flavor whose view owns the shared options file"),
        "{row}"
    );
    assert!(
        row.contains("flavor‹ = the flavor shown in the pane"),
        "{row}"
    );
    assert!(
        row.contains("flavor = underlined: in the closure, f reaches it"),
        "{row}"
    );
    assert!(
        row.contains("[h] notes = the port ships notes (pkg-help)"),
        "{row}"
    );
    assert!(
        row.contains("30 rows or more") && row.contains("V shows or hides it until you quit"),
        "{row}"
    );
    let keys = text(2);
    assert!(
        keys.contains("toggle option (group rules enforced), in the option pane"),
        "{keys}"
    );
    for want in [
        "Ctrl-B / Ctrl-F",
        "Ctrl-U clears",
        "  u ",
        "d / U",
        "  V ",
        "  v ",
        "  h ",
    ] {
        assert!(keys.contains(want), "{want:?} in\n{keys}");
    }
    assert!(!keys.contains("  I "), "I is no key");
}

/// `wrapped_height` counts every row the Paragraph lays the lines over,
/// rows of wrapped trailing spaces too (a padded key at the end of a deep
/// chain line): the oracle is the row a marker line after them lands on.
#[test]
fn wrapped_height_counts_rows_of_trailing_spaces() {
    use ratatui::buffer::Buffer;
    use ratatui::widgets::{Widget as _, Wrap};
    let rows_by_paragraph = |lines: &[Line<'static>], width: u16| {
        let mut all = lines.to_vec();
        all.push(Line::raw("X"));
        let area = Rect::new(0, 0, width, 40);
        let mut buf = Buffer::empty(area);
        Paragraph::new(all)
            .wrap(Wrap { trim: false })
            .render(area, &mut buf);
        (0..40)
            .find(|&y| buf[(0, y)].symbol() == "X")
            .map(usize::from)
            .unwrap()
    };
    for (text, width) in [
        ("ab        ", 4),
        ("  └─ dev/x            ", 10),
        ("abcdef    ", 5),
        ("abcd efgh ij", 4),
    ] {
        let lines = [Line::raw(text)];
        assert_eq!(
            popups::wrapped_height(&lines, width),
            rows_by_paragraph(&lines, width),
            "{text:?} in {width}"
        );
    }
}
