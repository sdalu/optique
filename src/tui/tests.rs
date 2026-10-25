use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::keys::page_step;
use super::overlays::parse_bulk;
use super::refresh::{REFRESH_DEBOUNCE, refreshed_message};
use super::undo::{UNDO_MAX, push_capped};
use super::*;
use crate::deps::EXTRA_SHOWN;
use crate::draft;
use crate::query::refresher::RefreshEvent;

#[test]
fn undo_stack_drops_the_oldest_past_the_cap() {
    let mut stack: Vec<(String, BTreeSet<String>)> = Vec::new();
    for n in 0..UNDO_MAX + 50 {
        push_capped(&mut stack, (format!("port_{n}"), BTreeSet::new()));
    }
    assert_eq!(stack.len(), UNDO_MAX);
    // The newest change is on top, the 50 oldest are gone.
    assert_eq!(stack.last().unwrap().0, format!("port_{}", UNDO_MAX + 49));
    assert_eq!(stack.first().unwrap().0, "port_50");
}

/// Build the real App over the given closure — same session, same keymap,
/// same drawing code as a live TUI, only the terminal is in memory. Each
/// entry is a port and the ports it depends on; every port carries one
/// option, so none of them is hidden as optionless.
pub(super) fn app_for(tmp: &std::path::Path, graph: &[(&str, &[&str])]) -> App {
    use crate::model::options::PortOptions;
    use crate::model::port::{DepEdge, PortInfo};

    let port = |origin: &str, deps: &[&str]| PortInfo {
        options: PortOptions {
            complete: vec!["DOCS".to_string()],
            ..Default::default()
        },
        deps: deps
            .iter()
            .map(|dep| DepEdge {
                target: PortKey::parse(dep).expect("test dep parses"),
                spec: format!("dep:{dep}"),
                test_only: false,
            })
            .collect(),
        ..PortInfo::stub(origin)
    };
    let mut ports = std::collections::BTreeMap::new();
    for (origin, deps) in graph {
        let info = port(origin, deps);
        ports.insert(info.canonical.clone(), info);
    }
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    let files = OptionsFiles::new(&tmp.join("options"));
    let session = Session::new(ports, HashMap::new(), &roots, &files, false);
    let db =
        StagingDb::create(tmp, &files, session.states.keys()).expect("staging db under a tempdir");
    let refresher = crate::query::refresher::spawn(
        crate::query::makerunner::QueryCtx {
            portsdir: tmp.to_path_buf(),
            make_conf: None,
            port_dbdir: db.path().to_path_buf(),
            overlays: Vec::new(),
            identity: crate::query::makerunner::Identity::Invoker,
            framework: Vec::new(),
        },
        1,
        crate::cache::Cache::disabled(),
        crate::moved::Moved::parse(""),
        Default::default(),
    );
    build_app(
        session,
        files,
        db,
        refresher,
        Default::default(),
        Vec::new(),
        false,
    )
}

/// The port pane says which overlay a port comes from, and nothing for
/// a port the ports tree answers.
#[test]
fn the_port_pane_names_its_overlay() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[]), ("cat/beta", &[])]);
    let ov = tmp.path().join("ov");
    std::fs::create_dir_all(ov.join("cat/beta")).unwrap();
    std::fs::write(ov.join("cat/beta/Makefile"), "").unwrap();
    app.overlays = vec![crate::config::Overlay {
        name: Some("mine".into()),
        path: ov,
    }];
    let shown = screen(&mut app);
    assert!(!shown.contains("overlay"), "cat/alpha is the tree's");
    assert!(
        pane_bottom(&shown)
            .chars()
            .skip(1)
            .all(|c| c == '─' || c == '┘'),
        "{shown}"
    );
    press(&mut app, 'j');
    let shown = screen(&mut app);
    assert!(
        pane_bottom(&shown).starts_with("└ overlay: mine ─"),
        "{shown}"
    );
    assert!(
        shown.lines().next().unwrap().contains("┌ beta-1.0 ─"),
        "the title is the package:\n{shown}"
    );
}

/// The option pane's title is the versioned package, the flavors after
/// it for a flavored port, cut at a word when it does not fit.
#[test]
fn the_pane_title_is_the_package() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha@py311", &[])]);
    let key = PortKey::parse("cat/alpha@py311").unwrap();
    let info = app.session.ports.get_mut(&key).unwrap();
    info.flavors = vec!["py311".into(), "py312".into()];
    let shown = screen(&mut app);
    let top = shown.lines().next().unwrap();
    assert!(
        top.contains("┌ alpha-1.0 · flavors: py311*‹ py312 ─"),
        "{top}"
    );
    assert!(!top.contains("cat/alpha"), "{top}");

    let info = app.session.ports.get_mut(&key).unwrap();
    info.pkgname = "alpha-with-a-long-package-name-1.0.0_1,1".into();
    let shown = screen_at(&mut app, 80, 24);
    let top = shown.lines().next().unwrap();
    let title = top.split('┌').nth(1).unwrap().split(" ─").next().unwrap();
    assert!(title.ends_with('…'), "{top}");
    let body = title.trim_start().trim_end_matches('…');
    let full = "alpha-with-a-long-package-name-1.0.0_1,1 · flavors: py311*‹ py312";
    assert!(
        full.starts_with(body) && full[body.len()..].starts_with(' '),
        "{top}"
    );
}

/// In the option pane's title, a flavor the closure holds is underlined
/// and one it does not hold is plain — focused or not, since underline is
/// an attribute rather than a colour; the marks are never underlined.
#[test]
fn the_pane_title_underlines_the_flavors_in_the_closure() {
    use ratatui::style::Modifier;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(
        tmp.path(),
        &[("cat/alpha@py311", &[]), ("cat/alpha@py312", &[])],
    );
    for f in ["py311", "py312"] {
        let key = PortKey::parse(&format!("cat/alpha@{f}")).unwrap();
        app.session.ports.get_mut(&key).unwrap().flavors =
            vec!["py311".into(), "py312".into(), "py313".into()];
    }
    for focused in [true, false] {
        if focused {
            hit(&mut app, KeyCode::Enter);
        } else {
            hit(&mut app, KeyCode::Esc);
        }
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 35)).unwrap();
        terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let top = ui::screen_rows(&buffer)[0].clone();
        assert!(
            top.contains("alpha-1.0 · flavors: py311*‹ py312 py313 "),
            "{top}"
        );
        let underlined = |text: &str| {
            let col = top[..top.find(text).unwrap()].chars().count() as u16;
            buffer[(col, 0)].modifier.contains(Modifier::UNDERLINED)
        };
        assert!(
            underlined("py311"),
            "focused {focused}: in the closure, current"
        );
        assert!(underlined("py312"), "focused {focused}: in the closure");
        assert!(
            !underlined("py313"),
            "focused {focused}: not in the closure"
        );
        assert!(
            !underlined("*‹"),
            "focused {focused}: the marks are not the name"
        );
    }
}

/// A port of two options, NAME_A and NAME_B, under an overlay named
/// `mine` when `overlay`.
fn footer_app(tmp: &std::path::Path, overlay: bool) -> App {
    let mut app = app_for(tmp, &[("cat/alpha", &[])]);
    let key = PortKey::parse("cat/alpha").unwrap();
    let info = app.session.ports.get_mut(&key).unwrap();
    info.options.complete = vec!["NAME_A".into(), "NAME_B".into()];
    if overlay {
        let ov = tmp.join("ov");
        std::fs::create_dir_all(ov.join("cat/alpha")).unwrap();
        std::fs::write(ov.join("cat/alpha/Makefile"), "").unwrap();
        app.overlays = vec![crate::config::Overlay {
            name: Some("mine".into()),
            path: ov,
        }];
    }
    app.rebuild_editor();
    app
}

/// The option pane's bottom border: the overlay on the left; on the
/// right the staged changes, and the cursor's place among the options
/// while the pane has the focus; a plain border when there is neither.
#[test]
fn the_pane_bottom_border_says_overlay_changes_and_place() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = footer_app(tmp.path(), false);
    let plain = pane_bottom(&screen(&mut app));
    assert!(
        plain
            .trim_start_matches('└')
            .trim_end_matches('┘')
            .chars()
            .all(|c| c == '─'),
        "{plain}"
    );
    hit(&mut app, KeyCode::Enter);
    assert!(
        pane_bottom(&screen(&mut app)).ends_with("═ 1/2 ╝"),
        "place only:\n{}",
        screen(&mut app)
    );
    press(&mut app, 'j');
    press(&mut app, ' ');
    assert!(
        pane_bottom(&screen(&mut app)).ends_with("═ +NAME_B · 2/2 ╝"),
        "{}",
        screen(&mut app)
    );
    press(&mut app, 'k');
    press(&mut app, ' ');
    assert!(pane_bottom(&screen(&mut app)).ends_with("═ +NAME_A +NAME_B · 1/2 ╝"));
    hit(&mut app, KeyCode::Esc);
    let shown = screen(&mut app);
    assert!(
        pane_bottom(&shown).ends_with("─ +NAME_A +NAME_B ┘"),
        "changes only:\n{shown}"
    );

    let tmp = tempfile::tempdir().unwrap();
    let mut app = footer_app(tmp.path(), true);
    let shown = screen(&mut app);
    let bottom = pane_bottom(&shown);
    assert!(
        bottom.starts_with("└ overlay: mine ─") && bottom.ends_with("──┘"),
        "overlay only:\n{shown}"
    );
    hit(&mut app, KeyCode::Enter);
    press(&mut app, ' ');
    let bottom = pane_bottom(&screen(&mut app));
    assert!(
        bottom.starts_with("╚ overlay: mine ═") && bottom.ends_with("═ +NAME_A · 1/2 ╝"),
        "{bottom}"
    );
}

/// Changes too long for the border lose their start, the place stays.
#[test]
fn long_changes_lose_their_start_and_keep_the_place() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let key = PortKey::parse("cat/alpha").unwrap();
    let info = app.session.ports.get_mut(&key).unwrap();
    info.options.complete = (1..=12).map(|i| format!("OPTION_{i:02}")).collect();
    app.rebuild_editor();
    hit(&mut app, KeyCode::Enter);
    for _ in 0..12 {
        press(&mut app, ' ');
        press(&mut app, 'j');
    }
    let bottom = pane_bottom(&screen_at(&mut app, 80, 24));
    assert!(
        bottom.starts_with("╚ …") && bottom.ends_with(" +OPTION_12 · 12/12 ╝"),
        "{bottom}"
    );
}

/// An option row keeps every column before its description: on a narrow
/// pane the last one has its own text cut, never a `…` after a whole one.
/// A description cut down to its first word goes: `Install…` says
/// nothing the option's name did not. Two words or more stay.
#[test]
fn a_cut_description_keeps_two_words_or_goes() {
    use crate::model::options::OptionDef;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let key = PortKey::parse("cat/alpha").unwrap();
    let info = app.session.ports.get_mut(&key).unwrap();
    info.options.complete = vec!["DOCS".into()];
    info.options.defs.insert(
        "DOCS".into(),
        OptionDef {
            desc: "Installation documentation and examples".into(),
            ..Default::default()
        },
    );
    app.rebuild_editor();
    let mut two = false;
    for width in 60..=140 {
        let shown = screen_at(&mut app, width, 24);
        let row = shown
            .lines()
            .find(|l| l.contains("[ ] DOCS"))
            .unwrap_or_else(|| panic!("{shown}"));
        assert!(!row.contains("Installation…"), "width {width}: {row}");
        two |= row.contains("Installation documentation…");
    }
    assert!(two, "two words shown at some width");
}

#[test]
fn a_narrow_row_keeps_its_columns_and_cuts_the_last() {
    use crate::model::options::OptionDef;
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let key = PortKey::parse("cat/alpha").unwrap();
    let info = app.session.ports.get_mut(&key).unwrap();
    info.options.complete = vec!["HTTP".into(), "HTTP_AUTH_REQUEST_MODULE".into()];
    info.options.defs.insert(
        "HTTP".into(),
        OptionDef {
            desc: "Enable the HTTP module".into(),
            ..Default::default()
        },
    );
    info.options.defs.insert(
        "HTTP_AUTH_REQUEST_MODULE".into(),
        OptionDef {
            implies: vec!["HTTP".into()],
            ignore: Some("no".into()),
            desc: "Authentication by subrequest".into(),
            ..Default::default()
        },
    );
    let name = info.options_name.clone();
    let staged = &mut app.session.states.get_mut(&name).unwrap().staged;
    staged.insert("HTTP".into());
    staged.insert("HTTP_AUTH_REQUEST_MODULE".into());
    app.rebuild_editor();
    let shown = screen_at(&mut app, 80, 24);
    let row = shown
        .lines()
        .find(|l| l.contains("[x] HTTP "))
        .unwrap_or_else(|| panic!("{shown}"));
    let pane = row.split('│').nth(1).unwrap().trim_end();
    assert!(pane.contains(" off ") && pane.contains(" impl"), "{pane}");
    assert!(pane.ends_with('…') && !pane.contains("Enable"), "{pane}");
    assert!(
        !pane.contains(" …"),
        "never a `…` after a whole column: {pane}"
    );
    // The implier's badge is its last column, cut in its own text.
    let row = shown.lines().find(|l| l.contains("[x] HTTP_AUTH")).unwrap();
    let pane = row.split('│').nth(1).unwrap().trim_end();
    assert!(pane.ends_with(" off     ⚠ign…"), "{pane}");
    // On a wide screen the description shows whole again.
    let shown = screen_at(&mut app, 180, 24);
    assert!(
        shown.contains("implied by HTTP_AUTH_REQUEST_MODULE  Enable the HTTP module"),
        "{shown}"
    );
    // Narrower still, the badge itself is cut, the columns kept.
    let shown = screen_at(&mut app, 70, 24);
    for row in shown.lines().filter(|l| l.contains("[x] ")) {
        let pane = row.split('│').nth(1).unwrap().trim_end();
        assert!(
            !pane.ends_with("on…") && !pane.ends_with("off…") && !pane.ends_with(" …"),
            "{pane}"
        );
    }
}

/// The panel shows by the pane's size alone, however many options the
/// port has: the rows scroll under it, and the selected one stays in view.
#[test]
fn description_panel_shows_whatever_the_option_count() {
    for n in [2, 60] {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
        let key = PortKey::parse("cat/alpha").unwrap();
        let info = app.session.ports.get_mut(&key).unwrap();
        info.options.complete = (1..=n).map(|i| format!("OPT{i:02}")).collect();
        info.about.comment = "The alpha port".to_string();
        app.about_text.insert(key.clone(), "word ".repeat(400));
        app.rebuild_editor();
        app.focus = Focus::Editor;
        app.editor_idx = app.editor_rows.len() - 1;
        let shown = screen(&mut app);
        assert!(shown.contains("The alpha port"), "{n} options:\n{shown}");
        assert!(
            shown.contains(&format!("OPT{n:02}")),
            "the selected row in view:\n{shown}"
        );
    }
}

/// The row a rule (├…┤, ╟…╢ while the pane has the focus) of the option
/// pane sits on, counted from the top.
fn rule_rows(shown: &str) -> Vec<usize> {
    shown
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains(['├', '╟']))
        .map(|(i, _)| i)
        .collect()
}

/// The detail panel is one height whatever the option, and stays put,
/// empty, while no option is highlighted: nothing in the pane moves.
#[test]
fn the_detail_panel_never_moves() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let key = PortKey::parse("cat/alpha").unwrap();
    let info = app.session.ports.get_mut(&key).unwrap();
    info.options.complete = vec!["LONG".into(), "SHORT".into()];
    let def = |d: &str| crate::model::options::OptionDef {
        desc: d.into(),
        ..Default::default()
    };
    info.options
        .defs
        .insert("LONG".into(), def(&"a long description ".repeat(12)));
    info.options.defs.insert("SHORT".into(), def("short"));
    app.rebuild_editor();

    let on_list = screen(&mut app);
    assert!(on_list.contains("Enter: edit the options"), "{on_list}");
    assert!(
        !on_list.contains("├ LONG") && !on_list.contains("╟ LONG"),
        "no option named while none is highlighted"
    );
    hit(&mut app, KeyCode::Enter);
    let long = screen(&mut app);
    assert!(long.contains("╟ LONG"), "{long}");
    assert!(long.contains('…'), "a long description is cut");
    press(&mut app, 'j');
    let short = screen(&mut app);
    assert!(short.contains("╟ SHORT"), "{short}");
    assert_eq!(rule_rows(&on_list), rule_rows(&long));
    assert_eq!(rule_rows(&long), rule_rows(&short));
    // g/G work in the pane too.
    press(&mut app, 'g');
    assert!(screen(&mut app).contains("╟ LONG"));
    press(&mut app, 'G');
    assert!(screen(&mut app).contains("╟ SHORT"));
}

/// A description cut short says where the rest is, and `v` gets it from
/// either pane; a message lasts until the next key.
#[test]
fn a_cut_description_points_at_v() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let key = PortKey::parse("cat/alpha").unwrap();
    app.session.ports.get_mut(&key).unwrap().about.comment = "The alpha port".into();
    app.about_text.insert(key.clone(), "word ".repeat(400));
    assert!(
        screen(&mut app).contains("The alpha port"),
        "shown on 35 rows"
    );
    press(&mut app, 'V');
    assert!(!screen(&mut app).contains("The alpha port"), "V hides it");
    press(&mut app, 'V');
    assert!(app.message.is_some());
    assert!(
        screen(&mut app).contains("[v] full text ──┤"),
        "a hint, at the right"
    );
    hit(&mut app, KeyCode::Enter);
    assert!(app.message.is_none(), "cleared by the next key");
    press(&mut app, 'v');
    assert_eq!(
        app.port_help.as_ref().map(|h| h.title),
        Some("about"),
        "v from the option pane"
    );
    hit(&mut app, KeyCode::Esc);

    // A description that fits says nothing more.
    app.about_text.insert(key, "short".into());
    assert!(!screen(&mut app).contains("[v] full text"));
}

/// The [h] notes marker survives a title too long for the pane.
#[test]
fn the_help_marker_survives_a_long_title() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let key = PortKey::parse("cat/alpha").unwrap();
    let info = app.session.ports.get_mut(&key).unwrap();
    info.pkgname = "alpha-with-a-very-long-package-name-1.0.0_1,1".repeat(2);
    info.pkg_help = Some("/nonexistent/pkg-help".into());
    let shown = screen(&mut app);
    assert!(shown.contains("[h] notes"), "{shown}");
}

/// A popup too small for its text says so on its right border: a
/// scrollbar whose thumb follows the scroll; one that fits has none.
#[test]
fn a_popup_that_cannot_hold_its_text_shows_a_scrollbar() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let key = PortKey::parse("cat/alpha").unwrap();
    // The rows of the right border holding the thumb, top to bottom.
    let thumb = |shown: &str| -> Vec<usize> {
        shown
            .lines()
            .enumerate()
            .filter(|(_, l)| l.contains('█'))
            .map(|(i, _)| i)
            .collect()
    };
    // Bullets, so each keeps a line of its own.
    let lines = (1..=200).map(|i| format!("- line {i}")).collect();
    app.port_help = Some(PortHelp {
        title: "pkg-help",
        key: key.clone(),
        lines,
        scroll: 0,
    });
    let top = thumb(&screen(&mut app));
    assert!(!top.is_empty(), "a bar when the text overflows");
    app.port_help.as_mut().unwrap().scroll = u16::MAX;
    let bottom = thumb(&screen(&mut app));
    assert!(
        bottom[0] > top[top.len() - 1],
        "the thumb follows the scroll: {top:?} {bottom:?}"
    );
    // One size wherever it is, moving down one step at a time.
    let mut last = top.clone();
    for scroll in 0..=200 {
        app.port_help.as_mut().unwrap().scroll = scroll;
        let at = thumb(&screen(&mut app));
        assert_eq!(at.len(), top.len(), "scroll {scroll}: {at:?}");
        assert!(at[0] >= last[0], "scroll {scroll}: moved up");
        last = at;
    }
    assert_eq!(last, bottom);
    let shown = screen(&mut app);
    assert!(
        shown.contains('▲') && shown.contains('▼'),
        "the ends are marked"
    );

    app.port_help = Some(PortHelp {
        title: "pkg-help",
        key,
        lines: vec!["short".into()],
        scroll: 0,
    });
    assert!(thumb(&screen(&mut app)).is_empty(), "no bar when it fits");
}

/// The panel reads a port's pkg-descr once, not at every redraw: the loop
/// redraws every 150 ms, and the ports tree may be on NFS.
#[test]
fn description_panel_reads_pkg_descr_once() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let descr = tmp.path().join("pkg-descr");
    std::fs::write(&descr, "Alpha does things.\n").unwrap();
    let key = PortKey::parse("cat/alpha").unwrap();
    app.session.ports.get_mut(&key).unwrap().about.pkg_descr = Some(descr.display().to_string());
    // Built with the panel on, the app has read the port already.
    app.about_text.clear();
    app.rebuild_editor();
    assert!(screen(&mut app).contains("Alpha does things."));
    std::fs::remove_file(&descr).unwrap();
    assert!(
        screen(&mut app).contains("Alpha does things."),
        "read again at redraw"
    );
}

/// A Latin-1 pkg-descr is shown, its stray byte replaced, not refused as
/// unreadable.
#[test]
fn a_latin1_pkg_descr_is_shown() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let descr = tmp.path().join("pkg-descr");
    std::fs::write(&descr, b"Alpha fait des choses \xe9tranges.\n").unwrap();
    let key = PortKey::parse("cat/alpha").unwrap();
    app.session.ports.get_mut(&key).unwrap().about.pkg_descr = Some(descr.display().to_string());
    app.about_text.clear();
    app.rebuild_editor();
    assert!(screen(&mut app).contains("Alpha fait des choses"));
}

/// A pkg-descr's escape sequence never reaches the terminal: on screen it
/// is spaces, the panel and the `v` overlay alike, its tabs kept to be laid
/// out.
#[test]
fn a_pkg_descr_shows_no_control_character() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let descr = tmp.path().join("pkg-descr");
    std::fs::write(&descr, b"Alpha\x1b[2J clears\rthe\tscreen.\nTwice.\n").unwrap();
    let key = PortKey::parse("cat/alpha").unwrap();
    app.session.ports.get_mut(&key).unwrap().about.pkg_descr = Some(descr.display().to_string());
    app.about_text.clear();
    app.rebuild_editor();
    let shown = screen(&mut app);
    assert!(shown.contains("Alpha [2J clears the"), "{shown}");
    assert!(!shown.contains('\x1b') && !shown.contains('\r'));
    app.focus = Focus::Editor;
    app.open_port_about();
    let lines = &app.port_help.as_ref().unwrap().lines;
    let tail = &lines[lines.len() - 2..];
    assert_eq!(tail, ["Alpha [2J clears the\tscreen.", "Twice."]);
}

/// A pkg-descr that cannot be read is said so, not shown as blank for
/// the rest of the session: the panel tries it again next time.
#[test]
fn description_panel_says_when_pkg_descr_is_unreadable() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[]), ("cat/beta", &[])]);
    let descr = tmp.path().join("pkg-descr");
    let key = PortKey::parse("cat/alpha").unwrap();
    app.session.ports.get_mut(&key).unwrap().about.pkg_descr = Some(descr.display().to_string());
    // A frame on 35 rows shows the panel, which has read the port.
    screen(&mut app);
    app.about_text.clear();
    app.rebuild_editor();
    let msg = app
        .message
        .as_ref()
        .map(|(m, _)| m.clone())
        .unwrap_or_default();
    assert!(msg.starts_with("cannot read"), "{msg}");
    assert!(screen(&mut app).contains("No description available"));

    std::fs::write(&descr, "Alpha does things.\n").unwrap();
    app.move_selection(1);
    app.move_selection(-1);
    assert!(
        screen(&mut app).contains("Alpha does things."),
        "read again once readable"
    );
}

/// The staging copy is never rewritten while a re-scan may be reading it:
/// a cache entry would pair the bytes the scan saw first with a result
/// computed from the new ones. The port waits, and goes once it is done.
#[test]
fn staging_is_not_rewritten_under_a_running_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let key = PortKey::parse("cat/alpha").unwrap();
    let staged = tmp.path().join("db/cat_alpha/options");
    assert!(!staged.exists());
    let due = Instant::now() - REFRESH_DEBOUNCE * 2;
    app.pending.insert(key.clone(), due);
    app.refreshing = 1;
    app.flush_due_refreshes();
    assert!(!staged.exists(), "written under a running re-scan");
    assert!(app.pending.contains_key(&key), "the port still waits");
    app.refreshing = 0;
    app.flush_due_refreshes();
    assert!(staged.is_file());
    assert!(app.pending.is_empty());
}

/// Through the real refresher: a sent batch counts as outstanding until
/// its scan reports done, and a port due meanwhile waits for that.
#[test]
fn a_sent_batch_holds_staging_until_its_scan_is_done() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[]), ("cat/beta", &[])]);
    let due = Instant::now() - REFRESH_DEBOUNCE * 2;
    let staged = |name: &str| tmp.path().join("db").join(name).join("options");

    app.pending
        .insert(PortKey::parse("cat/alpha").unwrap(), due);
    app.flush_due_refreshes();
    assert!(staged("cat_alpha").is_file());
    assert_eq!(app.refreshing, 1, "the sent batch is outstanding");

    app.pending.insert(PortKey::parse("cat/beta").unwrap(), due);
    app.flush_due_refreshes();
    assert!(
        !staged("cat_beta").exists(),
        "written under a running re-scan"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    while app.refreshing > 0 {
        assert!(Instant::now() < deadline, "the re-scan never reported done");
        app.drain_refresh_events();
        std::thread::sleep(Duration::from_millis(5));
    }
    app.flush_due_refreshes();
    assert!(staged("cat_beta").is_file());
    assert_eq!(app.refreshing, 1);
}

fn press(app: &mut App, c: char) -> KeyOutcome {
    let mods = if c.is_ascii_uppercase() {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    };
    dispatch_key(app, KeyEvent::new(KeyCode::Char(c), mods))
}

fn hit(app: &mut App, code: KeyCode) -> KeyOutcome {
    dispatch_key(app, KeyEvent::new(code, KeyModifiers::NONE))
}

fn ctrl(app: &mut App, c: char) -> KeyOutcome {
    dispatch_key(app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn selected(app: &App) -> String {
    app.selected_key()
        .map(|k| k.to_string())
        .unwrap_or_default()
}

fn visible(app: &App) -> Vec<String> {
    app.visible.iter().map(ToString::to_string).collect()
}

fn staged(app: &App, origin: &str) -> Vec<String> {
    let info = &app.session.ports[&PortKey::parse(origin).unwrap()];
    app.session
        .state(info)
        .unwrap()
        .staged
        .iter()
        .cloned()
        .collect()
}

/// Three unconfigured-or-not ports: alpha and gamma owe a decision,
/// beta's file already records its one option.
fn three_ports(tmp: &std::path::Path) -> App {
    let beta = tmp.join("options/cat_beta");
    std::fs::create_dir_all(&beta).unwrap();
    std::fs::write(
        beta.join("options"),
        "_OPTIONS_READ=beta-1.0\n_FILE_COMPLETE_OPTIONS_LIST=DOCS\nOPTIONS_FILE_UNSET+=DOCS\n",
    )
    .unwrap();
    app_for(
        tmp,
        &[("cat/alpha", &[]), ("cat/beta", &[]), ("cat/gamma", &[])],
    )
}

#[test]
fn quitting_asks_only_when_there_is_something_to_lose() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    assert!(matches!(ctrl(&mut app, 'l'), KeyOutcome::Repaint));
    assert!(
        matches!(press(&mut app, 'q'), KeyOutcome::Quit),
        "nothing staged"
    );
    assert!(matches!(ctrl(&mut app, 'c'), KeyOutcome::Quit));

    hit(&mut app, KeyCode::Enter);
    press(&mut app, ' ');
    assert!(app.session.dirty());
    assert!(matches!(press(&mut app, 'q'), KeyOutcome::Continue));
    assert!(app.quit_confirm);
    press(&mut app, 'x');
    assert!(!app.quit_confirm, "any other key goes back");
    ctrl(&mut app, 'c');
    assert!(app.quit_confirm, "Ctrl-C asks too");
    assert!(matches!(press(&mut app, 'y'), KeyOutcome::Quit));
    assert!(draft::load(&app.options_dir).is_some(), "y saved a draft");

    // That session ended there; the same one, asked again.
    app.quit_confirm = false;
    press(&mut app, 'q');
    assert!(matches!(press(&mut app, 'n'), KeyOutcome::Quit));
    assert!(draft::load(&app.options_dir).is_none(), "n discards it");
}

/// The quit prompt answers y and n, either case, and Ctrl-C as y; Esc
/// and any other key stay, the old s and d included.
#[test]
fn the_quit_prompt_answers_y_n_and_ctrl_c() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    hit(&mut app, KeyCode::Enter);
    press(&mut app, ' ');
    press(&mut app, 'q');
    let shown = screen(&mut app);
    assert!(shown.contains("Keep staged edits as a draft?"), "{shown}");
    assert!(
        shown.contains("y save & quit · n discard & quit · Esc stay"),
        "{shown}"
    );
    for stay in ['s', 'S', 'd', 'D', 'x'] {
        app.quit_confirm = true;
        assert!(
            matches!(press(&mut app, stay), KeyOutcome::Continue),
            "{stay}"
        );
        assert!(!app.quit_confirm, "{stay} stays");
    }
    app.quit_confirm = true;
    hit(&mut app, KeyCode::Esc);
    assert!(!app.quit_confirm, "Esc stays");
    app.quit_confirm = true;
    assert!(
        matches!(ctrl(&mut app, 'y'), KeyOutcome::Continue),
        "Ctrl-Y is not y"
    );
    assert!(draft::load(&app.options_dir).is_none(), "nothing saved yet");

    // A second Ctrl-C keeps the edits.
    press(&mut app, 'q');
    assert!(matches!(ctrl(&mut app, 'c'), KeyOutcome::Quit));
    assert!(
        draft::load(&app.options_dir).is_some(),
        "Ctrl-C at the prompt saved a draft"
    );
    app.quit_confirm = false;
    press(&mut app, 'q');
    assert!(matches!(press(&mut app, 'N'), KeyOutcome::Quit));
    assert!(draft::load(&app.options_dir).is_none(), "N discards it");
    app.quit_confirm = false;
    press(&mut app, 'q');
    assert!(matches!(press(&mut app, 'Y'), KeyOutcome::Quit));
    assert!(draft::load(&app.options_dir).is_some(), "Y saves it");
}

/// Ctrl-C quits from inside any popup, the prompts and the filter: at
/// once with nothing staged, through the quit prompt otherwise.
#[test]
fn ctrl_c_quits_from_anywhere() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    let help = tmp.path().join("pkg-help");
    std::fs::write(&help, "notes\n").unwrap();
    let key = PortKey::parse("cat/alpha").unwrap();
    app.session.ports.get_mut(&key).unwrap().pkg_help = Some(help.display().to_string());
    // Each opens something that takes keys of its own.
    let openers: [&dyn Fn(&mut App); 9] = [
        &|a| {
            press(a, '?');
        },
        &|a| {
            press(a, 'h');
        },
        &|a| {
            press(a, 'v');
        },
        &|a| {
            press(a, 'r');
        },
        &|a| {
            press(a, '/');
        },
        &|a| {
            press(a, 'B');
        },
        &|a| {
            hit(a, KeyCode::Enter);
        },
        &|a| {
            hit(a, KeyCode::Enter);
            press(a, 'i');
        },
        &|a| {
            press(a, 'a');
        },
    ];
    for (i, open) in openers.iter().enumerate() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = three_ports(tmp.path());
        app.session.ports.get_mut(&key).unwrap().pkg_help = Some(help.display().to_string());
        open(&mut app);
        assert!(
            app.top_overlay().is_some() || app.focus != Focus::List,
            "opener {i}"
        );
        assert!(
            matches!(ctrl(&mut app, 'c'), KeyOutcome::Quit),
            "opener {i}: nothing staged"
        );
    }
    // With edits staged, it asks first, over whatever is open.
    hit(&mut app, KeyCode::Enter);
    press(&mut app, ' ');
    press(&mut app, '?');
    assert!(matches!(ctrl(&mut app, 'c'), KeyOutcome::Continue));
    assert!(app.quit_confirm && app.help_tab.is_some());
    hit(&mut app, KeyCode::Esc);
    assert!(
        !app.quit_confirm && app.help_tab.is_some(),
        "Esc goes back to the help"
    );
}

#[test]
fn help_tabs_cycle_scroll_and_close() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    let n = ui::HELP_TABS.len();
    hit(&mut app, KeyCode::F(1));
    assert_eq!(app.help_tab, Some(0));
    hit(&mut app, KeyCode::Tab);
    assert_eq!(app.help_tab, Some(1));
    hit(&mut app, KeyCode::BackTab);
    hit(&mut app, KeyCode::BackTab);
    assert_eq!(app.help_tab, Some(n - 1), "wraps backwards");
    hit(&mut app, KeyCode::Right);
    assert_eq!(app.help_tab, Some(0), "and forwards");
    hit(&mut app, KeyCode::Left);
    assert_eq!(app.help_tab, Some(n - 1));
    press(&mut app, '2');
    assert_eq!(app.help_tab, Some(1));
    press(&mut app, '9');
    assert_eq!(app.help_tab, Some(1), "no ninth tab");
    screen(&mut app);
    let page = page_step(app.page.help) as u16;
    assert!(page > 1, "the box's height, from the last frame");
    press(&mut app, 'j');
    hit(&mut app, KeyCode::PageDown);
    assert_eq!(app.help_scroll, 1 + page);
    ctrl(&mut app, 'f');
    assert_eq!(app.help_scroll, 1 + 2 * page, "Ctrl-F pages too");
    ctrl(&mut app, 'b');
    press(&mut app, 'k');
    hit(&mut app, KeyCode::PageUp);
    assert_eq!(app.help_scroll, 0);
    press(&mut app, 'G');
    assert_eq!(app.help_scroll, u16::MAX, "G: the end, clamped when drawn");
    hit(&mut app, KeyCode::Home);
    assert_eq!(
        (app.help_tab, app.help_scroll),
        (Some(1), 0),
        "Home: the top"
    );
    // vi's h and l switch no tab: they are other keys, which close.
    press(&mut app, 'l');
    assert_eq!(app.help_tab, None, "l closes");
    press(&mut app, '?');
    press(&mut app, 'h');
    assert_eq!(app.help_tab, None, "h closes");
    press(&mut app, '?');
    press(&mut app, '2');
    press(&mut app, 'j');
    press(&mut app, 'x');
    assert_eq!((app.help_tab, app.help_scroll), (None, 0));
    press(&mut app, '?');
    assert_eq!(app.help_tab, Some(0));
    for tab in 0..n {
        app.help_tab = Some(tab);
        assert!(!screen(&mut app).is_empty());
    }
}

#[test]
fn pkg_help_opens_scrolls_and_closes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    press(&mut app, 'h');
    assert!(app.port_help.is_none());
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(m, _)| m.contains("no pkg-help"))
    );

    let help = tmp.path().join("pkg-help");
    std::fs::write(&help, "first line\nsecond line\n").unwrap();
    let key = app.selected_key().unwrap();
    app.session.ports.get_mut(&key).unwrap().pkg_help = Some(help.display().to_string());
    press(&mut app, 'h');
    let h = app.port_help.as_ref().expect("opened");
    assert_eq!((h.title, h.lines.len()), ("pkg-help", 2));
    assert!(screen(&mut app).contains("second line"));
    let page = page_step(app.page.port_help) as u16;
    assert!(page > 1, "the box's height, from the last frame");
    press(&mut app, 'j');
    hit(&mut app, KeyCode::PageDown);
    assert_eq!(app.port_help.as_ref().unwrap().scroll, 1 + page);
    ctrl(&mut app, 'f');
    assert_eq!(
        app.port_help.as_ref().unwrap().scroll,
        1 + 2 * page,
        "Ctrl-F pages too"
    );
    ctrl(&mut app, 'b');
    press(&mut app, 'k');
    hit(&mut app, KeyCode::PageUp);
    assert_eq!(app.port_help.as_ref().unwrap().scroll, 0);
    press(&mut app, 'G');
    assert_eq!(app.port_help.as_ref().unwrap().scroll, u16::MAX);
    press(&mut app, 'g');
    assert_eq!(app.port_help.as_ref().unwrap().scroll, 0);
    press(&mut app, 'x');
    assert!(app.port_help.is_none());
    hit(&mut app, KeyCode::Enter);
    press(&mut app, 'h');
    assert!(app.port_help.is_some(), "h works from the option pane too");
}

#[test]
fn the_bulk_prompt_edits_and_applies() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    press(&mut app, 'B');
    assert_eq!(app.bulk.as_deref(), Some(""));
    hit(&mut app, KeyCode::Esc);
    assert!(app.bulk.is_none());
    press(&mut app, 'B');
    for c in "DOCS=onx".chars() {
        press(&mut app, c);
    }
    hit(&mut app, KeyCode::Backspace);
    ctrl(&mut app, 'w');
    assert_eq!(
        app.bulk.as_deref(),
        Some("DOCS=on"),
        "no control characters"
    );
    hit(&mut app, KeyCode::Enter);
    assert!(app.bulk.is_none());
    for p in ["cat/alpha", "cat/beta", "cat/gamma"] {
        assert_eq!(staged(&app, p), ["DOCS"], "{p}");
    }
    hit(&mut app, KeyCode::Enter);
    press(&mut app, 'B');
    assert_eq!(
        app.bulk.as_deref(),
        Some("DOCS="),
        "pre-filled from the option pane"
    );
}

#[test]
fn the_filter_narrows_the_list() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    press(&mut app, '/');
    assert!(app.focus == Focus::Filter);
    for c in "bet".chars() {
        press(&mut app, c);
    }
    assert_eq!(visible(&app), ["cat/beta"]);
    hit(&mut app, KeyCode::Backspace);
    hit(&mut app, KeyCode::Backspace);
    assert_eq!(app.filter, "b");
    hit(&mut app, KeyCode::Enter);
    assert!(app.focus == Focus::List);
    assert_eq!(app.filter, "b", "Enter keeps the filter");
    press(&mut app, '/');
    hit(&mut app, KeyCode::Esc);
    assert!(app.focus == Focus::List && app.filter.is_empty());
    assert_eq!(visible(&app).len(), 3);
}

/// `/` starts a fresh filter; Esc on the list lets go of one left there,
/// keeping the port; the cursor shows only while the filter is typed.
/// While typing a filter an arrow accepts it, as Enter does, and moves
/// on the list it left; j and k stay letters of the filter.
#[test]
fn an_arrow_accepts_the_filter_and_moves() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(
        tmp.path(),
        &[("cat/jam", &[]), ("cat/jar", &[]), ("cat/kit", &[])],
    );
    press(&mut app, '/');
    press(&mut app, 'j');
    press(&mut app, 'a');
    assert!(
        app.focus == Focus::Filter && app.filter == "ja",
        "{:?}",
        app.filter
    );
    let first = app.selected_key();
    hit(&mut app, KeyCode::Down);
    assert!(app.focus == Focus::List, "the filter is accepted");
    assert_eq!(app.filter, "ja", "and kept");
    assert_ne!(app.selected_key(), first, "the selection moved down");
    press(&mut app, '/');
    press(&mut app, 'k');
    hit(&mut app, KeyCode::Up);
    assert!(app.focus == Focus::List && app.filter == "k");
}

#[test]
fn a_filter_starts_fresh_and_esc_clears_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    press(&mut app, '/');
    assert!(
        list_bottom(&screen(&mut app)).ends_with(" filter: ▏ ╝"),
        "{}",
        screen(&mut app)
    );
    for c in "gam".chars() {
        press(&mut app, c);
    }
    assert!(
        list_bottom(&screen(&mut app)).ends_with(" filter: gam▏ ╝"),
        "{}",
        screen(&mut app)
    );
    hit(&mut app, KeyCode::Enter);
    let shown = screen(&mut app);
    assert!(list_bottom(&shown).ends_with(" filter: gam ╝"), "{shown}");
    assert!(!shown.contains('▏'), "no cursor once left:\n{shown}");
    assert_eq!(selected(&app), "cat/gamma");

    press(&mut app, '/');
    assert!(
        app.focus == Focus::Filter && app.filter.is_empty(),
        "fresh: {:?}",
        app.filter
    );
    assert_eq!(visible(&app).len(), 3, "the whole list again");
    assert_eq!(selected(&app), "cat/gamma", "on the same port");
    press(&mut app, 'a');
    hit(&mut app, KeyCode::Enter);
    assert_eq!(visible(&app), ["cat/alpha", "cat/gamma", "cat/beta"]);
    press(&mut app, 'j');
    hit(&mut app, KeyCode::Esc);
    assert!(app.filter.is_empty() && app.focus == Focus::List);
    assert_eq!(visible(&app).len(), 3);
    assert_eq!(selected(&app), "cat/gamma", "the selection kept");
    assert!(!screen(&mut app).contains("filter:"));
    assert!(
        list_bottom(&screen(&mut app))
            .trim_start_matches('╚')
            .trim_end_matches('╝')
            .chars()
            .all(|c| c == '═')
    );
}

/// The bottom border of the port list, corners included.
fn list_bottom(shown: &str) -> String {
    let lines: Vec<&str> = shown.lines().collect();
    let row = lines[lines.len() - 2];
    let end = row.find(['┘', '╝']).expect("the list's bottom corner");
    row[..end + row[end..].chars().next().unwrap().len_utf8()].to_string()
}

/// The bottom border of the option pane, corners included.
fn pane_bottom(shown: &str) -> String {
    let lines: Vec<&str> = shown.lines().collect();
    let row = lines[lines.len() - 2];
    let list = list_bottom(shown);
    row[list.len()..].to_string()
}

/// The filter sits on the list's bottom border, at its right, never in
/// its title; too long, it loses its start, the end being typed in view.
#[test]
fn the_filter_is_on_the_bottom_border() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    press(&mut app, '/');
    for c in "a-very-long-filter-that-ends-in-ginx-module".chars() {
        press(&mut app, c);
    }
    let shown = screen_at(&mut app, 80, 24);
    let top = shown.lines().next().unwrap();
    assert!(
        top.starts_with("╔ Ports (0) ═") && !top.contains("filter"),
        "{top}"
    );
    let bottom = list_bottom(&shown);
    assert!(
        bottom.starts_with("╚ filter: …") && bottom.ends_with("-ginx-module▏ ╝"),
        "{bottom}"
    );
    assert!(!bottom.contains("a-very"), "{bottom}");
    hit(&mut app, KeyCode::Enter);
    let bottom = list_bottom(&screen_at(&mut app, 80, 24));
    assert!(
        bottom.ends_with("ginx-module ╝") && bottom.contains(" filter: …"),
        "{bottom}"
    );
    // A short one is whole.
    press(&mut app, '/');
    press(&mut app, 'x');
    assert!(list_bottom(&screen_at(&mut app, 80, 24)).ends_with("═ filter: x▏ ╝"));
}

#[test]
fn list_keys_move_and_jump_between_problems() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    // Problems first: alpha and gamma owe a decision, beta is ok.
    assert_eq!(visible(&app), ["cat/alpha", "cat/gamma", "cat/beta"]);
    press(&mut app, 'j');
    assert_eq!(selected(&app), "cat/gamma");
    press(&mut app, 'k');
    press(&mut app, 'k');
    assert_eq!(selected(&app), "cat/alpha", "stays at the top");
    press(&mut app, 'G');
    assert_eq!(selected(&app), "cat/beta");
    press(&mut app, 'g');
    screen(&mut app);
    hit(&mut app, KeyCode::PageDown);
    assert_eq!(selected(&app), "cat/beta", "clamped");
    hit(&mut app, KeyCode::PageUp);
    assert_eq!(selected(&app), "cat/alpha");
    press(&mut app, 'n');
    assert_eq!(selected(&app), "cat/gamma");
    press(&mut app, 'n');
    assert_eq!(selected(&app), "cat/alpha", "wraps past the ok port");
    press(&mut app, 'p');
    assert_eq!(selected(&app), "cat/gamma");

    // When nothing needs attention, n says so and stays put.
    let tmp = tempfile::tempdir().unwrap();
    let beta = tmp.path().join("options/cat_beta");
    std::fs::create_dir_all(&beta).unwrap();
    std::fs::write(
        beta.join("options"),
        "_FILE_COMPLETE_OPTIONS_LIST=DOCS\nOPTIONS_FILE_UNSET+=DOCS\n",
    )
    .unwrap();
    let mut app = app_for(tmp.path(), &[("cat/beta", &[])]);
    press(&mut app, 'n');
    assert_eq!(selected(&app), "cat/beta");
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(m, _)| m == "no ports need attention")
    );
}

#[test]
fn editor_keys_toggle_undo_reset_and_revert() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    let key = PortKey::parse("cat/alpha").unwrap();
    let info = app.session.ports.get_mut(&key).unwrap();
    info.options.complete = vec!["DOCS".into(), "NLS".into()];
    info.options.defaults = ["NLS".to_string()].into();
    app.rebuild_editor();

    press(&mut app, 'l');
    assert!(app.focus == Focus::List, "l is no key");
    hit(&mut app, KeyCode::Enter);
    assert!(app.focus == Focus::Editor);
    press(&mut app, ' ');
    assert_eq!(staged(&app, "cat/alpha"), ["DOCS"]);
    press(&mut app, 'j');
    hit(&mut app, KeyCode::Enter);
    assert_eq!(staged(&app, "cat/alpha"), ["DOCS"], "Enter does not toggle");
    assert!(app.undo.len() == 1, "nor leaves an undo step");
    press(&mut app, ' ');
    assert_eq!(staged(&app, "cat/alpha"), ["DOCS", "NLS"]);
    press(&mut app, 'j');
    press(&mut app, ' ');
    assert_eq!(
        staged(&app, "cat/alpha"),
        ["DOCS"],
        "j stops at the last option"
    );
    press(&mut app, 'k');
    press(&mut app, 'k');
    press(&mut app, ' ');
    assert!(staged(&app, "cat/alpha").is_empty(), "k stops at the first");
    press(&mut app, 'u');
    assert_eq!(staged(&app, "cat/alpha"), ["DOCS"]);
    press(&mut app, 'd');
    assert_eq!(staged(&app, "cat/alpha"), ["NLS"], "d: the port's defaults");
    press(&mut app, 'U');
    assert!(
        staged(&app, "cat/alpha").is_empty(),
        "U: the saved state (none)"
    );
    press(&mut app, 'i');
    assert!(app.opt_info.is_some());
    press(&mut app, 'x');
    assert!(app.opt_info.is_none(), "any key closes the details");
    hit(&mut app, KeyCode::Esc);
    assert!(app.focus == Focus::List);
    hit(&mut app, KeyCode::Tab);
    hit(&mut app, KeyCode::Left);
    assert!(app.focus == Focus::List);
    while app.undo.pop().is_some() {}
    press(&mut app, 'u');
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(m, _)| m == "nothing to undo")
    );
}

/// From the port list too: d resets the selected port to its defaults,
/// U reverts it to its saved state, u undoes the last change, each one
/// undoable like a toggle.
#[test]
fn list_keys_reset_revert_and_undo() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    let key = PortKey::parse("cat/alpha").unwrap();
    app.session.ports.get_mut(&key).unwrap().options.defaults = ["DOCS".to_string()].into();
    app.rebuild_editor();
    assert!(app.focus == Focus::List);
    press(&mut app, 'd');
    assert_eq!(staged(&app, "cat/alpha"), ["DOCS"], "d from the list");
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(m, _)| m == "reset to port defaults")
    );
    press(&mut app, 'U');
    assert!(staged(&app, "cat/alpha").is_empty(), "U from the list");
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(m, _)| m == "reverted to saved state")
    );
    press(&mut app, 'u');
    assert_eq!(staged(&app, "cat/alpha"), ["DOCS"], "u undoes the revert");
    press(&mut app, 'u');
    assert!(staged(&app, "cat/alpha").is_empty(), "and then the reset");
    assert!(app.undo.is_empty());
}

/// n and p work from the option pane: the next port needing attention,
/// the cursor on its first option, the pane keeping the focus.
#[test]
fn n_and_p_jump_from_the_option_pane() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    for p in ["cat/alpha", "cat/gamma"] {
        let info = app
            .session
            .ports
            .get_mut(&PortKey::parse(p).unwrap())
            .unwrap();
        info.options.complete = vec!["DOCS".into(), "NLS".into()];
    }
    app.rebuild_editor();
    hit(&mut app, KeyCode::Enter);
    press(&mut app, 'j');
    assert_eq!(app.editor_idx, 1);
    press(&mut app, 'n');
    assert_eq!(selected(&app), "cat/gamma");
    assert!(app.focus == Focus::Editor);
    assert_eq!(app.editor_idx, 0, "on the first option");
    press(&mut app, 'j');
    press(&mut app, 'p');
    assert_eq!(selected(&app), "cat/alpha");
    assert_eq!((app.focus == Focus::Editor, app.editor_idx), (true, 0));
}

/// Enter only enters the pane; once there it does nothing at all.
#[test]
fn enter_in_the_option_pane_does_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    hit(&mut app, KeyCode::Enter);
    for _ in 0..3 {
        hit(&mut app, KeyCode::Enter);
    }
    assert!(app.focus == Focus::Editor);
    assert!(!app.session.dirty() && app.undo.is_empty());
    assert!(app.top_overlay().is_none());
}

/// The description keys: v opens the port's, V toggles the panel, I is
/// no key, and i on the list says where it works instead.
#[test]
fn description_keys_v_and_big_v() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let key = PortKey::parse("cat/alpha").unwrap();
    app.session.ports.get_mut(&key).unwrap().about.comment = "The alpha port".into();
    assert!(screen(&mut app).contains("The alpha port"));
    press(&mut app, 'I');
    assert!(app.about_shown() && app.message.is_none(), "I does nothing");
    press(&mut app, 'V');
    assert!(!app.about_shown());
    assert_eq!(
        app.message.as_ref().map(|(m, _)| m.as_str()),
        Some("port description hidden (V: show · v: full text)")
    );
    press(&mut app, 'V');
    assert_eq!(
        app.message.as_ref().map(|(m, _)| m.as_str()),
        Some("port description shown (V: hide · v: full text)")
    );
    press(&mut app, 'i');
    assert!(
        app.port_help.is_none() && app.opt_info.is_none(),
        "i on the list opens nothing"
    );
    assert_eq!(
        app.message.clone(),
        Some((
            "i: option details, in the option pane (Enter) — v: port description".into(),
            false
        ))
    );
    press(&mut app, 'v');
    assert_eq!(app.port_help.as_ref().map(|h| h.title), Some("about"));
}

/// Space on the list toggles nothing: it says which key edits.
#[test]
fn space_on_the_list_says_enter_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    app.clear_message();
    press(&mut app, ' ');
    assert!(app.focus == Focus::List);
    assert!(
        !app.session.dirty() && app.undo.is_empty(),
        "Space on the list toggles nothing"
    );
    assert_eq!(
        app.message.clone(),
        Some(("Enter: edit the options".into(), false))
    );
    assert!(
        status_row(&mut app, 80).starts_with(" Enter: edit the options"),
        "{}",
        status_row(&mut app, 80)
    );
}

/// vi's l is no key: not into the option pane, not a jump in why.
#[test]
fn l_is_no_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = looped_app(tmp.path());
    press(&mut app, 'l');
    assert!(app.focus == Focus::List && app.top_overlay().is_none());
    press(&mut app, 'r');
    hit(&mut app, KeyCode::Down);
    hit(&mut app, KeyCode::Down);
    press(&mut app, 'l');
    assert!(app.why.is_none(), "l closes why, as any other key");
    assert_eq!(selected(&app), "cat/alpha", "and jumps nowhere");
}

/// PgDn/PgUp and Ctrl-F/B page the list and the option pane by the rows
/// the last frame showed, less one; Ctrl-F is not f.
#[test]
fn page_keys_move_by_the_visible_height() {
    let tmp = tempfile::tempdir().unwrap();
    let names: Vec<String> = (0..80).map(|i| format!("cat/p{i:02}")).collect();
    let graph: Vec<(&str, &[&str])> = names.iter().map(|n| (n.as_str(), &[] as &[&str])).collect();
    let mut app = app_for(tmp.path(), &graph);
    screen(&mut app);
    // 35 rows: the status bar and the list's border off.
    assert_eq!(app.page.list, 32);
    let page = page_step(app.page.list);
    hit(&mut app, KeyCode::PageDown);
    assert_eq!(app.list_state.selected(), Some(page));
    ctrl(&mut app, 'f');
    assert_eq!(app.list_state.selected(), Some(2 * page));
    assert!(app.message.is_none(), "Ctrl-F is not f: {:?}", app.message);
    ctrl(&mut app, 'b');
    hit(&mut app, KeyCode::PageUp);
    assert_eq!(app.list_state.selected(), Some(0));
    ctrl(&mut app, 'g');
    assert_eq!(app.list_state.selected(), Some(0), "Ctrl-G is not G");

    // The option pane: its rows between the two panels.
    let key = app.selected_key().unwrap();
    app.session.ports.get_mut(&key).unwrap().options.complete =
        (0..60).map(|i| format!("OPT{i:02}")).collect();
    app.rebuild_editor();
    hit(&mut app, KeyCode::Enter);
    screen(&mut app);
    // At 100 columns the pane is wide enough for a one-line detail.
    assert_eq!(
        app.page.options,
        32 - 2 - 6,
        "detail 1 + rule, description 5 + rule"
    );
    let page = page_step(app.page.options);
    ctrl(&mut app, 'f');
    assert_eq!(app.editor_idx, page);
    let shown = screen(&mut app);
    assert!(
        shown.contains(&format!("OPT{page:02}")) && shown.contains(&format!("OPT{:02}", page - 1)),
        "{shown}"
    );
    hit(&mut app, KeyCode::PageDown);
    assert_eq!(app.editor_idx, 2 * page);
    hit(&mut app, KeyCode::PageDown);
    assert_eq!(app.editor_idx, 59, "stops at the last");
    ctrl(&mut app, 'b');
    assert_eq!(app.editor_idx, 59 - page);
    hit(&mut app, KeyCode::PageUp);
    hit(&mut app, KeyCode::PageUp);
    assert_eq!(app.editor_idx, 0);
}

/// A page that ends on a group header stops on the option before it,
/// never on the header.
#[test]
fn a_page_in_the_option_pane_lands_on_an_option() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    app.editor_rows = vec![
        EditorRow::Option("A".into()),
        EditorRow::Option("B".into()),
        EditorRow::GroupHeader(0),
        EditorRow::Option("C".into()),
    ];
    app.editor_idx = 0;
    app.page.options = 3;
    app.editor_page(1);
    assert_eq!(app.editor_idx, 1, "two rows down is the header");
    app.editor_page(1);
    assert_eq!(app.editor_idx, 3);
    app.page.options = 1;
    app.editor_idx = 1;
    app.editor_page(1);
    assert_eq!(app.editor_idx, 3, "a page of one row skips the header");
}

#[test]
fn view_keys_filter_and_reorder_the_list() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    press(&mut app, 't');
    assert_eq!(visible(&app), ["cat/alpha", "cat/gamma"]);
    press(&mut app, 't');
    assert_eq!(visible(&app).len(), 3);
    press(&mut app, 's');
    assert_eq!(
        visible(&app),
        ["cat/alpha", "cat/beta", "cat/gamma"],
        "alphabetical"
    );
    press(&mut app, 's');
    assert_eq!(visible(&app), ["cat/alpha", "cat/gamma", "cat/beta"]);
    press(&mut app, 'm');
    assert!(app.mc_relax);
    press(&mut app, 'm');
    press(&mut app, 'w');
    assert!(!app.mc_relax && app.warn_mc);
    press(&mut app, 'w');
    assert!(!app.warn_mc);
}

#[test]
fn f_steps_through_the_flavors_of_an_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(
        tmp.path(),
        &[
            ("cat/alpha@a", &[]),
            ("cat/alpha@b", &[]),
            ("cat/beta", &[]),
        ],
    );
    for k in ["cat/alpha@a", "cat/alpha@b"] {
        app.session
            .ports
            .get_mut(&PortKey::parse(k).unwrap())
            .unwrap()
            .flavors = vec!["a".into(), "b".into()];
    }
    app.select_index(0);
    assert_eq!(selected(&app), "cat/alpha@a");
    press(&mut app, 'f');
    assert_eq!(selected(&app), "cat/alpha@b");
    press(&mut app, 'f');
    assert_eq!(selected(&app), "cat/alpha@a", "wraps");
    press(&mut app, 'G');
    press(&mut app, 'f');
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(m, _)| m.contains("no other flavors"))
    );
    press(&mut app, '/');
    for c in "@a".chars() {
        press(&mut app, c);
    }
    hit(&mut app, KeyCode::Enter);
    press(&mut app, 'f');
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(m, _)| m.contains("hidden by the current view"))
    );
}

/// `f` steps past a flavor the list does not show (here hidden by a filter)
/// to the next one it does, rather than stopping on it.
#[test]
fn f_skips_a_flavor_the_list_does_not_show() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(
        tmp.path(),
        &[
            ("cat/alpha@a", &[]),
            ("cat/alpha@b", &[]),
            ("cat/alpha@c", &[]),
        ],
    );
    for k in ["cat/alpha@a", "cat/alpha@b", "cat/alpha@c"] {
        app.session
            .ports
            .get_mut(&PortKey::parse(k).unwrap())
            .unwrap()
            .flavors = vec!["a".into(), "b".into(), "c".into()];
    }
    let b = PortKey::parse("cat/alpha@b").unwrap();
    app.visible.retain(|k| *k != b);
    app.select_index(0);
    assert_eq!(selected(&app), "cat/alpha@a");
    press(&mut app, 'f');
    assert_eq!(selected(&app), "cat/alpha@c", "past the unlisted @b");
}

/// `U` or `d` that changes nothing leaves no undo entry: the next `u` undoes
/// the change before it.
#[test]
fn a_reset_changing_nothing_leaves_no_undo_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[]), ("cat/beta", &[])]);
    app.select_index(0);
    let before = app.undo.len();
    press(&mut app, 'U');
    assert_eq!(app.undo.len(), before);
    // On a full stack the oldest entry stays.
    for i in 0..200 {
        app.push_undo_entry(format!("entry{i}"), Default::default());
    }
    let oldest = app.undo.first().cloned();
    press(&mut app, 'U');
    assert_eq!(app.undo.first().cloned(), oldest);
    assert!(
        app.message
            .as_ref()
            .is_some_and(|(m, _)| m.contains("nothing to change"))
    );
}

/// Replace the app's refresher with channels the test drives: what a
/// background scan would send arrives exactly when the test says.
fn fake_refresher(
    app: &mut App,
) -> (
    crossbeam_channel::Sender<RefreshEvent>,
    crossbeam_channel::Receiver<Vec<PortKey>>,
) {
    let (req_tx, req_rx) = crossbeam_channel::unbounded();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
    app.refresher = Refresher {
        tx: req_tx,
        rx: ev_rx,
    };
    (ev_tx, req_rx)
}

fn done(ports: Vec<crate::model::port::PortInfo>, errors: Vec<(PortKey, String)>) -> RefreshEvent {
    let ports = ports
        .into_iter()
        .map(|i| (i.canonical.clone(), i))
        .collect();
    RefreshEvent::Done {
        result: Box::new(crate::query::scanner::ScanResult {
            ports,
            errors,
            ..Default::default()
        }),
        batches: 1,
    }
}

fn info_of(app: &App, origin: &str) -> crate::model::port::PortInfo {
    app.session.ports[&PortKey::parse(origin).unwrap()].clone()
}

/// A re-scan's ports merge into the closure: a new dependency joins,
/// one no longer reached from the roots leaves, and the line says so.
#[test]
fn a_rescan_adds_and_drops_dependencies() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(
        tmp.path(),
        &[("cat/alpha", &["cat/beta"]), ("cat/beta", &[])],
    );
    app.session.roots = vec![PortKey::parse("cat/alpha").unwrap()];
    let (ev, _req) = fake_refresher(&mut app);
    let mut alpha = info_of(&app, "cat/alpha");
    let mut gamma = info_of(&app, "cat/beta");
    gamma.key = PortKey::parse("cat/gamma").unwrap();
    gamma.canonical = gamma.key.clone();
    gamma.options_name = "cat_gamma".into();
    alpha.deps[0].target = gamma.key.clone();
    app.refreshing = 1;
    app.clear_message();
    ev.send(done(vec![alpha, gamma], vec![])).unwrap();
    app.drain_refresh_events();
    assert_eq!(app.refreshing, 0);
    assert_eq!(visible(&app), ["cat/alpha", "cat/gamma"]);
    assert!(
        app.session.state(&info_of(&app, "cat/gamma")).is_some(),
        "the newcomer got a state"
    );
    let (msg, err) = app.message.clone().unwrap();
    assert_eq!(
        (msg.as_str(), err),
        ("dependencies refreshed: +cat/gamma −cat/beta", false)
    );

    // A merge that only drops is reported too; one that changes nothing
    // says nothing.
    let mut alpha = info_of(&app, "cat/alpha");
    alpha.deps.clear();
    app.clear_message();
    ev.send(done(vec![alpha.clone()], vec![])).unwrap();
    app.drain_refresh_events();
    assert_eq!(
        app.message.clone().unwrap().0,
        "dependencies refreshed: −cat/gamma"
    );
    // And one that only adds.
    let mut beta = info_of(&app, "cat/alpha");
    beta.key = PortKey::parse("cat/beta").unwrap();
    beta.canonical = beta.key.clone();
    beta.options_name = "cat_beta".into();
    let mut with_beta = alpha.clone();
    with_beta.deps = vec![crate::model::port::DepEdge {
        target: beta.key.clone(),
        spec: "dep:cat/beta".into(),
        test_only: false,
    }];
    app.clear_message();
    ev.send(done(vec![with_beta, beta], vec![])).unwrap();
    app.drain_refresh_events();
    assert_eq!(
        app.message.clone().unwrap().0,
        "dependencies refreshed: +cat/beta"
    );
    alpha = info_of(&app, "cat/alpha");
    app.clear_message();
    ev.send(done(vec![alpha], vec![])).unwrap();
    app.drain_refresh_events();
    assert!(app.message.is_none(), "nothing changed: {:?}", app.message);
}

/// Under t, a re-query that makes the edited port ok keeps it listed and
/// in the pane, so Space still edits it; it goes once the selection
/// moves to another port.
#[test]
fn a_rescan_keeps_the_selected_port_listed_until_the_selection_moves() {
    let tmp = tempfile::tempdir().unwrap();
    alpha_file(tmp.path(), &["DOCS"], &[]);
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[]), ("cat/beta", &[])]);
    alpha_mut(&mut app).options.complete = vec!["DOCS".into(), "NEW".into()];
    app.rebuild_visible(None);
    press(&mut app, 't');
    let at = app
        .visible
        .iter()
        .position(|k| k.origin == "cat/alpha")
        .unwrap();
    app.select_index(at);
    hit(&mut app, KeyCode::Enter);
    assert!(app.focus == Focus::Editor);
    let (ev, _req) = fake_refresher(&mut app);
    let mut alpha = info_of(&app, "cat/alpha");
    alpha.options.complete = vec!["DOCS".into()];
    app.refreshing = 1;
    ev.send(done(vec![alpha], vec![])).unwrap();
    app.drain_refresh_events();
    assert!(app.effective_status(&info_of(&app, "cat/alpha")) == UiStatus::Ok);
    assert!(app.hide_ok, "t still on");
    assert_eq!(selected(&app), "cat/alpha");
    assert!(app.focus == Focus::Editor, "the pane keeps the focus");
    assert_eq!(
        app.editor_port.as_ref().map(ToString::to_string).as_deref(),
        Some("cat/alpha")
    );
    hit(&mut app, KeyCode::Char(' '));
    assert_eq!(staged(&app, "cat/alpha"), ["DOCS"]);
    assert!(
        staged(&app, "cat/beta").is_empty(),
        "the port the pane did not show"
    );
    // Moved away, the ok port goes at the next rebuild.
    hit(&mut app, KeyCode::Esc);
    let at = app
        .visible
        .iter()
        .position(|k| k.origin == "cat/beta")
        .unwrap();
    app.select_index(at);
    app.refresh_view();
    assert_eq!(visible(&app), ["cat/beta"]);
}

/// A re-query that takes the edited port out of the closure cannot keep
/// it: the focus goes back to the list, and the line says why.
#[test]
fn a_rescan_that_drops_the_edited_port_returns_to_the_list() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(
        tmp.path(),
        &[("cat/alpha", &["cat/gamma"]), ("cat/gamma", &[])],
    );
    app.session.roots = vec![PortKey::parse("cat/alpha").unwrap()];
    let at = app
        .visible
        .iter()
        .position(|k| k.origin == "cat/gamma")
        .unwrap();
    app.select_index(at);
    hit(&mut app, KeyCode::Enter);
    assert!(app.focus == Focus::Editor);
    let (ev, _req) = fake_refresher(&mut app);
    let mut alpha = info_of(&app, "cat/alpha");
    alpha.deps.clear();
    app.refreshing = 1;
    ev.send(done(vec![alpha], vec![])).unwrap();
    app.drain_refresh_events();
    assert_eq!(visible(&app), ["cat/alpha"]);
    assert!(app.focus == Focus::List, "the pane would show another port");
    let (msg, _) = app.message.clone().unwrap();
    assert_eq!(msg, "cat/gamma left the closure: back to the list");
}

/// A newcomer was queried without its real options file, which staging
/// never held: the file is copied into staging byte for byte and the
/// port queried again with it. One whose file staging holds is not.
#[test]
fn a_newcomer_is_queried_again_with_its_real_options_file() {
    let tmp = tempfile::tempdir().unwrap();
    let real = b"# by hand\n_OPTIONS_READ=gamma-1.0\n_FILE_COMPLETE_OPTIONS_LIST=DOCS\nOPTIONS_FILE_SET+=DOCS\n";
    std::fs::create_dir_all(tmp.path().join("options/cat_gamma")).unwrap();
    std::fs::write(tmp.path().join("options/cat_gamma/options"), real).unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let (ev, req) = fake_refresher(&mut app);
    let staged = tmp.path().join("db/cat_gamma/options");
    assert!(!staged.exists());
    let mut alpha = info_of(&app, "cat/alpha");
    let mut gamma = alpha.clone();
    gamma.key = PortKey::parse("cat/gamma").unwrap();
    gamma.canonical = gamma.key.clone();
    gamma.options_name = "cat_gamma".into();
    let mut delta = gamma.clone();
    delta.key = PortKey::parse("cat/delta").unwrap();
    delta.canonical = delta.key.clone();
    delta.options_name = "cat_delta".into();
    alpha.deps = [&gamma, &delta]
        .iter()
        .map(|p| crate::model::port::DepEdge {
            target: p.key.clone(),
            spec: format!("dep:{}", p.key),
            test_only: false,
        })
        .collect();
    app.refreshing = 1;
    ev.send(done(vec![alpha, gamma, delta], vec![])).unwrap();
    app.drain_refresh_events();
    let gamma = PortKey::parse("cat/gamma").unwrap();
    assert!(
        app.pending.contains_key(&gamma),
        "the newcomer is queried again"
    );
    assert!(
        !app.pending
            .contains_key(&PortKey::parse("cat/delta").unwrap()),
        "a newcomer with no file was queried as it is"
    );
    app.pending
        .insert(gamma.clone(), Instant::now() - REFRESH_DEBOUNCE * 2);
    app.flush_due_refreshes();
    assert_eq!(
        std::fs::read(&staged).unwrap(),
        real,
        "its real file, byte for byte"
    );
    assert_eq!(req.try_recv().unwrap(), vec![gamma]);
}

/// An edit to a shared file is queried for every flavor reading it: a
/// dependency the edit gives a sibling flavor joins the closure too.
#[test]
fn an_edit_requeries_every_flavor_sharing_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let (_ev, req) = fake_refresher(&mut app);
    let alpha = PortKey::parse("cat/alpha").unwrap();
    let mut sibling = info_of(&app, "cat/alpha");
    sibling.key = PortKey::parse("cat/alpha@nox").unwrap();
    sibling.canonical = sibling.key.clone();
    let name = sibling.options_name.clone();
    app.session
        .ports
        .insert(sibling.key.clone(), sibling.clone());
    app.session.owners.insert(name, alpha.clone());
    app.pending
        .insert(alpha.clone(), Instant::now() - REFRESH_DEBOUNCE * 2);
    app.flush_due_refreshes();
    let mut sent = req.try_recv().unwrap();
    sent.sort();
    assert_eq!(sent, vec![alpha, sibling.key]);
}

/// A newcomer with an options.local and no options file is queried again
/// with that options.local, which make reads all the same.
#[test]
fn a_newcomer_with_only_an_options_local_is_queried_again_with_it() {
    let tmp = tempfile::tempdir().unwrap();
    let local = b"OPTIONS_FILE_SET+=DOCS\n";
    std::fs::create_dir_all(tmp.path().join("options/cat_gamma")).unwrap();
    std::fs::write(tmp.path().join("options/cat_gamma/options.local"), local).unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let (ev, req) = fake_refresher(&mut app);
    let mut alpha = info_of(&app, "cat/alpha");
    let mut gamma = alpha.clone();
    gamma.key = PortKey::parse("cat/gamma").unwrap();
    gamma.canonical = gamma.key.clone();
    gamma.options_name = "cat_gamma".into();
    alpha.deps = vec![crate::model::port::DepEdge {
        target: gamma.key.clone(),
        spec: "dep:cat/gamma".into(),
        test_only: false,
    }];
    app.refreshing = 1;
    ev.send(done(vec![alpha, gamma], vec![])).unwrap();
    app.drain_refresh_events();
    let gamma = PortKey::parse("cat/gamma").unwrap();
    assert!(
        app.pending.contains_key(&gamma),
        "the newcomer is queried again"
    );
    app.pending
        .insert(gamma.clone(), Instant::now() - REFRESH_DEBOUNCE * 2);
    app.flush_due_refreshes();
    let staged = tmp.path().join("db/cat_gamma/options.local");
    assert_eq!(
        std::fs::read(&staged).unwrap(),
        local,
        "its options.local, byte for byte"
    );
    assert_eq!(req.try_recv().unwrap(), vec![gamma]);
}

/// Progress shows while a scan runs and goes with it; a dependency that
/// failed to query is reported with the first error.
#[test]
fn a_rescan_reports_progress_and_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let (ev, _req) = fake_refresher(&mut app);
    app.refreshing = 1;
    app.clear_message();
    ev.send(RefreshEvent::Progress {
        done: 3,
        discovered: 5,
    })
    .unwrap();
    app.drain_refresh_events();
    assert_eq!(app.refresh_progress, Some((3, 5)));
    assert!(app.message.is_none(), "nothing merged yet");
    let failed = PortKey::parse("cat/broken").unwrap();
    ev.send(done(
        vec![info_of(&app, "cat/alpha")],
        vec![(failed, "no such port".into())],
    ))
    .unwrap();
    app.drain_refresh_events();
    assert_eq!(app.refresh_progress, None);
    let (msg, err) = app.message.clone().unwrap();
    assert_eq!(
        msg,
        "refresh: 1 dep error(s), e.g. cat/broken: no such port"
    );
    assert!(err);
}

/// A port that turns IGNORE/BROKEN after an option change is blamed on
/// that change; one with no recent change is not; recovering clears it.
#[test]
fn a_port_that_breaks_is_blamed_on_the_last_change() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[]), ("cat/beta", &[])]);
    let (ev, _req) = fake_refresher(&mut app);
    app.refresh_snapshots.insert(
        "cat_alpha".into(),
        (["DOCS".to_string()].into(), Some(BTreeSet::new())),
    );
    let mut alpha = info_of(&app, "cat/alpha");
    alpha.ignore = Some(format!("needs   {}", "x".repeat(100)));
    let mut beta = info_of(&app, "cat/beta");
    beta.broken = Some("fails anyway".into());
    ev.send(done(vec![beta.clone(), alpha.clone()], vec![]))
        .unwrap();
    app.drain_refresh_events();
    let (msg, err) = app.message.clone().unwrap();
    assert!(err);
    assert!(msg.starts_with("cat/alpha: now IGNORE (needs x"), "{msg}");
    assert!(
        msg.ends_with("…) — +DOCS"),
        "reason cut at 80 characters: {msg}"
    );
    assert_eq!(
        app.blame.get("cat_alpha").map(String::as_str),
        Some("+DOCS")
    );
    assert!(
        !app.blame.contains_key("cat_beta"),
        "no recent change to blame"
    );

    // BROKEN names itself first when both are set.
    alpha.broken = Some("short".into());
    app.blame.clear();
    app.session.ports.get_mut(&alpha.key).unwrap().ignore = None;
    ev.send(done(vec![alpha.clone()], vec![])).unwrap();
    app.drain_refresh_events();
    assert!(
        app.message
            .clone()
            .unwrap()
            .0
            .starts_with("cat/alpha: now BROKEN (short)")
    );

    alpha.broken = None;
    alpha.ignore = None;
    ev.send(done(vec![alpha], vec![])).unwrap();
    app.drain_refresh_events();
    assert!(
        !app.blame.contains_key("cat_alpha"),
        "a port that recovers is cleared"
    );
}

/// A port that just broke outranks the refresh count on the line, even
/// when the same re-scan also added a dependency.
#[test]
fn a_rescan_that_breaks_a_port_and_adds_a_dep_keeps_the_blame() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let (ev, _req) = fake_refresher(&mut app);
    app.refresh_snapshots.insert(
        "cat_alpha".into(),
        (["DOCS".to_string()].into(), Some(BTreeSet::new())),
    );
    let mut alpha = info_of(&app, "cat/alpha");
    let mut gamma = alpha.clone();
    gamma.key = PortKey::parse("cat/gamma").unwrap();
    gamma.canonical = gamma.key.clone();
    gamma.options_name = "cat_gamma".into();
    alpha.ignore = Some("needs gamma".into());
    alpha.deps = vec![crate::model::port::DepEdge {
        target: gamma.key.clone(),
        spec: "dep:cat/gamma".into(),
        test_only: false,
    }];
    ev.send(done(vec![alpha, gamma], vec![])).unwrap();
    app.drain_refresh_events();
    assert!(
        app.session
            .ports
            .contains_key(&PortKey::parse("cat/gamma").unwrap()),
        "added"
    );
    let (msg, err) = app.message.clone().unwrap();
    assert_eq!(
        (msg.as_str(), err),
        ("cat/alpha: now IGNORE (needs gamma) — +DOCS", true)
    );

    // A dep error outranks it too.
    let failed = PortKey::parse("cat/broken").unwrap();
    let mut alpha = info_of(&app, "cat/alpha");
    alpha.deps.clear();
    ev.send(done(vec![alpha], vec![(failed, "no such port".into())]))
        .unwrap();
    app.drain_refresh_events();
    let (msg, err) = app.message.clone().unwrap();
    assert_eq!(
        (msg.as_str(), err),
        (
            "refresh: 1 dep error(s), e.g. cat/broken: no such port",
            true
        )
    );
}

/// "dependency loop gone" is news, not an alert: posted by the same
/// rescan that broke a port, it leaves the blame on the line.
#[test]
fn a_loop_gone_in_the_rescan_that_broke_a_port_keeps_the_blame() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(
        tmp.path(),
        &[("cat/alpha", &["cat/beta"]), ("cat/beta", &["cat/alpha"])],
    );
    let (ev, _req) = fake_refresher(&mut app);
    press(&mut app, 'L');
    app.refresh_snapshots.insert(
        "cat_beta".into(),
        (["DOCS".to_string()].into(), Some(BTreeSet::new())),
    );
    let mut beta = info_of(&app, "cat/beta");
    beta.deps.clear();
    beta.ignore = Some("needs alpha".into());
    ev.send(done(vec![beta], vec![])).unwrap();
    app.drain_refresh_events();
    assert!(
        app.loops.is_empty() && !app.only_loops,
        "the L view is still left"
    );
    let (msg, err) = app.message.clone().unwrap();
    assert_eq!(
        (msg.as_str(), err),
        ("cat/beta: now IGNORE (needs alpha) — +DOCS", true)
    );
}

/// The m and w views and the blacklist change what counts as a problem.
#[test]
fn views_change_what_counts_as_a_problem() {
    let tmp = tempfile::tempdir().unwrap();
    let pd = tmp.path().join("poudriere.d");
    std::fs::create_dir_all(&pd).unwrap();
    std::fs::write(pd.join("blacklist"), "cat/gamma\n").unwrap();
    let mut app = three_ports(tmp.path());
    for p in ["cat/alpha", "cat/beta"] {
        let info = app
            .session
            .ports
            .get_mut(&PortKey::parse(p).unwrap())
            .unwrap();
        info.options.mc_set = ["DOCS".to_string()].into();
    }
    let status = |app: &App, p: &str| app.effective_status(&info_of(app, p));
    // alpha: no file, but make.conf decides DOCS: done, m view or not,
    // as apply writes it none.
    assert_eq!(status(&app, "cat/alpha"), UiStatus::Ok);
    press(&mut app, 'm');
    assert_eq!(status(&app, "cat/alpha"), UiStatus::Ok);
    // beta: its file turns DOCS off, make.conf turns it on.
    assert_eq!(status(&app, "cat/beta"), UiStatus::Ok);
    press(&mut app, 'w');
    assert_eq!(status(&app, "cat/beta"), UiStatus::McDeviation);
    // alpha's staged set predates make.conf's stance too: also ≠.
    assert_eq!(status(&app, "cat/alpha"), UiStatus::McDeviation);
    press(&mut app, 't');
    assert_eq!(visible(&app), ["cat/gamma", "cat/alpha", "cat/beta"]);
    press(&mut app, 'w');
    assert_eq!(
        visible(&app),
        ["cat/gamma"],
        "without ≠, only gamma owes a decision"
    );
    // A blacklisted port never needs attention.
    app.blacklist = crate::config::Blacklist::load(&pd, None, "default", None);
    assert_eq!(status(&app, "cat/gamma"), UiStatus::Ok);
}

/// The apply preview scrolls, closes without writing, and once applied
/// any key closes the result.
#[test]
fn the_apply_preview_scrolls_and_closes() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    press(&mut app, 'a');
    press(&mut app, 'j');
    press(&mut app, 'j');
    press(&mut app, 'k');
    assert_eq!(app.modal.as_ref().unwrap().scroll, 1);
    press(&mut app, 'k');
    press(&mut app, 'k');
    assert_eq!(app.modal.as_ref().unwrap().scroll, 0);
    hit(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
    assert!(
        !tmp.path().join("options/cat_alpha").exists(),
        "closing writes nothing"
    );
    press(&mut app, 't');
    press(&mut app, 'a');
    press(&mut app, 'y');
    assert!(app.modal.as_ref().unwrap().done.is_some());
    assert!(
        visible(&app).is_empty(),
        "rebuilt under the result: nothing needs attention"
    );
    press(&mut app, 'x');
    assert!(app.modal.is_none());
}

/// A port with no file make.conf settles gets none from apply, and the
/// list counts it done; an edit to it is written, even once a refresh has
/// queried the edit into the port's effective options, where it reads as
/// what sync would record.
#[test]
fn apply_leaves_a_port_make_conf_settles_but_writes_its_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    let key = PortKey::parse("cat/alpha").unwrap();
    let info = app.session.ports.get_mut(&key).unwrap();
    info.options.mc_unset = ["DOCS".to_string()].into();
    info.options.effective.clear();
    let state = app.session.states.get_mut("cat_alpha").unwrap();
    state.staged.clear();
    state.baseline.clear();
    assert_eq!(
        app.effective_status(&info_of(&app, "cat/alpha")),
        UiStatus::Ok
    );
    let touched = |app: &App| {
        let planned = app.compute_writes();
        planned.touched().map(str::to_owned).collect::<Vec<_>>()
    };
    assert!(!touched(&app).contains(&"cat_alpha".to_string()));

    app.session.states.get_mut("cat_alpha").unwrap().staged = ["DOCS".to_string()].into();
    let (ev, _req) = fake_refresher(&mut app);
    let mut alpha = info_of(&app, "cat/alpha");
    alpha.options.effective = ["DOCS".to_string()].into();
    ev.send(done(vec![alpha], vec![])).unwrap();
    app.drain_refresh_events();
    assert!(
        app.session.edited(&info_of(&app, "cat/alpha")),
        "still edited"
    );
    assert!(touched(&app).contains(&"cat_alpha".to_string()));
    press(&mut app, 'a');
    press(&mut app, 'y');
    let written = std::fs::read_to_string(tmp.path().join("options/cat_alpha/options")).unwrap();
    assert!(written.contains("OPTIONS_FILE_SET+=DOCS"), "{written}");
}

/// Flavors sharing a file make.conf decides for each get none from apply,
/// though the shared staged set holds an option one of them lacks.
#[test]
fn apply_leaves_flavors_make_conf_settles() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha@a", &[]), ("cat/alpha@b", &[])]);
    for (k, complete) in [
        ("cat/alpha@a", &["DOCS", "GUI"][..]),
        ("cat/alpha@b", &["DOCS"]),
    ] {
        let info = app
            .session
            .ports
            .get_mut(&PortKey::parse(k).unwrap())
            .unwrap();
        info.flavors = vec!["a".into(), "b".into()];
        info.options.complete = complete.iter().map(ToString::to_string).collect();
        info.options.effective = info.options.complete.iter().cloned().collect();
        info.options.mc_set = ["DOCS".to_string(), "GUI".to_string()].into();
    }
    let state = app.session.states.get_mut("cat_alpha").unwrap();
    state.staged = ["DOCS".to_string(), "GUI".to_string()].into();
    state.baseline = state.staged.clone();
    for k in ["cat/alpha@a", "cat/alpha@b"] {
        assert_eq!(app.effective_status(&info_of(&app, k)), UiStatus::Ok, "{k}");
    }
    let planned = app.compute_writes();
    assert_eq!(planned.touched().count(), 0, "{:?}", planned.writes);
}

/// Apply writes every port, listed or not, so its preview warns about a
/// conflict the filter keeps off the list too.
#[test]
fn the_apply_preview_warns_about_a_conflict_hidden_by_the_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    let key = PortKey::parse("cat/alpha").unwrap();
    let info = app.session.ports.get_mut(&key).unwrap();
    info.options.complete = vec!["DOCS".into(), "NLS".into()];
    info.options.defs.insert(
        "DOCS".into(),
        crate::model::options::OptionDef {
            prevents: vec!["NLS".into()],
            ..Default::default()
        },
    );
    app.session.states.get_mut("cat_alpha").unwrap().staged =
        ["DOCS".to_string(), "NLS".to_string()].into();
    assert_eq!(
        app.session.status(&info_of(&app, "cat/alpha")),
        UiStatus::Conflict
    );
    press(&mut app, '/');
    for c in "gam".chars() {
        press(&mut app, c);
    }
    hit(&mut app, KeyCode::Enter);
    assert_eq!(visible(&app), ["cat/gamma"]);
    press(&mut app, 'a');
    let modal = app.modal.as_ref().expect("the apply preview opened");
    assert_eq!(modal.conflicted, [key]);
    let shown = screen(&mut app);
    assert!(shown.contains(" apply with 1 conflicting? "), "{shown}");
    assert!(
        shown.contains("conflicting (Y writes them, s skips them):"),
        "{shown}"
    );
}

/// The apply's result as its lines read, the options directory whole;
/// empty while there is none.
fn done_text(app: &App) -> String {
    let dir = app.options_dir.display().to_string();
    let done = app.modal.as_ref().and_then(|m| m.done.as_ref());
    done.map_or_else(String::new, |d| d.lines(&dir).join("\n"))
}

/// Put `origin` in conflict: two members of a SINGLE group staged on.
fn make_conflict(app: &mut App, origin: &str) {
    use crate::model::options::{GroupKind, OptionGroup};
    let key = PortKey::parse(origin).unwrap();
    let info = app.session.ports.get_mut(&key).unwrap();
    info.options.complete = vec!["DOCS".into(), "X11".into(), "WAYLAND".into()];
    info.options.groups = vec![OptionGroup {
        kind: GroupKind::Single,
        name: "GUI".into(),
        desc: String::new(),
        members: vec!["X11".into(), "WAYLAND".into()],
    }];
    let name = info.options_name.clone();
    app.session.states.get_mut(&name).unwrap().staged =
        ["X11".to_string(), "WAYLAND".to_string()].into();
    assert_eq!(
        app.session.status(&info_of(app, origin)),
        UiStatus::Conflict
    );
}

const BETA_FILE: &[u8] =
    b"_OPTIONS_READ=beta-1.0\n_FILE_COMPLETE_OPTIONS_LIST=DOCS\nOPTIONS_FILE_UNSET+=DOCS\n";

/// Nothing in conflict: the preview asks y or n, and y writes.
#[test]
fn a_clean_apply_is_confirmed_with_y() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    press(&mut app, 'a');
    let shown = screen(&mut app);
    assert!(shown.contains(" apply? "), "{shown}");
    assert!(
        shown.contains(" y: write · n: cancel · j/k scroll "),
        "{shown}"
    );
    assert!(!shown.contains("conflicting"), "{shown}");
    press(&mut app, 'y');
    let done = done_text(&app);
    assert!(done.starts_with("2 file(s) written"), "{done}");
    assert!(!done.contains("skipped"), "{done}");
    let shown = screen(&mut app);
    assert!(shown.contains(" apply — done "), "{shown}");
    assert!(shown.contains(" any key closes "), "{shown}");
    assert!(tmp.path().join("options/cat_alpha/options").is_file());
}

/// A re-query merged while the preview is open changes the plan: y
/// writes nothing then, the preview shows the new plan and the line
/// says so; the next y writes what it shows.
#[test]
fn a_plan_changed_under_the_preview_is_shown_again_not_written() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let (ev, _req) = fake_refresher(&mut app);
    press(&mut app, 'a');
    assert_eq!(app.modal.as_ref().unwrap().writes.len(), 1);
    let mut alpha = info_of(&app, "cat/alpha");
    let mut gamma = alpha.clone();
    gamma.key = PortKey::parse("cat/gamma").unwrap();
    gamma.canonical = gamma.key.clone();
    gamma.options_name = "cat_gamma".into();
    alpha.deps = vec![crate::model::port::DepEdge {
        target: gamma.key.clone(),
        spec: "dep:cat/gamma".into(),
        test_only: false,
    }];
    app.refreshing = 1;
    ev.send(done(vec![alpha, gamma], vec![])).unwrap();
    app.drain_refresh_events();
    press(&mut app, 'y');
    assert!(
        !tmp.path().join("options/cat_alpha").exists(),
        "nothing written"
    );
    let modal = app.modal.as_ref().expect("the preview stays open");
    assert!(modal.done.is_none());
    assert_eq!(modal.writes.len(), 2, "it shows the new plan");
    let (msg, err) = app.message.clone().unwrap();
    assert!(
        msg.starts_with("the plan changed since the preview"),
        "{msg}"
    );
    assert!(err);
    press(&mut app, 'y');
    assert!(
        done_text(&app).starts_with("2 file(s) written"),
        "{}",
        done_text(&app)
    );
}

/// A conflict that appears under the preview is not written by the y
/// the preview without it allowed: the preview asks again, Y or s.
#[test]
fn a_conflict_appearing_under_the_preview_is_asked_about() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    press(&mut app, 'a');
    assert!(app.modal.as_ref().unwrap().conflicted.is_empty());
    make_conflict(&mut app, "cat/alpha");
    press(&mut app, 'y');
    assert!(
        !tmp.path().join("options/cat_alpha").exists(),
        "nothing written"
    );
    let modal = app.modal.as_ref().expect("the preview stays open");
    assert!(modal.done.is_none());
    assert_eq!(modal.conflicted.len(), 1);
    press(&mut app, 'y');
    assert!(
        app.modal.as_ref().unwrap().done.is_none(),
        "y is no answer to a conflict"
    );
}

/// With a port in conflict, y is no answer: the preview stays open,
/// nothing is written, and the line says what the keys are.
#[test]
fn a_conflicting_apply_ignores_y() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    make_conflict(&mut app, "cat/beta");
    press(&mut app, 'a');
    let shown = screen(&mut app);
    assert!(shown.contains(" apply with 1 conflicting? "), "{shown}");
    assert!(
        shown.contains(" Y: write anyway · s: skip conflicting · n: cancel · j/k scroll "),
        "{shown}"
    );
    // The conflicting port heads the list, under its own heading.
    let rows: Vec<&str> = shown.lines().collect();
    let heading = rows
        .iter()
        .position(|r| r.contains("conflicting (Y writes them, s skips them):"));
    let heading = heading.expect("the conflicting heading");
    assert!(rows[heading + 1].contains("cat/beta"), "{shown}");
    assert!(
        !shown.contains('⚠'),
        "the heading replaces the warning: {shown}"
    );
    press(&mut app, 'y');
    let modal = app.modal.as_ref().expect("y leaves the preview open");
    assert!(modal.done.is_none());
    assert_eq!(
        app.message,
        Some((
            "conflicts remain: Y writes anyway · s skips them · n cancels".to_string(),
            false
        ))
    );
    assert!(
        !tmp.path().join("options/cat_alpha").exists(),
        "y writes nothing"
    );
    assert_eq!(
        std::fs::read(tmp.path().join("options/cat_beta/options")).unwrap(),
        BETA_FILE
    );
    press(&mut app, 'n');
    assert!(app.modal.is_none(), "n cancels");
    assert!(
        !tmp.path().join("options/cat_alpha").exists(),
        "n writes nothing"
    );
}

/// Y writes every file, the conflicting one included, and the draft has
/// served its purpose.
#[test]
fn a_conflicting_apply_writes_all_with_capital_y() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    make_conflict(&mut app, "cat/beta");
    draft::save(&app.session, &app.options_dir).unwrap();
    press(&mut app, 'a');
    press(&mut app, 'Y');
    let done = done_text(&app);
    assert!(done.starts_with("3 file(s) written"), "{done}");
    assert!(!done.contains("skipped"), "{done}");
    assert_ne!(
        std::fs::read(tmp.path().join("options/cat_beta/options")).unwrap(),
        BETA_FILE
    );
    assert!(
        draft::load(&app.options_dir).is_none(),
        "the draft is discarded"
    );
}

/// s writes every file but the conflicting port's, which stays as it is
/// on disk with its edits staged, still in conflict, still in the draft.
#[test]
fn a_conflicting_apply_skips_the_conflict_with_s() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    make_conflict(&mut app, "cat/beta");
    draft::save(&app.session, &app.options_dir).unwrap();
    press(&mut app, 'a');
    press(&mut app, 's');
    let done = done_text(&app);
    assert!(done.starts_with("2 file(s) written"), "{done}");
    assert!(
        done.contains("\n1 port(s) skipped: conflicting options"),
        "{done}"
    );
    assert!(tmp.path().join("options/cat_alpha/options").is_file());
    assert!(tmp.path().join("options/cat_gamma/options").is_file());
    assert_eq!(
        std::fs::read(tmp.path().join("options/cat_beta/options")).unwrap(),
        BETA_FILE
    );
    assert_eq!(
        app.session.status(&info_of(&app, "cat/beta")),
        UiStatus::Conflict
    );
    assert_eq!(
        staged(&app, "cat/beta"),
        ["WAYLAND", "X11"],
        "the edit stays staged"
    );
    assert!(app.session.dirty());
    assert!(draft::load(&app.options_dir).is_some(), "the draft is kept");
    press(&mut app, 'x');
    assert!(app.modal.is_none());
    // Applied again, only the conflict is left to write.
    press(&mut app, 'a');
    assert_eq!(app.modal.as_ref().unwrap().writes.len(), 1);
}

/// Flavors share one options file: a conflict in any of them makes the
/// file conflicting, and s skips it, written from the default flavor or
/// not.
#[test]
fn a_conflicting_flavor_skips_the_shared_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(
        tmp.path(),
        &[
            ("cat/alpha@a", &[]),
            ("cat/alpha@b", &[]),
            ("cat/beta", &[]),
        ],
    );
    for k in ["cat/alpha@a", "cat/alpha@b"] {
        let info = app
            .session
            .ports
            .get_mut(&PortKey::parse(k).unwrap())
            .unwrap();
        info.flavors = vec!["a".into(), "b".into()];
        info.options.complete = vec!["DOCS".into(), "X11".into(), "WAYLAND".into()];
    }
    // Only the non-default flavor has the group the staged set breaks.
    make_conflict(&mut app, "cat/alpha@b");
    assert_ne!(
        app.session.status(&info_of(&app, "cat/alpha@a")),
        UiStatus::Conflict
    );
    press(&mut app, 'a');
    let modal = app.modal.as_ref().unwrap();
    assert_eq!(modal.conflicted, [PortKey::parse("cat/alpha@b").unwrap()]);
    assert_eq!(modal.conflicting_files, ["cat_alpha".to_string()].into());
    press(&mut app, 's');
    let done = done_text(&app);
    assert!(done.starts_with("1 file(s) written"), "{done}");
    assert!(
        done.contains("\n1 port(s) skipped: conflicting options"),
        "{done}"
    );
    assert!(
        !tmp.path().join("options/cat_alpha").exists(),
        "the shared file is skipped"
    );
    assert!(tmp.path().join("options/cat_beta/options").is_file());
}

/// Under -m the defaults are the decision: a port with no options file
/// (or a stale one whose new options the defaults settle) is ≈, not ?,
/// without the m view; an edit shows beside it.
#[test]
fn minimal_mode_counts_bare_defaults_as_decided() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    let status = |app: &App, p: &str| app.effective_status(&info_of(app, p));
    assert_eq!(
        status(&app, "cat/alpha"),
        UiStatus::Unconfigured,
        "without -m"
    );
    // As -m sets them both.
    app.minimal = true;
    app.session.minimal = true;
    assert!(!app.mc_relax);
    assert_eq!(status(&app, "cat/alpha"), UiStatus::Ok);
    hit(&mut app, KeyCode::Enter);
    press(&mut app, ' ');
    assert_eq!(
        status(&app, "cat/alpha"),
        UiStatus::Ok,
        "an edit is no state"
    );
    assert!(
        app.session.edited(&info_of(&app, "cat/alpha")),
        "but it shows: ≈*"
    );
    assert!(
        screen(&mut app).contains("≈* cat/alpha"),
        "{}",
        screen(&mut app)
    );
}

/// In minimal mode apply removes a file that only repeats the defaults
/// and says so.
#[test]
fn minimal_apply_reports_the_files_it_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    app.minimal = true;
    press(&mut app, 'a');
    press(&mut app, 'y');
    let done = done_text(&app);
    assert!(
        done.contains("\n1 redundant file(s) removed (--minimal)"),
        "{done}"
    );
    assert!(!tmp.path().join("options/cat_beta").exists());
}

/// A write that fails writes nothing (apply is all-or-nothing), so it
/// removes nothing either, and the staged edits stay staged.
#[test]
fn a_failed_apply_removes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    app.minimal = true;
    hit(&mut app, KeyCode::Enter);
    press(&mut app, ' ');
    // The temp file cannot be created where a directory stands.
    std::fs::create_dir_all(tmp.path().join("options/cat_alpha/.options.optique.tmp")).unwrap();
    press(&mut app, 'a');
    press(&mut app, 'y');
    let done = done_text(&app);
    assert!(done.starts_with("nothing written"), "{done}");
    assert!(done.contains("\ncat/alpha: creating "), "{done}");
    assert!(!done.contains("removed"), "{done}");
    assert!(
        tmp.path().join("options/cat_beta/options").is_file(),
        "beta's file kept"
    );
    assert!(app.session.dirty(), "the edit is still staged");
}

/// A failed apply says so in its title and border, names the options
/// directory once and each port's reason with its path cut short, and
/// leaves the failure in the status bar once closed, until the next key.
#[test]
fn a_failed_apply_reads_as_failed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    std::fs::create_dir_all(tmp.path().join("options/cat_alpha/.options.optique.tmp")).unwrap();
    press(&mut app, 'a');
    press(&mut app, 'y');
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
    let shown = ui::screen_rows(terminal.backend().buffer()).join("\n");
    assert!(shown.contains(" apply — FAILED "), "{shown}");
    assert!(!shown.contains(" apply — done "), "{shown}");
    assert_eq!(
        shown.matches("nothing written to ").count(),
        1,
        "the directory once:\n{shown}"
    );
    assert!(
        shown.contains("cat/alpha: creating …/.options.optique.tmp: "),
        "{shown}"
    );
    let alpha = tmp.path().join("options/cat_alpha").display().to_string();
    assert!(
        !shown.contains(&alpha),
        "the reason's path is cut short:\n{shown}"
    );
    // The border in red: the cell under the title's first corner.
    let (row, line) = shown
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains(" apply — FAILED "))
        .unwrap();
    let col = line[..line.find(" apply — FAILED ").unwrap()]
        .chars()
        .count()
        - 1;
    assert_eq!(
        terminal.backend().buffer()[(col as u16, row as u16)].fg,
        ratatui::style::Color::Red
    );

    press(&mut app, 'x');
    assert!(app.modal.is_none());
    let (msg, error) = app
        .message
        .clone()
        .expect("the failure is left in the status bar");
    assert!(error, "{msg}");
    assert!(
        msg.starts_with("apply failed: nothing written — creating …/.options.optique.tmp: "),
        "{msg}"
    );
    assert!(screen_at(&mut app, 80, 24).contains(" apply failed: nothing written — "));
    press(&mut app, 'j');
    assert!(
        app.message.is_none(),
        "an ordinary message: the next key clears it"
    );
}

/// A successful apply leaves what it wrote, removed and skipped in the
/// status bar once its box is closed.
#[test]
fn a_done_apply_leaves_its_count_in_the_status_bar() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    press(&mut app, 'a');
    press(&mut app, 'y');
    assert!(screen(&mut app).contains(" apply — done "));
    assert!(app.message.is_none(), "nothing under the box yet");
    press(&mut app, 'x');
    assert_eq!(app.message, Some(("2 file(s) written".to_string(), false)));

    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    app.minimal = true;
    press(&mut app, 'a');
    press(&mut app, 'y');
    hit(&mut app, KeyCode::Enter);
    assert_eq!(
        app.message,
        Some(("0 file(s) written, 1 removed".to_string(), false))
    );

    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    make_conflict(&mut app, "cat/beta");
    press(&mut app, 'a');
    press(&mut app, 's');
    hit(&mut app, KeyCode::Esc);
    assert_eq!(
        app.message,
        Some((
            "2 file(s) written, 1 skipped: conflicting".to_string(),
            false
        ))
    );
}

/// The list, its divider and its order are rebuilt as soon as the apply
/// is done, under its result box.
#[test]
fn the_divider_follows_an_apply_under_its_result() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = five_ports(tmp.path());
    assert!(
        screen(&mut app).contains("║── 2 ok "),
        "{}",
        screen(&mut app)
    );
    press(&mut app, 'a');
    press(&mut app, 's');
    let shown = screen(&mut app);
    assert!(shown.contains(" apply — done "), "{shown}");
    assert!(
        shown.contains("║── 4 ok "),
        "rebuilt under the box:\n{shown}"
    );
    assert_eq!(
        visible(&app),
        [
            "cat/zeta",
            "cat/alpha",
            "cat/beta",
            "cat/delta",
            "cat/gamma"
        ]
    );
}

/// In the preview only n, N and Esc cancel: Enter, typed by habit, and
/// any other key that is no answer say what the answers are.
#[test]
fn only_n_and_esc_cancel_the_apply_preview() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    for cancel in [KeyCode::Char('n'), KeyCode::Char('N'), KeyCode::Esc] {
        press(&mut app, 'a');
        hit(&mut app, cancel);
        assert!(app.modal.is_none(), "{cancel:?} cancels");
    }
    press(&mut app, 'a');
    for other in [
        KeyCode::Enter,
        KeyCode::Char('x'),
        KeyCode::Char('s'),
        KeyCode::Tab,
    ] {
        hit(&mut app, other);
        assert!(
            app.modal.as_ref().is_some_and(|m| m.done.is_none()),
            "{other:?} does not cancel"
        );
        assert_eq!(
            app.message,
            Some(("y writes · n cancels".to_string(), false)),
            "{other:?}"
        );
    }
    assert!(
        !tmp.path().join("options/cat_alpha").exists(),
        "nothing written"
    );
    hit(&mut app, KeyCode::Esc);

    make_conflict(&mut app, "cat/beta");
    press(&mut app, 'a');
    for other in [KeyCode::Enter, KeyCode::Char('y')] {
        hit(&mut app, other);
        assert!(
            app.modal.as_ref().is_some_and(|m| m.done.is_none()),
            "{other:?} does not cancel"
        );
        assert_eq!(
            app.message,
            Some((
                "conflicts remain: Y writes anyway · s skips them · n cancels".to_string(),
                false
            )),
            "{other:?}"
        );
    }
    assert!(
        matches!(ctrl(&mut app, 'c'), KeyOutcome::Continue),
        "Ctrl-C asks to quit"
    );
    assert!(app.quit_confirm);
}

/// The text of the bottom-border hint on the row holding `marker`.
fn border_hint(shown: &str, marker: &str) -> String {
    let row = shown
        .lines()
        .find(|l| l.contains(marker) && l.contains('└'));
    let row = row.unwrap_or_else(|| panic!("no border with {marker}:\n{shown}"));
    let after = &row[row.find('└').unwrap() + '└'.len_utf8()..];
    // Up to the border's dashes, or its corner when the hint fills it.
    after
        .split(['─', '┘'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// `hint` is one of `forms` whole, or the last of them cut after a
/// whole token with `…`.
fn whole_or_cut_at_a_token(hint: &str, forms: &[&str]) -> bool {
    if forms.contains(&hint) {
        return true;
    }
    let Some(kept) = hint.strip_suffix(" … ") else {
        return false;
    };
    let last = forms.last().unwrap();
    last.starts_with(kept) && last[kept.len()..].starts_with(" · ")
}

/// The popups' bottom hints are fitted to the box: the long form, a
/// short one, or the short one cut at a token — never mid-word.
#[test]
fn popup_hints_are_never_cut_mid_word() {
    let apply = [
        " y: write · n: cancel · j/k scroll ",
        " y write · n cancel · j/k ",
    ];
    let conflict = [
        " Y: write anyway · s: skip conflicting · n: cancel · j/k scroll ",
        " Y write · s skip · n cancel ",
    ];
    let why = [
        " Enter: jump · r: why of selection · j/k move · other key closes ",
        " Enter jump · r why · j/k move · other key closes ",
        " Enter jump · r why · j/k ",
    ];
    let bulk = [
        " 3 carry DOCS · Enter apply · Esc cancel · Ctrl-U clear ",
        " 3 carry DOCS · Enter · Esc · ^U clear ",
    ];
    let help = [
        " 1-4/Tab/←→ switch · ↑/↓ scroll · other key closes ",
        " 1-4/Tab switch · j/k scroll · other closes ",
        " 1-4 · j/k · other closes ",
    ];
    let help_fits = [
        " 1-4/Tab/←→ switch · any other key closes ",
        " 1-4/Tab switch · other closes ",
    ];
    for (width, height) in [(60, 20), (80, 24), (100, 35)] {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = three_ports(tmp.path());
        press(&mut app, 'a');
        let hint = border_hint(&screen_at(&mut app, width, height), "write");
        assert!(whole_or_cut_at_a_token(&hint, &apply), "{width}: {hint:?}");
        hit(&mut app, KeyCode::Esc);

        press(&mut app, 'r');
        let hint = border_hint(&screen_at(&mut app, width, height), "Enter");
        assert!(whole_or_cut_at_a_token(&hint, &why), "{width}: {hint:?}");
        hit(&mut app, KeyCode::Esc);

        press(&mut app, 'B');
        for c in "docs".chars() {
            press(&mut app, c);
        }
        let hint = border_hint(&screen_at(&mut app, width, height), "carry");
        assert!(whole_or_cut_at_a_token(&hint, &bulk), "{width}: {hint:?}");
        hit(&mut app, KeyCode::Esc);

        press(&mut app, '?');
        let hint = border_hint(&screen_at(&mut app, width, height), "closes");
        assert!(
            whole_or_cut_at_a_token(&hint, &help) || whole_or_cut_at_a_token(&hint, &help_fits),
            "{width}: {hint:?}"
        );
        hit(&mut app, KeyCode::Esc);

        make_conflict(&mut app, "cat/beta");
        press(&mut app, 'a');
        let hint = border_hint(&screen_at(&mut app, width, height), "write");
        assert!(
            whole_or_cut_at_a_token(&hint, &conflict),
            "{width}: {hint:?}"
        );
    }
}

/// The quit prompt is as high as its lines and shows them whole, at 80
/// and 60 columns; at 40 its keys take two lines, none of them cut.
#[test]
fn the_quit_prompt_shows_its_lines_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    hit(&mut app, KeyCode::Enter);
    press(&mut app, ' ');
    for (width, height) in [(80, 24), (60, 20)] {
        press(&mut app, 'q');
        let shown = screen_at(&mut app, width, height);
        let rows: Vec<&str> = shown.lines().collect();
        let top = rows
            .iter()
            .position(|r| r.contains("┌ quit "))
            .expect("the prompt");
        assert!(
            rows[top + 1].contains("│Keep staged edits as a draft?"),
            "{width}:\n{shown}"
        );
        assert!(
            rows[top + 2].contains("│y save & quit · n discard & quit · Esc stay"),
            "{width}:\n{shown}"
        );
        // Under them, only the count of background makes, when other
        // tests run some.
        let makes = rows[top + 3].contains("background make process");
        assert!(
            rows[top + 3].contains('└') || makes,
            "as high as its lines: {width}:\n{shown}"
        );
        let left = rows[top].chars().position(|c| c == '┌').unwrap();
        assert_eq!(left, (width as usize - 52) / 2, "52 wide, centred: {width}");
        assert!(matches!(hit(&mut app, KeyCode::Esc), KeyOutcome::Continue));
        assert!(!app.quit_confirm, "Esc stays");
    }
    press(&mut app, 'q');
    let shown = screen_at(&mut app, 40, 20);
    let rows: Vec<&str> = shown.lines().collect();
    let top = rows
        .iter()
        .position(|r| r.contains("┌ quit "))
        .expect("the prompt");
    assert!(
        rows[top + 2].contains("│y save & quit · n discard & quit"),
        "{shown}"
    );
    assert!(rows[top + 3].contains("│Esc stay"), "{shown}");
    assert_eq!(
        rows[top].chars().position(|c| c == '┌'),
        Some(2),
        "36 wide: {shown}"
    );
    assert!(
        matches!(press(&mut app, 'n'), KeyOutcome::Quit),
        "n still answers"
    );
}

/// The option details name the group as its header does: NAME — LABEL.
#[test]
fn the_option_details_name_the_group_as_its_header() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    make_conflict(&mut app, "cat/alpha");
    app.refresh_view();
    app.select_index(0);
    assert_eq!(selected(&app), "cat/alpha");
    hit(&mut app, KeyCode::Enter);
    let mut found = false;
    for _ in 0..3 {
        press(&mut app, 'i');
        let shown = screen(&mut app);
        if shown.contains("group: ") {
            assert!(
                shown.contains("group: GUI — single (pick exactly one)"),
                "{shown}"
            );
            found = true;
            break;
        }
        hit(&mut app, KeyCode::Esc);
        press(&mut app, 'j');
    }
    assert!(found, "an option of the group has its details");
}

/// A help line too long for the box goes on under its description's
/// first column, not under the key.
#[test]
fn a_wrapped_help_line_hangs_under_its_description() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    // The column `text` starts at on the row holding it, and the first
    // column of text inside the box on the row under it.
    let hangs = |shown: &str, text: &str| {
        let rows: Vec<&str> = shown.lines().collect();
        let at = rows.iter().position(|r| r.contains(text));
        let at = at.unwrap_or_else(|| panic!("{text}:\n{shown}"));
        let chars: Vec<char> = rows[at].chars().collect();
        let text_col = rows[at][..rows[at].find(text).unwrap()].chars().count();
        let left = chars[..text_col]
            .iter()
            .rposition(|&c| c == '│')
            .expect("the box's left border");
        let next: Vec<char> = rows[at + 1].chars().collect();
        let first = (left + 1..next.len()).find(|&i| !next[i].is_whitespace());
        (text_col, first)
    };
    press(&mut app, '?');
    press(&mut app, '3');
    let shown = screen_at(&mut app, 60, 20);
    let (text_col, next) = hangs(&shown, "undo the last change");
    assert_eq!(
        next,
        Some(text_col),
        "a key line hangs under its description:\n{shown}"
    );

    // A legend's too, under the column after " = ".
    press(&mut app, '2');
    press(&mut app, 'G');
    let shown = screen_at(&mut app, 60, 20);
    let (text_col, next) = hangs(&shown, "the description of the");
    assert_eq!(
        next,
        Some(text_col),
        "a legend hangs under its description:\n{shown}"
    );
}

/// A draft left by a quit is adopted at start, one undo step per port;
/// a draft that no longer changes anything is dropped.
#[test]
fn a_saved_draft_is_restored_at_start() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    hit(&mut app, KeyCode::Enter);
    press(&mut app, ' ');
    press(&mut app, 'q');
    press(&mut app, 'y');
    drop(app);

    let mut app = three_ports(tmp.path());
    assert_eq!(staged(&app, "cat/alpha"), ["DOCS"]);
    assert!(
        app.pending
            .contains_key(&PortKey::parse("cat/alpha").unwrap())
    );
    assert!(
        app.message.as_ref().is_some_and(
            |(m, _)| m.starts_with("restored draft (1 port, 0m ago, U per port reverts)")
        ),
        "{:?}",
        app.message
    );
    press(&mut app, 'u');
    assert!(
        staged(&app, "cat/alpha").is_empty(),
        "the restore is undoable"
    );
    drop(app);

    // Applied since: the draft changes nothing any more and is dropped.
    let opts = tmp.path().join("options/cat_alpha");
    std::fs::create_dir_all(&opts).unwrap();
    std::fs::write(
        opts.join("options"),
        "_FILE_COMPLETE_OPTIONS_LIST=DOCS\nOPTIONS_FILE_SET+=DOCS\n",
    )
    .unwrap();
    let app = three_ports(tmp.path());
    assert!(draft::load(&app.options_dir).is_none());
    assert_eq!(
        app.message.as_ref().map(|(m, _)| m.as_str()),
        Some("1 never configured — a: write defaults for all · n: next problem · ?: help"),
        "no draft to speak of"
    );
}

/// A restored draft the user undoes by hand leaves nothing staged, so the
/// quit asks nothing — and must not leave the draft to come back on the
/// next launch, whichever way the session is quit.
#[test]
fn a_reverted_draft_is_not_restored_again() {
    for quit in ['q', 'c'] {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = three_ports(tmp.path());
        hit(&mut app, KeyCode::Enter);
        press(&mut app, ' ');
        press(&mut app, 'q');
        press(&mut app, 'y');
        drop(app);

        let mut app = three_ports(tmp.path());
        assert_eq!(staged(&app, "cat/alpha"), ["DOCS"], "restored");
        press(&mut app, 'u');
        assert!(!app.session.dirty());
        let outcome = if quit == 'q' {
            press(&mut app, 'q')
        } else {
            ctrl(&mut app, 'c')
        };
        assert!(
            matches!(outcome, KeyOutcome::Quit),
            "{quit}: nothing to lose"
        );
        drop(app);

        let app = three_ports(tmp.path());
        assert!(
            staged(&app, "cat/alpha").is_empty(),
            "{quit}: the draft came back"
        );
        assert!(draft::load(&app.options_dir).is_none(), "{quit}");
    }
}

/// Opening the apply preview, confirming it and reloading afterwards read
/// only the files apply wrote: everything else is as the scan read it.
#[test]
fn apply_rereads_only_the_files_it_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    // cat/beta is configured and stays as it is; cat/alpha is new.
    let beta = tmp.path().join("options/cat_beta");
    std::fs::create_dir_all(&beta).unwrap();
    std::fs::write(
        beta.join("options"),
        "_OPTIONS_READ=beta-1.0\n_FILE_COMPLETE_OPTIONS_LIST=DOCS\nOPTIONS_FILE_UNSET+=DOCS\n",
    )
    .unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[]), ("cat/beta", &[])]);
    let at_start = app.options_files.disk_reads();
    assert_eq!(at_start, 2, "one read per file to build the session");
    assert!(
        tmp.path().join("db/cat_beta/options").is_file(),
        "staging seeded"
    );
    // Edited behind optique's back: apply plans against what the session
    // read, so a port nobody touched in the TUI is not rewritten.
    std::fs::write(beta.join("options"), "OPTIONS_FILE_SET+=DOCS\n").unwrap();

    dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
    );
    assert!(app.modal.is_some(), "the apply preview opened");
    dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
    );
    let done = done_text(&app);
    assert!(done.starts_with("1 file(s) written"), "{done}");
    assert!(
        !done.contains("redundant"),
        "nothing removed, nothing said: {done}"
    );
    assert!(tmp.path().join("options/cat_alpha/options").is_file());
    assert_eq!(
        std::fs::read(beta.join("options")).unwrap(),
        b"OPTIONS_FILE_SET+=DOCS\n"
    );
    assert_eq!(
        app.options_files.disk_reads(),
        at_start + 1,
        "only cat_alpha re-read"
    );
    let alpha = PortKey::parse("cat/alpha").unwrap();
    assert!(
        app.session
            .state(&app.session.ports[&alpha])
            .unwrap()
            .saved
            .is_some()
    );
}

/// The option pane opens with what the port is (on at start, 'V' toggles
/// it), and 'v' on the list shows all of it, WWW included.
#[test]
fn port_description_panel_and_v_overlay() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let descr = tmp.path().join("pkg-descr");
    std::fs::write(&descr, "Alpha does\nthings.\n\nWWW: https://old.example/\n").unwrap();
    let key = PortKey::parse("cat/alpha").unwrap();
    let about = &mut app.session.ports.get_mut(&key).unwrap().about;
    about.comment = "The alpha port".to_string();
    about.www = "https://alpha.example/".to_string();
    about.pkg_descr = Some(descr.display().to_string());

    app.about_text.clear();
    app.rebuild_editor();
    let shown = screen(&mut app);
    assert!(shown.contains("The alpha port"), "{shown}");
    assert!(
        shown.contains("Alpha does things."),
        "paragraph re-wrapped: {shown}"
    );
    assert!(
        !shown.contains("old.example"),
        "old-style WWW: left to the overlay"
    );

    // 'V' hides it again and brings it back, from either pane.
    dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT),
    );
    assert!(!screen(&mut app).contains("Alpha does things."));
    app.focus = Focus::Editor;
    dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT),
    );
    assert!(screen(&mut app).contains("Alpha does things."));
    app.focus = Focus::List;

    dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE),
    );
    let help = app
        .port_help
        .as_ref()
        .expect("'v' on the list opens the overlay");
    assert_eq!(help.title, "about");
    assert_eq!(help.lines[0], "The alpha port");
    assert_eq!(help.lines[1], "WWW: https://alpha.example/");
    assert!(help.lines.iter().any(|l| l == "Alpha does"));
}

/// The smallest closure with a dependency loop: two ports needing each
/// other.
fn looped_app(tmp: &std::path::Path) -> App {
    app_for(
        tmp,
        &[("cat/alpha", &["cat/beta"]), ("cat/beta", &["cat/alpha"])],
    )
}

/// Everything the in-memory screen shows, rows joined by newlines.
fn screen(app: &mut App) -> String {
    screen_at(app, 100, 35)
}

/// The same on a `width` x `height` screen.
fn screen_at(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| ui::draw(f, app)).unwrap();
    ui::screen_rows(terminal.backend().buffer()).join("\n")
}

/// The rows holding a popup's scrollbar thumb, top to bottom.
fn thumb_rows(shown: &str) -> Vec<usize> {
    shown
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains('█'))
        .map(|(i, _)| i)
        .collect()
}

/// The bottom border of the popup whose hint contains `hint`.
fn hint_row<'a>(shown: &'a str, hint: &str) -> &'a str {
    shown.lines().find(|l| l.contains(hint)).unwrap_or_default()
}

/// The why popup takes the one scroll key set — the cursor moving a row,
/// a page or to either end, the view and its scrollbar following — and
/// none of those keys closes it; its keys are on its bottom border, out
/// of the scrolled text, however many dependents it lists.
#[test]
fn the_why_popup_scrolls_without_closing() {
    let tmp = tempfile::tempdir().unwrap();
    let names: Vec<String> = (0..ui::WHY_MAX_DEPENDENTS + 5)
        .map(|i| format!("cat/d{i:02}"))
        .collect();
    let mut graph: Vec<(&str, &[&str])> =
        vec![("cat/alpha", &["cat/beta"]), ("cat/beta", &["cat/alpha"])];
    for name in &names {
        graph.push((name, &["cat/alpha"]));
    }
    let mut app = app_for(tmp.path(), &graph);
    press(&mut app, 'r');
    let count = app.why.as_ref().unwrap().entries().len();
    let shown = screen_at(&mut app, 80, 24);
    let hint = hint_row(&shown, "j/k move · other key closes");
    assert!(
        hint.contains('└') && hint.contains('┘'),
        "on the bottom border:\n{shown}"
    );
    assert_eq!(shown.matches("other key closes").count(), 1, "{shown}");
    let top = thumb_rows(&shown);
    assert!(!top.is_empty(), "the text overflows:\n{shown}");
    let page = page_step(app.page.why);
    assert!(page > 1, "the box's height, from the last frame");

    hit(&mut app, KeyCode::PageDown);
    assert_eq!(
        app.why.as_ref().map(|w| w.selected),
        Some(page),
        "PgDn moves, not closes"
    );
    let middle = thumb_rows(&screen_at(&mut app, 80, 24));
    assert!(middle[0] > top[0], "the thumb follows: {top:?} {middle:?}");
    ctrl(&mut app, 'f');
    let further = (2 * page).min(count - 1);
    assert_eq!(
        app.why.as_ref().map(|w| w.selected),
        Some(further),
        "Ctrl-F too"
    );
    ctrl(&mut app, 'b');
    assert_eq!(app.why.as_ref().map(|w| w.selected), Some(further - page));
    press(&mut app, 'G');
    assert_eq!(
        app.why.as_ref().map(|w| w.selected),
        Some(count - 1),
        "G: the last"
    );
    let shown = screen_at(&mut app, 80, 24);
    // beta depends on alpha too: six dependents past the cap.
    assert!(
        shown.contains("+ 6 more"),
        "the end of the text in view:\n{shown}"
    );
    let bottom = thumb_rows(&shown);
    assert!(
        bottom[0] > middle[0],
        "the thumb at the bottom: {middle:?} {bottom:?}"
    );
    press(&mut app, 'g');
    assert_eq!(
        app.why.as_ref().map(|w| w.selected),
        Some(0),
        "g: the first"
    );
    hit(&mut app, KeyCode::End);
    assert_eq!(
        app.why.as_ref().map(|w| w.selected),
        Some(count - 1),
        "End: the last"
    );
    hit(&mut app, KeyCode::Home);
    hit(&mut app, KeyCode::PageUp);
    assert_eq!(app.why.as_ref().map(|w| w.selected), Some(0));
    assert_eq!(
        thumb_rows(&screen_at(&mut app, 80, 24)),
        top,
        "back at the top"
    );
    press(&mut app, 'x');
    assert!(app.why.is_none(), "any other key closes");
}

/// A port with one option described at length.
fn long_described(tmp: &std::path::Path, words: usize) -> App {
    let mut app = app_for(tmp, &[("cat/alpha", &[])]);
    let key = PortKey::parse("cat/alpha").unwrap();
    let desc = (0..words)
        .map(|i| format!("word{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    app.session
        .ports
        .get_mut(&key)
        .unwrap()
        .options
        .defs
        .insert(
            "DOCS".into(),
            crate::model::options::OptionDef {
                desc,
                ..Default::default()
            },
        );
    hit(&mut app, KeyCode::Enter);
    press(&mut app, 'i');
    assert!(app.opt_info.is_some());
    app
}

/// The option detail scrolls a description too long for a short screen,
/// its scrollbar following, and the scroll keys do not close it.
#[test]
fn the_option_detail_scrolls_without_closing() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = long_described(tmp.path(), 300);
    let shown = screen_at(&mut app, 80, 24);
    let hint = hint_row(&shown, " j/k scroll · any other key closes ");
    assert!(hint.contains('└'), "on the bottom border:\n{shown}");
    assert!(
        shown.contains("│word0 word1 "),
        "the popup's first row:\n{shown}"
    );
    let top = thumb_rows(&shown);
    assert!(!top.is_empty(), "the text overflows:\n{shown}");
    let page = page_step(app.page.opt_info) as u16;
    assert!(page > 1, "the box's height, from the last frame");
    let scroll = |app: &App| app.opt_info.as_ref().map(|i| i.scroll);

    hit(&mut app, KeyCode::PageDown);
    assert_eq!(scroll(&app), Some(page), "PgDn scrolls, not closes");
    let shown = screen_at(&mut app, 80, 24);
    assert!(
        !shown.contains("│word0 word1 "),
        "scrolled past the first row:\n{shown}"
    );
    assert!(thumb_rows(&shown)[0] > top[0], "the thumb follows");
    ctrl(&mut app, 'f');
    assert!(scroll(&app).is_some(), "Ctrl-F too");
    press(&mut app, 'G');
    assert_eq!(
        scroll(&app),
        Some(u16::MAX),
        "G: to the end, clamped when drawn"
    );
    let shown = screen_at(&mut app, 80, 24);
    assert!(
        shown.contains("source: port default"),
        "the end in view:\n{shown}"
    );
    let last = scroll(&app).unwrap();
    assert!(last < u16::MAX && last >= page, "clamped: {last}");
    press(&mut app, 'k');
    assert_eq!(scroll(&app), Some(last - 1));
    hit(&mut app, KeyCode::End);
    hit(&mut app, KeyCode::Home);
    press(&mut app, 'g');
    press(&mut app, 'j');
    assert_eq!(scroll(&app), Some(1));
    press(&mut app, 'x');
    assert!(app.opt_info.is_none(), "any other key closes");

    // A description that fits: no bar, and any key closes, it says.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = long_described(tmp.path(), 3);
    let shown = screen_at(&mut app, 80, 24);
    assert!(
        hint_row(&shown, " any key closes ").contains('└'),
        "{shown}"
    );
    assert!(thumb_rows(&shown).is_empty(), "{shown}");
}

/// The apply preview takes the one scroll key set, its list moving by a
/// row, a page or to either end, the scrollbar following; none of those
/// keys cancels it, and y still writes.
#[test]
fn the_apply_preview_scrolls_without_cancelling() {
    let tmp = tempfile::tempdir().unwrap();
    let names: Vec<String> = (0..40).map(|i| format!("cat/p{i:02}")).collect();
    let graph: Vec<(&str, &[&str])> = names.iter().map(|n| (n.as_str(), &[][..])).collect();
    let mut app = app_for(tmp.path(), &graph);
    press(&mut app, 'a');
    let shown = screen_at(&mut app, 80, 24);
    let top = thumb_rows(&shown);
    assert!(!top.is_empty(), "the list overflows:\n{shown}");
    let page = page_step(app.page.apply);
    assert!(page > 1, "the box's height, from the last frame");
    let scroll = |app: &App| app.modal.as_ref().map(|m| m.scroll);

    hit(&mut app, KeyCode::PageDown);
    assert_eq!(scroll(&app), Some(page), "PgDn scrolls, not cancels");
    ctrl(&mut app, 'f');
    assert_eq!(scroll(&app), Some(2 * page), "Ctrl-F too");
    let middle = thumb_rows(&screen_at(&mut app, 80, 24));
    assert!(middle[0] > top[0], "the thumb follows: {top:?} {middle:?}");
    ctrl(&mut app, 'b');
    assert_eq!(scroll(&app), Some(page));
    press(&mut app, 'G');
    let shown = screen_at(&mut app, 80, 24);
    assert!(
        shown.contains("cat/p39"),
        "the last entry in view:\n{shown}"
    );
    let last = scroll(&app).unwrap();
    assert!(last < names.len(), "clamped to the last page: {last}");
    assert!(thumb_rows(&shown)[0] > middle[0], "the thumb at the bottom");
    press(&mut app, 'k');
    assert_eq!(scroll(&app), Some(last - 1), "a row up from the last page");
    hit(&mut app, KeyCode::End);
    hit(&mut app, KeyCode::Home);
    assert_eq!(scroll(&app), Some(0));
    press(&mut app, 'G');
    press(&mut app, 'g');
    assert_eq!(scroll(&app), Some(0));
    hit(&mut app, KeyCode::PageUp);
    assert!(
        app.modal.as_ref().is_some_and(|m| m.done.is_none()),
        "still the preview"
    );
    press(&mut app, 'y');
    let done = done_text(&app);
    assert!(done.starts_with("40 file(s) written"), "{done}");
}

/// A pkg-help paragraph hard-wrapped at 72 columns is one paragraph in
/// the popup, wrapped to its width; a list and an indented block keep
/// their lines.
#[test]
fn a_pkg_help_paragraph_is_reflowed() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let help = tmp.path().join("pkg-help");
    std::fs::write(
        &help,
        "This port needs a line\nwrapped by hand here.\n\n- one item\n- two items\n\n  % run this\n  % then that\n",
    )
    .unwrap();
    let key = app.selected_key().unwrap();
    app.session.ports.get_mut(&key).unwrap().pkg_help = Some(help.display().to_string());
    press(&mut app, 'h');
    let shown = screen(&mut app);
    assert!(
        shown.contains("This port needs a line wrapped by hand here."),
        "{shown}"
    );
    for kept in [
        "- one item ",
        "- two items ",
        "  % run this ",
        "  % then that ",
    ] {
        assert!(
            shown
                .lines()
                .any(|l| l.contains(&format!("│{kept}")) || l.contains(&format!("│ {kept}"))),
            "{kept:?} on a line of its own:\n{shown}"
        );
    }
}

/// A loop is visible without asking: both ports wear the ∞ badge, and the
/// status bar says how many are caught in one.
#[test]
fn looped_ports_are_badged_in_the_list() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = looped_app(tmp.path());
    assert_eq!(app.loops.len(), 1, "one loop over the two ports");
    app.clear_message();
    let text = screen(&mut app);
    assert!(text.contains("cat/alpha \u{221e}"), "{text}");
    assert!(text.contains("cat/beta \u{221e}"), "{text}");
    assert!(
        text.contains("2\u{221e} L:loops"),
        "the status bar counts them:\n{text}"
    );
}

/// `L` narrows the list to the tangle, and says so in the pane title.
#[test]
fn the_loop_view_lists_only_tangled_ports() {
    let tmp = tempfile::tempdir().unwrap();
    // Two ports needing each other, plus one that needs neither.
    let mut app = app_for(
        tmp.path(),
        &[
            ("cat/alpha", &["cat/beta"]),
            ("cat/beta", &["cat/alpha"]),
            ("cat/loner", &[]),
        ],
    );
    assert_eq!(app.visible.len(), 3);
    dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE),
    );
    assert!(app.only_loops);
    assert_eq!(app.visible.len(), 2, "{:?}", app.visible);
    assert!(!app.visible.iter().any(|k| k.origin == "cat/loner"));
    let text = screen(&mut app);
    assert!(text.contains("in a dependency loop"), "{text}");

    // And back.
    dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE),
    );
    assert!(!app.only_loops);
    assert_eq!(app.visible.len(), 3);
}

/// With nothing to show the view is refused rather than emptying the list.
#[test]
fn the_loop_view_is_refused_without_a_loop() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/one", &[]), ("cat/two", &[])]);
    assert!(app.loops.is_empty());
    dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE),
    );
    assert!(!app.only_loops, "the view must not turn on");
    assert_eq!(app.visible.len(), 2, "the list is untouched");
    assert_eq!(
        app.message.as_ref().map(|(m, _)| m.as_str()),
        Some("no dependency loop in this closure")
    );
}

/// A loop broken by an option edit leaves the view rather than stranding
/// the user in an empty list.
#[test]
fn breaking_the_last_loop_leaves_the_loop_view() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = looped_app(tmp.path());
    dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE),
    );
    assert!(app.only_loops && app.visible.len() == 2);

    // What a background re-query would merge: cat/beta no longer needs
    // cat/alpha, so the cycle is gone.
    let beta = PortKey::parse("cat/beta").unwrap();
    app.session.ports.get_mut(&beta).unwrap().deps.clear();
    app.clear_message();
    app.refresh_loops(false);
    assert!(app.loops.is_empty());
    assert!(!app.only_loops, "the view let go");
    assert_eq!(
        app.message.as_ref().map(|(m, _)| m.as_str()),
        Some("dependency loop gone; showing all ports again")
    );
    let keep = app.selected_key();
    app.rebuild_visible(keep);
    assert_eq!(app.visible.len(), 2, "both ports are listed again");
}

/// Through a real merge: a re-scan that breaks the last loop under L
/// lists every port again, and one that makes a new loop under L lists
/// its ports — the list is built from the loops the merge left.
#[test]
fn a_rescan_that_breaks_the_last_loop_lists_every_port() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(
        tmp.path(),
        &[
            ("cat/alpha", &["cat/beta"]),
            ("cat/beta", &["cat/alpha"]),
            ("cat/gamma", &[]),
            ("cat/delta", &[]),
        ],
    );
    let (ev, _req) = fake_refresher(&mut app);
    press(&mut app, 'L');
    assert_eq!(visible(&app), ["cat/alpha", "cat/beta"]);

    // gamma and delta start needing each other: a second loop, shown.
    let mut gamma = info_of(&app, "cat/gamma");
    let mut delta = info_of(&app, "cat/delta");
    gamma.deps = info_of(&app, "cat/beta").deps;
    gamma.deps[0].target = delta.key.clone();
    delta.deps = info_of(&app, "cat/beta").deps;
    delta.deps[0].target = gamma.key.clone();
    ev.send(done(vec![gamma, delta], vec![])).unwrap();
    app.drain_refresh_events();
    assert!(app.only_loops);
    assert_eq!(visible(&app).len(), 4, "{:?}", visible(&app));

    // That loop broken again: its ports leave the view.
    let mut delta = info_of(&app, "cat/delta");
    delta.deps.clear();
    ev.send(done(vec![delta], vec![])).unwrap();
    app.drain_refresh_events();
    assert!(app.only_loops);
    assert_eq!(visible(&app), ["cat/alpha", "cat/beta"]);

    // The last loop broken: the view lets go, and the list says so too.
    let mut beta = info_of(&app, "cat/beta");
    beta.deps.clear();
    app.clear_message();
    ev.send(done(vec![beta], vec![])).unwrap();
    app.drain_refresh_events();
    assert!(app.loops.is_empty() && !app.only_loops);
    assert_eq!(
        app.message.as_ref().map(|(m, _)| m.as_str()),
        Some("dependency loop gone; showing all ports again")
    );
    assert_eq!(visible(&app).len(), 4, "{:?}", visible(&app));
}

/// `r` explains the loop, not just the chain and the dependents, and
/// lists its ports one per row so each can be reached.
#[test]
fn why_overlay_lists_the_loop() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = looped_app(tmp.path());
    dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
    );
    let text = screen(&mut app);
    assert!(
        text.contains("caught in a dependency loop (2 ports)"),
        "{text}"
    );
    // The walk starts where the user is standing and closes back on it.
    assert!(text.contains("cat/alpha  (this port)"), "{text}");
    assert!(text.contains("back to cat/alpha"), "{text}");
}

/// The walk is rotated to the port the overlay is about, so the loop
/// always reads from where the user is.
#[test]
fn the_loop_walk_starts_at_the_port_asked_about() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = looped_app(tmp.path());
    dispatch_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(app.selected_key().unwrap().origin, "cat/beta");
    dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
    );
    let text = screen(&mut app);
    assert!(text.contains("cat/beta  (this port)"), "{text}");
    assert!(text.contains("back to cat/beta"), "{text}");
}

/// Every port of the loop is navigable: Enter on one jumps the list to
/// it, exactly like a chain or dependent entry.
#[test]
fn enter_on_a_loop_port_jumps_to_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = looped_app(tmp.path());
    dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
    );
    // cat/alpha is a root, so the entries are: the one-port chain, then
    // the cycle (alpha, beta), then the dependents (beta).
    let entries: Vec<String> = app
        .why
        .as_ref()
        .unwrap()
        .entries()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        entries,
        ["cat/alpha", "cat/alpha", "cat/beta", "cat/beta"],
        "{entries:?}"
    );

    // Down twice lands on the cycle's second port.
    for _ in 0..2 {
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    assert_eq!(app.why.as_ref().unwrap().selected, 2);
    dispatch_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.why.is_none(), "Enter closes the overlay");
    assert_eq!(app.selected_key().unwrap().origin, "cat/beta");
}

/// Under L the why overlay still lists dependents outside the loop, and
/// Enter on one leaves the loop view to reach it, as it clears a filter.
#[test]
fn enter_on_a_dependent_outside_the_loop_view_selects_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(
        tmp.path(),
        &[
            ("cat/alpha", &["cat/beta"]),
            ("cat/beta", &["cat/alpha"]),
            ("cat/gamma", &["cat/alpha"]),
        ],
    );
    press(&mut app, 'L');
    assert_eq!(visible(&app), ["cat/alpha", "cat/beta"]);
    assert_eq!(selected(&app), "cat/alpha");
    press(&mut app, 'r');
    let at = app
        .why
        .as_ref()
        .unwrap()
        .entries()
        .iter()
        .position(|k| k.origin == "cat/gamma");
    for _ in 0..at.expect("gamma is a dependent of alpha") {
        hit(&mut app, KeyCode::Down);
    }
    hit(&mut app, KeyCode::Enter);
    assert_eq!(selected(&app), "cat/gamma");
    assert!(!app.only_loops, "the loop view let go");
    let (msg, err) = app.message.clone().unwrap();
    assert_eq!(
        (msg.as_str(), err),
        ("jumped to cat/gamma (loop view left)", false)
    );
}

/// Enter in the why overlay on a port without options, which no view
/// lists, changes nothing: not the filter, nor the selection, nor the
/// pane, and Space then edits the port the pane shows.
#[test]
fn a_jump_to_an_optionless_port_leaves_the_view_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(
        tmp.path(),
        &[
            ("cat/aaa", &[]),
            ("cat/zeta", &[]),
            ("cat/beta", &["cat/zeta"]),
        ],
    );
    app.session
        .ports
        .get_mut(&PortKey::parse("cat/beta").unwrap())
        .unwrap()
        .options = Default::default();
    app.rebuild_visible(None);
    press(&mut app, '/');
    for c in "zeta".chars() {
        press(&mut app, c);
    }
    hit(&mut app, KeyCode::Enter);
    hit(&mut app, KeyCode::Enter);
    assert_eq!(selected(&app), "cat/zeta");
    assert!(app.focus == Focus::Editor);
    press(&mut app, 'r');
    let at = app
        .why
        .as_ref()
        .unwrap()
        .entries()
        .iter()
        .position(|k| k.origin == "cat/beta");
    for _ in 0..at.expect("beta is a dependent of zeta") {
        hit(&mut app, KeyCode::Down);
    }
    hit(&mut app, KeyCode::Enter);
    assert_eq!(app.filter, "zeta", "the filter stays");
    assert_eq!(visible(&app), ["cat/zeta"]);
    assert_eq!(selected(&app), "cat/zeta");
    assert_eq!(
        app.editor_port.as_ref().map(ToString::to_string).as_deref(),
        Some("cat/zeta")
    );
    let (msg, err) = app.message.clone().unwrap();
    assert_eq!(
        (msg.as_str(), err),
        (
            "cat/beta has no options and is not listed: not jumped to",
            true
        )
    );
    hit(&mut app, KeyCode::Char(' '));
    assert_eq!(staged(&app, "cat/zeta"), ["DOCS"]);
    assert!(
        staged(&app, "cat/aaa").is_empty(),
        "the port the pane did not show"
    );
}

/// Rows built for another port are never drawn, nor edited, under the
/// selected one: a group header past the port's groups does not panic.
#[test]
fn the_pane_never_draws_another_ports_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/aaa", &[]), ("cat/zeta", &[])]);
    app.editor_rows = vec![EditorRow::GroupHeader(3), EditorRow::Option("GONE".into())];
    app.editor_idx = 1;
    app.editor_port = Some(PortKey::parse("cat/zeta").unwrap());
    assert_eq!(selected(&app), "cat/aaa");
    let shown = screen(&mut app);
    assert!(shown.contains("DOCS") && !shown.contains("GONE"), "{shown}");
    assert_eq!(cursor(&app), "DOCS");
}

/// On an 80x30 terminal the why box wraps the loop's note line; the
/// cursor on the last dependent must still be scrolled into view.
#[test]
fn the_why_cursor_stays_in_view_on_a_small_terminal() {
    let tmp = tempfile::tempdir().unwrap();
    let names: Vec<String> = (0..ui::WHY_MAX_DEPENDENTS)
        .map(|i| format!("cat/d{i:02}"))
        .collect();
    let mut graph: Vec<(&str, &[&str])> =
        vec![("cat/alpha", &["cat/beta"]), ("cat/beta", &["cat/alpha"])];
    for name in &names {
        graph.push((name, &["cat/alpha"]));
    }
    let mut app = app_for(tmp.path(), &graph);
    assert_eq!(selected(&app), "cat/alpha");
    press(&mut app, 'r');
    let entries = app.why.as_ref().unwrap().entries().len();
    for _ in 0..entries {
        hit(&mut app, KeyCode::Down);
    }
    let last = app
        .why
        .as_ref()
        .unwrap()
        .entries()
        .last()
        .unwrap()
        .to_string();

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
    terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
    let buffer = terminal.backend().buffer();
    let highlighted: Vec<String> = (0..30u16)
        .map(|y| {
            (0..80u16)
                .map(|x| &buffer[(x, y)])
                .filter(|c| c.modifier.contains(ratatui::style::Modifier::REVERSED))
                .map(ratatui::buffer::Cell::symbol)
                .collect()
        })
        .collect();
    assert!(
        highlighted.iter().any(|row| row.contains(&last)),
        "{last} not highlighted in view: {highlighted:?}"
    );
}

/// A port that depends on itself is a loop of one, and says so instead of
/// pointing back at itself twice.
#[test]
fn a_self_dependency_reads_as_one_port() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/solo", &["cat/solo"])]);
    assert_eq!(app.loops.len(), 1);
    dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
    );
    let text = screen(&mut app);
    assert!(
        text.contains("caught in a dependency loop (1 port)"),
        "{text}"
    );
    assert!(text.contains("depends on itself"), "{text}");
    assert!(
        !text.contains("back to"),
        "no return arrow for a loop of one:\n{text}"
    );
}

/// A component wider than its shortest cycle lists the rest of the tangle
/// too, and those ports are navigable as well: each can reach every other,
/// so none of them can be built first either.
#[test]
fn the_wider_tangle_is_listed_and_navigable() {
    let tmp = tempfile::tempdir().unwrap();
    // A hub every spoke needs, and which needs every spoke: one component
    // of 1 + n ports whose shortest cycle is just hub → spoke.
    let n = EXTRA_SHOWN + 3;
    let spokes: Vec<String> = (0..n).map(|i| format!("cat/s{i:03}")).collect();
    let spoke_refs: Vec<&str> = spokes.iter().map(String::as_str).collect();
    let hub: &[&str] = &spoke_refs;
    let mut graph: Vec<(&str, &[&str])> = vec![("cat/hub", hub)];
    let back: &[&str] = &["cat/hub"];
    for spoke in &spoke_refs {
        graph.push((spoke, back));
    }
    let mut app = app_for(tmp.path(), &graph);
    assert_eq!(app.loops.len(), 1, "one component over every port");

    app.jump_to_port(&PortKey::parse("cat/hub").unwrap());
    dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
    );
    let text = screen(&mut app);
    assert!(
        text.contains(&format!("caught in a dependency loop ({} ports)", n + 1)),
        "{text}"
    );
    assert!(text.contains("also in the tangle:"), "{text}");
    assert!(
        text.contains("+ 2 more"),
        "the cap counts the rest:\n{text}"
    );

    // The shown tangled ports are entries, so Enter reaches them.
    let why = app.why.as_ref().unwrap();
    let dl = why.dep_loop.as_ref().unwrap();
    assert_eq!(dl.walk.len(), 2, "hub -> spoke -> hub");
    assert_eq!(dl.tangled.len(), EXTRA_SHOWN);
    assert_eq!(dl.hidden, 2);
    let entries = why.entries().len();
    assert!(
        entries >= dl.walk.len() + dl.tangled.len(),
        "the tangle must be navigable: {entries} entries"
    );
}

/// `r` inside the overlay walks the cycle port by port.
#[test]
fn r_on_a_loop_port_reopens_why_there() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = looped_app(tmp.path());
    dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
    );
    for _ in 0..2 {
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
    );
    let why = app.why.as_ref().expect("the overlay stays open");
    assert_eq!(why.key.origin, "cat/beta");
    assert_eq!(why.selected, 0, "the cursor restarts on the new port");
    // And the new overlay's own walk starts there.
    assert_eq!(why.dep_loop.as_ref().unwrap().walk[0].origin, "cat/beta");
}

#[test]
fn bulk_parse_forms_and_errors() {
    assert_eq!(parse_bulk("docs=on").unwrap(), ("DOCS".to_string(), true));
    assert_eq!(
        parse_bulk(" X11 = OFF ").unwrap(),
        ("X11".to_string(), false)
    );
    assert_eq!(parse_bulk("nls+").unwrap(), ("NLS".to_string(), true));
    assert_eq!(parse_bulk("nls-").unwrap(), ("NLS".to_string(), false));
    assert!(parse_bulk("docs=maybe").is_err());
    assert!(parse_bulk("docs").is_err());
    assert!(parse_bulk("=on").is_err());
}

/// The status bar's last row on a `width`-column screen.
fn status_row(app: &mut App, width: u16) -> String {
    screen_at(app, width, 24)
        .lines()
        .last()
        .unwrap_or_default()
        .to_string()
}

/// At 80 columns the bar keeps the counts and the keys that always
/// hold, then the focused pane's keys, cut at a whole token.
#[test]
fn the_status_bar_names_the_focused_panes_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    press(&mut app, 'j');
    let row = status_row(&mut app, 80);
    assert!(
        row.starts_with(" 0✗ 0* 2? 0! 1✓  ?:help a:apply q:quit Enter:edit"),
        "{row}"
    );
    assert!(!row.contains("optionless"), "{row}");
    assert!(!row.contains("Space:toggle"), "{row}");
    hit(&mut app, KeyCode::Enter);
    let row = status_row(&mut app, 80);
    assert!(row.contains(" ?:help a:apply q:quit Space:toggle"), "{row}");
    assert!(!row.contains("Enter:edit"), "{row}");
    hit(&mut app, KeyCode::Esc);
    let wide = status_row(&mut app, 160);
    assert!(
        wide.trim_end().ends_with("/:filter v:description"),
        "all of it: {wide}"
    );
    assert!(!wide.contains('…'), "{wide}");
}

/// Whatever the width, the legend ends on a whole token or on `…`.
#[test]
fn the_status_bar_never_shows_half_a_token() {
    let tokens = [
        "?:help",
        "a:apply",
        "q:quit",
        "Enter:edit",
        "n/p:next-problem",
        "t:attention",
        "/:filter",
        "v:description",
        "Space:toggle",
        "i:info",
        "d:defaults",
        "U:revert",
        "u:undo",
        "…",
    ];
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    for editor in [false, true] {
        if editor {
            hit(&mut app, KeyCode::Enter);
        } else {
            app.clear_message();
        }
        for width in 40..=140 {
            let row = status_row(&mut app, width);
            let legend = row.split_once(" 1✓ ").map(|(_, l)| l).unwrap_or_default();
            for word in legend.split_whitespace() {
                assert!(tokens.contains(&word), "{word:?} at {width}: {row}");
            }
        }
    }
}

/// alpha and gamma unconfigured, beta and delta ok, zeta in conflict
/// (by an edit), in problems-first order.
fn five_ports(tmp: &std::path::Path) -> App {
    for name in ["cat_beta", "cat_delta"] {
        let dir = tmp.join("options").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("options"),
            "_FILE_COMPLETE_OPTIONS_LIST=DOCS\nOPTIONS_FILE_UNSET+=DOCS\n",
        )
        .unwrap();
    }
    let mut app = app_for(
        tmp,
        &[
            ("cat/alpha", &[]),
            ("cat/beta", &[]),
            ("cat/delta", &[]),
            ("cat/gamma", &[]),
            ("cat/zeta", &[]),
        ],
    );
    make_conflict(&mut app, "cat/zeta");
    app.refresh_view();
    assert_eq!(
        visible(&app),
        [
            "cat/zeta",
            "cat/alpha",
            "cat/gamma",
            "cat/beta",
            "cat/delta"
        ]
    );
    app.select_index(0);
    app
}

/// The list row drawn highlighted (the selection as the widget shows
/// it), on the 100x35 screen.
fn highlighted(app: &mut App) -> String {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 35)).unwrap();
    terminal.draw(|f| ui::draw(f, app)).unwrap();
    let buffer = terminal.backend().buffer();
    let rows = ui::screen_rows(buffer);
    let y = (0..35u16)
        .find(|&y| {
            buffer
                .cell((1, y))
                .is_some_and(|c| c.modifier.contains(ratatui::style::Modifier::REVERSED))
        })
        .expect("a highlighted row");
    rows[y as usize].clone()
}

/// Two columns before the name: the state, then `*` for staged edits.
/// An edit hides no state, so an edited ok port is still ok: it sorts
/// with them, n/p pass it, t hides it, and the bar counts it twice.
#[test]
fn list_rows_show_the_state_then_the_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = five_ports(tmp.path());
    for (port, on) in [("cat/alpha", "DOCS"), ("cat/beta", "DOCS")] {
        app.jump_to_port(&PortKey::parse(port).unwrap());
        app.session
            .toggle(&PortKey::parse(port).unwrap(), on)
            .unwrap();
    }
    app.refresh_view();
    let shown = screen(&mut app);
    for row in [
        "✗* cat/zeta",
        "?* cat/alpha",
        "?  cat/gamma",
        "✓* cat/beta",
        "✓  cat/delta",
    ] {
        assert!(shown.contains(&format!("║{row}")), "{row}:\n{shown}");
    }
    assert_eq!(
        visible(&app),
        [
            "cat/zeta",
            "cat/alpha",
            "cat/gamma",
            "cat/beta",
            "cat/delta"
        ],
        "the edited ok port sorts with the ok ones"
    );
    app.select_index(0);
    for want in ["cat/alpha", "cat/gamma", "cat/zeta"] {
        press(&mut app, 'n');
        assert_eq!(selected(&app), want, "n passes the edited ok port");
    }
    press(&mut app, 'p');
    assert_eq!(selected(&app), "cat/gamma");
    app.clear_message();
    let row = status_row(&mut app, 100);
    assert!(
        row.starts_with(" 1✗ 3* 2? 0! 2✓ "),
        "every port by its state, edits apart: {row}"
    );
    press(&mut app, 't');
    assert_eq!(
        visible(&app),
        ["cat/zeta", "cat/alpha", "cat/gamma"],
        "edits are not attention"
    );
}

/// The row of the divider and of each port, on the 100x35 screen.
fn row_of(shown: &str, text: &str) -> usize {
    shown
        .lines()
        .position(|l| l.contains(text))
        .unwrap_or_else(|| panic!("{text}:\n{shown}"))
}

/// In problems-first order a divider counts the ok ports below the
/// problems, across the list's width; never under s or t, nor when one
/// group is empty.
#[test]
fn a_divider_parts_the_problems_from_the_ok_ports() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = five_ports(tmp.path());
    let shown = screen(&mut app);
    // The list is 42 columns wide: 40 inside its borders.
    let divider = format!("║── 2 ok {}║", "─".repeat(32));
    let at = row_of(&shown, &divider);
    assert_eq!(row_of(&shown, "cat/gamma") + 1, at);
    assert_eq!(row_of(&shown, "cat/beta"), at + 1);
    assert!(
        shown.contains("╔ Ports (5) "),
        "the title counts ports only:\n{shown}"
    );
    press(&mut app, 's');
    assert!(
        !screen(&mut app).contains(" ok ─"),
        "not in alphabetical order"
    );
    press(&mut app, 's');
    assert!(screen(&mut app).contains(&divider));
    press(&mut app, 't');
    assert!(!screen(&mut app).contains(" ok ─"), "no ok port under t");
    press(&mut app, 't');

    // One group alone: no divider.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[]), ("cat/gamma", &[])]);
    assert!(!screen(&mut app).contains(" ok ─"), "problems only");
    let tmp = tempfile::tempdir().unwrap();
    let mut app = five_ports(tmp.path());
    press(&mut app, '/');
    for c in "delta".chars() {
        press(&mut app, c);
    }
    assert!(!screen(&mut app).contains(" ok ─"), "ok ports only");
}

/// No key lands on the divider, and the highlight follows the selected
/// port across it, whatever moved it there.
#[test]
fn the_selection_steps_over_the_divider() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = five_ports(tmp.path());
    let port = |app: &mut App| {
        let row = highlighted(app);
        assert!(
            !row.contains(" ok ─"),
            "the divider is never selected: {row}"
        );
        assert!(
            row.contains(&selected(app)),
            "{} not drawn at {row}",
            selected(app)
        );
        selected(app)
    };
    assert_eq!(port(&mut app), "cat/zeta");
    press(&mut app, 'j');
    press(&mut app, 'j');
    assert_eq!(port(&mut app), "cat/gamma");
    press(&mut app, 'j');
    assert_eq!(port(&mut app), "cat/beta", "j steps over it");
    press(&mut app, 'k');
    assert_eq!(port(&mut app), "cat/gamma", "k too");
    hit(&mut app, KeyCode::Down);
    hit(&mut app, KeyCode::Down);
    assert_eq!(port(&mut app), "cat/delta");
    hit(&mut app, KeyCode::Up);
    hit(&mut app, KeyCode::Up);
    assert_eq!(port(&mut app), "cat/gamma");
    press(&mut app, 'G');
    assert_eq!(port(&mut app), "cat/delta");
    press(&mut app, 'g');
    assert_eq!(port(&mut app), "cat/zeta");
    hit(&mut app, KeyCode::End);
    assert_eq!(port(&mut app), "cat/delta");
    hit(&mut app, KeyCode::Home);
    assert_eq!(port(&mut app), "cat/zeta");
    hit(&mut app, KeyCode::PageDown);
    assert_eq!(port(&mut app), "cat/delta");
    hit(&mut app, KeyCode::PageUp);
    assert_eq!(port(&mut app), "cat/zeta");
    ctrl(&mut app, 'f');
    assert_eq!(port(&mut app), "cat/delta");
    ctrl(&mut app, 'b');
    assert_eq!(port(&mut app), "cat/zeta");
    press(&mut app, 'p');
    assert_eq!(port(&mut app), "cat/gamma", "p wraps over the ok ports");
    press(&mut app, 'n');
    assert_eq!(port(&mut app), "cat/zeta");
    app.jump_to_port(&PortKey::parse("cat/beta").unwrap());
    assert_eq!(port(&mut app), "cat/beta");

    // A filter rebuilds the list and moves the divider; Esc keeps the port.
    press(&mut app, '/');
    press(&mut app, 'l');
    hit(&mut app, KeyCode::Enter);
    assert_eq!(visible(&app), ["cat/alpha", "cat/delta"]);
    assert!(screen(&mut app).contains("║── 1 ok ─"));
    press(&mut app, 'j');
    assert_eq!(port(&mut app), "cat/delta");
    press(&mut app, '/');
    hit(&mut app, KeyCode::Esc);
    assert_eq!(visible(&app).len(), 5);
    assert_eq!(port(&mut app), "cat/delta");
    // Under t the ok port it jumps to is shown again, past the divider.
    press(&mut app, 'g');
    press(&mut app, 't');
    app.jump_to_port(&PortKey::parse("cat/beta").unwrap());
    assert_eq!(port(&mut app), "cat/beta");
}

/// The focused pane has a double border, the other a plain one, and the
/// option pane's rules join whichever it has.
#[test]
fn the_focused_pane_has_a_double_border() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    let key = PortKey::parse("cat/alpha").unwrap();
    let info = app.session.ports.get_mut(&key).unwrap();
    info.about.comment = "The alpha port".to_string();
    app.rebuild_editor();
    let on_list = screen(&mut app);
    assert!(on_list.contains("╔ Ports"), "{on_list}");
    assert!(on_list.contains("┌ alpha-1.0"), "{on_list}");
    assert!(!on_list.contains(['╟', '╢']), "{on_list}");
    assert_eq!(rule_rows(&on_list).len(), 2, "{on_list}");
    for y in rule_rows(&on_list) {
        let line = on_list.lines().nth(y).unwrap();
        assert!(line.contains('├') && line.ends_with('┤'), "{line}");
    }
    hit(&mut app, KeyCode::Enter);
    let on_options = screen(&mut app);
    assert!(on_options.contains("┌ Ports"), "{on_options}");
    assert!(on_options.contains("╔ alpha-1.0"), "{on_options}");
    assert!(!on_options.contains(['├', '┤']), "{on_options}");
    assert_eq!(rule_rows(&on_options), rule_rows(&on_list));
    for y in rule_rows(&on_options) {
        let line = on_options.lines().nth(y).unwrap();
        assert!(line.contains('╟') && line.ends_with('╢'), "{line}");
    }
}

/// The ports with no options are counted in the list's title, not in
/// the status bar, and not at all when there are none.
#[test]
fn the_optionless_count_is_in_the_list_title() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    app.clear_message();
    let shown = screen(&mut app);
    assert!(shown.contains("╔ Ports (3) "), "{shown}");
    app.hidden = 151;
    let shown = screen(&mut app);
    assert!(shown.contains("╔ Ports (3 · 151 optionless) "), "{shown}");
    assert!(
        !shown
            .lines()
            .last()
            .unwrap_or_default()
            .contains("optionless")
    );
    press(&mut app, 't');
    let shown = screen_at(&mut app, 200, 24);
    assert!(
        shown.contains("╔ Ports (2/3 · 151 optionless) — needs attention "),
        "{shown}"
    );
}

/// A list row too long for the pane loses its category first, then the
/// end of its name, never a badge; a row that fits is left alone.
#[test]
fn a_long_list_row_cuts_its_category_first() {
    let tmp = tempfile::tempdir().unwrap();
    let ffmpeg = "multimedia/ffmpeg@default";
    let libav = "multimedia/gstreamer1-libav@default";
    let good = "multimedia/gstreamer1-plugins-good@default";
    let mut app = app_for(
        tmp.path(),
        &[
            (ffmpeg, &[]),
            (libav, &[]),
            (good, &["cat/b"]),
            ("cat/b", &[good]),
            ("cat/a", &[]),
        ],
    );
    for origin in [ffmpeg, libav, good] {
        let key = PortKey::parse(origin).unwrap();
        app.session.ports.get_mut(&key).unwrap().broken = Some("fails".into());
    }
    app.clear_message();
    // The list pane's rows, between its borders.
    let rows = |app: &mut App, width: u16| -> Vec<String> {
        screen_at(app, width, 24)
            .lines()
            .skip(1)
            .map(|l| {
                l.split('║')
                    .nth(1)
                    .unwrap_or_default()
                    .trim_end()
                    .to_string()
            })
            .filter(|r| !r.is_empty() && !r.starts_with('═'))
            .collect()
    };
    let at80 = rows(&mut app, 80);
    assert!(at80.iter().any(|r| r.ends_with(" cat/a")), "{at80:?}");
    assert!(
        at80.iter()
            .any(|r| r.ends_with(" multimedia/ffmpeg@default ⚠")),
        "fits, unchanged: {at80:?}"
    );
    assert!(
        at80.iter()
            .any(|r| r.ends_with(" m…/gstreamer1-libav@default ⚠")),
        "{at80:?}"
    );
    assert!(
        at80.iter()
            .any(|r| r.ends_with(" …/gstreamer1-plugins-goo… ⚠ ∞")),
        "{at80:?}"
    );
    let at60 = rows(&mut app, 60);
    assert!(
        at60.iter().any(|r| r.ends_with(" mu…/ffmpeg@default ⚠")),
        "{at60:?}"
    );
    assert!(
        at60.iter().any(|r| r.ends_with(" …/gstreamer1-liba… ⚠")),
        "{at60:?}"
    );
    assert!(
        at60.iter().any(|r| r.ends_with(" …/gstreamer1-pl… ⚠ ∞")),
        "{at60:?}"
    );
    // Every row in its pane, its markers in their two columns.
    for r in at60.iter().chain(&at80) {
        assert!(r.chars().nth(2) == Some(' '), "{r}");
    }
    assert!(at60.iter().all(|r| r.chars().count() <= 23), "{at60:?}");
}

/// The name of a list row in a given room, cut as the row cuts it.
#[test]
fn fit_port_name_cuts_the_category_then_the_end() {
    let key = PortKey::parse("multimedia/ffmpeg@default").unwrap();
    assert_eq!(ui::fit_port_name(&key, 25), "multimedia/ffmpeg@default");
    assert_eq!(ui::fit_port_name(&key, 22), "multim…/ffmpeg@default");
    assert_eq!(ui::fit_port_name(&key, 17), "m…/ffmpeg@default");
    assert_eq!(ui::fit_port_name(&key, 16), "…/ffmpeg@default");
    assert_eq!(ui::fit_port_name(&key, 15), "…/ffmpeg@defau…");
    let plain = PortKey::parse("www/nginx").unwrap();
    assert_eq!(ui::fit_port_name(&plain, 8), "w…/nginx");
    assert_eq!(ui::fit_port_name(&plain, 7), "…/nginx");
}

/// Too long for the pane, the list's title drops the optionless count,
/// then names its view in short, and only then cuts.
#[test]
fn the_list_title_keeps_its_view_before_the_optionless_count() {
    let top = |app: &mut App, width| {
        screen_at(app, width, 24)
            .lines()
            .next()
            .unwrap()
            .to_string()
    };
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    app.clear_message();
    app.hidden = 151;
    press(&mut app, 't');
    app.listable = 142;
    let shown = top(&mut app, 90);
    assert!(
        shown.starts_with("╔ Ports (2/142) — needs attention ═"),
        "{shown}"
    );
    // At 80 columns the pane has 32 for its title: the view in short.
    let shown = top(&mut app, 80);
    assert!(
        shown.starts_with("╔ Ports (2/142) — attention ═"),
        "{shown}"
    );

    let tmp = tempfile::tempdir().unwrap();
    let mut looped = looped_app(tmp.path());
    looped.clear_message();
    looped.hidden = 151;
    press(&mut looped, 'L');
    let shown = top(&mut looped, 90);
    assert!(
        shown.starts_with("╔ Ports (2) — in a dependency loop ═"),
        "{shown}"
    );
    let shown = top(&mut looped, 80);
    assert!(shown.starts_with("╔ Ports (2) — in a loop ═"), "{shown}");
    // Wide enough, the count is back.
    let shown = top(&mut looped, 200);
    assert!(
        shown.starts_with("╔ Ports (2 · 151 optionless) — in a dependency loop ═"),
        "{shown}"
    );
    // Both views: in short, cut at a word when even that is too long.
    press(&mut looped, 't');
    let shown = top(&mut looped, 100);
    assert!(
        shown.starts_with("╔ Ports (2/2) — in a loop — attention ═"),
        "{shown}"
    );
    let shown = top(&mut looped, 80);
    assert!(
        shown.starts_with("╔ Ports (2/2) — in a loop… ═"),
        "never a dangling dash: {shown}"
    );
}

/// News from the background waits for the line: it does not replace a
/// message nobody has had a key's time to read, unless it is an error,
/// and nothing but an error replaces an error.
#[test]
fn background_news_does_not_hide_an_unread_message() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    let shown = |app: &App| app.message.clone().unwrap_or_default();
    app.notify("dependency loop gone", false);
    assert!(
        shown(&app).0.starts_with("2 never configured"),
        "the summary holds: {:?}",
        shown(&app)
    );
    press(&mut app, 'j');
    app.notify("dependency loop gone", false);
    assert_eq!(
        shown(&app),
        ("dependency loop gone".to_string(), false),
        "nothing to replace after a key"
    );
    press(&mut app, 'f');
    assert!(shown(&app).0.contains("no other flavors"));
    app.notify("dependency loop: a → b → a", true);
    assert_eq!(
        shown(&app),
        ("dependency loop: a → b → a".to_string(), true),
        "an error wins"
    );
    app.notify("dependency loop gone", false);
    assert!(shown(&app).1, "an error is not replaced by news");
    app.message_unread = false;
    app.notify("dependency loop gone", false);
    assert!(shown(&app).1, "not even once read");
    app.notify("refresh: 1 dep error(s)", true);
    assert_eq!(shown(&app).0, "refresh: 1 dep error(s)");
}

/// A refresh names the ports it brought in and dropped, up to three;
/// past that it counts them.
#[test]
fn a_refresh_names_up_to_three_ports() {
    let key = |o: &str| PortKey::parse(o).unwrap();
    let added = [key("audio/opencore-amr")];
    let removed = [key("devel/foo")];
    assert_eq!(
        refreshed_message(&added, &removed),
        "dependencies refreshed: +audio/opencore-amr −devel/foo"
    );
    let three = [key("cat/a"), key("cat/b"), key("cat/c")];
    assert_eq!(
        refreshed_message(&three, &[]),
        "dependencies refreshed: +cat/a +cat/b +cat/c"
    );
    assert_eq!(
        refreshed_message(&three, &removed),
        "dependencies refreshed: +3 −1 port(s)"
    );
}

/// At start the line says what the closure needs, in list order, and
/// only what it does need.
#[test]
fn the_first_message_sums_up_what_needs_doing() {
    let tmp = tempfile::tempdir().unwrap();
    let stale = tmp.path().join("options/cat_beta");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(
        stale.join("options"),
        "_FILE_COMPLETE_OPTIONS_LIST=OLD\nOPTIONS_FILE_SET+=OLD\n",
    )
    .unwrap();
    let ok = tmp.path().join("options/cat_eps");
    std::fs::create_dir_all(&ok).unwrap();
    std::fs::write(
        ok.join("options"),
        "_FILE_COMPLETE_OPTIONS_LIST=DOCS\nOPTIONS_FILE_SET+=DOCS\n",
    )
    .unwrap();
    let ports: [(&str, &[&str]); 5] = [
        ("cat/alpha", &[]),
        ("cat/beta", &[]),
        ("cat/gamma", &[]),
        ("cat/delta", &[]),
        ("cat/eps", &[]),
    ];
    let mut app = app_for(tmp.path(), &ports);
    make_conflict(&mut app, "cat/alpha");
    app.rebuild_visible(None);
    app.show_summary(None);
    let full = "1 conflicting · 2 never configured · 1 stale — a: write defaults for all · n: next problem · ?: help";
    assert_eq!(app.message, Some((full.to_string(), false)));
    assert_eq!(status_row(&mut app, 102).trim_end(), format!(" {full}"));
    // The keys go from the right, whole, before the counts do.
    assert_eq!(
        status_row(&mut app, 100).trim_end(),
        " 1 conflicting · 2 never configured · 1 stale — a: write defaults for all · n: next problem …"
    );
    assert_eq!(
        status_row(&mut app, 80).trim_end(),
        " 1 conflicting · 2 never configured · 1 stale — a: write defaults for all …"
    );
    assert_eq!(
        status_row(&mut app, 60).trim_end(),
        " 1 conflicting · 2 never configured · 1 stale …"
    );
    assert_eq!(
        status_row(&mut app, 40).trim_end(),
        " 1 conflicting · 2 never configured …"
    );

    // No port never configured: no `a` to offer.
    let tmp = tempfile::tempdir().unwrap();
    let stale = tmp.path().join("options/cat_beta");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(
        stale.join("options"),
        "_FILE_COMPLETE_OPTIONS_LIST=OLD\nOPTIONS_FILE_SET+=OLD\n",
    )
    .unwrap();
    let ok = tmp.path().join("options/cat_alpha");
    std::fs::create_dir_all(&ok).unwrap();
    std::fs::write(
        ok.join("options"),
        "_FILE_COMPLETE_OPTIONS_LIST=DOCS\nOPTIONS_FILE_SET+=DOCS\n",
    )
    .unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[]), ("cat/beta", &[])]);
    make_conflict(&mut app, "cat/alpha");
    app.rebuild_visible(None);
    app.show_summary(None);
    let full = "1 conflicting · 1 stale — n: next problem · ?: help";
    assert_eq!(app.message, Some((full.to_string(), false)));
    assert_eq!(status_row(&mut app, 60).trim_end(), format!(" {full}"));
    assert_eq!(
        status_row(&mut app, 40).trim_end(),
        " 1 conflicting · 1 stale …"
    );

    let tmp = tempfile::tempdir().unwrap();
    for name in ["cat_alpha", "cat_beta"] {
        let dir = tmp.path().join("options").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("options"),
            "_FILE_COMPLETE_OPTIONS_LIST=DOCS\nOPTIONS_FILE_SET+=DOCS\n",
        )
        .unwrap();
    }
    let app = app_for(tmp.path(), &[("cat/alpha", &[]), ("cat/beta", &[])]);
    assert_eq!(
        app.message,
        Some(("all 2 ports are configured — ?: help".to_string(), false))
    );
}

/// A restored draft leads the first message, whole; how to revert it
/// goes first when the line is short, then the counts at a whole token.
#[test]
fn the_first_message_leads_with_a_restored_draft() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    hit(&mut app, KeyCode::Enter);
    press(&mut app, ' ');
    press(&mut app, 'q');
    press(&mut app, 'y');
    drop(app);

    let mut app = three_ports(tmp.path());
    assert_eq!(
        app.message.clone().unwrap().0,
        "restored draft (1 port, 0m ago, U per port reverts) · 2 never configured — a: write defaults for all · n: next · ?: help"
    );
    assert_eq!(
        status_row(&mut app, 102).trim_end(),
        " restored draft (1 port, 0m ago) · 2 never configured — a: write defaults for all · n: next · ?: help"
    );
    assert_eq!(
        status_row(&mut app, 100).trim_end(),
        " restored draft (1 port, 0m ago) · 2 never configured — a: write defaults for all · n: next …"
    );
    assert_eq!(
        status_row(&mut app, 84).trim_end(),
        " restored draft (1 port, 0m ago) · 2 never configured — a: write defaults for all …"
    );
    assert_eq!(
        status_row(&mut app, 80).trim_end(),
        " restored draft (1 port, 0m ago) · 2 never configured …"
    );
    assert_eq!(
        status_row(&mut app, 60).trim_end(),
        " restored draft (1 port, 0m ago) · 2 never configured …"
    );
    assert_eq!(
        status_row(&mut app, 40).trim_end(),
        " restored draft (1 port, 0m ago) …"
    );
    assert_eq!(
        status_row(&mut app, 20).trim_end(),
        " restored draft (1 p",
        "kept whole, cut by the edge"
    );
    let summary = app.summary.as_ref().unwrap();
    assert_eq!(
        summary.fit(60),
        "restored draft (1 port, 0m ago) · 2 never configured …"
    );
}

/// A refresh landing before the first key leaves the first message up;
/// after a key it has the line.
#[test]
fn the_first_message_outlives_a_startup_refresh() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(
        tmp.path(),
        &[("cat/alpha", &["cat/beta"]), ("cat/beta", &[])],
    );
    app.session.roots = vec![PortKey::parse("cat/alpha").unwrap()];
    let (ev, _req) = fake_refresher(&mut app);
    let first = app.message.clone();
    assert_eq!(
        first.as_ref().unwrap().0,
        "2 never configured — a: write defaults for all · n: next problem · ?: help"
    );
    let mut alpha = info_of(&app, "cat/alpha");
    alpha.deps.clear();
    app.hidden = 7;
    ev.send(done(vec![alpha.clone()], vec![])).unwrap();
    app.drain_refresh_events();
    assert_eq!(visible(&app), ["cat/alpha"], "merged all the same");
    assert_eq!(app.hidden, 0, "the optionless count follows the merge");
    assert_eq!(app.message, first, "the summary is still up");
    assert!(app.summary.is_some());

    press(&mut app, 'j');
    let mut beta = info_of(&app, "cat/alpha");
    beta.key = PortKey::parse("cat/beta").unwrap();
    beta.canonical = beta.key.clone();
    beta.options_name = "cat_beta".into();
    alpha.deps = vec![crate::model::port::DepEdge {
        target: beta.key.clone(),
        spec: "dep:cat/beta".into(),
        test_only: false,
    }];
    ev.send(done(vec![alpha, beta], vec![])).unwrap();
    app.drain_refresh_events();
    assert_eq!(
        app.message.clone().unwrap().0,
        "dependencies refreshed: +cat/beta"
    );
}

/// The description panel's default: hidden on a terminal under 30 rows,
/// shown from 30, decided at every frame.
#[test]
fn the_description_panel_shows_from_30_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let key = PortKey::parse("cat/alpha").unwrap();
    app.session.ports.get_mut(&key).unwrap().about.comment = "The alpha port".into();
    assert!(
        !screen_at(&mut app, 80, 24).contains("The alpha port"),
        "hidden on 24 rows"
    );
    assert!(
        !screen_at(&mut app, 80, 29).contains("The alpha port"),
        "hidden on 29 rows"
    );
    assert!(
        screen_at(&mut app, 80, 30).contains("The alpha port"),
        "shown on 30 rows"
    );
}

/// V is a choice that holds until quit: no resize undoes it, either way.
#[test]
fn v_overrides_the_height_both_ways() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let key = PortKey::parse("cat/alpha").unwrap();
    app.session.ports.get_mut(&key).unwrap().about.comment = "The alpha port".into();
    let msg = |app: &App| {
        app.message
            .as_ref()
            .map(|(m, _)| m.clone())
            .unwrap_or_default()
    };

    assert!(!screen_at(&mut app, 80, 24).contains("The alpha port"));
    press(&mut app, 'V');
    assert_eq!(msg(&app), "port description shown (V: hide · v: full text)");
    assert!(
        screen_at(&mut app, 80, 24).contains("The alpha port"),
        "V shows it on 24 rows"
    );
    assert!(screen_at(&mut app, 80, 40).contains("The alpha port"));
    assert!(
        screen_at(&mut app, 80, 24).contains("The alpha port"),
        "and it stays"
    );

    press(&mut app, 'V');
    assert_eq!(
        msg(&app),
        "port description hidden (V: show · v: full text)"
    );
    assert!(
        !screen_at(&mut app, 80, 40).contains("The alpha port"),
        "V hides it on 40 rows"
    );
    assert!(!screen_at(&mut app, 80, 24).contains("The alpha port"));
    assert!(
        !screen_at(&mut app, 80, 40).contains("The alpha port"),
        "and it stays"
    );
}

/// With no V pressed, a resize across 30 rows shows or hides the panel,
/// and the one that shows it reads the description it needs.
#[test]
fn a_resize_across_30_rows_turns_the_panel_and_reads_the_description() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
    let descr = tmp.path().join("pkg-descr");
    std::fs::write(&descr, "Alpha does things.\n").unwrap();
    let key = PortKey::parse("cat/alpha").unwrap();
    app.session.ports.get_mut(&key).unwrap().about.pkg_descr = Some(descr.display().to_string());
    app.rebuild_editor();
    assert!(!screen_at(&mut app, 80, 24).contains("Alpha does things."));
    assert!(
        !app.about_text.contains_key(&key),
        "nothing read while hidden"
    );
    assert!(
        screen_at(&mut app, 80, 30).contains("Alpha does things."),
        "read on the resize"
    );
    assert!(app.about_text.contains_key(&key));
    assert!(
        !screen_at(&mut app, 80, 24).contains("Alpha does things."),
        "hidden again"
    );
}

/// A port with a long option description, group description, banner
/// and CONFLICT, for the cut tests.
fn long_texts_app(tmp: &std::path::Path) -> App {
    use crate::model::options::{GroupKind, OptionDef, OptionGroup};
    let mut app = app_for(tmp, &[("cat/alpha", &[])]);
    let key = PortKey::parse("cat/alpha").unwrap();
    let info = app.session.ports.get_mut(&key).unwrap();
    info.options.complete = vec!["DOCS".into(), "A".into(), "B".into()];
    info.options.defs.insert(
        "DOCS".into(),
        OptionDef {
            desc: "Build and install the documentation of every module".into(),
            ..Default::default()
        },
    );
    info.options.defs.insert(
        "A".into(),
        OptionDef {
            prevents: vec!["B".into()],
            prevents_msg: Some(
                "the two backends install the same files under the same names and cannot be \
                 installed together on one system, whatever the flavor, in any jail or set"
                    .into(),
            ),
            ..Default::default()
        },
    );
    info.options.groups.push(OptionGroup {
        kind: GroupKind::Multi,
        name: "BACKENDS".into(),
        desc: "Storage backends this port links against at build time".into(),
        members: vec!["A".into(), "B".into()],
    });
    info.broken = Some("does not build against the current toolchain release at all".into());
    let name = info.options_name.clone();
    let staged = &mut app.session.states.get_mut(&name).unwrap().staged;
    staged.insert("A".into());
    staged.insert("B".into());
    app.rebuild_editor();
    app
}

/// Whether `row` ends in `…` right after a whole word of `text`.
fn cut_at_a_word(row: &str, text: &str) -> bool {
    let Some(body) = row
        .trim_end_matches(['│', '║'])
        .trim_end()
        .strip_suffix('…')
    else {
        return false;
    };
    // The longest tail of the row that starts the text.
    (0..body.len())
        .filter(|&i| body.is_char_boundary(i))
        .map(|i| &body[i..])
        .find(|tail| !tail.is_empty() && text.starts_with(tail))
        .is_some_and(|tail| text[tail.len()..].starts_with(' ') && !tail.ends_with(' '))
}

/// Option rows, group headers and banners wider than the pane end in
/// `…` at a word, not mid-word at the border.
#[test]
fn cut_rows_end_with_an_ellipsis_at_a_word() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = long_texts_app(tmp.path());
    let shown = screen_at(&mut app, 80, 24);
    let row = |needle: &str| {
        shown
            .lines()
            .find(|l| l.contains(needle))
            .unwrap_or_default()
            .to_string()
    };
    let docs = row("DOCS");
    assert!(
        cut_at_a_word(&docs, "Build and install the documentation of every module"),
        "{docs}\n{shown}"
    );
    let header = row("── BACKENDS");
    assert!(
        cut_at_a_word(
            &header,
            "── BACKENDS — multi (pick at least one): Storage backends this port links against"
        ),
        "{header}\n{shown}"
    );
    let broken = row("BROKEN:");
    assert!(
        cut_at_a_word(
            &broken,
            "does not build against the current toolchain release at all"
        ),
        "{broken}\n{shown}"
    );
}

/// A CONFLICT too long for a line wraps to a second, under its text,
/// and what does not fit in two ends in `…`.
#[test]
fn a_long_conflict_wraps_to_two_lines_then_is_cut() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = long_texts_app(tmp.path());
    let shown = screen_at(&mut app, 80, 24);
    let lines: Vec<&str> = shown.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains("CONFLICT: "))
        .expect(&shown);
    // The option pane's text, between its borders.
    let pane = |l: &str| l.split(['│', '║']).nth(3).unwrap_or_default().to_string();
    let first = pane(lines[at]);
    let second = pane(lines[at + 1]);
    let indent = first.find("CONFLICT: ").unwrap() + "CONFLICT: ".len();
    assert!(
        second.starts_with(&" ".repeat(indent)),
        "under its text:\n{shown}"
    );
    assert!(!second[indent..].starts_with(' '), "{second:?}");
    assert!(
        second.trim_end().ends_with('…'),
        "cut after two lines:\n{shown}"
    );
    assert!(
        !pane(lines[at + 2]).starts_with(&" ".repeat(indent)),
        "no third line:\n{shown}"
    );
    assert!(
        !first.trim_end().ends_with('…'),
        "the first line is whole words, unmarked"
    );
}

/// The list's title ends in `…` at a word when it does not fit.
#[test]
fn a_long_list_title_ends_with_an_ellipsis_at_a_word() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    press(&mut app, '/');
    for c in "a long filter text that cannot fit".chars() {
        press(&mut app, c);
    }
    hit(&mut app, KeyCode::Enter);
    press(&mut app, 't');
    let shown = screen_at(&mut app, 60, 24);
    let top = shown.lines().next().unwrap();
    let title = top.split('╔').nth(1).unwrap().split(" ═").next().unwrap();
    assert!(title.ends_with('…'), "{top}");
    let body = title.trim_start().trim_end_matches('…');
    assert!(!body.ends_with(' '));
    let full = "Ports (0/0) — needs attention";
    assert!(
        full.starts_with(body) && full[body.len()..].starts_with(' '),
        "{top}"
    );
    assert!(!top.contains("filter"), "{top}");
}

/// The bulk prompt's title says which ports it works on and what
/// narrows them.
#[test]
fn the_bulk_title_names_its_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    press(&mut app, 'B');
    assert!(
        screen(&mut app).contains("┌ bulk over the 3 listed ports ─"),
        "{}",
        screen(&mut app)
    );
    hit(&mut app, KeyCode::Esc);

    press(&mut app, '/');
    for c in "a".chars() {
        press(&mut app, c);
    }
    hit(&mut app, KeyCode::Enter);
    let n = app.visible.len();
    press(&mut app, 'B');
    assert!(
        screen(&mut app).contains(&format!("┌ bulk over the {n} listed ports (filter: a) ─")),
        "{}",
        screen(&mut app)
    );
    hit(&mut app, KeyCode::Esc);
    hit(&mut app, KeyCode::Esc);

    press(&mut app, 't');
    press(&mut app, 'B');
    assert!(
        screen(&mut app).contains("┌ bulk over the 2 listed ports (attention only) ─"),
        "{}",
        screen(&mut app)
    );

    let tmp = tempfile::tempdir().unwrap();
    let mut looped = looped_app(tmp.path());
    press(&mut looped, 'L');
    press(&mut looped, 'B');
    assert!(
        screen(&mut looped).contains("┌ bulk over the 2 listed ports (in a loop) ─"),
        "{}",
        screen(&mut looped)
    );
    hit(&mut looped, KeyCode::Esc);
    press(&mut looped, 't');
    press(&mut looped, 'B');
    let n = looped.visible.len();
    let title = format!(
        "┌ bulk over the {n} listed port{} (attention only) (in a loop) ─",
        if n == 1 { "" } else { "s" }
    );
    assert!(
        screen(&mut looped).contains(&title),
        "{}",
        screen(&mut looped)
    );
}

/// The bottom border counts, as it is typed, the listed ports carrying
/// the option, and says so in red when none does.
#[test]
fn the_bulk_prompt_counts_the_ports_carrying_the_option() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    press(&mut app, 'B');
    assert!(
        screen(&mut app).contains("└ Enter apply · Esc cancel · Ctrl-U clear ─"),
        "{}",
        screen(&mut app)
    );
    for c in "docs".chars() {
        press(&mut app, c);
    }
    assert!(
        screen(&mut app).contains("└ 3 carry DOCS · Enter apply · Esc cancel · Ctrl-U clear ─")
    );
    for c in "=on".chars() {
        press(&mut app, c);
    }
    assert!(screen(&mut app).contains("└ 3 carry DOCS · Enter apply"));
    ctrl(&mut app, 'u');
    for c in "X11+".chars() {
        press(&mut app, c);
    }
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 35)).unwrap();
    terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
    let shown = ui::screen_rows(terminal.backend().buffer()).join("\n");
    let warning = "no listed port has X11";
    assert!(
        shown.contains(&format!("└ {warning} · Enter apply")),
        "{shown}"
    );
    // In red: the cell under its first letter.
    let buffer = terminal.backend().buffer();
    let (row, line) = shown
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains(warning))
        .unwrap();
    let col = line[..line.find(warning).unwrap()].chars().count();
    assert_eq!(
        buffer[(col as u16, row as u16)].fg,
        ratatui::style::Color::Red
    );
}

/// Ctrl-U clears the bulk line, and is no key anywhere else.
#[test]
fn ctrl_u_clears_the_bulk_line_only() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    let before = (selected(&app), app.filter.clone(), app.focus == Focus::List);
    ctrl(&mut app, 'u');
    assert_eq!(
        (selected(&app), app.filter.clone(), app.focus == Focus::List),
        before
    );
    assert!(app.message.is_none() && app.undo.is_empty() && !app.session.dirty());
    hit(&mut app, KeyCode::Enter);
    press(&mut app, ' ');
    assert!(app.session.dirty());
    ctrl(&mut app, 'u');
    assert_eq!(app.undo.len(), 1, "Ctrl-U is not u: nothing undone");

    press(&mut app, 'B');
    for c in "DOCS=off".chars() {
        press(&mut app, c);
    }
    ctrl(&mut app, 'u');
    assert_eq!(
        app.bulk.as_deref(),
        Some(""),
        "cleared, the prompt still open"
    );
}

/// A bulk input that does not parse is quoted whole in the error.
#[test]
fn a_bulk_error_quotes_the_whole_input() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = three_ports(tmp.path());
    press(&mut app, 'B');
    for c in " CURL=DOCS=off ".chars() {
        press(&mut app, c);
    }
    hit(&mut app, KeyCode::Enter);
    assert_eq!(
        app.message,
        Some((
            "\"CURL=DOCS=off\": expected OPTION=on|off (or OPTION+ / OPTION-)".to_string(),
            true
        ))
    );
    assert_eq!(
        parse_bulk("=on").unwrap_err(),
        "\"=on\": no option name — expected OPTION=on|off (or OPTION+ / OPTION-)"
    );
    assert_eq!(
        parse_bulk("docs").unwrap_err(),
        "\"docs\": expected OPTION=on|off (or OPTION+ / OPTION-)"
    );
}

/// An options file for cat/alpha listing `complete`, `set` on and every
/// other option off.
fn alpha_file(tmp: &std::path::Path, complete: &[&str], set: &[&str]) {
    let dir = tmp.join("options/cat_alpha");
    std::fs::create_dir_all(&dir).unwrap();
    let mut text = format!(
        "_OPTIONS_READ=alpha-1.0\n_FILE_COMPLETE_OPTIONS_LIST={}\n",
        complete.join(" ")
    );
    for o in complete {
        let verb = if set.contains(o) { "SET" } else { "UNSET" };
        text.push_str(&format!("OPTIONS_FILE_{verb}+={o}\n"));
    }
    std::fs::write(dir.join("options"), text).unwrap();
}

/// cat/alpha alone, with the options `complete`, its file already
/// written (or not) by the caller.
fn alpha_app(tmp: &std::path::Path, complete: &[&str]) -> App {
    let mut app = app_for(tmp, &[("cat/alpha", &[])]);
    alpha_mut(&mut app).options.complete = complete.iter().map(ToString::to_string).collect();
    app.rebuild_visible(None);
    app.rebuild_editor();
    app
}

fn alpha_mut(app: &mut App) -> &mut crate::model::port::PortInfo {
    app.session
        .ports
        .get_mut(&PortKey::parse("cat/alpha").unwrap())
        .unwrap()
}

fn stage(app: &mut App, name: &str, on: &[&str]) {
    app.session.states.get_mut(name).unwrap().staged = on.iter().map(ToString::to_string).collect();
}

/// The option under the cursor.
fn cursor(app: &App) -> String {
    match app.editor_rows.get(app.editor_idx) {
        Some(EditorRow::Option(o)) => o.clone(),
        _ => "(not an option)".into(),
    }
}

/// The make.conf column of `opt`'s row on the 100x35 screen: its text and
/// its style.
fn mc_column(app: &mut App, opt: &str) -> (String, ratatui::style::Style) {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 35)).unwrap();
    terminal.draw(|f| ui::draw(f, app)).unwrap();
    let buffer = terminal.backend().buffer();
    let rows = ui::screen_rows(buffer);
    let name = format!(" {opt:<12} ");
    let (y, row) = rows
        .iter()
        .enumerate()
        .find(|(_, r)| r.contains(&format!("]{name}")) || r.contains(&format!("){name}")))
        .unwrap_or_else(|| panic!("no row for {opt}:\n{}", rows.join("\n")));
    // The checkbox's end, the name, the default (` off`), then ` ` and
    // the column's three.
    let at = row[..row.find(&name).unwrap()].chars().count() + name.len() + 4;
    let text: String = row.chars().skip(at).take(3).collect();
    let cell = buffer.cell((at as u16, y as u16)).unwrap();
    (text.trim().to_string(), cell.style())
}

/// Under make.conf's OPTIONS_OVERRIDE every option shows locked, in the
/// FORCE knob's red: on where the override names it, off elsewhere.
#[test]
fn an_overridden_port_shows_every_option_locked() {
    use ratatui::style::{Color, Modifier};
    let tmp = tempfile::tempdir().unwrap();
    alpha_file(tmp.path(), &["A", "B"], &["B"]);
    let mut app = alpha_app(tmp.path(), &["A", "B"]);
    alpha_mut(&mut app).options.r#override = Some(["A".to_string()].into());
    for (opt, want) in [("A", "on"), ("B", "off")] {
        let (text, style) = mc_column(&mut app, opt);
        assert_eq!(text, want, "{opt}");
        assert!(
            style.fg == Some(Color::Red) && style.add_modifier.contains(Modifier::BOLD),
            "{opt}: {style:?}"
        );
    }
    let info = info_of(&app, "cat/alpha");
    assert_eq!(
        info.options.provenance("B", Some(true)),
        crate::model::options::Provenance::Override
    );
}

/// An options file holding a line optique cannot model shows no
/// make.conf verdict, since the layers make dumped after reading it may
/// be the file's own (inert) OPTIONS_SET, and says why above the rows.
#[test]
fn an_unmanageable_file_shows_no_makeconf_verdict() {
    let tmp = tempfile::tempdir().unwrap();
    alpha_file(tmp.path(), &["A", "B"], &["B"]);
    let path = tmp.path().join("options/cat_alpha/options");
    let text = std::fs::read_to_string(&path).unwrap() + "OPTIONS_SET=A\n";
    std::fs::write(&path, text).unwrap();
    let mut app = alpha_app(tmp.path(), &["A", "B"]);
    alpha_mut(&mut app).options.mc_set = ["A".to_string()].into();
    assert_eq!(mc_column(&mut app, "A").0, "");
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 35)).unwrap();
    terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
    let rows = ui::screen_rows(terminal.backend().buffer());
    assert!(
        rows.iter()
            .any(|r| r.contains("UNMANAGED: options line 5 assigns OPTIONS_SET")),
        "{}",
        rows.join("\n")
    );
}

/// A listed origin MOVED moves: its target's pane says so in a MOVED:
/// line, followed or not, and one not followed, in no row, is counted
/// first in the first-run message.
#[test]
fn a_listed_moved_origin_shows_on_its_target_and_in_the_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = alpha_app(tmp.path(), &["A"]);
    let moved = |listed: &str, to: &str, followed| crate::moved::ListedMove {
        listed: PortKey::parse(listed).unwrap(),
        to: PortKey::parse(to).unwrap(),
        reason: "renamed".into(),
        followed,
        can_follow: true,
        in_tree: false,
    };
    app.session.listed_moves = vec![
        moved("cat/old", "cat/alpha", true),
        moved("cat/gone", "cat/elsewhere", false),
    ];
    app.show_summary(None);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 35)).unwrap();
    terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
    let rows = ui::screen_rows(terminal.backend().buffer());
    let shown = rows.join("\n");
    assert!(
        shown.contains("MOVED: listed as cat/old (renamed); fix the list"),
        "{shown}"
    );
    assert!(!shown.contains("cat/gone"), "{shown}");
    assert!(
        rows.last()
            .unwrap()
            .contains("1 listed moved (fix the list)"),
        "{shown}"
    );

    app.session.listed_moves = vec![moved("cat/old@all", "cat/alpha@all", false)];
    terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
    let shown = ui::screen_rows(terminal.backend().buffer()).join("\n");
    assert!(
        shown.contains("MOVED: listed as cat/old@all (renamed), not followed;"),
        "{shown}"
    );
    assert!(
        shown.contains("fix the list, or --follow-moved"),
        "wrapped under its text: {shown}"
    );
}

/// The make.conf column says what make.conf says whenever a layer names
/// the option, the options file deciding or not: the last layer wins,
/// in its colour; blank only when no layer names it.
#[test]
fn the_makeconf_column_shows_makeconf_even_where_the_file_decides() {
    use ratatui::style::{Color, Modifier};
    let tmp = tempfile::tempdir().unwrap();
    let all = ["A", "B", "C", "D", "E", "F"];
    alpha_file(tmp.path(), &all, &["A", "B", "C", "E", "F"]);
    let mut app = alpha_app(tmp.path(), &all);
    let o = &mut alpha_mut(&mut app).options;
    o.mc_set = ["A".to_string(), "B".to_string()].into();
    o.mc_unset = ["E".to_string(), "F".to_string()].into();
    o.port_set = ["C".to_string(), "F".to_string()].into();
    o.port_unset = ["B".to_string()].into();
    o.force_unset = ["C".to_string()].into();
    let green = |c: Color| {
        move |s: ratatui::style::Style| s.fg == Some(c) && !s.add_modifier.contains(Modifier::BOLD)
    };
    let (text, style) = mc_column(&mut app, "A");
    assert!(
        text == "on" && green(Color::Green)(style),
        "global SET: {text} {style:?}"
    );
    let (text, style) = mc_column(&mut app, "B");
    assert!(
        text == "off" && green(Color::LightGreen)(style),
        "per-port UNSET after the global SET: {text} {style:?}"
    );
    let (text, style) = mc_column(&mut app, "C");
    assert!(
        text == "off"
            && style.fg == Some(Color::Red)
            && style.add_modifier.contains(Modifier::BOLD),
        "FORCE after the per-port SET: {text} {style:?}"
    );
    let (text, _) = mc_column(&mut app, "D");
    assert_eq!(text, "", "no layer names D");
    let (text, style) = mc_column(&mut app, "E");
    assert!(
        text == "off" && green(Color::Green)(style),
        "the file sets E, make.conf still says off: {text}"
    );
    let (text, style) = mc_column(&mut app, "F");
    assert!(
        text == "on" && green(Color::LightGreen)(style),
        "per-port SET after the global UNSET: {text}"
    );
    // Where the value comes from is unchanged: the file decides A.
    let info = info_of(&app, "cat/alpha");
    assert_eq!(
        info.options.provenance("A", Some(true)),
        crate::model::options::Provenance::File
    );
}

/// A stale port says what its file is behind on above the rows, each
/// part only when it has options.
#[test]
fn a_stale_port_has_a_banner_naming_its_new_and_obsolete_options() {
    let line = |file: &[&str], complete: &[&str]| {
        let tmp = tempfile::tempdir().unwrap();
        alpha_file(tmp.path(), file, &[]);
        let mut app = alpha_app(tmp.path(), complete);
        let shown = screen(&mut app);
        shown.lines().find(|l| l.contains("STALE:")).map(|l| {
            let at = l.find("STALE:").unwrap();
            l[at..].trim_end_matches(['│', '║']).trim_end().to_string()
        })
    };
    assert_eq!(
        line(&["A", "OLD"], &["A", "B", "C"]).as_deref(),
        Some("STALE: new B C · obsolete OLD")
    );
    assert_eq!(line(&["A"], &["A", "B"]).as_deref(), Some("STALE: new B"));
    assert_eq!(
        line(&["A", "OLD"], &["A"]).as_deref(),
        Some("STALE: obsolete OLD")
    );
    assert_eq!(line(&["A"], &["A"]), None, "not stale");
    // Never configured: nothing to be behind on.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = alpha_app(tmp.path(), &["A"]);
    assert!(!screen(&mut app).contains("STALE:"));
}

/// A stale port the list counts as done (≈ under -m or the m view) has
/// its STALE banner dimmed, saying what settles it, not in alarm red.
#[test]
fn a_settled_stale_port_has_a_dimmed_banner() {
    let banner = |app: &mut App| {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 35)).unwrap();
        terminal.draw(|f| ui::draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rows = ui::screen_rows(buffer);
        let (y, row) = rows
            .iter()
            .enumerate()
            .find(|(_, r)| r.contains("STALE:"))
            .expect("a STALE banner");
        let at = row.find("STALE:").unwrap();
        let x = row[..at].chars().count() as u16;
        let text = row[at..].trim_end_matches(['│', '║']).trim_end();
        (text.to_string(), buffer[(x, y as u16)].fg)
    };
    use ratatui::style::Color;
    let tmp = tempfile::tempdir().unwrap();
    alpha_file(tmp.path(), &["A", "OLD"], &[]);
    let mut app = alpha_app(tmp.path(), &["A"]);
    assert_eq!(
        banner(&mut app),
        ("STALE: obsolete OLD".to_string(), Color::LightRed)
    );

    app.minimal = true;
    app.session.minimal = true;
    assert_eq!(
        app.effective_status(&info_of(&app, "cat/alpha")),
        UiStatus::Ok
    );
    assert_eq!(
        banner(&mut app),
        (
            "STALE: obsolete OLD — settled by the defaults (-m)".to_string(),
            Color::DarkGray
        )
    );

    app.minimal = false;
    app.session.minimal = false;
    app.mc_relax = true;
    assert_eq!(
        banner(&mut app),
        (
            "STALE: obsolete OLD — settled by make.conf (m view)".to_string(),
            Color::DarkGray
        )
    );
}

/// The obsolete options, after the last option where the cursor never
/// goes, come into view on that last option, on a pane too short to
/// hold every row.
#[test]
fn the_last_option_brings_the_obsolete_section_into_view() {
    let tmp = tempfile::tempdir().unwrap();
    alpha_file(tmp.path(), &["OLD1", "OLD2"], &["OLD1"]);
    let names: Vec<String> = (0..40).map(|i| format!("OPT{i:02}")).collect();
    let complete: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut app = alpha_app(tmp.path(), &complete);
    hit(&mut app, KeyCode::Enter);
    assert!(
        !screen_at(&mut app, 100, 30).contains("OLD2"),
        "out of view at the top"
    );
    hit(&mut app, KeyCode::Char('G'));
    assert_eq!(cursor(&app), "OPT39");
    let shown = screen_at(&mut app, 100, 30);
    for want in ["OPT39", "── obsolete", "OLD1", "OLD2"] {
        assert!(shown.contains(want), "{want} in view:\n{shown}");
    }
}

/// A port that gained options says how many after its name, `+N` in
/// the yellow of the option pane's N, settled (≈ under -m) or not.
#[test]
fn a_port_with_new_options_has_a_plus_n_badge() {
    let row = |app: &mut App| {
        screen(app)
            .lines()
            .find(|l| l.contains("cat/alpha"))
            .unwrap()
            .to_string()
    };
    let tmp = tempfile::tempdir().unwrap();
    alpha_file(tmp.path(), &["A", "OLD"], &["A"]);
    let mut app = alpha_app(tmp.path(), &["A", "B", "C"]);
    assert!(
        row(&mut app).contains("!  cat/alpha +2 "),
        "{}",
        row(&mut app)
    );
    // -m sets both: the app's view and the session's notion of decided.
    app.minimal = true;
    app.session.minimal = true;
    assert!(
        row(&mut app).contains("≈  cat/alpha +2 "),
        "{}",
        row(&mut app)
    );
    // Only obsolete options: nothing new to point at.
    let mut app = alpha_app(tmp.path(), &["A"]);
    assert!(!row(&mut app).contains('+'), "{}", row(&mut app));
    // Never configured: no file to be behind on.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = alpha_app(tmp.path(), &["A", "B"]);
    assert!(!row(&mut app).contains('+'), "{}", row(&mut app));
}

/// The list's +N, the STALE line and the pane's N count the same
/// options, on a hand-edited file too: IMAP has a line but no place in
/// the list and is settled; POP3 is listed with no line and MQTT has
/// neither, so both are new.
#[test]
fn plus_n_and_the_panes_n_marks_agree() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("options/cat_alpha");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("options"),
        "_OPTIONS_READ=alpha-1.0\n_FILE_COMPLETE_OPTIONS_LIST=A POP3\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=IMAP\n",
    )
    .unwrap();
    let mut app = alpha_app(tmp.path(), &["A", "IMAP", "MQTT", "POP3"]);
    let shown = screen(&mut app);
    assert!(shown.contains("cat/alpha +2 "), "{shown}");
    assert!(shown.contains("STALE: new MQTT POP3"), "{shown}");
    let marked: Vec<&str> = ["A", "IMAP", "MQTT", "POP3"]
        .into_iter()
        .filter(|o| {
            shown
                .lines()
                .any(|l| l.contains("│N [") && l.contains(&format!("] {o} ")))
        })
        .collect();
    assert_eq!(marked, ["MQTT", "POP3"], "{shown}");
}

/// A STALE line too long for the pane wraps to a second, under its
/// text, and is cut with `…` past that.
#[test]
fn a_long_stale_banner_wraps_to_two_lines_then_is_cut() {
    let tmp = tempfile::tempdir().unwrap();
    alpha_file(tmp.path(), &["A"], &[]);
    let names: Vec<String> = (0..20).map(|i| format!("NEWOPTION{i:02}")).collect();
    let mut complete = vec!["A"];
    complete.extend(names.iter().map(String::as_str));
    let mut app = alpha_app(tmp.path(), &complete);
    let shown = screen_at(&mut app, 80, 24);
    let lines: Vec<&str> = shown.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains("STALE: "))
        .expect(&shown);
    let pane = |l: &str| l.split(['│', '║']).nth(3).unwrap_or_default().to_string();
    let first = pane(lines[at]);
    let second = pane(lines[at + 1]);
    let indent = first.find("STALE: ").unwrap() + "STALE: ".len();
    assert!(first[indent..].starts_with("new NEWOPTION00"), "{shown}");
    assert!(
        second.starts_with(&" ".repeat(indent)) && !second[indent..].starts_with(' '),
        "under its text:\n{shown}"
    );
    assert!(
        second.trim_end().ends_with('…'),
        "cut after two lines:\n{shown}"
    );
    assert!(
        !pane(lines[at + 2]).starts_with(&" ".repeat(indent)),
        "no third line:\n{shown}"
    );
}

/// Entering the pane puts the cursor on what needs attention: a
/// conflict's first member, else the first new option, else the first
/// option — never a header.
#[test]
fn entering_the_pane_lands_on_the_first_option_needing_attention() {
    use crate::model::options::{GroupKind, OptionGroup};
    // Stale: the first new option.
    let tmp = tempfile::tempdir().unwrap();
    alpha_file(tmp.path(), &["A"], &["A"]);
    let mut app = alpha_app(tmp.path(), &["A", "B", "C"]);
    hit(&mut app, KeyCode::Enter);
    assert_eq!(cursor(&app), "B");

    // Stale and conflicting: the conflict first, past the group header.
    let tmp = tempfile::tempdir().unwrap();
    alpha_file(tmp.path(), &["A"], &["A"]);
    let mut app = alpha_app(tmp.path(), &["A", "B", "C", "D"]);
    alpha_mut(&mut app).options.groups.push(OptionGroup {
        kind: GroupKind::Single,
        name: "G".into(),
        desc: String::new(),
        members: vec!["C".into(), "D".into()],
    });
    stage(&mut app, "cat_alpha", &["C", "D"]);
    app.rebuild_editor();
    for key in [KeyCode::Tab, KeyCode::Right] {
        hit(&mut app, key);
        assert_eq!(
            (cursor(&app), app.editor_idx),
            ("C".to_string(), 3),
            "{key:?}"
        );
        hit(&mut app, KeyCode::Esc);
    }

    // Nothing to attend to: the first option, the header passed by.
    let tmp = tempfile::tempdir().unwrap();
    let mut app = alpha_app(tmp.path(), &["X", "Y"]);
    alpha_mut(&mut app).options.groups.push(OptionGroup {
        kind: GroupKind::Radio,
        name: "R".into(),
        desc: String::new(),
        members: vec!["X".into(), "Y".into()],
    });
    app.rebuild_editor();
    hit(&mut app, KeyCode::Enter);
    assert_eq!((cursor(&app), app.editor_idx), ("X".to_string(), 1));
}

/// The first member of the first rule broken, in row order: a PREVENTS
/// pair's too.
#[test]
fn a_prevents_conflict_lands_on_its_first_member_in_row_order() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = alpha_app(tmp.path(), &["A", "B", "C", "D"]);
    alpha_mut(&mut app).options.defs.insert(
        "D".into(),
        crate::model::options::OptionDef {
            prevents: vec!["B".into()],
            ..Default::default()
        },
    );
    stage(&mut app, "cat_alpha", &["B", "D"]);
    app.rebuild_editor();
    hit(&mut app, KeyCode::Enter);
    assert_eq!(cursor(&app), "B");
}

/// n/p from the option pane land the same way on the port they reach,
/// and so do a jump from r and f.
#[test]
fn n_p_and_jumps_from_the_pane_land_on_what_needs_attention() {
    use crate::model::options::{GroupKind, OptionGroup};
    let tmp = tempfile::tempdir().unwrap();
    for (name, complete) in [("cat_beta", "DOCS"), ("cat_gamma", "DOCS X Y")] {
        let dir = tmp.path().join("options").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let mut text = format!("_OPTIONS_READ=x-1.0\n_FILE_COMPLETE_OPTIONS_LIST={complete}\n");
        for o in complete.split(' ') {
            text.push_str(&format!("OPTIONS_FILE_UNSET+={o}\n"));
        }
        std::fs::write(dir.join("options"), text).unwrap();
    }
    let mut app = app_for(
        tmp.path(),
        &[
            ("cat/alpha", &["cat/beta"]),
            ("cat/beta", &[]),
            ("cat/gamma", &[]),
        ],
    );
    let beta = app
        .session
        .ports
        .get_mut(&PortKey::parse("cat/beta").unwrap())
        .unwrap();
    beta.options.complete = vec!["DOCS".into(), "NEW".into()];
    let gamma = app
        .session
        .ports
        .get_mut(&PortKey::parse("cat/gamma").unwrap())
        .unwrap();
    gamma.options.complete = vec!["DOCS".into(), "X".into(), "Y".into()];
    gamma.options.groups.push(OptionGroup {
        kind: GroupKind::Single,
        name: "G".into(),
        desc: String::new(),
        members: vec!["X".into(), "Y".into()],
    });
    stage(&mut app, "cat_gamma", &["X", "Y"]);
    app.rebuild_visible(None);
    app.rebuild_editor();
    assert_eq!(visible(&app), ["cat/gamma", "cat/alpha", "cat/beta"]);
    hit(&mut app, KeyCode::Enter);
    assert_eq!(
        (selected(&app), cursor(&app)),
        ("cat/gamma".into(), "X".into())
    );
    press(&mut app, 'j');
    press(&mut app, 'n');
    assert_eq!(
        (selected(&app), cursor(&app)),
        ("cat/alpha".into(), "DOCS".into())
    );
    press(&mut app, 'n');
    assert_eq!(
        (selected(&app), cursor(&app)),
        ("cat/beta".into(), "NEW".into())
    );
    press(&mut app, 'p');
    press(&mut app, 'p');
    assert_eq!(
        (selected(&app), cursor(&app)),
        ("cat/gamma".into(), "X".into()),
        "not where it was left"
    );
    assert!(app.focus == Focus::Editor);

    // r on beta lists alpha among its dependents; Enter jumps there and
    // back, landing each time.
    press(&mut app, 'p');
    assert_eq!(
        (selected(&app), cursor(&app)),
        ("cat/beta".into(), "NEW".into())
    );
    press(&mut app, 'r');
    let at = app
        .why
        .as_ref()
        .unwrap()
        .entries()
        .iter()
        .position(|k| k.to_string() == "cat/alpha");
    app.why.as_mut().unwrap().selected = at.expect("alpha depends on beta");
    hit(&mut app, KeyCode::Enter);
    assert_eq!(
        (selected(&app), cursor(&app)),
        ("cat/alpha".into(), "DOCS".into())
    );
    assert!(app.focus == Focus::Editor);
}

/// The flavor jump keeps the option the cursor is on, to compare it
/// across flavors; when the other flavor lacks it, it lands as on
/// entering the pane: on what needs attention.
#[test]
fn f_from_the_pane_keeps_the_option_or_lands_on_what_needs_attention() {
    use crate::model::options::{GroupKind, OptionGroup};
    let tmp = tempfile::tempdir().unwrap();
    let mut app = app_for(tmp.path(), &[("cat/alpha@a", &[]), ("cat/alpha@b", &[])]);
    for k in ["cat/alpha@a", "cat/alpha@b"] {
        let info = app
            .session
            .ports
            .get_mut(&PortKey::parse(k).unwrap())
            .unwrap();
        info.flavors = vec!["a".into(), "b".into()];
        info.options_name = format!("cat_alpha_{}", &k[10..]);
        info.options.complete = vec!["DOCS".into(), "X".into(), "Y".into()];
    }
    let a = app
        .session
        .ports
        .get_mut(&PortKey::parse("cat/alpha@a").unwrap())
        .unwrap();
    a.options.complete.push("ONLY_A".into());
    let b = app
        .session
        .ports
        .get_mut(&PortKey::parse("cat/alpha@b").unwrap())
        .unwrap();
    b.options.groups.push(OptionGroup {
        kind: GroupKind::Radio,
        name: "G".into(),
        desc: String::new(),
        members: vec!["X".into(), "Y".into()],
    });
    let state = |staged: &[&str]| crate::session::OptState {
        saved: None,
        baseline: BTreeSet::new(),
        staged: staged.iter().map(ToString::to_string).collect(),
    };
    app.session.states.insert("cat_alpha_a".into(), state(&[]));
    app.session
        .states
        .insert("cat_alpha_b".into(), state(&["X", "Y"]));
    app.session
        .owners
        .insert("cat_alpha_a".into(), PortKey::parse("cat/alpha@a").unwrap());
    app.session
        .owners
        .insert("cat_alpha_b".into(), PortKey::parse("cat/alpha@b").unwrap());
    app.rebuild_visible(None);
    let a = app
        .visible
        .iter()
        .position(|k| k.to_string() == "cat/alpha@a")
        .unwrap();
    app.select_index(a);
    hit(&mut app, KeyCode::Enter);
    press(&mut app, 'G');
    assert_eq!(cursor(&app), "ONLY_A");
    press(&mut app, 'f');
    assert_eq!(
        (selected(&app), cursor(&app)),
        ("cat/alpha@b".into(), "X".into()),
        "b lacks ONLY_A"
    );
    press(&mut app, 'f');
    assert_eq!(
        (selected(&app), cursor(&app)),
        ("cat/alpha@a".into(), "X".into()),
        "back on a, X kept"
    );
    press(&mut app, 'j');
    assert_eq!(cursor(&app), "Y");
    press(&mut app, 'f');
    assert_eq!(
        (selected(&app), cursor(&app)),
        ("cat/alpha@b".into(), "Y".into()),
        "the same option"
    );
}

/// The CONFLICT banner names what make.conf did: the members it turns
/// on and the layer that does, a FORCE knob, the members to pick from
/// and those it turns off; the plain violations keep their text.
#[test]
fn a_conflict_names_its_makeconf_cause() {
    use crate::model::options::{GroupKind, OptionGroup};
    let setup = |file: bool| {
        let tmp = tempfile::tempdir().unwrap();
        // The file records A alone.
        if file {
            alpha_file(tmp.path(), &["A"], &["A"]);
        }
        let mut app = alpha_app(tmp.path(), &["A", "B", "C", "D"]);
        alpha_mut(&mut app).options.groups.push(OptionGroup {
            kind: GroupKind::Single,
            name: "G".into(),
            desc: String::new(),
            members: vec!["A".into(), "B".into(), "C".into()],
        });
        (tmp, app)
    };
    let causes = |app: &App| -> Vec<(String, Option<String>)> {
        let info = info_of(app, "cat/alpha");
        app.session
            .conflicts(&info)
            .into_iter()
            .map(|c| (c.text, c.cause))
            .collect()
    };
    let too_many = "group G needs exactly one member enabled (2 enabled)".to_string();

    // Too many on, through make.conf: each layer named.
    let (_tmp, mut app) = setup(false);
    let o = &mut alpha_mut(&mut app).options;
    o.mc_set = ["A".to_string()].into();
    o.port_set = ["B".to_string()].into();
    stage(&mut app, "cat_alpha", &["A", "B"]);
    assert_eq!(
        causes(&app),
        [(
            too_many.clone(),
            Some("A on by make.conf OPTIONS_SET; B on by make.conf cat_alpha_SET".into())
        )]
    );
    let shown = screen_at(&mut app, 200, 30);
    assert!(
        shown.contains(&format!(
            "CONFLICT: {too_many} — A on by make.conf OPTIONS_SET;"
        )),
        "{shown}"
    );
    let info = info_of(&app, "cat/alpha");
    assert_eq!(
        app.session.violations(&info),
        std::slice::from_ref(&too_many),
        "the violation's own text is unchanged"
    );

    // The file decides A: only B is make.conf's doing.
    let (_tmp, mut app) = setup(true);
    let o = &mut alpha_mut(&mut app).options;
    o.mc_set = ["A".to_string(), "B".to_string()].into();
    stage(&mut app, "cat_alpha", &["A", "B"]);
    assert_eq!(
        causes(&app),
        [(
            too_many.clone(),
            Some("B on by make.conf OPTIONS_SET".into())
        )]
    );

    // A FORCE knob.
    let (_tmp, mut app) = setup(true);
    alpha_mut(&mut app).options.port_force_set = ["C".to_string()].into();
    stage(&mut app, "cat_alpha", &["A", "C"]);
    assert_eq!(
        causes(&app),
        [(
            too_many.clone(),
            Some("C forced on by make.conf; fix it there".into())
        )]
    );

    // None on: the members to pick from, and those make.conf turns off.
    let none = "group G needs exactly one member enabled (0 enabled)".to_string();
    let (_tmp, mut app) = setup(false);
    let o = &mut alpha_mut(&mut app).options;
    o.mc_unset = ["A".to_string()].into();
    o.force_unset = ["C".to_string()].into();
    stage(&mut app, "cat_alpha", &[]);
    assert_eq!(
        causes(&app),
        [(
            none.clone(),
            Some("pick one of A B C (A, C off by make.conf)".into())
        )]
    );
    let (_tmp, mut app) = setup(false);
    stage(&mut app, "cat_alpha", &[]);
    assert_eq!(causes(&app), [(none, Some("pick one of A B C".into()))]);

    // PREVENTS: its text, and make.conf's hand when it has one.
    let (_tmp, mut app) = setup(false);
    alpha_mut(&mut app).options.defs.insert(
        "D".into(),
        crate::model::options::OptionDef {
            prevents: vec!["A".into()],
            ..Default::default()
        },
    );
    stage(&mut app, "cat_alpha", &["A", "D"]);
    let prevents = "D conflicts with A: mutually exclusive".to_string();
    assert_eq!(causes(&app), [(prevents.clone(), None)]);
    alpha_mut(&mut app).options.mc_set = ["D".to_string()].into();
    assert_eq!(
        causes(&app),
        [(prevents, Some("D on by make.conf OPTIONS_SET".into()))]
    );
}
