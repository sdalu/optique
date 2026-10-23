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
    /// Ports in conflict whose options files the pass would change: `Y`
    /// writes them anyway, `s` leaves them out.
    pub conflicted: Vec<PortKey>,
    /// Their options files; a file flavors share conflicts when any of
    /// them does.
    pub conflicting_files: BTreeSet<String>,
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
    /// Rows of the text scrolled past, kept by the drawing code so the
    /// cursor stays in view.
    pub scroll: usize,
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

/// The option-detail overlay: the port and the option it is about, looked
/// up at draw time so a background refresh keeps the popup current.
pub struct OptInfo {
    pub key: PortKey,
    pub opt: String,
    /// Rows scrolled past (clamped at draw time).
    pub scroll: u16,
}

/// The overlay drawn over the panes, when one is open.
pub(crate) enum OverlayKind {
    QuitConfirm,
    Apply,
    Bulk,
    /// The help overlay, on this tab.
    Help(usize),
    PortHelp,
    Why,
    OptInfo,
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
    /// Trees laid over the ports tree, in lookup order: the port pane names
    /// the one a port comes from.
    pub overlays: Vec<crate::config::Overlay>,
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
    /// The option pane opens with the port's description. On at start; 'V'
    /// turns it off and on.
    pub show_about: bool,
    /// pkg-descr text per port, read when the panel first shows that port
    /// (on a change of selection or of the panel, never from the draw path:
    /// the loop redraws every 150 ms and the ports tree may be on NFS). A
    /// file that cannot be read is not kept, so it is tried again.
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
    /// Option-detail overlay, when open.
    pub opt_info: Option<OptInfo>,
    /// pkg-help or description overlay of the selected port, when open.
    pub port_help: Option<PortHelp>,
    /// Undo stack of (OPTIONS_NAME, staged set before the change), oldest
    /// first, capped at UNDO_MAX. Every TUI-driven mutation of a staged set
    /// pushes one entry; 'u' pops and restores the top one.
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
    /// Rows the last frame showed of each view a page key moves through.
    pub page: PageRows,
}

/// Rows of content each paged view showed on the last frame (inside its
/// border): PgUp/PgDn and Ctrl-B/F move by one less. Kept by the drawing
/// code, which is the one to know them; zero before the first frame.
#[derive(Default)]
pub struct PageRows {
    pub list: u16,
    /// The option rows, between the description and the detail panels.
    pub options: u16,
    pub help: u16,
    pub port_help: u16,
    pub why: u16,
    pub opt_info: u16,
    pub apply: u16,
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
    overlays: Vec<crate::config::Overlay>,
    minimal: bool,
) -> Result<()> {
    ensure_terminal()?;
    let mut app = build_app(session, files, staging_db, refresher, blacklist, overlays, minimal);
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    // Don't leave background queries churning after the UI is gone.
    stop_background_makes();
    result
}

/// Kill the make processes background refreshes still have running, saying
/// so on stderr. Shared by the real TUI and the headless driver.
fn stop_background_makes() {
    let stopped = crate::query::makerunner::kill_active_makes();
    if stopped > 0 {
        eprintln!("optique: stopped {stopped} in-flight make process(es)");
    }
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
    overlays: Vec<crate::config::Overlay>,
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
        overlays,
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
        show_about: true,
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
        page: PageRows::default(),
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
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    // Ctrl-L: full repaint to clear terminal artifacts — works in every
    // mode (modal, filter, help) without consuming any state.
    if ctrl && key.code == KeyCode::Char('l') {
        return KeyOutcome::Repaint;
    }
    // A message lasts until the next key: left up, it would hide the key
    // legend for the rest of the session.
    app.message = None;

    // Ctrl-C quits from anywhere, before any popup or prompt can take it as
    // a key of its own; asked a second time, at the quit prompt, it keeps
    // the staged edits as a draft.
    if ctrl && key.code == KeyCode::Char('c') {
        if app.quit_confirm {
            return save_draft_and_quit(app);
        }
        return request_quit(app);
    }
    let code = plain_code(key);

    // Modal and quit-confirm grab all keys.
    if app.quit_confirm {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => return save_draft_and_quit(app),
            // Drop any older draft too: quitting on purpose without saving
            // must not resurrect stale intentions.
            KeyCode::Char('n') | KeyCode::Char('N') => {
                draft::discard(&app.options_dir);
                return KeyOutcome::Quit;
            }
            _ => app.quit_confirm = false,
        }
        return KeyOutcome::Continue;
    }
    if app.modal.is_some() {
        handle_modal_key(app, code);
        return KeyOutcome::Continue;
    }
    if let Some(tab) = app.help_tab {
        let n = ui::HELP_TABS.len();
        match code {
            KeyCode::Char(c @ '1'..='9') => {
                let idx = c as usize - '1' as usize;
                if idx < n {
                    app.open_help(idx);
                }
            }
            KeyCode::Tab | KeyCode::Right => app.open_help((tab + 1) % n),
            KeyCode::BackTab | KeyCode::Left => app.open_help((tab + n - 1) % n),
            _ => match scroll_key(code, app.help_scroll as usize, app.page.help) {
                Some(at) => app.help_scroll = clamp_u16(at),
                None => {
                    app.help_tab = None;
                    app.help_scroll = 0;
                }
            },
        }
        return KeyOutcome::Continue;
    }
    if let Some(help) = &mut app.port_help {
        // Clamped to the real content at draw time.
        match scroll_key(code, help.scroll as usize, app.page.port_help) {
            Some(at) => help.scroll = clamp_u16(at),
            None => app.port_help = None,
        }
        return KeyOutcome::Continue;
    }
    if let Some(why) = &mut app.why {
        // The scroll keys move the cursor, which the box keeps in view.
        if let Some(at) = scroll_key(code, why.selected, app.page.why) {
            why.selected = at.min(why.entries().len().saturating_sub(1));
            return KeyOutcome::Continue;
        }
        match code {
            KeyCode::Enter => {
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
    if let Some(info) = &mut app.opt_info {
        match scroll_key(code, info.scroll as usize, app.page.opt_info) {
            Some(at) => info.scroll = clamp_u16(at),
            None => app.opt_info = None,
        }
        return KeyOutcome::Continue;
    }
    // The bulk prompt takes text input, so it grabs the keys before the
    // filter branch and the plain command keys.
    if app.bulk.is_some() {
        match code {
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
            KeyCode::Char(c) => {
                if let Some(input) = app.bulk.as_mut() {
                    input.push(c);
                }
            }
            _ => {}
        }
        return KeyOutcome::Continue;
    }
    if app.focus == Focus::Filter {
        match code {
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
            KeyCode::Char(c) => {
                app.filter.push(c);
                app.rebuild_visible(None);
            }
            _ => {}
        }
        app.rebuild_editor();
        return KeyOutcome::Continue;
    }

    match code {
        KeyCode::Char('q') => return request_quit(app),
        KeyCode::Char('/') => {
            // A fresh filter: the old text goes, and the whole list with it.
            app.focus = Focus::Filter;
            if !app.filter.is_empty() {
                app.filter.clear();
                app.refresh_view();
            }
        }
        KeyCode::Char('?') | KeyCode::F(1) => app.open_help(0),
        KeyCode::Char('a') => app.open_apply_modal(),
        KeyCode::Char('B') => app.open_bulk(),
        // vi's pair: 'u' undoes the last change, whichever port it was on;
        // 'U' takes the selected port back to its saved state.
        KeyCode::Char('u') => app.undo_last(),
        KeyCode::Char('U') => app.reset_selected(Session::revert, "reverted to saved state"),
        KeyCode::Char('d') => app.reset_selected(Session::reset_to_defaults, "reset to port defaults"),
        KeyCode::Char('t') => app.toggle_view(
            |a| &mut a.hide_ok,
            "showing only ports needing attention (t to show all)",
            "showing all ports",
        ),
        KeyCode::Char('V') => {
            app.show_about = !app.show_about;
            app.flash(
                if app.show_about {
                    "port description shown (V: hide · v: full text)"
                } else {
                    "port description hidden (V: show · v: full text)"
                },
                false,
            );
            // After the flash: a pkg-descr that cannot be read says so.
            app.sync_about();
        }
        KeyCode::Char('m') => app.toggle_view(
            |a| &mut a.mc_relax,
            "make.conf-decided staleness counts as ok (≈, m to undo)",
            "make.conf-decided staleness counts as stale again",
        ),
        KeyCode::Char('L') => {
            // Turning the view off must always work, even once the loop it
            // was showing is gone; turning it on with nothing to show would
            // only empty the list.
            if !app.only_loops && app.loops.is_empty() {
                app.flash("no dependency loop in this closure", false);
            } else {
                app.only_loops = !app.only_loops;
                app.refresh_view();
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
        KeyCode::Char('s') => app.toggle_view(
            |a| &mut a.sort_problems_first,
            "sorting problems first",
            "alphabetical order (stable while editing)",
        ),
        KeyCode::Char('w') => app.toggle_view(
            |a| &mut a.warn_mc,
            "flagging options that contradict make.conf OPTIONS_SET/UNSET (≠, w to undo)",
            "make.conf contradictions no longer flagged",
        ),
        _ => match app.focus {
            Focus::List => handle_list_key(app, code),
            Focus::Editor => handle_editor_key(app, code),
            Focus::Filter => {}
        },
    }
    KeyOutcome::Continue
}

/// The key as the keymap reads it, its Ctrl folded in: Ctrl-F and Ctrl-B
/// are PgDn and PgUp, and any other Ctrl-letter is no key at all, so it is
/// never taken for the plain letter (Ctrl-F is not `f`). Ctrl-L and Ctrl-C
/// are answered before this.
fn plain_code(key: KeyEvent) -> KeyCode {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return key.code;
    }
    match key.code {
        KeyCode::Char('f') => KeyCode::PageDown,
        KeyCode::Char('b') => KeyCode::PageUp,
        KeyCode::Char(_) => KeyCode::Null,
        code => code,
    }
}

/// The one scroll key set of every scrolling popup — the help, pkg-help
/// and about, why, option info, the apply preview: `j`/`k` and `↑`/`↓` a
/// row, `g`/`G` and Home/End the first and the last, PgUp/PgDn (Ctrl-B/F
/// folded in) a page of the `rows` the popup last showed. The offset `at`
/// moved, `usize::MAX` for the last (the popup clamps it); None for any
/// other key, which keeps its popup's own meaning.
fn scroll_key(code: KeyCode, at: usize, rows: u16) -> Option<usize> {
    Some(match code {
        KeyCode::Down | KeyCode::Char('j') => at.saturating_add(1),
        KeyCode::Up | KeyCode::Char('k') => at.saturating_sub(1),
        KeyCode::PageDown => at.saturating_add(page_step(rows)),
        KeyCode::PageUp => at.saturating_sub(page_step(rows)),
        KeyCode::Home | KeyCode::Char('g') => 0,
        KeyCode::End | KeyCode::Char('G') => usize::MAX,
        _ => return None,
    })
}

/// A scroll offset kept in a u16 (a Paragraph's), saturated.
fn clamp_u16(at: usize) -> u16 {
    at.min(u16::MAX as usize) as u16
}

/// How far a page key moves in a view that showed `rows` rows: one less,
/// so the row at the edge stays in view (and at least one).
fn page_step(rows: u16) -> usize {
    (rows as usize).saturating_sub(1).max(1)
}

/// Quit, or ask first when there are staged edits to save as a draft or
/// discard. Shared by Ctrl-C and 'q'.
fn request_quit(app: &mut App) -> KeyOutcome {
    if app.session.dirty() {
        app.quit_confirm = true;
        return KeyOutcome::Continue;
    }
    // Nothing staged: a draft restored and then reverted by hand has
    // nothing left to keep and must not come back on the next launch.
    draft::discard(&app.options_dir);
    KeyOutcome::Quit
}

/// The quit prompt's yes: keep the staged edits as a draft, then quit. A
/// draft that cannot be saved says why and leaves the session open.
fn save_draft_and_quit(app: &mut App) -> KeyOutcome {
    match draft::save(&app.session, &app.options_dir) {
        // No flash on success: the terminal is about to be restored. The
        // next launch announces the draft instead.
        Ok(_) => KeyOutcome::Quit,
        Err(e) => {
            app.quit_confirm = false;
            app.flash(&format!("draft save failed: {e:#}"), true);
            KeyOutcome::Continue
        }
    }
}

fn handle_list_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char('j') | KeyCode::Down => app.move_selection(1),
        KeyCode::Char('k') | KeyCode::Up => app.move_selection(-1),
        KeyCode::Char('g') | KeyCode::Home => app.select_index(0),
        KeyCode::Char('G') | KeyCode::End => {
            app.select_index(app.visible.len().saturating_sub(1))
        }
        KeyCode::PageDown => app.move_selection(page_step(app.page.list) as isize),
        KeyCode::PageUp => app.move_selection(-(page_step(app.page.list) as isize)),
        KeyCode::Char('n') => {
            app.jump_problem(1);
        }
        KeyCode::Char('p') => {
            app.jump_problem(-1);
        }
        KeyCode::Char('f') => app.jump_flavor(),
        KeyCode::Char('r') => app.open_why(),
        // Same meaning as in the option pane, whose title carries the marker.
        KeyCode::Char('h') => app.open_port_help(),
        KeyCode::Char('v') => app.open_port_about(),
        // 'i' is the highlighted option's, and the list highlights none.
        KeyCode::Char('i') => {
            app.flash("i: option details, in the option pane (Enter) — v: port description", false)
        }
        // Esc lets go of a filter left on the list.
        KeyCode::Esc if !app.filter.is_empty() => {
            app.filter.clear();
            app.refresh_view();
        }
        KeyCode::Enter | KeyCode::Tab | KeyCode::Right if !app.editor_rows.is_empty() => {
            app.focus = Focus::Editor;
            app.editor_select_first();
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
        KeyCode::Char('g') | KeyCode::Home => app.editor_steps(-1, usize::MAX),
        KeyCode::Char('G') | KeyCode::End => app.editor_steps(1, usize::MAX),
        KeyCode::PageDown => app.editor_page(1),
        KeyCode::PageUp => app.editor_page(-1),
        // The next port needing attention, its options at once: the cursor
        // starts on its first one.
        KeyCode::Char(c @ ('n' | 'p')) => {
            if app.jump_problem(if c == 'n' { 1 } else { -1 }) {
                app.editor_select_first();
            }
        }
        KeyCode::Char('v') => app.open_port_about(),
        // Space alone toggles: Enter is the list's key into the pane, and
        // pressed once too often it must not flip an option.
        KeyCode::Char(' ') => app.toggle_current(),
        KeyCode::Char('f') => app.jump_flavor(),
        KeyCode::Char('r') => app.open_why(),
        KeyCode::Char('i') => app.open_opt_info(),
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
        app.refresh_view();
        return;
    }
    let conflicts = app.modal.as_ref().is_some_and(|m| !m.conflicted.is_empty());
    match code {
        KeyCode::Char('Y') => commit_apply(app, false),
        KeyCode::Char('y') if !conflicts => commit_apply(app, false),
        KeyCode::Char('s') if conflicts => commit_apply(app, true),
        // With conflicts a lowercase y is too easy to type by habit: it
        // writes nothing and leaves the choice open.
        KeyCode::Char('y') => {
            app.flash("conflicts remain: Y writes anyway · s skips them · n cancels", false)
        }
        _ => match scroll_key(code, app.modal.as_ref().map_or(0, |m| m.scroll), app.page.apply) {
            // In entries, clamped at draw time to the last page.
            Some(at) => {
                if let Some(modal) = app.modal.as_mut() {
                    modal.scroll = at;
                }
            }
            None => app.modal = None,
        },
    }
}

/// Write the apply plan, recomputed now: a background refresh may have
/// merged new ports since the preview was opened. `skip_conflicting` leaves
/// the files of ports in conflict as they are on disk, their edits staged.
fn commit_apply(app: &mut App, skip_conflicting: bool) {
    let mut planned = app.compute_writes();
    let (conflicted, files) = app.conflicting(&planned);
    let skipped = if skip_conflicting && !conflicted.is_empty() {
        planned = planned.without(&files);
        conflicted.len()
    } else {
        0
    };
    let done = apply::commit(&planned.writes, &planned.removals);
    // apply is all-or-nothing: after a failed write nothing is on disk, so
    // nothing is removed either, and the edits stay staged (and in the
    // draft) to be applied again.
    if !done.summary.failed.is_empty() {
        let mut text = format!("nothing written to {}", app.options_dir.display());
        for (key, msg) in &done.summary.failed {
            text.push_str(&format!("\nFAILED {key}: {msg}"));
        }
        if let Some(modal) = app.modal.as_mut() {
            modal.done = Some(text);
        }
        return;
    }
    let mut text = format!("{} file(s) written to {}", done.summary.written, app.options_dir.display());
    if skipped > 0 {
        text.push_str(&format!("\n{skipped} port(s) skipped: conflicting options"));
    }
    for (name, e) in &done.removed.failed {
        text.push_str(&format!("\nFAILED rm {name}: {e}"));
    }
    if done.removed.count > 0 {
        text.push_str(&format!("\n{} redundant file(s) removed (--minimal)", done.removed.count));
    }
    for note in &done.removed.notes {
        text.push_str(&format!("\nnote: {note}"));
    }
    // Only the files apply touched are read again; the rest are as the scan
    // read them, and a skipped port keeps its edits staged.
    let touched: Vec<&str> = planned.touched().collect();
    app.options_files.forget(touched.iter().copied());
    app.session.reload_saved(&app.options_files, touched.iter().copied());
    // The files on disk changed: attributions built on the pre-apply staged
    // history would now point at the wrong changes.
    app.blame.clear();
    // The edits are on disk now, and the draft has served its purpose —
    // unless some were skipped and are still to be applied.
    if skipped == 0 {
        draft::discard(&app.options_dir);
    }
    if let Some(modal) = app.modal.as_mut() {
        modal.done = Some(text);
    }
}

impl App {
    pub fn selected_key(&self) -> Option<PortKey> {
        self.list_state.selected().and_then(|i| self.visible.get(i)).cloned()
    }

    /// The overlay on screen: when several are open, the first in this order
    /// is the one drawn. `ui::draw` and the driver's `state` both use it.
    /// `dispatch_key` hands keys out in a different order on purpose (Ctrl-C
    /// is checked before every overlay, the bulk prompt after the popups),
    /// so it does not go through here.
    pub(crate) fn top_overlay(&self) -> Option<OverlayKind> {
        if self.quit_confirm {
            Some(OverlayKind::QuitConfirm)
        } else if self.modal.is_some() {
            Some(OverlayKind::Apply)
        } else if self.bulk.is_some() {
            Some(OverlayKind::Bulk)
        } else if let Some(tab) = self.help_tab {
            Some(OverlayKind::Help(tab))
        } else if self.port_help.is_some() {
            Some(OverlayKind::PortHelp)
        } else if self.why.is_some() {
            Some(OverlayKind::Why)
        } else if self.opt_info.is_some() {
            Some(OverlayKind::OptInfo)
        } else {
            None
        }
    }

    /// Rebuild the list and the editor after a change to what is listed or
    /// how, keeping the selection on the same port when it is still there.
    fn refresh_view(&mut self) {
        let keep = self.selected_key();
        self.rebuild_visible(keep);
        self.rebuild_editor();
    }

    /// Flip one of the list's view flags, refresh, and say which way it went.
    fn toggle_view(&mut self, flag: fn(&mut App) -> &mut bool, on: &str, off: &str) {
        let flag = flag(self);
        *flag = !*flag;
        let now = *flag;
        self.refresh_view();
        self.flash(if now { on } else { off }, false);
    }

    /// Open the help overlay on tab `idx`, scrolled to its top.
    fn open_help(&mut self, idx: usize) {
        self.help_tab = Some(idx);
        self.help_scroll = 0;
    }

    /// Run a whole-port reset ('d', 'U') on the selected port: undoable, and
    /// re-queried like any other change.
    fn reset_selected(&mut self, reset: fn(&mut Session, &PortKey), msg: &str) {
        if let Some(key) = self.selected_key() {
            self.push_undo(&key);
            reset(&mut self.session, &key);
            self.mark_pending(&key);
            self.flash(msg, false);
        }
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
        // Under -m the defaults are the decision, as if the m view were on:
        // a port they settle needs no file, so it is not waiting on anyone.
        if (self.mc_relax || self.minimal)
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

    /// Select the next port needing attention in direction `dir`, wrapping;
    /// false when none does.
    fn jump_problem(&mut self, dir: isize) -> bool {
        if self.visible.is_empty() {
            return false;
        }
        let len = self.visible.len() as isize;
        let start = self.list_state.selected().unwrap_or(0) as isize;
        let mut i = start;
        for _ in 0..len {
            i = (i + dir).rem_euclid(len);
            let info = &self.session.ports[&self.visible[i as usize]];
            if self.effective_status(info) != UiStatus::Ok {
                self.select_index(i as usize);
                return true;
            }
        }
        self.flash("no ports need attention", false);
        false
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
        // Every change of selection comes through here.
        self.sync_about();
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
        let owner_complete = &self.session.owner_info(info).options.complete;
        if let Some(state) = self.session.state(info) {
            if let Some(saved) = &state.saved {
                let file_known: BTreeSet<String> = saved
                    .complete
                    .iter()
                    .chain(saved.recorded())
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

    /// Up to `n` options in direction `dir`, stopping at the edges.
    fn editor_steps(&mut self, dir: isize, n: usize) {
        for _ in 0..n.min(self.editor_rows.len()) {
            self.editor_move(dir);
        }
    }

    /// A page of rows in direction `dir`: the option at most a page away,
    /// or the next one when the page holds none, stopping at the edges.
    fn editor_page(&mut self, dir: isize) {
        if self.editor_rows.is_empty() {
            return;
        }
        let from = self.editor_idx as isize;
        let last = self.editor_rows.len() as isize - 1;
        let mut i = (from + dir * page_step(self.page.options) as isize).clamp(0, last);
        // Back toward the start until an option: a header or a trailing
        // section is no place for the cursor.
        while i != from && !matches!(self.editor_rows[i as usize], EditorRow::Option(_)) {
            i -= dir;
        }
        if i == from {
            self.editor_move(dir);
        } else {
            self.editor_idx = i as usize;
        }
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
            let turning_on = self.session.state(info).is_some_and(|s| !s.staged.contains(&opt));
            let def = info.options.defs.get(&opt).filter(|_| turning_on);
            def.and_then(|d| match (&d.broken, &d.ignore) {
                (Some(m), _) => Some(format!("warning: {opt} marks this port BROKEN: {m}")),
                (None, Some(m)) => Some(format!("warning: {opt} marks this port IGNORED: {m}")),
                (None, None) => None,
            })
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
            scroll: 0,
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
        self.opt_info = Some(OptInfo { key, opt: opt.clone(), scroll: 0 });
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

    /// Read the selected port's pkg-descr for the panel, when the panel is
    /// on and has not read it yet. A file that cannot be read says so and is
    /// tried again next time.
    fn sync_about(&mut self) {
        if !self.show_about {
            return;
        }
        let Some(key) = self.selected_key() else { return };
        if self.about_text.contains_key(&key) {
            return;
        }
        match self.read_descr(&key) {
            Ok(text) => {
                self.about_text.insert(key, text.unwrap_or_default());
            }
            Err(e) => self.flash(&e, true),
        }
    }

    /// A port's pkg-descr as on disk: None when it ships none, the message
    /// to flash when it cannot be read. The one reader of the file, for the
    /// panel and the `v` overlay alike.
    fn read_descr(&self, key: &PortKey) -> Result<Option<String>, String> {
        let Some(path) = &self.session.ports[key].about.pkg_descr else { return Ok(None) };
        std::fs::read_to_string(path).map(Some).map_err(|e| format!("cannot read {path}: {e}"))
    }

    /// Open the selected port's description: its COMMENT and WWW, then its
    /// pkg-descr as written. A port whose pkg-descr cannot be read still
    /// shows the rest.
    fn open_port_about(&mut self) {
        let Some(key) = self.selected_key() else { return };
        let about = &self.session.ports[&key].about;
        let mut lines: Vec<String> =
            Some(about.comment.clone()).filter(|c| !c.is_empty()).into_iter().collect();
        for url in about.www.split_whitespace() {
            lines.push(format!("WWW: {url}"));
        }
        match self.read_descr(&key) {
            Ok(Some(text)) => {
                if !lines.is_empty() {
                    lines.push(String::new());
                }
                lines.extend(text.lines().map(str::to_string));
            }
            Ok(None) => {}
            Err(e) => self.flash(&e, true),
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
            crate::model::options::on_off(on),
            changed.len(),
            skipped.len()
        );
        if let Some((key, reason)) = skipped.first() {
            msg.push_str(&format!(" (first: {key}: {reason})"));
        }
        self.flash(&msg, changed.is_empty());
        self.refresh_view();
    }

    /// Adopt the draft saved by a previous session on this options dir, if
    /// any. Restored ports are marked pending so the background re-query
    /// picks up the dependencies their staged options pull in.
    fn restore_draft_if_any(&mut self) {
        let Some(draft) = draft::load(&self.options_dir) else { return };
        // Pre-images of every state the draft names, so 'u' can walk back the
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
                "restored draft with {restored} staged port(s) from {} — 'U' per port reverts, draft cleared on apply",
                draft::age_label(draft.age_secs())
            ),
            false,
        );
    }

    /// Record `staged` as the state of `options_name` before a change, for 'u'.
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
    /// produced it (toggle, d, U, bulk, draft restore).
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
        self.refresh_view();
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
                    // is_blocked: one of the two is set.
                    let cause = info
                        .broken
                        .as_ref()
                        .map(|m| ("BROKEN", m.as_str()))
                        .or(info.ignore.as_ref().map(|m| ("IGNORE", m.as_str())));
                    if let Some((label, reason)) = cause {
                        flash = Some(format!(
                            "{key}: now {label} ({}) — {diff}",
                            trim_reason(reason)
                        ));
                        self.blame.insert(info.options_name.clone(), diff);
                    }
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
            self.refresh_view();
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

    /// The ports in conflict whose options files `planned` changes, and
    /// those files. Every port counts, not just the listed ones: apply
    /// writes them all, and a conflict the filter or a view hides is written
    /// all the same. A port whose file the pass leaves alone does not count.
    fn conflicting(&self, planned: &apply::PlannedWrites) -> (Vec<PortKey>, BTreeSet<String>) {
        let touched: BTreeSet<&str> = planned.touched().collect();
        let mut files = BTreeSet::new();
        let ports = self
            .session
            .ports
            .iter()
            .filter(|(_, info)| touched.contains(info.options_name.as_str()))
            .filter(|(_, info)| self.session.status(info) == UiStatus::Conflict)
            .map(|(key, info)| {
                files.insert(info.options_name.clone());
                key.clone()
            })
            .collect();
        (ports, files)
    }

    fn open_apply_modal(&mut self) {
        let planned = self.compute_writes();
        if planned.writes.is_empty() && planned.removals.is_empty() {
            self.flash("nothing to write — everything is up to date", false);
        } else {
            let (conflicted, conflicting_files) = self.conflicting(&planned);
            self.modal = Some(ApplyModal {
                writes: planned.writes,
                removals: planned.removals,
                warnings: planned.warnings,
                scroll: 0,
                conflicted,
                conflicting_files,
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
            PortInfo {
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
            },
            1,
            crate::cache::Cache::disabled(),
            crate::moved::Moved::parse(""),
        );
        build_app(session, files, db, refresher, Default::default(), Vec::new(), false)
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
        app.overlays = vec![crate::config::Overlay { name: Some("mine".into()), path: ov }];
        assert!(!screen(&mut app).contains("overlay"), "cat/alpha is the tree's");
        press(&mut app, 'j');
        assert!(screen(&mut app).contains("cat/beta — beta-1.0 · overlay mine"), "{}", screen(&mut app));
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
            assert!(shown.contains(&format!("OPT{n:02}")), "the selected row in view:\n{shown}");
        }
    }

    /// The row a rule (├…┤) of the option pane sits on, counted from the top.
    fn rule_rows(shown: &str) -> Vec<usize> {
        shown.lines().enumerate().filter(|(_, l)| l.contains('├')).map(|(i, _)| i).collect()
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
        let def = |d: &str| crate::model::options::OptionDef { desc: d.into(), ..Default::default() };
        info.options.defs.insert("LONG".into(), def(&"a long description ".repeat(12)));
        info.options.defs.insert("SHORT".into(), def("short"));
        app.rebuild_editor();

        let on_list = screen(&mut app);
        assert!(on_list.contains("Enter: edit the options"), "{on_list}");
        assert!(!on_list.contains("├ LONG"), "no option named while none is highlighted");
        hit(&mut app, KeyCode::Enter);
        let long = screen(&mut app);
        assert!(long.contains("├ LONG"), "{long}");
        assert!(long.contains('…'), "a long description is cut");
        press(&mut app, 'j');
        let short = screen(&mut app);
        assert!(short.contains("├ SHORT"), "{short}");
        assert_eq!(rule_rows(&on_list), rule_rows(&long));
        assert_eq!(rule_rows(&long), rule_rows(&short));
        // g/G work in the pane too.
        press(&mut app, 'g');
        assert!(screen(&mut app).contains("├ LONG"));
        press(&mut app, 'G');
        assert!(screen(&mut app).contains("├ SHORT"));
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
        press(&mut app, 'V');
        assert!(!screen(&mut app).contains("The alpha port"), "V hides it");
        press(&mut app, 'V');
        assert!(app.message.is_some());
        assert!(screen(&mut app).contains("[v] full text ──┤"), "a hint, at the right");
        hit(&mut app, KeyCode::Enter);
        assert!(app.message.is_none(), "cleared by the next key");
        press(&mut app, 'v');
        assert_eq!(app.port_help.as_ref().map(|h| h.title), Some("about"), "v from the option pane");
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
            shown.lines().enumerate().filter(|(_, l)| l.contains('█')).map(|(i, _)| i).collect()
        };
        // Bullets, so each keeps a line of its own.
        let lines = (1..=200).map(|i| format!("- line {i}")).collect();
        app.port_help = Some(PortHelp { title: "pkg-help", key: key.clone(), lines, scroll: 0 });
        let top = thumb(&screen(&mut app));
        assert!(!top.is_empty(), "a bar when the text overflows");
        app.port_help.as_mut().unwrap().scroll = u16::MAX;
        let bottom = thumb(&screen(&mut app));
        assert!(bottom[0] > top[top.len() - 1], "the thumb follows the scroll: {top:?} {bottom:?}");
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
        assert!(shown.contains('▲') && shown.contains('▼'), "the ends are marked");

        app.port_help = Some(PortHelp { title: "pkg-help", key, lines: vec!["short".into()], scroll: 0 });
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
        assert!(screen(&mut app).contains("Alpha does things."), "read again at redraw");
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
        // Built with the panel on, the app has read the port already.
        app.about_text.clear();
        app.rebuild_editor();
        let msg = app.message.as_ref().map(|(m, _)| m.clone()).unwrap_or_default();
        assert!(msg.starts_with("cannot read"), "{msg}");
        assert!(screen(&mut app).contains("No description available"));

        std::fs::write(&descr, "Alpha does things.\n").unwrap();
        app.move_selection(1);
        app.move_selection(-1);
        assert!(screen(&mut app).contains("Alpha does things."), "read again once readable");
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
        assert!(shown.contains("y save & quit · n discard & quit · Esc stay"), "{shown}");
        for stay in ['s', 'S', 'd', 'D', 'x'] {
            app.quit_confirm = true;
            assert!(matches!(press(&mut app, stay), KeyOutcome::Continue), "{stay}");
            assert!(!app.quit_confirm, "{stay} stays");
        }
        app.quit_confirm = true;
        hit(&mut app, KeyCode::Esc);
        assert!(!app.quit_confirm, "Esc stays");
        app.quit_confirm = true;
        assert!(matches!(ctrl(&mut app, 'y'), KeyOutcome::Continue), "Ctrl-Y is not y");
        assert!(draft::load(&app.options_dir).is_none(), "nothing saved yet");

        // A second Ctrl-C keeps the edits.
        press(&mut app, 'q');
        assert!(matches!(ctrl(&mut app, 'c'), KeyOutcome::Quit));
        assert!(draft::load(&app.options_dir).is_some(), "Ctrl-C at the prompt saved a draft");
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
            &|a| { press(a, '?'); },
            &|a| { press(a, 'h'); },
            &|a| { press(a, 'v'); },
            &|a| { press(a, 'r'); },
            &|a| { press(a, '/'); },
            &|a| { press(a, 'B'); },
            &|a| { hit(a, KeyCode::Enter); },
            &|a| { hit(a, KeyCode::Enter); press(a, 'i'); },
            &|a| { press(a, 'a'); },
        ];
        for (i, open) in openers.iter().enumerate() {
            let tmp = tempfile::tempdir().unwrap();
            let mut app = three_ports(tmp.path());
            app.session.ports.get_mut(&key).unwrap().pkg_help = Some(help.display().to_string());
            open(&mut app);
            assert!(app.top_overlay().is_some() || app.focus != Focus::List, "opener {i}");
            assert!(matches!(ctrl(&mut app, 'c'), KeyOutcome::Quit), "opener {i}: nothing staged");
        }
        // With edits staged, it asks first, over whatever is open.
        hit(&mut app, KeyCode::Enter);
        press(&mut app, ' ');
        press(&mut app, '?');
        assert!(matches!(ctrl(&mut app, 'c'), KeyOutcome::Continue));
        assert!(app.quit_confirm && app.help_tab.is_some());
        hit(&mut app, KeyCode::Esc);
        assert!(!app.quit_confirm && app.help_tab.is_some(), "Esc goes back to the help");
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
        assert_eq!((app.help_tab, app.help_scroll), (Some(1), 0), "Home: the top");
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
        assert!(app.message.as_ref().is_some_and(|(m, _)| m.contains("no pkg-help")));

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
        assert_eq!(app.port_help.as_ref().unwrap().scroll, 1 + 2 * page, "Ctrl-F pages too");
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

    /// `/` starts a fresh filter; Esc on the list lets go of one left there,
    /// keeping the port; the cursor shows only while the filter is typed.
    #[test]
    fn a_filter_starts_fresh_and_esc_clears_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = three_ports(tmp.path());
        press(&mut app, '/');
        assert!(screen(&mut app).contains("Ports (3) — filter: ▏"), "{}", screen(&mut app));
        for c in "gam".chars() {
            press(&mut app, c);
        }
        assert!(screen(&mut app).contains("— filter: gam▏"));
        hit(&mut app, KeyCode::Enter);
        let shown = screen(&mut app);
        assert!(shown.contains("— filter: gam ") && !shown.contains('▏'), "no cursor once left:\n{shown}");
        assert_eq!(selected(&app), "cat/gamma");

        press(&mut app, '/');
        assert!(app.focus == Focus::Filter && app.filter.is_empty(), "fresh: {:?}", app.filter);
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
        assert_eq!(staged(&app, "cat/alpha"), ["DOCS"], "j stops at the last option");
        press(&mut app, 'k');
        press(&mut app, 'k');
        press(&mut app, ' ');
        assert!(staged(&app, "cat/alpha").is_empty(), "k stops at the first");
        press(&mut app, 'u');
        assert_eq!(staged(&app, "cat/alpha"), ["DOCS"]);
        press(&mut app, 'd');
        assert_eq!(staged(&app, "cat/alpha"), ["NLS"], "d: the port's defaults");
        press(&mut app, 'U');
        assert!(staged(&app, "cat/alpha").is_empty(), "U: the saved state (none)");
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
        assert!(app.message.as_ref().is_some_and(|(m, _)| m == "nothing to undo"));
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
        assert!(app.message.as_ref().is_some_and(|(m, _)| m == "reset to port defaults"));
        press(&mut app, 'U');
        assert!(staged(&app, "cat/alpha").is_empty(), "U from the list");
        assert!(app.message.as_ref().is_some_and(|(m, _)| m == "reverted to saved state"));
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
            let info = app.session.ports.get_mut(&PortKey::parse(p).unwrap()).unwrap();
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
        assert!(app.show_about && app.message.is_none(), "I does nothing");
        press(&mut app, 'V');
        assert!(!app.show_about);
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
        assert!(app.port_help.is_none() && app.opt_info.is_none(), "i on the list opens nothing");
        assert_eq!(
            app.message.clone(),
            Some(("i: option details, in the option pane (Enter) — v: port description".into(), false))
        );
        press(&mut app, 'v');
        assert_eq!(app.port_help.as_ref().map(|h| h.title), Some("about"));
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
        assert_eq!(app.page.options, 32 - 3 - 6, "detail 2 + rule, description 5 + rule");
        let page = page_step(app.page.options);
        ctrl(&mut app, 'f');
        assert_eq!(app.editor_idx, page);
        let shown = screen(&mut app);
        assert!(shown.contains(&format!("OPT{page:02}")) && shown.contains(&format!("OPT{:02}", page - 1)), "{shown}");
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
        let shown = screen(&mut app);
        assert!(shown.contains(" apply with 1 conflicting? "), "{shown}");
        assert!(shown.contains("conflicting (Y writes them, s skips them):"), "{shown}");
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
        app.session.states.get_mut(&name).unwrap().staged = ["X11".to_string(), "WAYLAND".to_string()].into();
        assert_eq!(app.session.status(&info_of(app, origin)), UiStatus::Conflict);
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
        assert!(shown.contains(" y: write · n: cancel · j/k scroll "), "{shown}");
        assert!(!shown.contains("conflicting"), "{shown}");
        press(&mut app, 'y');
        let done = app.modal.as_ref().and_then(|m| m.done.clone()).unwrap_or_default();
        assert!(done.starts_with("2 file(s) written"), "{done}");
        assert!(!done.contains("skipped"), "{done}");
        let shown = screen(&mut app);
        assert!(shown.contains(" apply — done "), "{shown}");
        assert!(shown.contains(" any key closes "), "{shown}");
        assert!(tmp.path().join("options/cat_alpha/options").is_file());
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
        assert!(shown.contains(" Y: write anyway · s: skip conflicting · n: cancel · j/k scroll "), "{shown}");
        // The conflicting port heads the list, under its own heading.
        let rows: Vec<&str> = shown.lines().collect();
        let heading = rows.iter().position(|r| r.contains("conflicting (Y writes them, s skips them):"));
        let heading = heading.expect("the conflicting heading");
        assert!(rows[heading + 1].contains("cat/beta"), "{shown}");
        assert!(!shown.contains('⚠'), "the heading replaces the warning: {shown}");
        press(&mut app, 'y');
        let modal = app.modal.as_ref().expect("y leaves the preview open");
        assert!(modal.done.is_none());
        assert_eq!(
            app.message,
            Some(("conflicts remain: Y writes anyway · s skips them · n cancels".to_string(), false))
        );
        assert!(!tmp.path().join("options/cat_alpha").exists(), "y writes nothing");
        assert_eq!(std::fs::read(tmp.path().join("options/cat_beta/options")).unwrap(), BETA_FILE);
        press(&mut app, 'n');
        assert!(app.modal.is_none(), "n cancels");
        assert!(!tmp.path().join("options/cat_alpha").exists(), "n writes nothing");
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
        let done = app.modal.as_ref().and_then(|m| m.done.clone()).unwrap_or_default();
        assert!(done.starts_with("3 file(s) written"), "{done}");
        assert!(!done.contains("skipped"), "{done}");
        assert_ne!(std::fs::read(tmp.path().join("options/cat_beta/options")).unwrap(), BETA_FILE);
        assert!(draft::load(&app.options_dir).is_none(), "the draft is discarded");
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
        let done = app.modal.as_ref().and_then(|m| m.done.clone()).unwrap_or_default();
        assert!(done.starts_with("2 file(s) written"), "{done}");
        assert!(done.contains("\n1 port(s) skipped: conflicting options"), "{done}");
        assert!(tmp.path().join("options/cat_alpha/options").is_file());
        assert!(tmp.path().join("options/cat_gamma/options").is_file());
        assert_eq!(std::fs::read(tmp.path().join("options/cat_beta/options")).unwrap(), BETA_FILE);
        assert_eq!(app.session.status(&info_of(&app, "cat/beta")), UiStatus::Conflict);
        assert_eq!(staged(&app, "cat/beta"), ["WAYLAND", "X11"], "the edit stays staged");
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
        let mut app = app_for(tmp.path(), &[("cat/alpha@a", &[]), ("cat/alpha@b", &[]), ("cat/beta", &[])]);
        for k in ["cat/alpha@a", "cat/alpha@b"] {
            let info = app.session.ports.get_mut(&PortKey::parse(k).unwrap()).unwrap();
            info.flavors = vec!["a".into(), "b".into()];
            info.options.complete = vec!["DOCS".into(), "X11".into(), "WAYLAND".into()];
        }
        // Only the non-default flavor has the group the staged set breaks.
        make_conflict(&mut app, "cat/alpha@b");
        assert_ne!(app.session.status(&info_of(&app, "cat/alpha@a")), UiStatus::Conflict);
        press(&mut app, 'a');
        let modal = app.modal.as_ref().unwrap();
        assert_eq!(modal.conflicted, [PortKey::parse("cat/alpha@b").unwrap()]);
        assert_eq!(modal.conflicting_files, ["cat_alpha".to_string()].into());
        press(&mut app, 's');
        let done = app.modal.as_ref().and_then(|m| m.done.clone()).unwrap_or_default();
        assert!(done.starts_with("1 file(s) written"), "{done}");
        assert!(done.contains("\n1 port(s) skipped: conflicting options"), "{done}");
        assert!(!tmp.path().join("options/cat_alpha").exists(), "the shared file is skipped");
        assert!(tmp.path().join("options/cat_beta/options").is_file());
    }

    /// Under -m the defaults are the decision: a port with no options file
    /// (or a stale one whose new options the defaults settle) is ✓, not ?,
    /// without the m view; an edit or a conflict still shows.
    #[test]
    fn minimal_mode_counts_bare_defaults_as_decided() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = three_ports(tmp.path());
        let status = |app: &App, p: &str| app.effective_status(&info_of(app, p));
        assert_eq!(status(&app, "cat/alpha"), UiStatus::Unconfigured, "without -m");
        // As -m sets them both.
        app.minimal = true;
        app.session.minimal = true;
        assert!(!app.mc_relax);
        assert_eq!(status(&app, "cat/alpha"), UiStatus::Ok);
        hit(&mut app, KeyCode::Enter);
        press(&mut app, ' ');
        assert_eq!(status(&app, "cat/alpha"), UiStatus::Edited, "an edit still shows");
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
        press(&mut app, 'y');
        drop(app);

        let mut app = three_ports(tmp.path());
        assert_eq!(staged(&app, "cat/alpha"), ["DOCS"]);
        assert!(app.pending.contains_key(&PortKey::parse("cat/alpha").unwrap()));
        assert!(app.message.as_ref().is_some_and(|(m, _)| m.starts_with("restored draft with 1 staged port(s)")));
        press(&mut app, 'u');
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
            press(&mut app, 'y');
            drop(app);

            let mut app = three_ports(tmp.path());
            assert_eq!(staged(&app, "cat/alpha"), ["DOCS"], "restored");
            press(&mut app, 'u');
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
        assert!(shown.contains("Alpha does things."), "paragraph re-wrapped: {shown}");
        assert!(!shown.contains("old.example"), "old-style WWW: left to the overlay");

        // 'V' hides it again and brings it back, from either pane.
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT));
        assert!(!screen(&mut app).contains("Alpha does things."));
        app.focus = Focus::Editor;
        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('V'), KeyModifiers::SHIFT));
        assert!(screen(&mut app).contains("Alpha does things."));
        app.focus = Focus::List;

        dispatch_key(&mut app, KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        let help = app.port_help.as_ref().expect("'v' on the list opens the overlay");
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
        shown.lines().enumerate().filter(|(_, l)| l.contains('█')).map(|(i, _)| i).collect()
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
        let names: Vec<String> = (0..ui::WHY_MAX_DEPENDENTS + 5).map(|i| format!("cat/d{i:02}")).collect();
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
        assert!(hint.contains('└') && hint.contains('┘'), "on the bottom border:\n{shown}");
        assert_eq!(shown.matches("other key closes").count(), 1, "{shown}");
        let top = thumb_rows(&shown);
        assert!(!top.is_empty(), "the text overflows:\n{shown}");
        let page = page_step(app.page.why);
        assert!(page > 1, "the box's height, from the last frame");

        hit(&mut app, KeyCode::PageDown);
        assert_eq!(app.why.as_ref().map(|w| w.selected), Some(page), "PgDn moves, not closes");
        let middle = thumb_rows(&screen_at(&mut app, 80, 24));
        assert!(middle[0] > top[0], "the thumb follows: {top:?} {middle:?}");
        ctrl(&mut app, 'f');
        let further = (2 * page).min(count - 1);
        assert_eq!(app.why.as_ref().map(|w| w.selected), Some(further), "Ctrl-F too");
        ctrl(&mut app, 'b');
        assert_eq!(app.why.as_ref().map(|w| w.selected), Some(further - page));
        press(&mut app, 'G');
        assert_eq!(app.why.as_ref().map(|w| w.selected), Some(count - 1), "G: the last");
        let shown = screen_at(&mut app, 80, 24);
        // beta depends on alpha too: six dependents past the cap.
        assert!(shown.contains("+ 6 more"), "the end of the text in view:\n{shown}");
        let bottom = thumb_rows(&shown);
        assert!(bottom[0] > middle[0], "the thumb at the bottom: {middle:?} {bottom:?}");
        press(&mut app, 'g');
        assert_eq!(app.why.as_ref().map(|w| w.selected), Some(0), "g: the first");
        hit(&mut app, KeyCode::End);
        assert_eq!(app.why.as_ref().map(|w| w.selected), Some(count - 1), "End: the last");
        hit(&mut app, KeyCode::Home);
        hit(&mut app, KeyCode::PageUp);
        assert_eq!(app.why.as_ref().map(|w| w.selected), Some(0));
        assert_eq!(thumb_rows(&screen_at(&mut app, 80, 24)), top, "back at the top");
        press(&mut app, 'x');
        assert!(app.why.is_none(), "any other key closes");
    }

    /// A port with one option described at length.
    fn long_described(tmp: &std::path::Path, words: usize) -> App {
        let mut app = app_for(tmp, &[("cat/alpha", &[])]);
        let key = PortKey::parse("cat/alpha").unwrap();
        let desc = (0..words).map(|i| format!("word{i}")).collect::<Vec<_>>().join(" ");
        app.session.ports.get_mut(&key).unwrap().options.defs.insert(
            "DOCS".into(),
            crate::model::options::OptionDef { desc, ..Default::default() },
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
        assert!(shown.contains("│word0 word1 "), "the popup's first row:\n{shown}");
        let top = thumb_rows(&shown);
        assert!(!top.is_empty(), "the text overflows:\n{shown}");
        let page = page_step(app.page.opt_info) as u16;
        assert!(page > 1, "the box's height, from the last frame");
        let scroll = |app: &App| app.opt_info.as_ref().map(|i| i.scroll);

        hit(&mut app, KeyCode::PageDown);
        assert_eq!(scroll(&app), Some(page), "PgDn scrolls, not closes");
        let shown = screen_at(&mut app, 80, 24);
        assert!(!shown.contains("│word0 word1 "), "scrolled past the first row:\n{shown}");
        assert!(thumb_rows(&shown)[0] > top[0], "the thumb follows");
        ctrl(&mut app, 'f');
        assert!(scroll(&app).is_some(), "Ctrl-F too");
        press(&mut app, 'G');
        assert_eq!(scroll(&app), Some(u16::MAX), "G: to the end, clamped when drawn");
        let shown = screen_at(&mut app, 80, 24);
        assert!(shown.contains("source: port default"), "the end in view:\n{shown}");
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
        assert!(hint_row(&shown, " any key closes ").contains('└'), "{shown}");
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
        assert!(shown.contains("cat/p39"), "the last entry in view:\n{shown}");
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
        assert!(app.modal.as_ref().is_some_and(|m| m.done.is_none()), "still the preview");
        press(&mut app, 'y');
        let done = app.modal.as_ref().and_then(|m| m.done.clone()).unwrap_or_default();
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
        assert!(shown.contains("This port needs a line wrapped by hand here."), "{shown}");
        for kept in ["- one item ", "- two items ", "  % run this ", "  % then that "] {
            assert!(
                shown.lines().any(|l| l.contains(&format!("│{kept}")) || l.contains(&format!("│ {kept}"))),
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
