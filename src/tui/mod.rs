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

/// Terminal rows from which the description panel shows unless 'V' said
/// otherwise: on a shorter one its five lines would take the option rows'
/// place.
const ABOUT_MIN_ROWS: u16 = 30;

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
    /// What the apply did, once applied.
    pub done: Option<ApplyDone>,
}

impl ApplyModal {
    /// Does the preview show this plan: the same files written with the
    /// same content, the same files removed, the same ports in conflict?
    fn shows(&self, planned: &apply::PlannedWrites, conflicted: &[PortKey]) -> bool {
        let writes = |ws: &[PendingWrite]| -> Vec<(String, String)> {
            ws.iter().map(|w| (w.options_name.clone(), w.content.clone())).collect()
        };
        let removals = |rs: &[crate::clean::Removal]| -> Vec<String> {
            rs.iter().map(|r| r.options_name.clone()).collect()
        };
        writes(&self.writes) == writes(&planned.writes)
            && removals(&self.removals) == removals(&planned.removals)
            && self.conflicted == conflicted
    }
}

/// What an apply did: its result box and the message left once the box
/// closes both read it.
pub struct ApplyDone {
    /// Ports whose options file could not be written, and why. Apply is
    /// all-or-nothing: with any, nothing was written and nothing removed.
    pub failed: Vec<(PortKey, String)>,
    pub written: usize,
    /// Ports in conflict left out (`s`), their edits still staged.
    pub skipped: usize,
    /// Redundant files removed (`--minimal`).
    pub removed: usize,
    /// Removals that failed, by file name, and why.
    pub rm_failed: Vec<(String, String)>,
    pub notes: Vec<String>,
}

impl ApplyDone {
    /// The result's lines, `dir` the options directory as it is to read;
    /// a failure's reasons are as `apply` gave them, paths whole.
    pub fn lines(&self, dir: &str) -> Vec<String> {
        if !self.failed.is_empty() {
            let mut lines = vec![format!("nothing written to {dir}")];
            lines.extend(self.failed.iter().map(|(key, msg)| format!("{key}: {msg}")));
            return lines;
        }
        let mut lines = vec![format!("{} file(s) written to {dir}", self.written)];
        if self.skipped > 0 {
            lines.push(format!("{} port(s) skipped: conflicting options", self.skipped));
        }
        lines.extend(self.rm_failed.iter().map(|(name, e)| format!("FAILED rm {name}: {e}")));
        if self.removed > 0 {
            lines.push(format!("{} redundant file(s) removed (--minimal)", self.removed));
        }
        lines.extend(self.notes.iter().map(|note| format!("note: {note}")));
        lines
    }

    /// The status-bar message left once the result box closes, and whether
    /// it is an error: what was written, or why nothing was, its paths cut
    /// to their last component.
    pub fn message(&self) -> (String, bool) {
        if let Some((_, reason)) = self.failed.first() {
            let reason = trim_reason(&ui::elide_paths(reason, 0));
            return (format!("apply failed: nothing written — {reason}"), true);
        }
        let mut msg = format!("{} file(s) written", self.written);
        if self.removed > 0 {
            msg.push_str(&format!(", {} removed", self.removed));
        }
        if self.skipped > 0 {
            msg.push_str(&format!(", {} skipped: conflicting", self.skipped));
        }
        (msg, false)
    }
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
    /// The listed ports, in list order. The `── N ok ──` divider is not
    /// one of them: `list_state` selects an index in here, and only the
    /// drawing maps it to a row of the list (`list_row`).
    pub visible: Vec<PortKey>,
    /// Where the divider sits: the index in `visible` of the first ok port,
    /// set in problems-first order when ports of both kinds are listed.
    pub divider: Option<usize>,
    pub list_state: ListState,
    pub focus: Focus,
    pub filter: String,
    pub editor_rows: Vec<EditorRow>,
    pub editor_idx: usize,
    /// The port `editor_rows` were built for: a rebuild for the same port
    /// keeps the cursor's option, one for another lands it afresh.
    pub editor_port: Option<PortKey>,
    pub message: Option<(String, bool)>,
    /// The message was set since the last key: news from the background
    /// does not take its place unless it is an error.
    pub message_unread: bool,
    /// The first-run summary while it is the message: the status bar fits
    /// it to the width at whole tokens, which a plain message is not.
    pub summary: Option<Summary>,
    pub modal: Option<ApplyModal>,
    pub quit_confirm: bool,
    /// Bulk-decision prompt: the text typed so far while it is open.
    pub bulk: Option<String>,
    /// Dependency loops in the current closure, recomputed after every
    /// background refresh: a toggle can create or break one by changing
    /// which dependencies a port pulls in.
    pub loops: Vec<DepLoop>,
    /// Ports hidden because they have no options (counted in the list's
    /// title).
    pub hidden: usize,
    /// When set, ports needing no attention (status ok) are not listed.
    pub hide_ok: bool,
    /// The port description panel as chosen with 'V', until quit; None
    /// while no choice was made, when the panel follows the terminal's
    /// height (`about_shown`).
    pub about_choice: Option<bool>,
    /// Rows of the terminal at the last frame, kept by the drawing code;
    /// zero before the first frame.
    pub screen_rows: u16,
    /// Whether the panel was shown when its text was last synced: a frame
    /// that turns it on (a resize) reads the selected port's pkg-descr.
    pub about_synced: bool,
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
    /// The selected port as a background refresh found it: listed whatever
    /// t or L say until the selection moves to another port or a view key
    /// is pressed, so a fix that makes it ok does not swap the pane's port.
    pub held: Option<PortKey>,
    /// OPTIONS_NAMEs of ports a re-query brought in, whose real options
    /// file staging does not hold yet: they were queried without it, so the
    /// next flush copies it in and queries them again.
    pub to_seed: BTreeSet<String>,
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
        divider: None,
        list_state: ListState::default(),
        focus: Focus::List,
        filter: String::new(),
        editor_rows: Vec::new(),
        editor_idx: 0,
        editor_port: None,
        message: None,
        message_unread: false,
        summary: None,
        modal: None,
        quit_confirm: false,
        bulk: None,
        loops,
        hidden,
        hide_ok: false,
        about_choice: None,
        screen_rows: 0,
        about_synced: false,
        about_text: HashMap::new(),
        only_loops: false,
        sort_problems_first: true,
        mc_relax: false,
        warn_mc: false,
        listable: 0,
        held: None,
        to_seed: BTreeSet::new(),
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
    let draft = app.restore_draft_if_any();
    app.rebuild_visible(None);
    app.rebuild_editor();
    app.show_summary(draft);
    app
}

/// The first-run message: what the closure needs, in list order, after the
/// restored draft if there is one. Kept in parts so the status bar can cut
/// it at a whole token, the draft part always kept.
pub struct Summary {
    /// The restored draft, short and with how to revert it.
    pub draft: Option<(String, String)>,
    /// The counts, or the all-configured line.
    pub items: Vec<String>,
    /// The keys to go on with.
    pub hints: Vec<String>,
}

impl Summary {
    /// The line in at most `width` columns: the draft part whole (with how
    /// to revert only when everything fits), then as many whole tokens as
    /// fit, a `…` standing for the rest.
    pub fn fit(&self, width: usize) -> String {
        let len = |s: &str| s.chars().count();
        if let Some((_, long)) = &self.draft {
            let full = self.line(long);
            if len(&full) <= width {
                return full;
            }
        }
        let head = self.draft.as_ref().map(|(short, _)| short.clone()).unwrap_or_default();
        let tokens = self.tokens(!head.is_empty());
        let mut out = head;
        for (i, (sep, tok)) in tokens.iter().enumerate() {
            let last = i + 1 == tokens.len();
            let room = if last { 0 } else { len(" …") };
            if len(&out) + len(sep) + len(tok) + room > width {
                out.push_str(if out.is_empty() { "…" } else { " …" });
                return out;
            }
            out.push_str(sep);
            out.push_str(tok);
        }
        out
    }

    /// The whole line, with `head` as the draft part.
    fn line(&self, head: &str) -> String {
        let mut out = head.to_string();
        for (sep, tok) in self.tokens(!head.is_empty()) {
            out.push_str(sep);
            out.push_str(&tok);
        }
        out
    }

    /// Each token after the draft part, with the separator before it.
    fn tokens(&self, after_head: bool) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        for (i, item) in self.items.iter().enumerate() {
            let sep = if i == 0 && !after_head { "" } else { " · " };
            out.push((sep, item.clone()));
        }
        for (i, hint) in self.hints.iter().enumerate() {
            let sep = if i > 0 {
                " · "
            } else if out.is_empty() && !after_head {
                ""
            } else {
                " — "
            };
            out.push((sep, hint.clone()));
        }
        out
    }
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
    // A key acts on the port the pane shows.
    app.sync_editor();
    // A message lasts until the next key: left up, it would hide the key
    // legend for the rest of the session.
    app.clear_message();

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
        // Ctrl-U clears the line, as in a shell; read before the folding
        // made it no key, and nowhere else.
        if ctrl && key.code == KeyCode::Char('u') {
            if let Some(input) = app.bulk.as_mut() {
                input.clear();
            }
            return KeyOutcome::Continue;
        }
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
            // An arrow accepts the filter as Enter does, and moves on the
            // list it left: type a few letters, then arrow to the port.
            KeyCode::Up | KeyCode::Down => {
                app.focus = Focus::List;
                app.move_selection(if code == KeyCode::Up { -1 } else { 1 });
            }
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
        // A port whose file takes no edit keeps its state; say why.
        KeyCode::Char('d') => match app.selected_key().and_then(|k| app.session.edit_refusal(&k)) {
            Some(why) => app.flash(&why, true),
            None => app.reset_selected(Session::reset_to_defaults, "reset to port defaults"),
        },
        KeyCode::Char('t') => app.toggle_view(
            |a| &mut a.hide_ok,
            "showing only ports needing attention (t to show all)",
            "showing all ports",
        ),
        KeyCode::Char('V') => {
            // A choice that holds until quit, whatever the terminal's size.
            let shown = !app.about_shown();
            app.about_choice = Some(shown);
            app.about_synced = shown;
            app.flash(
                if shown {
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
                app.toggle_view(
                    |a| &mut a.only_loops,
                    "showing only ports caught in a dependency loop (L to show all)",
                    "showing all ports",
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
        // Space toggles an option, and the list highlights none.
        KeyCode::Char(' ') => app.flash("Enter: edit the options", false),
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
    let Some(modal) = &app.modal else { return };
    if modal.done.is_some() {
        // The list was rebuilt as the apply ended; what it did stays in the
        // status bar until the next key.
        if let Some(done) = app.modal.take().and_then(|m| m.done) {
            let (msg, error) = done.message();
            app.flash(&msg, error);
        }
        return;
    }
    let conflicts = !modal.conflicted.is_empty();
    match code {
        KeyCode::Char('Y') => commit_apply(app, false),
        KeyCode::Char('y') if !conflicts => commit_apply(app, false),
        KeyCode::Char('s') if conflicts => commit_apply(app, true),
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => app.modal = None,
        _ => match scroll_key(code, modal.scroll, app.page.apply) {
            // In entries, clamped at draw time to the last page.
            Some(at) => {
                if let Some(modal) = app.modal.as_mut() {
                    modal.scroll = at;
                }
            }
            // No answer: the preview stays open and says which keys are.
            // Enter by habit must not cancel, and with conflicts a
            // lowercase y must not write.
            None => app.flash(
                if conflicts {
                    "conflicts remain: Y writes anyway · s skips them · n cancels"
                } else {
                    "y writes · n cancels"
                },
                false,
            ),
        },
    }
}

/// Write the apply plan the preview shows. It is recomputed now: a
/// background refresh may have merged new ports, or changed some, since the
/// preview was opened, and then nothing is written — the preview shows the
/// new plan and asks again. `skip_conflicting` leaves the files of ports in
/// conflict as they are on disk, their edits staged.
fn commit_apply(app: &mut App, skip_conflicting: bool) {
    let mut planned = app.compute_writes();
    let (conflicted, files) = app.conflicting(&planned);
    if app.modal.as_ref().is_some_and(|m| !m.shows(&planned, &conflicted)) {
        app.modal = None;
        app.open_apply_modal();
        if app.modal.is_some() {
            app.flash("the plan changed since the preview (a re-query): check it and answer again", true);
        } else {
            app.flash("the plan changed since the preview (a re-query): nothing to write now", true);
        }
        return;
    }
    let skipped = if skip_conflicting && !conflicted.is_empty() {
        planned = planned.without(&files);
        conflicted.len()
    } else {
        0
    };
    let done = apply::commit(&planned.writes, &planned.removals);
    let result = ApplyDone {
        failed: done.summary.failed,
        written: done.summary.written,
        skipped,
        removed: done.removed.count,
        rm_failed: done.removed.failed,
        notes: done.removed.notes,
    };
    // apply is all-or-nothing: after a failed write nothing is on disk, so
    // nothing is removed either, and the edits stay staged (and in the
    // draft) to be applied again.
    if result.failed.is_empty() {
        // Only the files apply touched are read again; the rest are as the
        // scan read them, and a skipped port keeps its edits staged.
        let touched: Vec<&str> = planned.touched().collect();
        app.options_files.forget(touched.iter().copied());
        app.session.reload_saved(&app.options_files, touched.iter().copied());
        // The files on disk changed: attributions built on the pre-apply
        // staged history would now point at the wrong changes.
        app.blame.clear();
        // The edits are on disk now, and the draft has served its purpose
        // — unless some were skipped and are still to be applied.
        if skipped == 0 {
            draft::discard(&app.options_dir);
        }
        // Statuses changed: the list, its divider and its order are
        // rebuilt now, under the result box, as the view says — the
        // result box says what became of the ports it lets go.
        app.change_view();
    }
    if let Some(modal) = app.modal.as_mut() {
        modal.done = Some(result);
    }
}

impl App {
    pub fn selected_key(&self) -> Option<PortKey> {
        self.list_state.selected().and_then(|i| self.visible.get(i)).cloned()
    }

    /// The list's row for the port at index `idx` of `visible`: one further
    /// down past the divider.
    pub(crate) fn list_row(&self, idx: usize) -> usize {
        match self.divider {
            Some(d) if idx >= d => idx + 1,
            _ => idx,
        }
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

    /// Rebuild the list and the editor after a change to what is listed (a
    /// refresh, an edit, an apply), keeping the selection on the same port:
    /// the selected port stays listed, whatever t or L say, until the
    /// selection moves to another one.
    fn refresh_view(&mut self) {
        self.held = self.selected_key();
        if let Some(why) = self.relist() {
            self.flash(&why, false);
        }
    }

    /// Rebuild the list and the editor after a view key changed how it is
    /// listed, or an apply what needs attention: the selected port stays
    /// only if the view lists it.
    fn change_view(&mut self) -> Option<String> {
        self.held = None;
        self.relist()
    }

    /// Rebuild the list and the editor, keeping the selection on the same
    /// port when it is still listed. When it is not, and the option pane
    /// had the focus, the focus goes back to the list rather than leave the
    /// pane on another port: what is returned says why, for the line.
    fn relist(&mut self) -> Option<String> {
        let keep = self.selected_key();
        self.rebuild_visible(keep.clone());
        let mut why = None;
        if let Some(key) = keep.filter(|k| Some(k) != self.selected_key().as_ref())
            && self.focus == Focus::Editor
        {
            self.focus = Focus::List;
            let reason = match self.session.ports.get(&key) {
                None => "left the closure",
                Some(info) if !info.options.has_options() => "has no options any more",
                Some(_) => "is not in this view",
            };
            why = Some(format!("{key} {reason}: back to the list"));
        }
        self.rebuild_editor();
        why
    }

    /// Flip one of the list's view flags, refresh, and say which way it went.
    fn toggle_view(&mut self, flag: fn(&mut App) -> &mut bool, on: &str, off: &str) {
        let flag = flag(self);
        *flag = !*flag;
        let now = *flag;
        let why = self.change_view();
        let said = if now { on } else { off };
        match why {
            Some(why) => self.flash(&format!("{said} — {why}"), false),
            None => self.flash(said, false),
        }
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
        self.blacklist.matches(&info.canonical, info.is_default_flavor())
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
        // Moved away: the port a refresh held goes at the next rebuild.
        if self.held.is_some() && self.selected_key() != self.held {
            self.held = None;
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
            .filter(|(key, _)| {
                !self.only_loops || self.loop_of(key).is_some() || self.held.as_ref() == Some(*key)
            })
            .map(|(key, info)| (self.effective_status(info), key.clone()))
            .collect();
        if self.sort_problems_first {
            items.sort();
        } else {
            items.sort_by(|a, b| a.1.cmp(&b.1));
        }
        self.listable = items.len();
        if self.hide_ok {
            let held = self.held.clone();
            items.retain(|(status, key)| *status != UiStatus::Ok || held.as_ref() == Some(key));
        }
        // Ok sorts last, so the ok ports (≈ and ✓) are the tail.
        let first_ok = items.iter().position(|(status, _)| *status == UiStatus::Ok);
        self.divider = first_ok.filter(|&i| self.sort_problems_first && i > 0);
        self.visible = items.into_iter().map(|(_, k)| k).collect();
        let idx = keep
            .and_then(|k| self.visible.iter().position(|v| *v == k))
            .unwrap_or(0);
        if self.visible.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(idx));
        }
        // Held only while selected.
        if self.held.is_some() && self.selected_key() != self.held {
            self.held = None;
        }
    }

    /// Rebuild the editor rows when they were built for another port than
    /// the selected one: the pane never shows, nor edits, one port's rows
    /// under another.
    pub(crate) fn sync_editor(&mut self) {
        if self.selected_key() != self.editor_port {
            self.rebuild_editor();
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
        let port = self.selected_key();
        // Another port: the cursor lands afresh, whatever option it was on.
        let previous = previous.filter(|_| port.is_some() && port == self.editor_port);
        self.editor_port = port.clone();
        let Some(key) = port else { return };
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
        if let Some(state) = self.session.state(info)
            && let Some(saved) = &state.saved
        {
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
        // Restore the cursor onto the same option if it still exists.
        if let Some(prev) = previous
            && let Some(i) = self
                .editor_rows
                .iter()
                .position(|r| matches!(r, EditorRow::Option(o) if *o == prev))
        {
            self.editor_idx = i;
            return;
        }
        self.editor_select_first();
    }

    /// Put the cursor on the port's first option needing attention: for a
    /// conflicting port, the first member in row order of the first rule
    /// it breaks; else for a stale one, its first new option (`N`); else
    /// its first option. Never on a header.
    fn editor_select_first(&mut self) {
        let row_of = |rows: &[EditorRow], pick: &dyn Fn(&str) -> bool| {
            rows.iter().position(|r| matches!(r, EditorRow::Option(o) if pick(o)))
        };
        let first = row_of(&self.editor_rows, &|_| true);
        let attention = self.selected_key().and_then(|key| {
            let info = &self.session.ports[&key];
            let conflict = self
                .session
                .conflicts(info)
                .iter()
                .find_map(|c| row_of(&self.editor_rows, &|o| c.members.iter().any(|m| m == o)));
            conflict.or_else(|| {
                let stale = self
                    .session
                    .stale_delta(info)
                    .is_some_and(|(added, removed)| !added.is_empty() || !removed.is_empty());
                let saved = self.session.state(info)?.saved.as_ref()?;
                stale.then(|| row_of(&self.editor_rows, &|o| !saved.decides(o))).flatten()
            })
        });
        self.editor_idx = attention.or(first).unwrap_or(0);
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
        // The option under the cursor, to compare it across flavors.
        let option = match self.editor_rows.get(self.editor_idx) {
            Some(EditorRow::Option(name)) if self.focus == Focus::Editor => Some(name.clone()),
            _ => None,
        };
        match self.visible.iter().position(|v| *v == next) {
            Some(idx) => {
                self.select_index(idx);
                // Same option on the other flavor when it has it; else the
                // pane lands as on entering it.
                let same = option.and_then(|o| {
                    self.editor_rows.iter().position(|r| matches!(r, EditorRow::Option(n) if *n == o))
                });
                if let Some(i) = same {
                    self.editor_idx = i;
                }
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
    /// target (filter, L, t) is widened as far as needed — the user asked to
    /// go there. A port no view lists (it has no options) is not jumped to:
    /// the view, the selection and the pane stay as they are.
    fn jump_to_port(&mut self, target: &PortKey) {
        let Some(info) = self.session.ports.get(target) else {
            self.flash(&format!("{target}: not in the closure"), true);
            return;
        };
        if let Some(idx) = self.visible.iter().position(|v| v == target) {
            self.select_index(idx);
            self.flash(&format!("jumped to {target}"), false);
            return;
        }
        if !info.options.has_options() {
            self.flash(&format!("{target} has no options and is not listed: not jumped to"), true);
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
        // A port with options is listed once no view hides it.
        self.rebuild_editor();
        self.flash(&format!("jumped to {target} ({})", widened.join(", ")), false);
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

    /// Whether the description panel shows: as chosen with 'V', else only
    /// on a terminal of ABOUT_MIN_ROWS rows or more, where it leaves the
    /// option rows room enough.
    pub fn about_shown(&self) -> bool {
        self.about_choice.unwrap_or(self.screen_rows >= ABOUT_MIN_ROWS)
    }

    /// Take the terminal's height at a frame, before it is drawn. A frame
    /// that turns the panel on (the first one, or a resize) reads the
    /// selected port's pkg-descr, once: only a change does. The first-run
    /// summary stays the message, as it did when the read came at start.
    pub(crate) fn note_screen(&mut self, rows: u16) {
        self.screen_rows = rows;
        let shown = self.about_shown();
        if shown == self.about_synced {
            return;
        }
        self.about_synced = shown;
        match self.summary.take() {
            Some(summary) => {
                let (message, unread) = (self.message.clone(), self.message_unread);
                self.sync_about();
                self.summary = Some(summary);
                self.message = message;
                self.message_unread = unread;
            }
            None => self.sync_about(),
        }
    }

    /// Read the selected port's pkg-descr for the panel, when the panel is
    /// on and has not read it yet. A file that cannot be read says so and is
    /// tried again next time.
    fn sync_about(&mut self) {
        if !self.about_shown() {
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

    /// What the bulk prompt works on, for its title: the listed ports, and
    /// what narrows them when anything does (the filter, t, L).
    pub(crate) fn bulk_scope(&self) -> String {
        let n = self.visible.len();
        let mut out = format!("bulk over the {n} listed port{}", if n == 1 { "" } else { "s" });
        if !self.filter.is_empty() {
            out.push_str(&format!(" (filter: {})", self.filter));
        }
        if self.hide_ok {
            out.push_str(" (attention only)");
        }
        if self.only_loops {
            out.push_str(" (in a loop)");
        }
        out
    }

    /// The option the bulk input names so far, and how many listed ports
    /// carry it; None while the input names none.
    pub(crate) fn bulk_carriers(&self) -> Option<(String, usize)> {
        let opt = bulk_option(self.bulk.as_deref()?)?;
        let n = self
            .visible
            .iter()
            .filter(|k| self.session.ports[*k].options.complete.contains(&opt))
            .count();
        Some((opt, n))
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
            if let Some(name) = self.session.ports.get(key).map(|i| i.options_name.clone())
                && let Some(staged) = before.get(&name).cloned()
            {
                self.push_undo_entry(name, staged);
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
    /// Returns how many ports it restored and how old it is, for the
    /// first-run message.
    fn restore_draft_if_any(&mut self) -> Option<(usize, String)> {
        let draft = draft::load(&self.options_dir)?;
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
            return None;
        }
        let owners: Vec<PortKey> =
            draft.staged.keys().filter_map(|name| self.session.owners.get(name).cloned()).collect();
        for key in owners {
            self.mark_pending(&key);
        }
        Some((restored, draft::age_label(draft.age_secs())))
    }

    /// Say, once the first scan is in, what the closure needs: the counts
    /// of each kind of port waiting on a decision, in list order, after the
    /// restored draft if any. Each port counts by its state, edits or not
    /// (an edit is not a decision applied); ≠ needs the w view, off at start.
    fn show_summary(&mut self, draft: Option<(usize, String)>) {
        let mut counts = [0usize; 5];
        for key in &self.visible {
            counts[self.effective_status(&self.session.ports[key]) as usize] += 1;
        }
        let mut items: Vec<String> = [
            (UiStatus::Conflict as usize, "conflicting"),
            (UiStatus::Unconfigured as usize, "never configured"),
            (UiStatus::Stale as usize, "stale"),
        ]
        .into_iter()
        .filter(|(status, _)| counts[*status] > 0)
        .map(|(status, label)| format!("{} {label}", counts[status]))
        .collect();
        // A listed origin MOVED moves and that was not followed is in no
        // row: the list names a port that is gone, and needs fixing.
        let flagged = self.session.listed_moves.iter().filter(|m| !m.followed).count();
        if flagged > 0 {
            items.insert(0, format!("{flagged} listed moved (fix the list)"));
        }
        let draft = draft.map(|(n, age)| {
            let ports = if n == 1 { "port" } else { "ports" };
            (
                format!("restored draft ({n} {ports}, {age})"),
                format!("restored draft ({n} {ports}, {age}, U per port reverts)"),
            )
        });
        let next = if draft.is_some() { "n: next" } else { "n: next problem" };
        let hints = if items.is_empty() && draft.is_none() {
            items.push(format!("all {} ports are configured", self.visible.len()));
            vec!["?: help".to_string()]
        } else {
            // The ports never configured take their defaults in one go.
            let defaults = (counts[UiStatus::Unconfigured as usize] > 0)
                .then(|| "a: write defaults for all".to_string());
            defaults.into_iter().chain([next.to_string(), "?: help".to_string()]).collect()
        };
        let summary = Summary { draft, items, hints };
        self.flash(&summary.fit(usize::MAX), false);
        self.summary = Some(summary);
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
            // A flavor make.conf moves OPTIONS_FILE for reads a file of its
            // own, not the staged one: its state is no content for the
            // shared file the other flavors are queried with.
            if crate::session::moved(info).is_some() {
                batch.push(key);
                continue;
            }
            let options_name = info.options_name.clone();
            let staged = state.staged.clone();
            // A newcomer left as its file has it gets the file itself, byte
            // for byte as the scan's cache knows it; an edited one, the
            // edit.
            let written = if self.to_seed.contains(&options_name) && staged == state.baseline {
                self.staging_db.seed(&self.options_files, &options_name).map(|_| ())
            } else {
                let local = if self.to_seed.remove(&options_name) {
                    self.staging_db.seed_local(&self.options_files, &options_name)
                } else {
                    Ok(())
                };
                let content = optionsfile::render(&info.pkgname, &info.options.complete, &staged);
                local.and_then(|()| self.staging_db.write(&options_name, content.as_bytes()))
            };
            if let Err(e) = written {
                self.notify(&format!("staging write failed: {e:#}"), true);
                continue;
            }
            self.record_snapshot(options_name, staged);
            batch.push(key);
        }
        for key in &batch {
            if let Some(info) = self.session.ports.get(key) {
                self.to_seed.remove(&info.options_name);
            }
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
                    let (a, r) = merged.get_or_insert((Vec::new(), Vec::new()));
                    a.extend(added);
                    r.extend(removed);
                    if let Some((key, msg)) = first_error {
                        self.notify(
                            &format!("refresh: {errors} dep error(s), e.g. {key}: {msg}"),
                            true,
                        );
                        alerted = true;
                    }
                    // A port that just broke outranks dep errors on the line.
                    if let Some(msg) = blamed {
                        self.notify(&msg, true);
                        alerted = true;
                    }
                }
            }
        }
        if self.refreshing == 0 {
            self.refresh_progress = None;
        }
        if let Some((added, removed)) = merged {
            // A newcomer was queried without its real options file, which
            // staging never had: it is copied in and the port queried again.
            for key in &added {
                let Some(info) = self.session.ports.get(key) else { continue };
                // One reading a file of its own was queried with it.
                if crate::session::moved(info).is_some() {
                    continue;
                }
                let name = &info.options_name;
                let real = self.options_files.bytes(name).is_some()
                    || self.options_files.local_bytes(name).is_some();
                if !self.staging_db.has(name) && real {
                    self.to_seed.insert(name.clone());
                    self.mark_pending(key);
                }
            }
            // The list's title counts the optionless ports the merge left.
            self.hidden =
                self.session.ports.values().filter(|p| !p.options.has_options()).count();
            if !(added.is_empty() && removed.is_empty()) && !alerted {
                self.notify(&refreshed_message(&added, &removed), false);
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
                self.notify(&msg, true);
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
                    self.notify(msg, false);
                }
            }
            None => {}
        }
    }

    /// A key was pressed: whatever was said has been read.
    fn clear_message(&mut self) {
        self.message = None;
        self.message_unread = false;
        self.summary = None;
    }

    /// Say something in answer to a key.
    fn flash(&mut self, msg: &str, error: bool) {
        self.message = Some((msg.to_string(), error));
        self.message_unread = true;
        self.summary = None;
    }

    /// Say something no key asked for (a refresh, a loop). It does not take
    /// the place of a message set since the last key, which the user has
    /// not had the time to read — unless it is an error; and nothing but an
    /// error takes the place of an error.
    fn notify(&mut self, msg: &str, error: bool) {
        if let Some((_, shown_error)) = &self.message
            && !error && (self.message_unread || *shown_error)
        {
            return;
        }
        self.flash(msg, error);
    }
}

/// What a refresh changed in the closure: the ports by name when there are
/// at most three of them, else how many came in and went.
fn refreshed_message(added: &[PortKey], removed: &[PortKey]) -> String {
    if added.len() + removed.len() > 3 {
        return format!("dependencies refreshed: +{} −{} port(s)", added.len(), removed.len());
    }
    let names: Vec<String> = added
        .iter()
        .map(|k| format!("+{k}"))
        .chain(removed.iter().map(|k| format!("−{k}")))
        .collect();
    format!("dependencies refreshed: {}", names.join(" "))
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
/// An error quotes the whole input, then says what was expected.
fn parse_bulk(input: &str) -> Result<(String, bool), String> {
    const SYNTAX: &str = "expected OPTION=on|off (or OPTION+ / OPTION-)";
    let input = input.trim();
    let fail = |reason: &str| Err(format!("\"{input}\": {reason}"));
    let (name, on) = if let Some((name, value)) = input.split_once('=') {
        match value.trim().to_lowercase().as_str() {
            "on" => (name, true),
            "off" => (name, false),
            _ => return fail(SYNTAX),
        }
    } else if let Some(name) = input.strip_suffix('+') {
        (name, true)
    } else if let Some(name) = input.strip_suffix('-') {
        (name, false)
    } else {
        return fail(SYNTAX);
    };
    let name = name.trim().to_uppercase();
    if name.is_empty() {
        return fail(&format!("no option name — {SYNTAX}"));
    }
    Ok((name, on))
}

/// The option a bulk input names so far, uppercased as `parse_bulk` reads
/// it: what comes before `=`, or before a trailing `+`/`-`, or the whole
/// text while it is still being typed. None while it names nothing.
fn bulk_option(input: &str) -> Option<String> {
    let input = input.trim();
    let name = match input.split_once('=') {
        Some((name, _)) => name,
        None => input.strip_suffix(['+', '-']).unwrap_or(input),
    };
    let name = name.trim().to_uppercase();
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests;
