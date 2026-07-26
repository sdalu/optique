mod ui;

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;

use crate::apply::{self, PendingWrite};
use crate::model::origin::PortKey;
use crate::optionsfile;
use crate::query::refresher::{Refresher, RefreshEvent};
use crate::session::{Session, UiStatus};
use crate::staging::StagingDb;

/// How long after the last toggle before the port is re-queried.
const REFRESH_DEBOUNCE: Duration = Duration::from_millis(300);

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
    pub warnings: Vec<String>,
    pub scroll: usize,
    pub conflicted: Vec<PortKey>,
    /// Result text once applied.
    pub done: Option<String>,
}

/// "Why is this port here?" overlay content, computed when it is opened.
pub struct WhyInfo {
    pub key: PortKey,
    /// Shortest root → port dependency chain, None when unreachable.
    pub chain: Option<Vec<PortKey>>,
    pub dependents: Vec<PortKey>,
}

pub struct App {
    pub session: Session,
    pub options_dir: PathBuf,
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
    /// Ports hidden because they have no options (status-bar info).
    pub hidden: usize,
    /// When set, ports needing no attention (status ok) are not listed.
    pub hide_ok: bool,
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
    pub show_help: bool,
    /// Dependency-chain ("why") overlay, when open.
    pub why: Option<WhyInfo>,
    /// Option-detail overlay: (port, option name). The data is looked up at
    /// draw time so a background refresh keeps the popup current.
    pub opt_info: Option<(PortKey, String)>,
    /// Ports awaiting a debounced background re-query.
    pub pending: HashMap<PortKey, Instant>,
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
    options_dir: PathBuf,
    staging_db: StagingDb,
    refresher: Refresher,
) -> Result<()> {
    ensure_terminal()?;
    let hidden = session.ports.values().filter(|p| !p.options.has_options()).count();
    let mut app = App {
        session,
        options_dir,
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
        hidden,
        hide_ok: false,
        sort_problems_first: true,
        mc_relax: false,
        warn_mc: false,
        listable: 0,
        staging_db,
        refresher,
        show_help: false,
        why: None,
        opt_info: None,
        pending: HashMap::new(),
        refreshing: 0,
        refresh_progress: None,
    };
    app.rebuild_visible(None);
    app.rebuild_editor();

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app);
    ratatui::restore();
    result
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

        // Ctrl-L: full repaint to clear terminal artifacts — works in every
        // mode (modal, filter, help) without consuming any state.
        if key.code == KeyCode::Char('l') && key.modifiers.contains(KeyModifiers::CONTROL) {
            terminal.clear()?;
            continue;
        }

        // Modal and quit-confirm grab all keys.
        if app.quit_confirm {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => return Ok(()),
                _ => app.quit_confirm = false,
            }
            continue;
        }
        if app.modal.is_some() {
            handle_modal_key(app, key.code);
            continue;
        }
        if app.show_help {
            app.show_help = false;
            continue;
        }
        if app.why.is_some() {
            app.why = None;
            continue;
        }
        if app.opt_info.is_some() {
            app.opt_info = None;
            continue;
        }
        // Ctrl-C always quits (with confirm if dirty) — checked before the
        // filter branch so it can't be swallowed as a literal 'c'.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if app.session.dirty() {
                app.quit_confirm = true;
                continue;
            }
            return Ok(());
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
            continue;
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
            continue;
        }

        match key.code {
            KeyCode::Char('q') => {
                if app.session.dirty() {
                    app.quit_confirm = true;
                } else {
                    return Ok(());
                }
            }
            KeyCode::Char('/') => app.focus = Focus::Filter,
            KeyCode::Char('?') | KeyCode::F(1) => app.show_help = true,
            KeyCode::Char('a') => app.open_apply_modal(),
            KeyCode::Char('B') => app.open_bulk(),
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
        KeyCode::PageDown => app.move_selection(15),
        KeyCode::PageUp => app.move_selection(-15),
        KeyCode::Char('n') => app.jump_problem(1),
        KeyCode::Char('p') => app.jump_problem(-1),
        KeyCode::Char('h') => app.show_help = true,
        KeyCode::Char('f') => app.jump_flavor(),
        KeyCode::Char('r') => app.open_why(),
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
        KeyCode::Esc | KeyCode::Char('h') | KeyCode::Tab | KeyCode::Left => {
            app.focus = Focus::List
        }
        KeyCode::Char('j') | KeyCode::Down => app.editor_move(1),
        KeyCode::Char('k') | KeyCode::Up => app.editor_move(-1),
        KeyCode::Char(' ') | KeyCode::Enter => app.toggle_current(),
        KeyCode::Char('f') => app.jump_flavor(),
        KeyCode::Char('r') => app.open_why(),
        KeyCode::Char('i') => app.open_opt_info(),
        KeyCode::Char('d') => {
            if let Some(key) = app.selected_key() {
                app.session.reset_to_defaults(&key);
                app.mark_pending(&key);
                app.flash("reset to port defaults", false);
            }
        }
        KeyCode::Char('u') => {
            if let Some(key) = app.selected_key() {
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
            let writes = app.compute_writes().writes;
            let summary = apply::apply(&writes);
            let mut text =
                format!("{} file(s) written to {}", summary.written, app.options_dir.display());
            for (key, msg) in &summary.failed {
                text.push_str(&format!("\nFAILED {key}: {msg}"));
            }
            app.session.reload_saved(&app.options_dir);
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

    /// Status with the mc_relax / warn_mc view rules applied.
    pub fn effective_status(&self, info: &crate::model::port::PortInfo) -> UiStatus {
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
        match self.session.toggle(&key, &opt) {
            Ok(()) => {
                self.mark_pending(&key);
                match warn {
                    Some(w) => self.flash(&w, true),
                    None => self.message = None,
                }
            }
            Err(e) => self.flash(&e, true),
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

    /// Compute the dependency chain and dependents of the selected port and
    /// open the "why" overlay on them (a snapshot: a background refresh may
    /// change the closure while it is displayed).
    fn open_why(&mut self) {
        let Some(key) = self.selected_key() else { return };
        self.why = Some(WhyInfo {
            chain: self.session.why_chain(&key),
            dependents: self.session.dependents(&key),
            key,
        });
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
        let (changed, skipped) = self.session.bulk_set(&keys, &opt, on);
        for key in &changed {
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

    /// Schedule the port for a debounced background re-query.
    fn mark_pending(&mut self, key: &PortKey) {
        self.pending.insert(key.clone(), Instant::now());
    }

    /// Push due pending ports to the refresher: write their staged options
    /// file into the staging db, then re-scan from them.
    fn flush_due_refreshes(&mut self) {
        if self.pending.is_empty() {
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
            let content =
                optionsfile::render(&info.pkgname, &info.options.complete, &state.staged);
            if let Err(e) = self.staging_db.write(&info.options_name, content.as_bytes()) {
                self.flash(&format!("staging write failed: {e:#}"), true);
                continue;
            }
            batch.push(key);
        }
        if !batch.is_empty() && self.refresher.tx.send(batch).is_ok() {
            self.refreshing += 1;
        }
    }

    /// Merge finished background scans into the session.
    fn drain_refresh_events(&mut self) {
        let mut merged = None;
        while let Ok(ev) = self.refresher.rx.try_recv() {
            match ev {
                RefreshEvent::Progress { done, discovered } => {
                    self.refresh_progress = Some((done, discovered));
                }
                RefreshEvent::Done { result, batches } => {
                    self.refreshing = self.refreshing.saturating_sub(batches);
                    let errors = result.errors.len();
                    let first_error = result.errors.first().cloned();
                    let (added, removed) =
                        self.session.merge(*result, &self.options_dir.clone());
                    let (a, r) = merged.get_or_insert((0usize, 0usize));
                    *a += added;
                    *r += removed;
                    if let Some((key, msg)) = first_error {
                        self.flash(
                            &format!("refresh: {errors} dep error(s), e.g. {key}: {msg}"),
                            true,
                        );
                    }
                }
            }
        }
        if self.refreshing == 0 {
            self.refresh_progress = None;
        }
        if let Some((added, removed)) = merged {
            let keep = self.selected_key();
            self.rebuild_visible(keep);
            self.rebuild_editor();
            if added > 0 || removed > 0 {
                self.flash(
                    &format!("dependencies refreshed: +{added} −{removed} port(s)"),
                    false,
                );
            }
        }
    }

    /// Plan the options files to write for the current staged state.
    fn compute_writes(&self) -> apply::PlannedWrites {
        let staged = self.session.ports.iter().filter_map(|(key, info)| {
            let state = self.session.state(info)?;
            Some((key, info, state.staged.clone()))
        });
        apply::plan_writes(staged, &self.options_dir)
    }

    fn open_apply_modal(&mut self) {
        let conflicted: Vec<PortKey> = self
            .visible
            .iter()
            .filter(|k| self.session.status(&self.session.ports[k]) == UiStatus::Conflict)
            .cloned()
            .collect();
        let planned = self.compute_writes();
        if planned.writes.is_empty() {
            self.flash("nothing to write — everything is up to date", false);
        } else {
            self.modal = Some(ApplyModal {
                writes: planned.writes,
                warnings: planned.warnings,
                scroll: 0,
                conflicted,
                done: None,
            });
        }
    }

    fn flash(&mut self, msg: &str, error: bool) {
        self.message = Some((msg.to_string(), error));
    }
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
