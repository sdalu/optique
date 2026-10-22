mod driver;
mod ui;

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;

pub use driver::run_driver;

use crate::apply::{self, PendingWrite};
use crate::deps::{DepLoop, EXTRA_SHOWN};
use crate::draft;
use crate::model::origin::PortKey;
use crate::optfiles::OptionsFiles;
use crate::optionsfile;
use crate::query::refresher::{Refresher, RefreshEvent};
use crate::session::{Session, UiStatus};
use crate::staging::StagingDb;

/// How long after the last toggle before the port is re-queried.
const REFRESH_DEBOUNCE: Duration = Duration::from_millis(300);

/// How many option-set snapshots the undo stack keeps (a session's worth of
/// mistakes; the oldest entries are dropped past this).
const UNDO_MAX: usize = 100;

#[derive(PartialEq)]
pub enum Focus {
    List,
    Editor,
    Filter,
}

/// One navigable line of the options editor.
pub enum EditorRow {
    GroupHeader(usize),
    /// Option name; selectable.
    Option(String),
    ObsoleteHeader,
    /// Option known only to the saved file; not selectable.
    Obsolete(String),
    ExcludedHeader,
    /// Option this flavor excludes but the file's owning (default) flavor
    /// still manages; informational, not selectable.
    Excluded(String),
}

pub struct ApplyModal {
    pub writes: Vec<PendingWrite>,
    /// Files removed instead of rewritten (minimal mode).
    pub removals: Vec<crate::clean::Removal>,
    pub warnings: Vec<String>,
    pub scroll: usize,
    pub conflicted: Vec<PortKey>,
    /// Result text once applied.
    pub done: Option<String>,
}

/// The dependency loop a port is caught in, as the "why" overlay shows it:
/// the cycle first, rotated to start at the port the overlay is about, then
/// the rest of the tangle. Every port listed is navigable.
pub struct WhyLoop {
    /// Ports on the shortest closed walk, in walk order.
    pub walk: Vec<PortKey>,
    /// Ports of the component the walk does not visit, capped for display.
    pub tangled: Vec<PortKey>,
    /// Tangled ports the cap left out.
    pub hidden: usize,
}

impl WhyLoop {
    /// Present `dl` from `key`'s point of view. The walk starts at `key` when
    /// it lies on the cycle, so the loop reads from where the user is
    /// standing rather than from whichever port sorted first.
    fn of(dl: &DepLoop, key: &PortKey) -> WhyLoop {
        let mut walk = dl.cycle.clone();
        if let Some(at) = walk.iter().position(|k| k == key) {
            walk.rotate_left(at);
        }
        let extra = dl.extra_members();
        WhyLoop {
            walk,
            tangled: extra.iter().take(EXTRA_SHOWN).map(|k| (*k).clone()).collect(),
            hidden: extra.len().saturating_sub(EXTRA_SHOWN),
        }
    }
}

/// "Why is this port here?" overlay content, computed when it is opened.
pub struct WhyInfo {
    pub key: PortKey,
    /// Shortest root → port dependency chain, None when unreachable.
    pub chain: Option<Vec<PortKey>>,
    /// The dependency loop it is caught in, when it is.
    pub dep_loop: Option<WhyLoop>,
    pub dependents: Vec<PortKey>,
    /// Cursor over the navigable entries (the chain, then the loop, then the
    /// shown dependents): Enter jumps to that port, 'r' re-opens why on it.
    pub selected: usize,
}

impl WhyInfo {
    /// Every navigable port, in cursor order. `ui::draw_why` numbers its rows
    /// in exactly this order, so the two must change together.
    pub fn entries(&self) -> Vec<&PortKey> {
        let mut out: Vec<&PortKey> = Vec::new();
        if let Some(chain) = &self.chain {
            out.extend(chain.iter());
        }
        if let Some(dl) = &self.dep_loop {
            out.extend(dl.walk.iter());
            out.extend(dl.tangled.iter());
        }
        out.extend(self.dependents.iter().take(crate::tui::ui::WHY_MAX_DEPENDENTS));
        out
    }
}

/// A port's own text — its pkg-help, or its description — read from disk
/// when the overlay is opened (never cached: it is a file the user may be
/// editing in another window).
pub struct PortHelp {
    /// What the overlay shows, for its title: "pkg-help" or "about".
    pub title: &'static str,
    pub key: PortKey,
    pub lines: Vec<String>,
    pub scroll: u16,
}

pub struct App {
    pub session: Session,
    pub options_dir: PathBuf,
    /// The options dir's files as the scan read them: newcomers merged from a
    /// background re-scan read theirs from here, and an apply forgets the
    /// ones it changed so only those are read again.
    pub options_files: OptionsFiles,
    /// --minimal: apply removes files that only repeat defaults + make.conf.
    pub minimal: bool,
    /// Ports poudriere won't build for this jail/tree/set. A display and
    /// attention concern only, so it lives here and not in the Session.
    pub blacklist: crate::config::Blacklist,
    pub visible: Vec<PortKey>,
    pub list_state: ListState,
    pub focus: Focus,
    pub filter: String,
    pub editor_rows: Vec<EditorRow>,
    pub editor_idx: usize,
    pub message: Option<(String, bool)>,
    pub modal: Option<ApplyModal>,
    pub quit_confirm: bool,
    /// Bulk-decision prompt: the text typed so far while it is open.
    pub bulk: Option<String>,
    /// Dependency loops in the current closure, recomputed after every
    /// background refresh: a toggle can create or break one by changing
    /// which dependencies a port pulls in.
    pub loops: Vec<DepLoop>,
    /// Ports hidden because they have no options (status-bar info).
    pub hidden: usize,
    /// When set, ports needing no attention (status ok) are not listed.
    pub hide_ok: bool,
    /// The option pane opens with the port's description when there is room.
    /// Off at start; 'I' turns it on and off.
    pub show_about: bool,
    /// pkg-descr text per port, read when the panel first shows that port:
    /// the loop redraws every 150 ms and the ports tree may be on NFS.
    pub about_text: HashMap<PortKey, String>,
    /// When set, only the ports caught in a dependency loop are listed.
    pub only_loops: bool,
    /// Problems-first ordering (true, default) or stable alphabetical order
    /// (false) — the latter keeps neighbors put while working down the list.
    pub sort_problems_first: bool,
    /// When set, stale ports whose added options are all decided by
    /// make.conf count as needing no attention.
    pub mc_relax: bool,
    /// When set, ports whose staged options contradict the global make.conf
    /// OPTIONS_SET/UNSET policy are flagged (≠) as needing attention.
    pub warn_mc: bool,
    /// Ports with options matching the filter, before hide_ok is applied.
    pub listable: usize,
    pub staging_db: StagingDb,
    pub refresher: Refresher,
    /// Help overlay visible?
    /// Help overlay: Some(active tab index) while open.
    pub help_tab: Option<usize>,
    /// Vertical scroll of the help tab's content (clamped at draw time).
    pub help_scroll: u16,
    /// Dependency-chain ("why") overlay, when open.
    pub why: Option<WhyInfo>,
    /// Option-detail overlay: (port, option name). The data is looked up at
    /// draw time so a background refresh keeps the popup current.
    pub opt_info: Option<(PortKey, String)>,
    /// pkg-help or description overlay of the selected port, when open.
    pub port_help: Option<PortHelp>,
    /// Undo stack of (OPTIONS_NAME, staged set before the change), oldest
    /// first, capped at UNDO_MAX. Every TUI-driven mutation of a staged set
    /// pushes one entry; 'U' pops and restores the top one.
    pub undo: Vec<(String, BTreeSet<String>)>,
    /// Ports awaiting a debounced background re-query.
    pub pending: HashMap<PortKey, Instant>,
    /// options_name -> staged set at the time of the last submitted refresh,
    /// plus the previous refresh's set for attribution diffs.
    pub refresh_snapshots: HashMap<String, (BTreeSet<String>, Option<BTreeSet<String>>)>,
    /// options_name -> the option changes blamed for the port having just
    /// turned IGNORE/BROKEN (e.g. `+HTTP_AUTH_KRB5 -GSSAPI_MIT`).
    pub blame: HashMap<String, String>,
    /// Outstanding background refresh batches.
    pub refreshing: usize,
    pub refresh_progress: Option<(usize, usize)>,
}

pub fn ensure_terminal() -> Result<()> {
    use std::io::IsTerminal as _;
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        anyhow::bail!("the TUI needs a real terminal; use `optique scan` or `optique sync` in scripts");
    }
    Ok(())
}

pub fn run(
    session: Session,
    files: OptionsFiles,
    staging_db: StagingDb,
    refresher: Refresher,
    blacklist: crate::config::Blacklist,
    minimal: bool,
) -> Result<()> {
    ensure_terminal()?;
    let mut app = build_app(session, files, staging_db, refresher, blacklist, minimal);
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    // Don't leave background queries churning after the UI is gone.
    let stopped = crate::query::makerunner::kill_active_makes();
    if stopped > 0 {
        eprintln!("optique: stopped {stopped} in-flight make process(es)");
    }
    result
}

/// Assemble the ready-to-draw App: initial state, adopted draft, first list
/// and editor build. Shared by the real TUI and the headless driver so both
/// start from exactly the same place.
pub(crate) fn build_app(
    session: Session,
    files: OptionsFiles,
    staging_db: StagingDb,
    refresher: Refresher,
    blacklist: crate::config::Blacklist,
    minimal: bool,
) -> App {
    let hidden = session.ports.values().filter(|p| !p.options.has_options()).count();
    let loops = session.dep_loops();
    let mut app = App {
        session,
        options_dir: files.dir().to_path_buf(),
        options_files: files,
        minimal,
        blacklist,
        visible: Vec::new(),
        list_state: ListState::default(),
        focus: Focus::List,
        filter: String::new(),
        editor_rows: Vec::new(),
        editor_idx: 0,
        message: None,
        modal: None,
        quit_confirm: false,
        bulk: None,
        loops,
        hidden,
        hide_ok: false,
        show_about: false,
        about_text: HashMap::new(),
        only_loops: false,
        sort_problems_first: true,
        mc_relax: false,
        warn_mc: false,
        listable: 0,
        staging_db,
        refresher,
        help_tab: None,
        help_scroll: 0,
        why: None,
        opt_info: None,
        port_help: None,
        undo: Vec::new(),
        pending: HashMap::new(),
        refresh_snapshots: HashMap::new(),
        blame: HashMap::new(),
        refreshing: 0,
        refresh_progress: None,
    };
    app.restore_draft_if_any();
    app.rebuild_visible(None);
    app.rebuild_editor();
    app
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        app.drain_refresh_events();
        app.flush_due_refreshes();
        terminal.draw(|f| ui::draw(f, app))?;
        if !event::poll(Duration::from_millis(150))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match dispatch_key(app, key) {
            KeyOutcome::Continue => {}
            KeyOutcome::Repaint => terminal.clear()?,
            KeyOutcome::Quit => return Ok(()),
        }
    }
}

/// What the caller of `dispatch_key` still has to do about a key.
pub(crate) enum KeyOutcome {
    /// The key was handled entirely by the state update.
    Continue,
    /// The screen needs a hard repaint (Ctrl-L); no state changed.
    Repaint,
    /// The session is over.
    Quit,
}

/// Apply one key press to the app. Everything terminal-related is left to the
/// caller through `KeyOutcome`, so the real event loop and the headless driver
/// share exactly one keymap.
fn dispatch_key(app: &mut App, key: KeyEvent) -> KeyOutcome {
    // Ctrl-L: full repaint to clear terminal artifacts — works in every
    // mode (modal, filter, help) without consuming any state.
    if key.code == KeyCode::Char('l') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return KeyOutcome::Repaint;
    }

    // Modal and quit-confirm grab all keys.
    if app.quit_confirm {
        match key.code {
            KeyCode::Char('s') | KeyCode::Char('S') => match draft::save(&app.session, &app.options_dir) {
                // No flash on success: the terminal is about to be
                // restored. The next launch announces the draft instead.
                Ok(_) => return KeyOutcome::Quit,
                Err(e) => {
                    app.quit_confirm = false;
                    app.flash(&format!("draft save failed: {e:#}"), true);
                }
            },
            // 'y' kept as an alias for the old confirm-and-quit muscle
            // memory. Drop any older draft too: quitting on purpose
            // without saving must not resurrect stale intentions.
            KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Char('y') | KeyCode::Char('Y') => {
                draft::discard(&app.options_dir);
                return KeyOutcome::Quit;
            }
            _ => app.quit_confirm = false,
        }
        return KeyOutcome::Continue;
    }
    if app.modal.is_some() {
        handle_modal_key(app, key.code);
        return KeyOutcome::Continue;
    }
    if let Some(tab) = app.help_tab {
        let n = ui::HELP_TABS.len();
        match key.code {
            KeyCode::Char(c @ '1'..='9') => {
                let idx = c as usize - '1' as usize;
                if idx < n {
                    app.help_tab = Some(idx);
                    app.help_scroll = 0;
                }
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                app.help_tab = Some((tab + 1) % n);
                app.help_scroll = 0;
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                app.help_tab = Some((tab + n - 1) % n);
                app.help_scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.help_scroll = app.help_scroll.saturating_add(1)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.help_scroll = app.help_scroll.saturating_sub(1)
            }
            KeyCode::PageDown => app.help_scroll = app.help_scroll.saturating_add(10),
            KeyCode::PageUp => app.help_scroll = app.help_scroll.saturating_sub(10),
            _ => {
                app.help_tab = None;
                app.help_scroll = 0;
            }
        }
        return KeyOutcome::Continue;
    }
    if let Some(help) = &mut app.port_help {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => help.scroll = help.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => help.scroll = help.scroll.saturating_sub(1),
            KeyCode::PageDown => help.scroll = help.scroll.saturating_add(10),
            KeyCode::PageUp => help.scroll = help.scroll.saturating_sub(10),
            KeyCode::Home | KeyCode::Char('g') => help.scroll = 0,
            // Clamped to the real content at draw time.
            KeyCode::End | KeyCode::Char('G') => help.scroll = u16::MAX,
            _ => app.port_help = None,
        }
        return KeyOutcome::Continue;
    }
    if let Some(why) = &mut app.why {
        let count = why.entries().len();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') if count > 0 => {
                why.selected = (why.selected + 1).min(count - 1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                why.selected = why.selected.saturating_sub(1);
            }
            KeyCode::Enter | KeyCode::Char('l') => {
                let target = why.entries().get(why.selected).cloned().cloned();
                app.why = None;
                if let Some(target) = target {
                    app.jump_to_port(&target);
                }
            }
            KeyCode::Char('r') => {
                // Walk the graph: re-open why on the highlighted port. Around
                // a loop this steps from port to port along the cycle.
                if let Some(target) = why.entries().get(why.selected).cloned().cloned() {
                    app.why = Some(app.why_for(target));
                }
            }
            _ => app.why = None,
        }
        return KeyOutcome::Continue;
    }
    if app.opt_info.is_some() {
        app.opt_info = None;
        return KeyOutcome::Continue;
    }
    // Ctrl-C always quits (with confirm if dirty) — checked before the
    // filter branch so it can't be swallowed as a literal 'c'.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if app.session.dirty() {
            app.quit_confirm = true;
            return KeyOutcome::Continue;
        }
        // Nothing staged: a draft restored and then reverted by hand has
        // nothing left to keep and must not come back on the next launch.
        draft::discard(&app.options_dir);
        return KeyOutcome::Quit;
    }
    // The bulk prompt takes text input, so it grabs the keys before the
    // filter branch and the plain command keys.
    if app.bulk.is_some() {
        match key.code {
            KeyCode::Esc => app.bulk = None,
            KeyCode::Enter => {
                if let Some(input) = app.bulk.take() {
                    app.run_bulk(&input);
                }
            }
            KeyCode::Backspace => {
                if let Some(input) = app.bulk.as_mut() {
                    input.pop();
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(input) = app.bulk.as_mut() {
                    input.push(c);
                }
            }
            _ => {}
        }
        return KeyOutcome::Continue;
    }
    if app.focus == Focus::Filter {
        match key.code {
            KeyCode::Esc => {
                app.filter.clear();
                app.focus = Focus::List;
                app.rebuild_visible(app.selected_key());
            }
            KeyCode::Enter => app.focus = Focus::List,
            KeyCode::Backspace => {
                app.filter.pop();
                app.rebuild_visible(None);
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.filter.push(c);
                app.rebuild_visible(None);
            }
            _ => {}
        }
        app.rebuild_editor();
        return KeyOutcome::Continue;
    }

    match key.code {
        KeyCode::Char('q') => {
            if app.session.dirty() {
                app.quit_confirm = true;
            } else {
                // As for Ctrl-C: nothing staged leaves no draft behind.
                draft::discard(&app.options_dir);
                return KeyOutcome::Quit;
            }
        }
        KeyCode::Char('/') => app.focus = Focus::Filter,
        KeyCode::Char('?') | KeyCode::F(1) => {
            app.help_tab = Some(0);
            app.help_scroll = 0;
        }
        KeyCode::Char('a') => app.open_apply_modal(),
        KeyCode::Char('B') => app.open_bulk(),
        // Shift-U undoes the last option change anywhere; lowercase 'u'
        // stays the per-port revert-to-saved in the editor.
        KeyCode::Char('U') => app.undo_last(),
        KeyCode::Char('t') => {
            app.hide_ok = !app.hide_ok;
            let keep = app.selected_key();
            app.rebuild_visible(keep);
            app.rebuild_editor();
            app.flash(
                if app.hide_ok {
                    "showing only ports needing attention (t to show all)"
                } else {
                    "showing all ports"
                },
                false,
            );
        }
        KeyCode::Char('I') => {
            app.show_about = !app.show_about;
            app.flash(
                if app.show_about {
                    "port description shown above the options when there is room"
                } else {
                    "port description hidden (I to show, i on the list for all of it)"
                },
                false,
            );
        }
        KeyCode::Char('m') => {
            app.mc_relax = !app.mc_relax;
            let keep = app.selected_key();
            app.rebuild_visible(keep);
            app.rebuild_editor();
            app.flash(
                if app.mc_relax {
                    "make.conf-decided staleness counts as ok (≈, m to undo)"
                } else {
                    "make.conf-decided staleness counts as stale again"
                },
                false,
            );
        }
        KeyCode::Char('L') => {
            // Turning the view off must always work, even once the loop it
            // was showing is gone; turning it on with nothing to show would
            // only empty the list.
            if !app.only_loops && app.loops.is_empty() {
                app.flash("no dependency loop in this closure", false);
            } else {
                app.only_loops = !app.only_loops;
                let keep = app.selected_key();
                app.rebuild_visible(keep);
                app.rebuild_editor();
                app.flash(
                    if app.only_loops {
                        "showing only ports caught in a dependency loop (L to show all)"
                    } else {
                        "showing all ports"
                    },
                    false,
                );
            }
        }
        KeyCode::Char('s') => {
            app.sort_problems_first = !app.sort_problems_first;
            let keep = app.selected_key();
            app.rebuild_visible(keep);
            app.rebuild_editor();
            app.flash(
                if app.sort_problems_first {
                    "sorting problems first"
                } else {
                    "alphabetical order (stable while editing)"
                },
                false,
            );
        }
        KeyCode::Char('w') => {
            app.warn_mc = !app.warn_mc;
            let keep = app.selected_key();
            app.rebuild_visible(keep);
            app.rebuild_editor();
            app.flash(
                if app.warn_mc {
                    "flagging options that contradict make.conf OPTIONS_SET/UNSET (≠, w to undo)"
                } else {
                    "make.conf contradictions no longer flagged"
                },
                false,
            );
        }
        _ => match app.focus {
            Focus::List => handle_list_key(app, key.code),
            Focus::Editor => handle_editor_key(app, key.code),
            Focus::Filter => {}
        },
    }
    KeyOutcome::Continue
}

fn handle_list_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('j') | KeyCode::Down => app.move_selection(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_selection(-1),
        KeyCode::Char('g') | KeyCode::Home => app.select_index(0),
        KeyCode::Char('G') | KeyCode::End => {
            app.select_index(app.visible.len().saturating_sub(1))
        }
        KeyCode::PageDown => app.move_selection(15),
        KeyCode::PageUp => app.move_selection(-15),
        KeyCode::Char('n') => app.jump_problem(1),
        KeyCode::Char('p') => app.jump_problem(-1),
        KeyCode::Char('f') => app.jump_flavor(),
        KeyCode::Char('r') => app.open_why(),
        // Same meaning as in the option pane, whose title carries the marker.
        KeyCode::Char('h') => app.open_port_help(),
        // The port as a whole; in the option pane 'i' is the option's.
        KeyCode::Char('i') => app.open_port_about(),
        KeyCode::Enter | KeyCode::Char('l') | KeyCode::Tab | KeyCode::Right => {
            if !app.editor_rows.is_empty() {
                app.focus = Focus::Editor;
                app.editor_select_first();
            }
        }
        _ => {}
    }
}

fn handle_editor_key(app: &mut App, code: KeyCode) {
    match code {
        // 'h' is the port's own help here, not vi-left: leaving the pane is
        // Esc/Tab/←, which every other overlay uses too.
        KeyCode::Esc | KeyCode::Tab | KeyCode::Left => app.focus = Focus::List,
        KeyCode::Char('h') => app.open_port_help(),
        KeyCode::Char('j') | KeyCode::Down => app.editor_move(1),
        KeyCode::Char('k') | KeyCode::Up => app.editor_move(-1),
        KeyCode::Char(' ') | KeyCode::Enter => app.toggle_current(),
        KeyCode::Char('f') => app.jump_flavor(),
        KeyCode::Char('r') => app.open_why(),
        KeyCode::Char('i') => app.open_opt_info(),
        KeyCode::Char('d') => {
            if let Some(key) = app.selected_key() {
                app.push_undo(&key);
                app.session.reset_to_defaults(&key);
                app.mark_pending(&key);
                app.flash("reset to port defaults", false);
            }
        }
        KeyCode::Char('u') => {
            if let Some(key) = app.selected_key() {
                app.push_undo(&key);
                app.session.revert(&key);
                app.mark_pending(&key);
                app.flash("reverted to saved state", false);
            }
        }
        _ => {}
    }
}

fn handle_modal_key(app: &mut App, code: KeyCode) {
    let showing_result = match &app.modal {
        Some(m) => m.done.is_some(),
        None => return,
    };
    if showing_result {
        app.modal = None;
        // Applied files changed statuses; refresh the list (matters with hide_ok).
        let keep = app.selected_key();
        app.rebuild_visible(keep);
        app.rebuild_editor();
        return;
    }
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            // Recompute the plan at confirmation time: a background refresh
            // may have merged new ports since the modal was opened.
            let planned = app.compute_writes();
            let writes = planned.writes;
            let summary = apply::apply(&writes);
            // apply is all-or-nothing: after a failed write nothing is on
            // disk, so nothing is removed either, and the edits stay staged
            // (and in the draft) to be applied again.
            if !summary.failed.is_empty() {
                let mut text = format!("nothing written to {}", app.options_dir.display());
                for (key, msg) in &summary.failed {
                    text.push_str(&format!("\nFAILED {key}: {msg}"));
                }
                if let Some(modal) = app.modal.as_mut() {
                    modal.writes = writes;
                    modal.done = Some(text);
                }
                return;
            }
            let mut removed = 0usize;
            let mut text =
                format!("{} file(s) written to {}", summary.written, app.options_dir.display());
            for r in &planned.removals {
                match crate::clean::remove_entry(r) {
                    Ok(_) => removed += 1,
                    Err(e) => text.push_str(&format!("\nFAILED rm {}: {e}", r.options_name)),
                }
            }
            if removed > 0 {
                text.push_str(&format!("\n{removed} redundant file(s) removed (--minimal)"));
            }
            // Only the files apply touched are read again; the rest are as
            // the scan read them.
            let touched: Vec<String> = writes
                .iter()
                .filter_map(|w| w.path.parent()?.file_name()?.to_str().map(str::to_string))
                .chain(planned.removals.iter().map(|r| r.options_name.clone()))
                .collect();
            app.options_files.forget(touched.iter().map(String::as_str));
            app.session.reload_saved(&app.options_files);
            // The files on disk changed: attributions built on the pre-apply
            // staged history would now point at the wrong changes.
            app.blame.clear();
            // The edits are on disk now; the draft has served its purpose.
            draft::discard(&app.options_dir);
            if let Some(modal) = app.modal.as_mut() {
                modal.writes = writes;
                modal.done = Some(text);
            }
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if let Some(modal) = app.modal.as_mut() {
                modal.scroll = modal.scroll.saturating_add(1);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if let Some(modal) = app.modal.as_mut() {
                modal.scroll = modal.scroll.saturating_sub(1);
            }
        }
        _ => app.modal = None,
    }
}

impl App {
    pub fn selected_key(&self) -> Option<PortKey> {
        self.list_state.selected().and_then(|i| self.visible.get(i)).cloned()
    }

    /// Is this port blacklisted for the current jail/tree/set?
    pub fn is_blacklisted(&self, info: &crate::model::port::PortInfo) -> bool {
        self.blacklist.matches(&info.key.origin)
    }

    /// The dependency loop this port is caught in, if any.
    pub fn loop_of(&self, key: &PortKey) -> Option<&DepLoop> {
        self.loops.iter().find(|dl| dl.contains(key))
    }

    /// Status with the mc_relax / warn_mc view rules applied.
    pub fn effective_status(&self, info: &crate::model::port::PortInfo) -> UiStatus {
        // Blacklisted here means never built here, so nothing about it can
        // demand a decision: n/p, t and the ≠ view all skip it.
        if self.is_blacklisted(info) {
            return UiStatus::Ok;
        }
        let mut status = self.session.status(info);
        if self.mc_relax
            && matches!(status, UiStatus::Stale | UiStatus::Unconfigured)
            && self.session.covered_by_makeconf(info)
        {
            status = UiStatus::Ok;
        }
        if self.warn_mc
            && status == UiStatus::Ok
            && !self.session.mc_deviations(info).is_empty()
        {
            status = UiStatus::McDeviation;
        }
        status
    }

    fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let cur = self.list_state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, self.visible.len() as isize - 1) as usize;
        self.select_index(next);
    }

    fn select_index(&mut self, idx: usize) {
        if self.visible.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(idx.min(self.visible.len() - 1)));
        }
        self.rebuild_editor();
    }

    fn jump_problem(&mut self, dir: isize) {
        if self.visible.is_empty() {
            return;
        }
        let len = self.visible.len() as isize;
        let start = self.list_state.selected().unwrap_or(0) as isize;
        let mut i = start;
        for _ in 0..len {
            i = (i + dir).rem_euclid(len);
            let info = &self.session.ports[&self.visible[i as usize]];
            if self.effective_status(info) != UiStatus::Ok {
                self.select_index(i as usize);
                return;
            }
        }
        self.flash("no ports need attention", false);
    }

    /// Rebuild the filtered, problems-first port list.
    pub fn rebuild_visible(&mut self, keep: Option<PortKey>) {
        let filter = self.filter.to_lowercase();
        let mut items: Vec<(UiStatus, PortKey)> = self
            .session
            .ports
            .iter()
            .filter(|(_, info)| info.options.has_options())
            .filter(|(key, info)| {
                filter.is_empty()
                    || key.to_string().to_lowercase().contains(&filter)
                    || info.pkgname.to_lowercase().contains(&filter)
            })
            .filter(|(key, _)| !self.only_loops || self.loop_of(key).is_some())
            .map(|(key, info)| (self.effective_status(info), key.clone()))
            .collect();
        if self.sort_problems_first {
            items.sort();
        } else {
            items.sort_by(|a, b| a.1.cmp(&b.1));
        }
        self.listable = items.len();
        if self.hide_ok {
            items.retain(|(status, _)| *status != UiStatus::Ok);
        }
        self.visible = items.into_iter().map(|(_, k)| k).collect();
        let idx = keep
            .and_then(|k| self.visible.iter().position(|v| *v == k))
            .unwrap_or(0);
        if self.visible.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(idx));
        }
    }

    /// Rebuild the editor rows for the selected port: top-level options in
    /// COMPLETE order, then each group, then obsolete saved-only options.
    pub fn rebuild_editor(&mut self) {
        let previous = match self.editor_rows.get(self.editor_idx) {
            Some(EditorRow::Option(name)) => Some(name.clone()),
            _ => None,
        };
        self.editor_rows.clear();
        self.editor_idx = 0;
        let Some(key) = self.selected_key() else { return };
        let info = &self.session.ports[&key];
        let opts = &info.options;
        let grouped: BTreeSet<&str> = opts
            .groups
            .iter()
            .flat_map(|g| g.members.iter().map(String::as_str))
            .collect();
        for opt in &opts.complete {
            if !grouped.contains(opt.as_str()) {
                self.editor_rows.push(EditorRow::Option(opt.clone()));
            }
        }
        for (gi, g) in opts.groups.iter().enumerate() {
            let members: Vec<&String> =
                g.members.iter().filter(|m| opts.complete.contains(*m)).collect();
            if members.is_empty() {
                continue;
            }
            self.editor_rows.push(EditorRow::GroupHeader(gi));
            for m in members {
                self.editor_rows.push(EditorRow::Option(m.clone()));
            }
        }
        let owner_complete: Vec<String> =
            self.session.owner_info(info).options.complete.clone();
        if let Some(state) = self.session.state(info) {
            if let Some(saved) = &state.saved {
                let file_known: BTreeSet<String> = saved
                    .complete
                    .iter()
                    .chain(saved.set.iter())
                    .chain(saved.unset.iter())
                    .filter(|o| !opts.complete.contains(*o))
                    .cloned()
                    .collect();
                // Known to the owning (default) flavor: excluded here, not obsolete.
                let (excluded, obsolete): (Vec<String>, Vec<String>) = file_known
                    .into_iter()
                    .partition(|o| owner_complete.contains(o));
                if !excluded.is_empty() {
                    self.editor_rows.push(EditorRow::ExcludedHeader);
                    for o in excluded {
                        self.editor_rows.push(EditorRow::Excluded(o));
                    }
                }
                if !obsolete.is_empty() {
                    self.editor_rows.push(EditorRow::ObsoleteHeader);
                    for o in obsolete {
                        self.editor_rows.push(EditorRow::Obsolete(o));
                    }
                }
            }
        }
        // Restore the cursor onto the same option if it still exists.
        if let Some(prev) = previous {
            if let Some(i) = self
                .editor_rows
                .iter()
                .position(|r| matches!(r, EditorRow::Option(o) if *o == prev))
            {
                self.editor_idx = i;
                return;
            }
        }
        self.editor_select_first();
    }

    fn editor_select_first(&mut self) {
        self.editor_idx = self
            .editor_rows
            .iter()
            .position(|r| matches!(r, EditorRow::Option(_)))
            .unwrap_or(0);
    }

    fn editor_move(&mut self, dir: isize) {
        if self.editor_rows.is_empty() {
            return;
        }
        let len = self.editor_rows.len() as isize;
        let mut i = self.editor_idx as isize;
        loop {
            i += dir;
            if i < 0 || i >= len {
                return; // stay put at the edges
            }
            if matches!(self.editor_rows[i as usize], EditorRow::Option(_)) {
                self.editor_idx = i as usize;
                return;
            }
        }
    }

    fn toggle_current(&mut self) {
        let Some(key) = self.selected_key() else { return };
        let Some(EditorRow::Option(opt)) = self.editor_rows.get(self.editor_idx) else {
            return;
        };
        let opt = opt.clone();
        // Warn when enabling an option flagged broken/ignored.
        let warn = {
            let info = &self.session.ports[&key];
            let state = self.session.state(info);
            let turning_on = state.map(|s| !s.staged.contains(&opt)).unwrap_or(false);
            if turning_on {
                info.options.defs.get(&opt).and_then(|d| {
                    d.broken
                        .as_ref()
                        .map(|m| format!("warning: {opt} marks this port BROKEN: {m}"))
                        .or_else(|| {
                            d.ignore
                                .as_ref()
                                .map(|m| format!("warning: {opt} marks this port IGNORED: {m}"))
                        })
                })
            } else {
                None
            }
        };
        self.push_undo(&key);
        match self.session.toggle(&key, &opt) {
            Ok(()) => {
                self.mark_pending(&key);
                match warn {
                    Some(w) => self.flash(&w, true),
                    None => self.message = None,
                }
            }
            Err(e) => {
                // A refused toggle changed nothing: drop the snapshot instead
                // of leaving a no-op entry on the stack.
                self.undo.pop();
                self.flash(&e, true);
            }
        }
    }

    /// Move the selection to the next flavor of the selected port's origin
    /// present in the closure, wrapping around in sorted order.
    fn jump_flavor(&mut self) {
        let Some(key) = self.selected_key() else { return };
        let siblings = self.session.siblings(&key);
        if siblings.len() < 2 {
            let msg = format!("no other flavors of {} in the closure", key.origin);
            self.flash(&msg, false);
            return;
        }
        let cur = siblings.iter().position(|k| *k == key).unwrap_or(0);
        let next = siblings[(cur + 1) % siblings.len()].clone();
        match self.visible.iter().position(|v| *v == next) {
            Some(idx) => {
                self.select_index(idx);
                self.flash(&format!("flavor {next}"), false);
            }
            None => {
                let msg = format!("flavor {next} is hidden by the current view");
                self.flash(&msg, false);
            }
        }
    }

    /// Assemble the "why" overlay for one port: its dependency chain, the
    /// loop it is caught in, and its dependents. A snapshot — a background
    /// refresh may change the closure while it is displayed.
    fn why_for(&self, key: PortKey) -> WhyInfo {
        WhyInfo {
            chain: self.session.why_chain(&key),
            dep_loop: self.loop_of(&key).map(|dl| WhyLoop::of(dl, &key)),
            dependents: self.session.dependents(&key),
            key,
            selected: 0,
        }
    }

    /// Open the "why" overlay on the selected port.
    fn open_why(&mut self) {
        let Some(key) = self.selected_key() else { return };
        self.why = Some(self.why_for(key));
    }

    /// Move the list selection to the given port. A view that hides the
    /// target (filter, L, t, optionless ports never listed) is widened as far
    /// as needed — the user asked to go there.
    fn jump_to_port(&mut self, target: &PortKey) {
        if !self.session.ports.contains_key(target) {
            self.flash(&format!("{target}: not in the closure"), true);
            return;
        }
        if let Some(idx) = self.visible.iter().position(|v| v == target) {
            self.select_index(idx);
            self.flash(&format!("jumped to {target}"), false);
            return;
        }
        let mut widened = Vec::new();
        if !self.filter.is_empty() {
            self.filter.clear();
            widened.push("filter cleared");
            self.rebuild_visible(Some(target.clone()));
        }
        if self.only_loops && !self.visible.contains(target) {
            self.only_loops = false;
            widened.push("loop view left");
            self.rebuild_visible(Some(target.clone()));
        }
        if self.hide_ok && !self.visible.contains(target) {
            self.hide_ok = false;
            widened.push("showing ok ports");
            self.rebuild_visible(Some(target.clone()));
        }
        if self.visible.contains(target) {
            self.rebuild_editor();
            self.flash(&format!("jumped to {target} ({})", widened.join(", ")), false);
        } else {
            // Only optionless ports remain unlistable.
            self.flash(&format!("{target} has no options and is not listed"), true);
        }
    }

    /// Open the detail overlay on the option under the editor cursor. Only
    /// the port and option name are stored; everything shown is looked up at
    /// draw time so the popup follows background refreshes.
    fn open_opt_info(&mut self) {
        let Some(key) = self.selected_key() else { return };
        let Some(EditorRow::Option(opt)) = self.editor_rows.get(self.editor_idx) else {
            return;
        };
        self.opt_info = Some((key, opt.clone()));
    }

    /// Open the selected port's pkg-help. Read here rather than at query time
    /// so the overlay always shows what is on disk now; only the fact that the
    /// file exists comes from the query.
    fn open_port_help(&mut self) {
        let Some(key) = self.selected_key() else { return };
        let Some(path) = self.session.ports[&key].pkg_help.clone() else {
            self.flash("this port ships no pkg-help", false);
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let lines = text.lines().map(str::to_string).collect();
                self.port_help = Some(PortHelp { title: "pkg-help", key, lines, scroll: 0 });
            }
            Err(e) => self.flash(&format!("cannot read {path}: {e}"), true),
        }
    }

    /// Read the selected port's pkg-descr for the panel, once per port.
    pub(crate) fn load_about_text(&mut self) {
        if !self.show_about {
            return;
        }
        let Some(key) = self.selected_key() else { return };
        if self.about_text.contains_key(&key) {
            return;
        }
        let text = self.session.ports[&key]
            .about
            .pkg_descr
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        self.about_text.insert(key, text);
    }

    /// Open the selected port's description: its COMMENT and WWW, then its
    /// pkg-descr as written. A port whose pkg-descr cannot be read still
    /// shows the rest.
    fn open_port_about(&mut self) {
        let Some(key) = self.selected_key() else { return };
        let about = &self.session.ports[&key].about;
        let mut lines = vec![about.comment.clone()];
        for url in about.www.split_whitespace() {
            lines.push(format!("WWW: {url}"));
        }
        if let Some(path) = &about.pkg_descr {
            match std::fs::read_to_string(path) {
                Ok(text) => {
                    lines.push(String::new());
                    lines.extend(text.lines().map(str::to_string));
                }
                Err(e) => self.flash(&format!("cannot read {path}: {e}"), true),
            }
        }
        self.port_help = Some(PortHelp { title: "about", key, lines, scroll: 0 });
    }

    /// Open the bulk-decision prompt, pre-filled with the option under the
    /// editor cursor when the editor has one selected.
    fn open_bulk(&mut self) {
        let prefill = match (&self.focus, self.editor_rows.get(self.editor_idx)) {
            (Focus::Editor, Some(EditorRow::Option(opt))) => format!("{opt}="),
            _ => String::new(),
        };
        self.bulk = Some(prefill);
    }

    /// Apply a typed bulk decision to every visible port carrying the option.
    fn run_bulk(&mut self, input: &str) {
        let (opt, on) = match parse_bulk(input) {
            Ok(parsed) => parsed,
            Err(e) => {
                self.flash(&e, true);
                return;
            }
        };
        let keys = self.visible.clone();
        // Snapshot every state the bulk could touch before it runs; only the
        // ports it really changed then get an undo entry, in the order they
        // were changed.
        let before: HashMap<String, BTreeSet<String>> = keys
            .iter()
            .filter_map(|k| self.session.ports.get(k))
            .filter_map(|info| {
                let state = self.session.states.get(&info.options_name)?;
                Some((info.options_name.clone(), state.staged.clone()))
            })
            .collect();
        let (changed, skipped) = self.session.bulk_set(&keys, &opt, on);
        for key in &changed {
            if let Some(name) = self.session.ports.get(key).map(|i| i.options_name.clone()) {
                if let Some(staged) = before.get(&name).cloned() {
                    self.push_undo_entry(name, staged);
                }
            }
            self.mark_pending(key);
        }
        let mut msg = format!(
            "{opt}={}: {} port(s) changed, {} skipped",
            if on { "on" } else { "off" },
            changed.len(),
            skipped.len()
        );
        if let Some((key, reason)) = skipped.first() {
            msg.push_str(&format!(" (first: {key}: {reason})"));
        }
        self.flash(&msg, changed.is_empty());
        let keep = self.selected_key();
        self.rebuild_visible(keep);
        self.rebuild_editor();
    }

    /// Adopt the draft saved by a previous session on this options dir, if
    /// any. Restored ports are marked pending so the background re-query
    /// picks up the dependencies their staged options pull in.
    fn restore_draft_if_any(&mut self) {
        let Some(draft) = draft::load(&self.options_dir) else { return };
        // Pre-images of every state the draft names, so 'U' can walk back the
        // restore one port at a time.
        let before: Vec<(String, BTreeSet<String>)> = draft
            .staged
            .keys()
            .filter_map(|name| {
                let state = self.session.states.get(name)?;
                Some((name.clone(), state.staged.clone()))
            })
            .collect();
        let restored = self.session.restore_draft(&draft);
        for (name, staged) in before {
            // Only the states the restore actually moved.
            if self.session.states.get(&name).map(|s| s.staged != staged).unwrap_or(false) {
                self.push_undo_entry(name, staged);
            }
        }
        if restored == 0 {
            // Nothing in it still applies (ports gone, or the edits already
            // landed) — a stale draft would only nag on every launch.
            draft::discard(&self.options_dir);
            return;
        }
        let owners: Vec<PortKey> =
            draft.staged.keys().filter_map(|name| self.session.owners.get(name).cloned()).collect();
        for key in owners {
            self.mark_pending(&key);
        }
        self.flash(
            &format!(
                "restored draft with {restored} staged port(s) from {} — 'u' per port reverts, draft cleared on apply",
                draft::age_label(draft.age_secs())
            ),
            false,
        );
    }

    /// Record `staged` as the state of `options_name` before a change, for 'U'.
    fn push_undo_entry(&mut self, options_name: String, staged: BTreeSet<String>) {
        push_capped(&mut self.undo, (options_name, staged));
    }

    /// Snapshot the current staged set of the state a port edits (its
    /// OPTIONS_NAME, shared by the flavors using the same file). Call this
    /// *before* mutating it.
    fn push_undo(&mut self, key: &PortKey) {
        let Some(name) = self.session.ports.get(key).map(|i| i.options_name.clone()) else {
            return;
        };
        let Some(staged) = self.session.states.get(&name).map(|s| s.staged.clone()) else {
            return;
        };
        self.push_undo_entry(name, staged);
    }

    /// Restore the staged set recorded by the most recent change, whatever
    /// produced it (toggle, d, u, bulk, draft restore).
    fn undo_last(&mut self) {
        let Some((options_name, staged)) = self.undo.pop() else {
            self.flash("nothing to undo", false);
            return;
        };
        let Some(state) = self.session.states.get_mut(&options_name) else {
            // The state left the session (its port dropped out of the closure
            // on a refresh); the entry is meaningless now.
            self.flash(&format!("{options_name} is no longer in the closure"), true);
            return;
        };
        state.staged = staged;
        // The options changed back, so the dependencies they pull in did too:
        // re-query through the port owning the file, like the restore flow.
        let owner = self.session.owners.get(&options_name).cloned().or_else(|| {
            self.session
                .ports
                .iter()
                .find(|(_, info)| info.options_name == options_name)
                .map(|(key, _)| key.clone())
        });
        if let Some(key) = owner {
            self.mark_pending(&key);
        }
        self.flash(&format!("undid change to {options_name}"), false);
        let keep = self.selected_key();
        self.rebuild_visible(keep);
        self.rebuild_editor();
    }

    /// Schedule the port for a debounced background re-query.
    fn mark_pending(&mut self, key: &PortKey) {
        self.pending.insert(key.clone(), Instant::now());
    }

    /// Push due pending ports to the refresher: write their staged options
    /// file into the staging db, then re-scan from them.
    ///
    /// Nothing is written while a re-scan is outstanding: it may still be
    /// reading the staging copy, and its cache entries pair the bytes it saw
    /// at lookup with what make computed, so a file rewritten under it would
    /// file one option value's result under the other's bytes. Due ports wait
    /// and go out together once it reports done.
    fn flush_due_refreshes(&mut self) {
        if self.pending.is_empty() || self.refreshing > 0 {
            return;
        }
        let now = Instant::now();
        let due: Vec<PortKey> = self
            .pending
            .iter()
            .filter(|(_, t)| now.duration_since(**t) >= REFRESH_DEBOUNCE)
            .map(|(k, _)| k.clone())
            .collect();
        if due.is_empty() {
            return;
        }
        let mut batch = Vec::new();
        for key in due {
            self.pending.remove(&key);
            let Some(info) = self.session.ports.get(&key) else { continue };
            let Some(state) = self.session.state(info) else { continue };
            let options_name = info.options_name.clone();
            let staged = state.staged.clone();
            let content = optionsfile::render(&info.pkgname, &info.options.complete, &staged);
            if let Err(e) = self.staging_db.write(&options_name, content.as_bytes()) {
                self.flash(&format!("staging write failed: {e:#}"), true);
                continue;
            }
            self.record_snapshot(options_name, staged);
            batch.push(key);
        }
        if !batch.is_empty() && self.refresher.tx.send(batch).is_ok() {
            self.refreshing += 1;
        }
    }

    /// Remember the staged set a refresh was submitted with, keeping the
    /// previous one for attribution. A resubmission of an unchanged set (the
    /// other flavors of an origin share the file, and a revert can land on the
    /// same set) keeps the recorded history instead of flattening it.
    fn record_snapshot(&mut self, options_name: String, staged: BTreeSet<String>) {
        match self.refresh_snapshots.get_mut(&options_name) {
            Some((current, previous)) => {
                if *current != staged {
                    *previous = Some(std::mem::replace(current, staged));
                }
            }
            None => {
                self.refresh_snapshots.insert(options_name, (staged, None));
            }
        }
    }

    /// The option changes to blame when a port turns IGNORE/BROKEN: the diff
    /// between the last two staged sets submitted for its options file, or
    /// staged-vs-baseline when only one refresh is on record.
    fn blame_diff(&self, info: &crate::model::port::PortInfo) -> String {
        if let Some((current, Some(previous))) = self.refresh_snapshots.get(&info.options_name) {
            return crate::session::staged_diff(previous, current);
        }
        match self.session.state(info) {
            Some(state) => crate::session::staged_diff(&state.baseline, &state.staged),
            None => String::new(),
        }
    }

    /// Attribute freshly discovered IGNORE/BROKEN states to recent option
    /// changes, comparing the incoming scan against the still-unmerged
    /// session. Returns the flash text for the last port that just turned
    /// bad, if any; ports that came back clean drop their attribution.
    fn attribute_blame(
        &mut self,
        fresh: &std::collections::BTreeMap<PortKey, crate::model::port::PortInfo>,
    ) -> Option<String> {
        use crate::session::is_blocked;
        let mut flash = None;
        for (key, info) in fresh {
            let was = self.session.ports.get(key).map(is_blocked).unwrap_or(false);
            match (was, is_blocked(info)) {
                (true, false) => {
                    self.blame.remove(&info.options_name);
                }
                (false, true) => {
                    let diff = self.blame_diff(info);
                    if diff.is_empty() {
                        continue; // nothing recent to point at
                    }
                    let (label, reason) = match (&info.broken, &info.ignore) {
                        (Some(m), _) => ("BROKEN", m.as_str()),
                        (None, Some(m)) => ("IGNORE", m.as_str()),
                        (None, None) => continue,
                    };
                    flash = Some(format!(
                        "{key}: now {label} ({}) — {diff}",
                        trim_reason(reason)
                    ));
                    self.blame.insert(info.options_name.clone(), diff);
                }
                _ => {}
            }
        }
        flash
    }

    /// Merge finished background scans into the session.
    fn drain_refresh_events(&mut self) {
        let mut merged = None;
        // Set once a dep error or a port that just broke is flashed: the
        // refresh count must not take its place on the line.
        let mut alerted = false;
        while let Ok(ev) = self.refresher.rx.try_recv() {
            match ev {
                RefreshEvent::Progress { done, discovered } => {
                    self.refresh_progress = Some((done, discovered));
                }
                RefreshEvent::Done { result, batches } => {
                    self.refreshing = self.refreshing.saturating_sub(batches);
                    let errors = result.errors.len();
                    let first_error = result.errors.first().cloned();
                    // Blame before the merge: it needs the pre-merge flags.
                    let blamed = self.attribute_blame(&result.ports);
                    let (added, removed) =
                        self.session.merge(*result, &self.options_files);
                    let (a, r) = merged.get_or_insert((0usize, 0usize));
                    *a += added;
                    *r += removed;
                    if let Some((key, msg)) = first_error {
                        self.flash(
                            &format!("refresh: {errors} dep error(s), e.g. {key}: {msg}"),
                            true,
                        );
                        alerted = true;
                    }
                    // A port that just broke outranks dep errors on the line.
                    if let Some(msg) = blamed {
                        self.flash(&msg, true);
                        alerted = true;
                    }
                }
            }
        }
        if self.refreshing == 0 {
            self.refresh_progress = None;
        }
        if let Some((added, removed)) = merged {
            if (added > 0 || removed > 0) && !alerted {
                self.flash(
                    &format!("dependencies refreshed: +{added} −{removed} port(s)"),
                    false,
                );
            }
            // Dep edges may have changed without any port appearing or
            // vanishing, so the loops are recomputed on every merge — and a
            // loop that just appeared outranks the refresh line. Before the
            // rebuild: the L view lists the loops as they are now, and is
            // left when the last one went.
            self.refresh_loops(alerted);
            let keep = self.selected_key();
            self.rebuild_visible(keep);
            self.rebuild_editor();
        }
    }

    /// Plan the options files to write for the current staged state.
    fn compute_writes(&self) -> apply::PlannedWrites {
        let staged = self.session.ports.iter().filter_map(|(key, info)| {
            let state = self.session.state(info)?;
            Some((key, info, state.staged.clone()))
        });
        apply::plan_writes(staged, &self.options_files, self.minimal)
    }

    fn open_apply_modal(&mut self) {
        // Every port, not just the listed ones: apply writes them all, and a
        // conflict the filter or a view hides is written all the same.
        let conflicted: Vec<PortKey> = self
            .session
            .ports
            .iter()
            .filter(|(_, info)| self.session.status(info) == UiStatus::Conflict)
            .map(|(key, _)| key.clone())
            .collect();
        let planned = self.compute_writes();
        if planned.writes.is_empty() && planned.removals.is_empty() {
            self.flash("nothing to write — everything is up to date", false);
        } else {
            self.modal = Some(ApplyModal {
                writes: planned.writes,
                removals: planned.removals,
                warnings: planned.warnings,
                scroll: 0,
                conflicted,
                done: None,
            });
        }
    }

    /// Recompute the closure's dependency loops, announcing one that just
    /// appeared (or the moment the last one goes away).
    /// `alerted`: an alert already holds the line; a loop gone is news, not
    /// an alert, and leaves it there (a loop that appeared outranks it).
    fn refresh_loops(&mut self, alerted: bool) {
        let before: Vec<Vec<PortKey>> = self.loops.iter().map(|dl| dl.members.clone()).collect();
        self.loops = self.session.dep_loops();
        match self.loops.iter().find(|dl| !before.contains(&dl.members)) {
            Some(fresh) => {
                let msg = format!("dependency loop: {}", fresh.render());
                self.flash(&msg, true);
            }
            None if self.loops.is_empty() && !before.is_empty() => {
                // The L view would now show an empty list, which says nothing
                // about why: leave it rather than strand the user in it.
                let msg = if self.only_loops {
                    self.only_loops = false;
                    "dependency loop gone; showing all ports again"
                } else {
                    "dependency loop gone"
                };
                if !alerted {
                    self.flash(msg, false);
                }
            }
            None => {}
        }
    }

    fn flash(&mut self, msg: &str, error: bool) {
        self.message = Some((msg.to_string(), error));
    }
}

/// Squeeze a BROKEN/IGNORE message onto one status-bar line: whitespace
/// collapsed, elided past ~80 characters.
fn trim_reason(msg: &str) -> String {
    const MAX: usize = 80;
    let msg: String = msg.split_whitespace().collect::<Vec<&str>>().join(" ");
    match msg.char_indices().nth(MAX) {
        None => msg,
        Some((cut, _)) => format!("{}…", &msg[..cut]),
    }
}

/// Push onto the undo stack, dropping the oldest entry once it is full (a
/// Vec shift at 100 entries costs nothing and keeps the newest undo on top).
fn push_capped(stack: &mut Vec<(String, BTreeSet<String>)>, entry: (String, BTreeSet<String>)) {
    if stack.len() >= UNDO_MAX {
        stack.remove(0);
    }
    stack.push(entry);
}

/// Parse a bulk decision: `NAME=on`/`NAME=off` (value case-insensitive), or the
/// shorthands `NAME+` (on) and `NAME-` (off). The option name is uppercased.
fn parse_bulk(input: &str) -> Result<(String, bool), String> {
    const SYNTAX: &str = "expected OPTION=on|off (or OPTION+ / OPTION-)";
    let input = input.trim();
    let (name, on) = if let Some((name, value)) = input.split_once('=') {
        match value.trim().to_lowercase().as_str() {
            "on" => (name, true),
            "off" => (name, false),
            other => return Err(format!("{other:?} is not on or off — {SYNTAX}")),
        }
    } else if let Some(name) = input.strip_suffix('+') {
        (name, true)
    } else if let Some(name) = input.strip_suffix('-') {
        (name, false)
    } else {
        return Err(SYNTAX.to_string());
    };
    let name = name.trim().to_uppercase();
    if name.is_empty() {
        return Err(format!("no option name — {SYNTAX}"));
    }
    Ok((name, on))
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let port = |origin: &str, deps: &[&str]| {
            let key = PortKey::parse(origin).expect("test origin parses");
            PortInfo {
                key: key.clone(),
                canonical: key,
                pkgname: format!("{}-1.0", origin.split('/').next_back().unwrap()),
                flavors: vec![],
                options_name: origin.replace('/', "_"),
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
                broken: None,
                ignore: None,
                deprecated: None,
                pkg_help: None,
                about: Default::default(),
                default_versions: vec![],
                warnings: vec![],
            }
        };
        let mut ports = std::collections::BTreeMap::new();
        for (origin, deps) in graph {
            let info = port(origin, deps);
            ports.insert(info.canonical.clone(), info);
        }
        let roots: Vec<PortKey> = ports.keys().cloned().collect();
        let files = OptionsFiles::new(&tmp.join("options"));
        let session = Session::new(ports, HashMap::new(), &roots, &files, false);
        let db = StagingDb::create(tmp, &files, session.states.keys())
            .expect("staging db under a tempdir");
        let refresher = crate::query::refresher::spawn(
            crate::query::makerunner::QueryCtx {
                portsdir: tmp.to_path_buf(),
                make_conf: None,
                port_dbdir: db.path().to_path_buf(),
                overlays: Vec::new(),
                inherit_env_overlays: false,
            },
            1,
            crate::cache::Cache::disabled(),
            crate::moved::Moved::parse(""),
        );
        build_app(session, files, db, refresher, Default::default(), false)
    }

    /// However many options a port has, turning the panel on never changes
    /// which of them the pane shows: it is sized from the rows they need.
    #[test]
    fn description_panel_never_hides_an_option() {
        let visible = |shown: &str, n: usize| -> Vec<usize> {
            (1..=n).filter(|i| shown.contains(&format!("OPT{i:02}"))).collect()
        };
        let mut panel_seen = false;
        for n in 16..=32 {
            let tmp = tempfile::tempdir().unwrap();
            let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
            let key = PortKey::parse("cat/alpha").unwrap();
            let info = app.session.ports.get_mut(&key).unwrap();
            info.options.complete = (1..=n).map(|i| format!("OPT{i:02}")).collect();
            info.about.comment = "The alpha port".to_string();
            app.about_text.insert(key.clone(), "word ".repeat(400));
            app.rebuild_editor();
            let without = visible(&screen(&mut app), n);
            dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('I'), KeyModifiers::SHIFT));
            let shown = screen(&mut app);
            assert_eq!(visible(&shown, n), without, "{n} options:\n{shown}");
            panel_seen |= shown.contains("The alpha port");
        }
        assert!(panel_seen, "the panel had room at least once");
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
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('I'), KeyModifiers::SHIFT));
        assert!(screen(&mut app).contains("Alpha does things."));
        std::fs::remove_file(&descr).unwrap();
        assert!(screen(&mut app).contains("Alpha does things."), "read again at redraw");
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

        app.pending.insert(PortKey::parse("cat/alpha").unwrap(), due);
        app.flush_due_refreshes();
        assert!(staged("cat_alpha").is_file());
        assert_eq!(app.refreshing, 1, "the sent batch is outstanding");

        app.pending.insert(PortKey::parse("cat/beta").unwrap(), due);
        app.flush_due_refreshes();
        assert!(!staged("cat_beta").exists(), "written under a running re-scan");

        let deadline = Instant::now() + std::time::Duration::from_secs(10);
        while app.refreshing > 0 {
            assert!(Instant::now() < deadline, "the re-scan never reported done");
            app.drain_refresh_events();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        app.flush_due_refreshes();
        assert!(staged("cat_beta").is_file());
        assert_eq!(app.refreshing, 1);
    }

    fn press(app: &mut App, c: char) -> KeyOutcome {
        let mods = if c.is_ascii_uppercase() { KeyModifiers::SHIFT } else { KeyModifiers::NONE };
        dispatch_key(app, KeyEvent::new(KeyCode::Char(c), mods))
    }

    fn hit(app: &mut App, code: KeyCode) -> KeyOutcome {
        dispatch_key(app, KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl(app: &mut App, c: char) -> KeyOutcome {
        dispatch_key(app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn selected(app: &App) -> String {
        app.selected_key().map(|k| k.to_string()).unwrap_or_default()
    }

    fn visible(app: &App) -> Vec<String> {
        app.visible.iter().map(|k| k.to_string()).collect()
    }

    fn staged(app: &App, origin: &str) -> Vec<String> {
        let info = &app.session.ports[&PortKey::parse(origin).unwrap()];
        app.session.state(info).unwrap().staged.iter().cloned().collect()
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
        app_for(tmp, &[("cat/alpha", &[]), ("cat/beta", &[]), ("cat/gamma", &[])])
    }

    #[test]
    fn quitting_asks_only_when_there_is_something_to_lose() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = three_ports(tmp.path());
        assert!(matches!(ctrl(&mut app, 'l'), KeyOutcome::Repaint));
        assert!(matches!(press(&mut app, 'q'), KeyOutcome::Quit), "nothing staged");
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
        assert!(matches!(press(&mut app, 's'), KeyOutcome::Quit));
        assert!(draft::load(&app.options_dir).is_some(), "s saved a draft");

        // That session ended there; the same one, asked again.
        app.quit_confirm = false;
        press(&mut app, 'q');
        assert!(matches!(press(&mut app, 'd'), KeyOutcome::Quit));
        assert!(draft::load(&app.options_dir).is_none(), "d discards it");
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
        press(&mut app, 'l');
        assert_eq!(app.help_tab, Some(0), "and forwards");
        press(&mut app, '2');
        assert_eq!(app.help_tab, Some(1));
        press(&mut app, '9');
        assert_eq!(app.help_tab, Some(1), "no ninth tab");
        press(&mut app, 'j');
        hit(&mut app, KeyCode::PageDown);
        assert_eq!(app.help_scroll, 11);
        press(&mut app, 'k');
        hit(&mut app, KeyCode::PageUp);
        assert_eq!(app.help_scroll, 0);
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
        assert!(app.message.as_ref().is_some_and(|(m, _)| m.contains("no pkg-help")));

        let help = tmp.path().join("pkg-help");
        std::fs::write(&help, "first line\nsecond line\n").unwrap();
        let key = app.selected_key().unwrap();
        app.session.ports.get_mut(&key).unwrap().pkg_help = Some(help.display().to_string());
        press(&mut app, 'h');
        let h = app.port_help.as_ref().expect("opened");
        assert_eq!((h.title, h.lines.len()), ("pkg-help", 2));
        assert!(screen(&mut app).contains("second line"));
        press(&mut app, 'j');
        hit(&mut app, KeyCode::PageDown);
        assert_eq!(app.port_help.as_ref().unwrap().scroll, 11);
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
        ctrl(&mut app, 'u');
        assert_eq!(app.bulk.as_deref(), Some("DOCS=on"), "no control characters");
        hit(&mut app, KeyCode::Enter);
        assert!(app.bulk.is_none());
        for p in ["cat/alpha", "cat/beta", "cat/gamma"] {
            assert_eq!(staged(&app, p), ["DOCS"], "{p}");
        }
        hit(&mut app, KeyCode::Enter);
        press(&mut app, 'B');
        assert_eq!(app.bulk.as_deref(), Some("DOCS="), "pre-filled from the option pane");
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
        std::fs::write(beta.join("options"), "_FILE_COMPLETE_OPTIONS_LIST=DOCS\nOPTIONS_FILE_UNSET+=DOCS\n")
            .unwrap();
        let mut app = app_for(tmp.path(), &[("cat/beta", &[])]);
        press(&mut app, 'n');
        assert_eq!(selected(&app), "cat/beta");
        assert!(app.message.as_ref().is_some_and(|(m, _)| m == "no ports need attention"));
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
        assert!(app.focus == Focus::Editor);
        press(&mut app, ' ');
        assert_eq!(staged(&app, "cat/alpha"), ["DOCS"]);
        press(&mut app, 'j');
        hit(&mut app, KeyCode::Enter);
        assert_eq!(staged(&app, "cat/alpha"), ["DOCS", "NLS"]);
        press(&mut app, 'j');
        press(&mut app, ' ');
        assert_eq!(staged(&app, "cat/alpha"), ["DOCS"], "j stops at the last option");
        press(&mut app, 'k');
        press(&mut app, 'k');
        press(&mut app, ' ');
        assert!(staged(&app, "cat/alpha").is_empty(), "k stops at the first");
        press(&mut app, 'U');
        assert_eq!(staged(&app, "cat/alpha"), ["DOCS"]);
        press(&mut app, 'd');
        assert_eq!(staged(&app, "cat/alpha"), ["NLS"], "d: the port's defaults");
        press(&mut app, 'u');
        assert!(staged(&app, "cat/alpha").is_empty(), "u: the saved state (none)");
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
        press(&mut app, 'U');
        assert!(app.message.as_ref().is_some_and(|(m, _)| m == "nothing to undo"));
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
        assert_eq!(visible(&app), ["cat/alpha", "cat/beta", "cat/gamma"], "alphabetical");
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
        let mut app = app_for(tmp.path(), &[("cat/alpha@a", &[]), ("cat/alpha@b", &[]), ("cat/beta", &[])]);
        for k in ["cat/alpha@a", "cat/alpha@b"] {
            app.session.ports.get_mut(&PortKey::parse(k).unwrap()).unwrap().flavors =
                vec!["a".into(), "b".into()];
        }
        app.select_index(0);
        assert_eq!(selected(&app), "cat/alpha@a");
        press(&mut app, 'f');
        assert_eq!(selected(&app), "cat/alpha@b");
        press(&mut app, 'f');
        assert_eq!(selected(&app), "cat/alpha@a", "wraps");
        press(&mut app, 'G');
        press(&mut app, 'f');
        assert!(app.message.as_ref().is_some_and(|(m, _)| m.contains("no other flavors")));
        press(&mut app, '/');
        for c in "@a".chars() {
            press(&mut app, c);
        }
        hit(&mut app, KeyCode::Enter);
        press(&mut app, 'f');
        assert!(app.message.as_ref().is_some_and(|(m, _)| m.contains("hidden by the current view")));
    }

    /// Replace the app's refresher with channels the test drives: what a
    /// background scan would send arrives exactly when the test says.
    fn fake_refresher(app: &mut App) -> (crossbeam_channel::Sender<RefreshEvent>, crossbeam_channel::Receiver<Vec<PortKey>>) {
        let (req_tx, req_rx) = crossbeam_channel::unbounded();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
        app.refresher = Refresher { tx: req_tx, rx: ev_rx };
        (ev_tx, req_rx)
    }

    fn done(ports: Vec<crate::model::port::PortInfo>, errors: Vec<(PortKey, String)>) -> RefreshEvent {
        let ports = ports.into_iter().map(|i| (i.canonical.clone(), i)).collect();
        RefreshEvent::Done {
            result: Box::new(crate::query::scanner::ScanResult { ports, errors, ..Default::default() }),
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
        let mut app = app_for(tmp.path(), &[("cat/alpha", &["cat/beta"]), ("cat/beta", &[])]);
        app.session.roots = vec![PortKey::parse("cat/alpha").unwrap()];
        let (ev, _req) = fake_refresher(&mut app);
        let mut alpha = info_of(&app, "cat/alpha");
        let mut gamma = info_of(&app, "cat/beta");
        gamma.key = PortKey::parse("cat/gamma").unwrap();
        gamma.canonical = gamma.key.clone();
        gamma.options_name = "cat_gamma".into();
        alpha.deps[0].target = gamma.key.clone();
        app.refreshing = 1;
        ev.send(done(vec![alpha, gamma], vec![])).unwrap();
        app.drain_refresh_events();
        assert_eq!(app.refreshing, 0);
        assert_eq!(visible(&app), ["cat/alpha", "cat/gamma"]);
        assert!(app.session.state(&info_of(&app, "cat/gamma")).is_some(), "the newcomer got a state");
        let (msg, err) = app.message.clone().unwrap();
        assert_eq!((msg.as_str(), err), ("dependencies refreshed: +1 −1 port(s)", false));

        // A merge that only drops is reported too; one that changes nothing
        // says nothing.
        let mut alpha = info_of(&app, "cat/alpha");
        alpha.deps.clear();
        ev.send(done(vec![alpha.clone()], vec![])).unwrap();
        app.drain_refresh_events();
        assert_eq!(app.message.clone().unwrap().0, "dependencies refreshed: +0 −1 port(s)");
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
        ev.send(done(vec![with_beta, beta], vec![])).unwrap();
        app.drain_refresh_events();
        assert_eq!(app.message.clone().unwrap().0, "dependencies refreshed: +1 −0 port(s)");
        alpha = info_of(&app, "cat/alpha");
        app.message = None;
        ev.send(done(vec![alpha], vec![])).unwrap();
        app.drain_refresh_events();
        assert!(app.message.is_none(), "nothing changed: {:?}", app.message);
    }

    /// Progress shows while a scan runs and goes with it; a dependency that
    /// failed to query is reported with the first error.
    #[test]
    fn a_rescan_reports_progress_and_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
        let (ev, _req) = fake_refresher(&mut app);
        app.refreshing = 1;
        ev.send(RefreshEvent::Progress { done: 3, discovered: 5 }).unwrap();
        app.drain_refresh_events();
        assert_eq!(app.refresh_progress, Some((3, 5)));
        assert!(app.message.is_none(), "nothing merged yet");
        let failed = PortKey::parse("cat/broken").unwrap();
        ev.send(done(vec![info_of(&app, "cat/alpha")], vec![(failed, "no such port".into())]))
            .unwrap();
        app.drain_refresh_events();
        assert_eq!(app.refresh_progress, None);
        let (msg, err) = app.message.clone().unwrap();
        assert_eq!(msg, "refresh: 1 dep error(s), e.g. cat/broken: no such port");
        assert!(err);
    }

    /// A port that turns IGNORE/BROKEN after an option change is blamed on
    /// that change; one with no recent change is not; recovering clears it.
    #[test]
    fn a_port_that_breaks_is_blamed_on_the_last_change() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_for(tmp.path(), &[("cat/alpha", &[]), ("cat/beta", &[])]);
        let (ev, _req) = fake_refresher(&mut app);
        app.refresh_snapshots
            .insert("cat_alpha".into(), (["DOCS".to_string()].into(), Some(BTreeSet::new())));
        let mut alpha = info_of(&app, "cat/alpha");
        alpha.ignore = Some(format!("needs   {}", "x".repeat(100)));
        let mut beta = info_of(&app, "cat/beta");
        beta.broken = Some("fails anyway".into());
        ev.send(done(vec![beta.clone(), alpha.clone()], vec![])).unwrap();
        app.drain_refresh_events();
        let (msg, err) = app.message.clone().unwrap();
        assert!(err);
        assert!(msg.starts_with("cat/alpha: now IGNORE (needs x"), "{msg}");
        assert!(msg.ends_with("…) — +DOCS"), "reason cut at 80 characters: {msg}");
        assert_eq!(app.blame.get("cat_alpha").map(String::as_str), Some("+DOCS"));
        assert!(!app.blame.contains_key("cat_beta"), "no recent change to blame");

        // BROKEN names itself first when both are set.
        alpha.broken = Some("short".into());
        app.blame.clear();
        app.session.ports.get_mut(&alpha.key).unwrap().ignore = None;
        ev.send(done(vec![alpha.clone()], vec![])).unwrap();
        app.drain_refresh_events();
        assert!(app.message.clone().unwrap().0.starts_with("cat/alpha: now BROKEN (short)"));

        alpha.broken = None;
        alpha.ignore = None;
        ev.send(done(vec![alpha], vec![])).unwrap();
        app.drain_refresh_events();
        assert!(!app.blame.contains_key("cat_alpha"), "a port that recovers is cleared");
    }

    /// A port that just broke outranks the refresh count on the line, even
    /// when the same re-scan also added a dependency.
    #[test]
    fn a_rescan_that_breaks_a_port_and_adds_a_dep_keeps_the_blame() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
        let (ev, _req) = fake_refresher(&mut app);
        app.refresh_snapshots
            .insert("cat_alpha".into(), (["DOCS".to_string()].into(), Some(BTreeSet::new())));
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
        assert!(app.session.ports.contains_key(&PortKey::parse("cat/gamma").unwrap()), "added");
        let (msg, err) = app.message.clone().unwrap();
        assert_eq!((msg.as_str(), err), ("cat/alpha: now IGNORE (needs gamma) — +DOCS", true));

        // A dep error outranks it too.
        let failed = PortKey::parse("cat/broken").unwrap();
        let mut alpha = info_of(&app, "cat/alpha");
        alpha.deps.clear();
        ev.send(done(vec![alpha], vec![(failed, "no such port".into())])).unwrap();
        app.drain_refresh_events();
        let (msg, err) = app.message.clone().unwrap();
        assert_eq!((msg.as_str(), err), ("refresh: 1 dep error(s), e.g. cat/broken: no such port", true));
    }

    /// "dependency loop gone" is news, not an alert: posted by the same
    /// rescan that broke a port, it leaves the blame on the line.
    #[test]
    fn a_loop_gone_in_the_rescan_that_broke_a_port_keeps_the_blame() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_for(tmp.path(), &[("cat/alpha", &["cat/beta"]), ("cat/beta", &["cat/alpha"])]);
        let (ev, _req) = fake_refresher(&mut app);
        press(&mut app, 'L');
        app.refresh_snapshots
            .insert("cat_beta".into(), (["DOCS".to_string()].into(), Some(BTreeSet::new())));
        let mut beta = info_of(&app, "cat/beta");
        beta.deps.clear();
        beta.ignore = Some("needs alpha".into());
        ev.send(done(vec![beta], vec![])).unwrap();
        app.drain_refresh_events();
        assert!(app.loops.is_empty() && !app.only_loops, "the L view is still left");
        let (msg, err) = app.message.clone().unwrap();
        assert_eq!((msg.as_str(), err), ("cat/beta: now IGNORE (needs alpha) — +DOCS", true));
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
            let info = app.session.ports.get_mut(&PortKey::parse(p).unwrap()).unwrap();
            info.options.mc_set = ["DOCS".to_string()].into();
        }
        let status = |app: &App, p: &str| app.effective_status(&info_of(app, p));
        // alpha: unconfigured, but make.conf decides DOCS.
        assert_eq!(status(&app, "cat/alpha"), UiStatus::Unconfigured);
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
        assert_eq!(visible(&app), ["cat/gamma"], "without ≠, only gamma owes a decision");
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
        assert!(!tmp.path().join("options/cat_alpha").exists(), "closing writes nothing");
        press(&mut app, 't');
        press(&mut app, 'a');
        press(&mut app, 'y');
        assert!(app.modal.as_ref().unwrap().done.is_some());
        assert_eq!(visible(&app), ["cat/alpha", "cat/gamma"], "not yet rebuilt");
        press(&mut app, 'x');
        assert!(app.modal.is_none());
        assert!(visible(&app).is_empty(), "applied: nothing needs attention");
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
            crate::model::options::OptionDef { prevents: vec!["NLS".into()], ..Default::default() },
        );
        app.session.states.get_mut("cat_alpha").unwrap().staged =
            ["DOCS".to_string(), "NLS".to_string()].into();
        assert_eq!(app.session.status(&info_of(&app, "cat/alpha")), UiStatus::Conflict);
        press(&mut app, '/');
        for c in "gam".chars() {
            press(&mut app, c);
        }
        hit(&mut app, KeyCode::Enter);
        assert_eq!(visible(&app), ["cat/gamma"]);
        press(&mut app, 'a');
        let modal = app.modal.as_ref().expect("the apply preview opened");
        assert_eq!(modal.conflicted, [key]);
        assert!(screen(&mut app).contains("1 port(s) still have conflicting options: cat/alpha"));
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
        let done = app.modal.as_ref().and_then(|m| m.done.clone()).unwrap_or_default();
        assert!(done.contains("\n1 redundant file(s) removed (--minimal)"), "{done}");
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
        let done = app.modal.as_ref().and_then(|m| m.done.clone()).unwrap_or_default();
        assert!(done.starts_with("nothing written"), "{done}");
        assert!(done.contains("FAILED cat/alpha"), "{done}");
        assert!(!done.contains("removed"), "{done}");
        assert!(tmp.path().join("options/cat_beta/options").is_file(), "beta's file kept");
        assert!(app.session.dirty(), "the edit is still staged");
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
        press(&mut app, 's');
        drop(app);

        let mut app = three_ports(tmp.path());
        assert_eq!(staged(&app, "cat/alpha"), ["DOCS"]);
        assert!(app.pending.contains_key(&PortKey::parse("cat/alpha").unwrap()));
        assert!(app.message.as_ref().is_some_and(|(m, _)| m.starts_with("restored draft with 1 staged port(s)")));
        press(&mut app, 'U');
        assert!(staged(&app, "cat/alpha").is_empty(), "the restore is undoable");
        drop(app);

        // Applied since: the draft changes nothing any more and is dropped.
        let opts = tmp.path().join("options/cat_alpha");
        std::fs::create_dir_all(&opts).unwrap();
        std::fs::write(opts.join("options"), "_FILE_COMPLETE_OPTIONS_LIST=DOCS\nOPTIONS_FILE_SET+=DOCS\n").unwrap();
        let app = three_ports(tmp.path());
        assert!(draft::load(&app.options_dir).is_none());
        assert!(app.message.is_none());
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
            press(&mut app, 's');
            drop(app);

            let mut app = three_ports(tmp.path());
            assert_eq!(staged(&app, "cat/alpha"), ["DOCS"], "restored");
            press(&mut app, 'U');
            assert!(!app.session.dirty());
            let outcome = if quit == 'q' { press(&mut app, 'q') } else { ctrl(&mut app, 'c') };
            assert!(matches!(outcome, KeyOutcome::Quit), "{quit}: nothing to lose");
            drop(app);

            let app = three_ports(tmp.path());
            assert!(staged(&app, "cat/alpha").is_empty(), "{quit}: the draft came back");
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
        assert!(tmp.path().join("db/cat_beta/options").is_file(), "staging seeded");
        // Edited behind optique's back: apply plans against what the session
        // read, so a port nobody touched in the TUI is not rewritten.
        std::fs::write(beta.join("options"), "OPTIONS_FILE_SET+=DOCS\n").unwrap();

        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(app.modal.is_some(), "the apply preview opened");
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        let done = app.modal.as_ref().and_then(|m| m.done.clone()).unwrap_or_default();
        assert!(done.starts_with("1 file(s) written"), "{done}");
        assert!(!done.contains("redundant"), "nothing removed, nothing said: {done}");
        assert!(tmp.path().join("options/cat_alpha/options").is_file());
        assert_eq!(std::fs::read(beta.join("options")).unwrap(), b"OPTIONS_FILE_SET+=DOCS\n");
        assert_eq!(app.options_files.disk_reads(), at_start + 1, "only cat_alpha re-read");
        let alpha = PortKey::parse("cat/alpha").unwrap();
        assert!(app.session.state(&app.session.ports[&alpha]).unwrap().saved.is_some());
    }

    /// 'I' opens the option pane with what the port is (off at start), and
    /// 'i' on the list shows all of it, WWW included.
    #[test]
    fn port_description_panel_and_i_overlay() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_for(tmp.path(), &[("cat/alpha", &[])]);
        let descr = tmp.path().join("pkg-descr");
        std::fs::write(&descr, "Alpha does\nthings.\n\nWWW: https://old.example/\n").unwrap();
        let key = PortKey::parse("cat/alpha").unwrap();
        let about = &mut app.session.ports.get_mut(&key).unwrap().about;
        about.comment = "The alpha port".to_string();
        about.www = "https://alpha.example/".to_string();
        about.pkg_descr = Some(descr.display().to_string());

        assert!(!screen(&mut app).contains("The alpha port"), "the panel starts off");
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('I'), KeyModifiers::SHIFT));
        let shown = screen(&mut app);
        assert!(shown.contains("The alpha port"), "{shown}");
        assert!(shown.contains("Alpha does things."), "paragraph re-wrapped: {shown}");
        assert!(!shown.contains("old.example"), "old-style WWW: left to the overlay");

        // 'I' hides it again and brings it back, from either pane.
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('I'), KeyModifiers::SHIFT));
        assert!(!screen(&mut app).contains("Alpha does things."));
        app.focus = Focus::Editor;
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('I'), KeyModifiers::SHIFT));
        assert!(screen(&mut app).contains("Alpha does things."));
        app.focus = Focus::List;

        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        let help = app.port_help.as_ref().expect("'i' on the list opens the overlay");
        assert_eq!(help.title, "about");
        assert_eq!(help.lines[0], "The alpha port");
        assert_eq!(help.lines[1], "WWW: https://alpha.example/");
        assert!(help.lines.iter().any(|l| l == "Alpha does"));
    }

    /// The smallest closure with a dependency loop: two ports needing each
    /// other.
    fn looped_app(tmp: &std::path::Path) -> App {
        app_for(tmp, &[("cat/alpha", &["cat/beta"]), ("cat/beta", &["cat/alpha"])])
    }

    /// Everything the in-memory screen shows, rows joined by newlines.
    fn screen(app: &mut App) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 35)).unwrap();
        terminal.draw(|f| ui::draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let (w, h) = (buffer.area.width as usize, buffer.area.height as usize);
        let cells = buffer.content();
        (0..h)
            .map(|y| {
                (0..w).map(|x| cells[y * w + x].symbol()).collect::<String>().trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A loop is visible without asking: both ports wear the ∞ badge, and the
    /// status bar says how many are caught in one.
    #[test]
    fn looped_ports_are_badged_in_the_list() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = looped_app(tmp.path());
        assert_eq!(app.loops.len(), 1, "one loop over the two ports");
        let text = screen(&mut app);
        assert!(text.contains("cat/alpha \u{221e}"), "{text}");
        assert!(text.contains("cat/beta \u{221e}"), "{text}");
        assert!(text.contains("2\u{221e} L:loops"), "the status bar counts them:\n{text}");
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
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE));
        assert!(app.only_loops);
        assert_eq!(app.visible.len(), 2, "{:?}", app.visible);
        assert!(!app.visible.iter().any(|k| k.origin == "cat/loner"));
        let text = screen(&mut app);
        assert!(text.contains("in a dependency loop"), "{text}");

        // And back.
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE));
        assert!(!app.only_loops);
        assert_eq!(app.visible.len(), 3);
    }

    /// With nothing to show the view is refused rather than emptying the list.
    #[test]
    fn the_loop_view_is_refused_without_a_loop() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_for(tmp.path(), &[("cat/one", &[]), ("cat/two", &[])]);
        assert!(app.loops.is_empty());
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE));
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
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('L'), KeyModifiers::NONE));
        assert!(app.only_loops && app.visible.len() == 2);

        // What a background re-query would merge: cat/beta no longer needs
        // cat/alpha, so the cycle is gone.
        let beta = PortKey::parse("cat/beta").unwrap();
        app.session.ports.get_mut(&beta).unwrap().deps.clear();
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
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        let text = screen(&mut app);
        assert!(text.contains("caught in a dependency loop (2 ports)"), "{text}");
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
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
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
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        // cat/alpha is a root, so the entries are: the one-port chain, then
        // the cycle (alpha, beta), then the dependents (beta).
        let entries: Vec<String> =
            app.why.as_ref().unwrap().entries().iter().map(|k| k.to_string()).collect();
        assert_eq!(entries, ["cat/alpha", "cat/alpha", "cat/beta", "cat/beta"], "{entries:?}");

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
        let at = app.why.as_ref().unwrap().entries().iter().position(|k| k.origin == "cat/gamma");
        for _ in 0..at.expect("gamma is a dependent of alpha") {
            hit(&mut app, KeyCode::Down);
        }
        hit(&mut app, KeyCode::Enter);
        assert_eq!(selected(&app), "cat/gamma");
        assert!(!app.only_loops, "the loop view let go");
        let (msg, err) = app.message.clone().unwrap();
        assert_eq!((msg.as_str(), err), ("jumped to cat/gamma (loop view left)", false));
    }

    /// On an 80x30 terminal the why box wraps the loop's note line; the
    /// cursor on the last dependent must still be scrolled into view.
    #[test]
    fn the_why_cursor_stays_in_view_on_a_small_terminal() {
        let tmp = tempfile::tempdir().unwrap();
        let names: Vec<String> = (0..ui::WHY_MAX_DEPENDENTS).map(|i| format!("cat/d{i:02}")).collect();
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
        let last = app.why.as_ref().unwrap().entries().last().unwrap().to_string();

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
        terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let highlighted: Vec<String> = (0..30u16)
            .map(|y| {
                (0..80u16)
                    .map(|x| &buffer[(x, y)])
                    .filter(|c| c.modifier.contains(ratatui::style::Modifier::REVERSED))
                    .map(|c| c.symbol())
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
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        let text = screen(&mut app);
        assert!(text.contains("caught in a dependency loop (1 port)"), "{text}");
        assert!(text.contains("depends on itself"), "{text}");
        assert!(!text.contains("back to"), "no return arrow for a loop of one:\n{text}");
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
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        let text = screen(&mut app);
        assert!(text.contains(&format!("caught in a dependency loop ({} ports)", n + 1)), "{text}");
        assert!(text.contains("also in the tangle:"), "{text}");
        assert!(text.contains("+ 2 more"), "the cap counts the rest:\n{text}");

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
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        for _ in 0..2 {
            dispatch_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        let why = app.why.as_ref().expect("the overlay stays open");
        assert_eq!(why.key.origin, "cat/beta");
        assert_eq!(why.selected, 0, "the cursor restarts on the new port");
        // And the new overlay's own walk starts there.
        assert_eq!(
            why.dep_loop.as_ref().unwrap().walk[0].origin,
            "cat/beta"
        );
    }

    #[test]
    fn bulk_parse_forms_and_errors() {
        assert_eq!(parse_bulk("docs=on").unwrap(), ("DOCS".to_string(), true));
        assert_eq!(parse_bulk(" X11 = OFF ").unwrap(), ("X11".to_string(), false));
        assert_eq!(parse_bulk("nls+").unwrap(), ("NLS".to_string(), true));
        assert_eq!(parse_bulk("nls-").unwrap(), ("NLS".to_string(), false));
        assert!(parse_bulk("docs=maybe").is_err());
        assert!(parse_bulk("docs").is_err());
        assert!(parse_bulk("=on").is_err());
    }
}
