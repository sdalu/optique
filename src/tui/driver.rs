//! Headless driver for the TUI: a line protocol on stdin that presses keys,
//! renders into an in-memory terminal and dumps the screen or the app state.
//! It exists to debug and end-to-end test the interface without a tty — the
//! keymap, the app and the drawing code are the real ones, only the backend
//! and the input source differ.

use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    App, Focus, KeyOutcome, OverlayKind, build_app, dispatch_key, stop_background_makes, ui,
};
use crate::optfiles::OptionsFiles;
use crate::query::refresher::Refresher;
use crate::session::Session;
use crate::staging::StagingDb;

/// Default in-memory screen size: wide enough for both panes to render the
/// way an ordinary terminal does.
const DEFAULT_COLS: u16 = 100;
const DEFAULT_ROWS: u16 = 35;

/// Screen dimensions a `resize` may ask for.
const MIN_DIM: u16 = 20;
const MAX_DIM: u16 = 500;

/// How long `settle` waits by default for background refreshes to land.
const SETTLE_DEFAULT_MS: u64 = 5000;

/// Poll interval of the `settle` loop.
const SETTLE_TICK: Duration = Duration::from_millis(25);

type DriverTerminal = Terminal<TestBackend>;

pub fn run_driver(
    session: Session,
    files: OptionsFiles,
    staging_db: StagingDb,
    refresher: Refresher,
    blacklist: crate::config::Blacklist,
    overlays: Vec<crate::config::Overlay>,
    minimal: bool,
) -> Result<()> {
    let mut app = build_app(
        session, files, staging_db, refresher, blacklist, overlays, minimal,
    );
    let mut terminal = Terminal::new(TestBackend::new(DEFAULT_COLS, DEFAULT_ROWS))?;

    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut line = String::new();
    loop {
        line.clear();
        // EOF closes the session as cleanly as an explicit `quit`.
        if input.read_line(&mut line)? == 0 {
            break;
        }
        let cmd = line.trim_end_matches(['\n', '\r']);
        if cmd.trim().is_empty() || cmd.trim_start().starts_with('#') {
            continue;
        }
        // Stand in for the real loop's tick: background scan results merge
        // before every command sees the app.
        app.drain_refresh_events();
        app.flush_due_refreshes();
        let done = exec(&mut app, &mut terminal, cmd.trim_start(), &mut out)?;
        out.flush()?;
        if done {
            break;
        }
    }
    // Same courtesy as the real TUI: no orphaned makes after the session.
    stop_background_makes();
    Ok(())
}

/// Run one protocol line. Returns true when the session is over. A malformed
/// command is reported on stdout and never ends the session.
fn exec(
    app: &mut App,
    terminal: &mut DriverTerminal,
    cmd: &str,
    out: &mut impl Write,
) -> Result<bool> {
    // `keys` needs the argument verbatim, trailing spaces included (they are
    // keystrokes too; the line ending is already gone), the others want it
    // tidy. Only ASCII whitespace separates the verb: it is
    // one byte, so the argument starts on a char boundary.
    let (verb, raw) = match cmd.find(|c: char| c.is_ascii_whitespace()) {
        Some(i) => (&cmd[..i], &cmd[i + 1..]),
        None => (cmd, ""),
    };
    let arg = raw.trim();
    match verb {
        "key" => match parse_key_spec(arg) {
            Some(key) => {
                if press(app, terminal, key, out)? {
                    return Ok(true);
                }
                writeln!(out, "ok key {arg}")?;
            }
            None => writeln!(out, "err unknown key: {arg}")?,
        },
        "keys" => {
            if raw.is_empty() {
                writeln!(out, "err keys needs text to type")?;
            } else {
                for c in raw.chars() {
                    if press(
                        app,
                        terminal,
                        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                        out,
                    )? {
                        return Ok(true);
                    }
                }
                writeln!(out, "ok keys {}", raw.chars().count())?;
            }
        }
        "dump" => dump(app, terminal, out)?,
        "state" => writeln!(out, "{}", state_json(app))?,
        "settle" => settle(app, arg, out)?,
        "resize" => resize(terminal, arg, out)?,
        "quit" => {
            writeln!(out, "ok quit")?;
            return Ok(true);
        }
        other => writeln!(out, "err unknown command: {other}")?,
    }
    Ok(false)
}

/// Feed one key to the shared keymap. Returns true when it ended the session
/// (the acknowledgement is printed here, so the caller only has to stop).
/// A frame is drawn first, as the real loop draws one before every key: the
/// page keys move by what the last frame showed.
fn press(
    app: &mut App,
    terminal: &mut DriverTerminal,
    key: KeyEvent,
    out: &mut impl Write,
) -> Result<bool> {
    terminal.draw(|f| ui::draw(f, app))?;
    match dispatch_key(app, key) {
        // Ctrl-L only asks a real terminal for a repaint; nothing to clear here.
        KeyOutcome::Continue | KeyOutcome::Repaint => Ok(false),
        KeyOutcome::Quit => {
            writeln!(out, "ok quit")?;
            Ok(true)
        }
    }
}

/// Draw a frame and print the resulting screen, one line per row with
/// trailing blanks stripped.
fn dump(app: &mut App, terminal: &mut DriverTerminal, out: &mut impl Write) -> Result<()> {
    terminal.draw(|f| ui::draw(f, app))?;
    let buffer = terminal.backend().buffer();
    writeln!(out, "screen {}x{}", buffer.area.width, buffer.area.height)?;
    for row in ui::screen_rows(buffer) {
        writeln!(out, "{row}")?;
    }
    writeln!(out, "end")?;
    Ok(())
}

/// One-line JSON snapshot of everything a test wants to assert on without
/// reading the screen.
fn state_json(app: &App) -> String {
    let focus = match app.focus {
        Focus::List => "list",
        Focus::Editor => "editor",
        Focus::Filter => "filter",
    };
    // The overlay ui::draw puts on screen, so the one reported is the one
    // actually shown.
    let overlay = match app.top_overlay() {
        Some(OverlayKind::QuitConfirm) => "quit_confirm",
        Some(OverlayKind::Apply) => "apply",
        Some(OverlayKind::Bulk) => "bulk",
        Some(OverlayKind::Help(_)) => "help",
        Some(OverlayKind::PortHelp) => "port_help",
        Some(OverlayKind::Why) => "why",
        Some(OverlayKind::OptInfo) => "opt_info",
        None => "none",
    };
    serde_json::json!({
        "focus": focus,
        "selected": app.selected_key().map(|k| k.to_string()),
        "visible": app.visible.len(),
        "listable": app.listable,
        "refreshing": app.refreshing,
        "pending": app.pending.len(),
        "message": app.message.as_ref().map(|(m, _)| m.clone()),
        "overlay": overlay,
        "filter": app.filter,
        "dirty": app.session.dirty(),
        "loops": app.loops.len(),
    })
    .to_string()
}

/// Wait until no background refresh is outstanding and nothing is waiting on
/// the debounce. The repeated flush is what lets the 300ms debounce expire.
fn settle(app: &mut App, arg: &str, out: &mut impl Write) -> Result<()> {
    let timeout_ms = if arg.is_empty() {
        SETTLE_DEFAULT_MS
    } else {
        match arg.parse::<u64>() {
            Ok(ms) => ms,
            Err(_) => {
                return Ok(writeln!(
                    out,
                    "err settle timeout must be milliseconds: {arg}"
                )?);
            }
        }
    };
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        app.drain_refresh_events();
        app.flush_due_refreshes();
        if app.refreshing == 0 && app.pending.is_empty() {
            return Ok(writeln!(out, "ok settled")?);
        }
        if Instant::now() >= deadline {
            return Ok(writeln!(
                out,
                "err settle timeout (refreshing={} pending={})",
                app.refreshing,
                app.pending.len()
            )?);
        }
        std::thread::sleep(SETTLE_TICK);
    }
}

/// Recreate the in-memory terminal at `<W>x<H>`, clamped to something a
/// layout can be drawn into.
fn resize(terminal: &mut DriverTerminal, arg: &str, out: &mut impl Write) -> Result<()> {
    let dims = arg
        .split_once(['x', 'X'])
        .and_then(|(w, h)| Some((w.trim().parse::<u16>().ok()?, h.trim().parse::<u16>().ok()?)));
    let Some((w, h)) = dims else {
        return Ok(writeln!(out, "err resize wants <W>x<H>, got: {arg}")?);
    };
    let (w, h) = (w.clamp(MIN_DIM, MAX_DIM), h.clamp(MIN_DIM, MAX_DIM));
    *terminal = Terminal::new(TestBackend::new(w, h))?;
    writeln!(out, "ok resize {w}x{h}")?;
    Ok(())
}

/// Strip a case-insensitive modifier prefix, but only when a key still
/// follows it: a lone `ctrl-` modifies nothing.
fn strip_modifier<'a>(spec: &'a str, prefix: &str) -> Option<&'a str> {
    let head = spec.get(..prefix.len())?;
    (head.eq_ignore_ascii_case(prefix) && spec.len() > prefix.len()).then(|| &spec[prefix.len()..])
}

/// Parse a key spec: optional `ctrl-`/`alt-` prefixes, then a single
/// character (case significant) or a key name. Returns None for anything
/// unrecognized so the driver can report it instead of guessing.
fn parse_key_spec(spec: &str) -> Option<KeyEvent> {
    let mut rest = spec.trim();
    let mut mods = KeyModifiers::NONE;
    loop {
        if let Some(tail) = strip_modifier(rest, "ctrl-") {
            mods |= KeyModifiers::CONTROL;
            rest = tail;
        } else if let Some(tail) = strip_modifier(rest, "alt-") {
            mods |= KeyModifiers::ALT;
            rest = tail;
        } else {
            break;
        }
    }
    // A single character is itself, verbatim: 'B' is the bulk key, 'b' is not.
    let mut chars = rest.chars();
    let code = match (chars.next(), chars.next()) {
        (None, _) => return None,
        (Some(c), None) => KeyCode::Char(c),
        _ => match rest.to_ascii_lowercase().as_str() {
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" | "shift-tab" => KeyCode::BackTab,
            "space" => KeyCode::Char(' '),
            "backspace" | "bs" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "pgup" | "pageup" => KeyCode::PageUp,
            "pgdn" | "pagedown" => KeyCode::PageDown,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "question" => KeyCode::Char('?'),
            "slash" => KeyCode::Char('/'),
            name => {
                let n: u8 = name.strip_prefix('f')?.parse().ok()?;
                if !(1..=12).contains(&n) {
                    return None;
                }
                KeyCode::F(n)
            }
        },
    };
    Some(KeyEvent::new(code, mods))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(s: &str) -> (KeyCode, KeyModifiers) {
        let key = parse_key_spec(s).unwrap_or_else(|| panic!("{s:?} must parse"));
        (key.code, key.modifiers)
    }

    #[test]
    fn key_specs_cover_chars_names_and_modifiers() {
        // Single characters keep their case: 'B' and 'b' are different keys.
        assert_eq!(spec("j"), (KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(spec("B"), (KeyCode::Char('B'), KeyModifiers::NONE));
        assert_eq!(spec("?"), (KeyCode::Char('?'), KeyModifiers::NONE));
        assert_eq!(spec("/"), (KeyCode::Char('/'), KeyModifiers::NONE));
        assert_eq!(spec("é"), (KeyCode::Char('é'), KeyModifiers::NONE));

        assert_eq!(spec("ctrl-l"), (KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert_eq!(spec("CTRL-c"), (KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(spec("alt-x"), (KeyCode::Char('x'), KeyModifiers::ALT));
        assert_eq!(
            spec("ctrl-alt-enter"),
            (KeyCode::Enter, KeyModifiers::CONTROL | KeyModifiers::ALT)
        );

        assert_eq!(spec("enter"), (KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(spec("esc"), (KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(spec("tab"), (KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(spec("backtab"), (KeyCode::BackTab, KeyModifiers::NONE));
        assert_eq!(spec("space"), (KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(spec("pgdn"), (KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(spec("question"), (KeyCode::Char('?'), KeyModifiers::NONE));
        assert_eq!(spec("F1"), (KeyCode::F(1), KeyModifiers::NONE));
        assert_eq!(spec("f12"), (KeyCode::F(12), KeyModifiers::NONE));
    }

    /// Run protocol lines against a real App, returning what they printed.
    fn run(app: &mut App, lines: &[&str]) -> String {
        let mut terminal = Terminal::new(TestBackend::new(DEFAULT_COLS, DEFAULT_ROWS)).unwrap();
        let mut out = Vec::new();
        for line in lines {
            exec(app, &mut terminal, line, &mut out).unwrap();
        }
        String::from_utf8(out).unwrap()
    }

    /// A separator outside ASCII is no separator: the line is a malformed
    /// command, reported like any other instead of crashing the driver.
    #[test]
    fn a_non_ascii_space_after_the_verb_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = super::super::tests::app_for(tmp.path(), &[("cat/alpha", &[])]);
        let out = run(&mut app, &["key\u{a0}x", "keys\u{2003}ab"]);
        assert_eq!(
            out,
            "err unknown command: key\u{a0}x\nerr unknown command: keys\u{2003}ab\n"
        );
    }

    /// `keys` types every character, trailing spaces included.
    #[test]
    fn keys_presses_trailing_spaces_too() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = super::super::tests::app_for(tmp.path(), &[("cat/alpha", &[])]);
        let out = run(&mut app, &["key /", "keys ab  "]);
        assert_eq!(out, "ok key /\nok keys 4\n");
        assert_eq!(app.filter, "ab  ");
    }

    /// A page key moves by the rows the screen shows, a dump or not before
    /// it: the driver draws ahead of every key, as the real loop does.
    #[test]
    fn a_page_key_pages_without_a_dump_first() {
        let tmp = tempfile::tempdir().unwrap();
        let names: Vec<String> = (0..60).map(|i| format!("cat/p{i:02}")).collect();
        let graph: Vec<(&str, &[&str])> =
            names.iter().map(|n| (n.as_str(), &[] as &[&str])).collect();
        let mut app = super::super::tests::app_for(tmp.path(), &graph);
        run(&mut app, &["key pgdn"]);
        // 35 rows, less the status bar and the list's border, less one.
        assert_eq!(
            app.list_state.selected(),
            Some(DEFAULT_ROWS as usize - 1 - 2 - 1)
        );
    }

    #[test]
    fn unknown_key_specs_are_rejected() {
        for bad in [
            "",
            " ",
            "nosuchkey",
            "f0",
            "f13",
            "ctrl-",
            "alt-",
            "enterr",
            "0x41",
        ] {
            assert!(parse_key_spec(bad).is_none(), "{bad:?} must not parse");
        }
    }
}
