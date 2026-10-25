//! The interactive interface: the app state the panes draw from, the app
//! built over a scanned session, and the event loop.

mod apply_modal;
mod driver;
mod editor;
mod keys;
mod overlays;
mod refresh;
mod ui;
mod undo;

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::widgets::ListState;

pub use driver::run_driver;

use crate::deps::DepLoop;
use crate::model::origin::PortKey;
use crate::optfiles::OptionsFiles;
use crate::query::refresher::Refresher;
use crate::session::{Session, UiStatus};
use crate::staging::StagingDb;

use apply_modal::ApplyModal;
use keys::{KeyOutcome, clamp_u16, dispatch_key};
use overlays::WhyInfo;

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
        anyhow::bail!(
            "the TUI needs a real terminal; use `optique scan` or `optique sync` in scripts"
        );
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
    let mut app = build_app(
        session, files, staging_db, refresher, blacklist, overlays, minimal,
    );
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
    let stopped = crate::query::makerunner::quit_makes();
    if stopped > 0 {
        errln!("optique: stopped {stopped} in-flight make process(es)");
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
    let hidden = session
        .ports
        .values()
        .filter(|p| !p.options.has_options())
        .count();
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
        let head = self
            .draft
            .as_ref()
            .map(|(short, _)| short.clone())
            .unwrap_or_default();
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
        let Event::Key(key) = event::read()? else {
            continue;
        };
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

impl App {
    pub fn selected_key(&self) -> Option<PortKey> {
        self.list_state
            .selected()
            .and_then(|i| self.visible.get(i))
            .cloned()
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
        self.blacklist
            .matches(&info.canonical, info.is_default_flavor())
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
        if self.warn_mc && status == UiStatus::Ok && !self.session.mc_deviations(info).is_empty() {
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
            self.list_state
                .select(Some(idx.min(self.visible.len() - 1)));
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
        let flagged = self
            .session
            .listed_moves
            .iter()
            .filter(|m| !m.followed)
            .count();
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
        let next = if draft.is_some() {
            "n: next"
        } else {
            "n: next problem"
        };
        let hints = if items.is_empty() && draft.is_none() {
            items.push(format!("all {} ports are configured", self.visible.len()));
            vec!["?: help".to_string()]
        } else {
            // The ports never configured take their defaults in one go.
            let defaults = (counts[UiStatus::Unconfigured as usize] > 0)
                .then(|| "a: write defaults for all".to_string());
            defaults
                .into_iter()
                .chain([next.to_string(), "?: help".to_string()])
                .collect()
        };
        let summary = Summary {
            draft,
            items,
            hints,
        };
        self.flash(&summary.fit(usize::MAX), false);
        self.summary = Some(summary);
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
            && !error
            && (self.message_unread || *shown_error)
        {
            return;
        }
        self.flash(msg, error);
    }
}

#[cfg(test)]
mod tests;
