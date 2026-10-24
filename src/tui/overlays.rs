//! The popups over the panes (why, option info, port help, about, bulk)
//! and the description panel.

use std::collections::{BTreeSet, HashMap};

use anyhow::Result;

use crate::deps::{DepLoop, EXTRA_SHOWN};
use crate::model::origin::PortKey;

use super::{App, EditorRow, Focus, OptInfo, PortHelp};

/// Terminal rows from which the description panel shows unless 'V' said
/// otherwise: on a shorter one its five lines would take the option rows'
/// place.
const ABOUT_MIN_ROWS: u16 = 30;

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
            tangled: extra
                .iter()
                .take(EXTRA_SHOWN)
                .map(|k| (*k).clone())
                .collect(),
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
        out.extend(
            self.dependents
                .iter()
                .take(crate::tui::ui::WHY_MAX_DEPENDENTS),
        );
        out
    }
}

/// Parse a bulk decision: `NAME=on`/`NAME=off` (value case-insensitive), or the
/// shorthands `NAME+` (on) and `NAME-` (off). The option name is uppercased.
/// An error quotes the whole input, then says what was expected.
pub(super) fn parse_bulk(input: &str) -> Result<(String, bool), String> {
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

impl App {
    /// Assemble the "why" overlay for one port: its dependency chain, the
    /// loop it is caught in, and its dependents. A snapshot — a background
    /// refresh may change the closure while it is displayed.
    pub(super) fn why_for(&self, key: PortKey) -> WhyInfo {
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
    pub(super) fn open_why(&mut self) {
        let Some(key) = self.selected_key() else {
            return;
        };
        self.why = Some(self.why_for(key));
    }

    /// Move the list selection to the given port. A view that hides the
    /// target (filter, L, t) is widened as far as needed — the user asked to
    /// go there. A port no view lists (it has no options) is not jumped to:
    /// the view, the selection and the pane stay as they are.
    pub(super) fn jump_to_port(&mut self, target: &PortKey) {
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
            self.flash(
                &format!("{target} has no options and is not listed: not jumped to"),
                true,
            );
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
        self.flash(
            &format!("jumped to {target} ({})", widened.join(", ")),
            false,
        );
    }

    /// Open the detail overlay on the option under the editor cursor. Only
    /// the port and option name are stored; everything shown is looked up at
    /// draw time so the popup follows background refreshes.
    pub(super) fn open_opt_info(&mut self) {
        let Some(key) = self.selected_key() else {
            return;
        };
        let Some(EditorRow::Option(opt)) = self.editor_rows.get(self.editor_idx) else {
            return;
        };
        self.opt_info = Some(OptInfo {
            key,
            opt: opt.clone(),
            scroll: 0,
        });
    }

    /// Open the selected port's pkg-help. Read here rather than at query time
    /// so the overlay always shows what is on disk now; only the fact that the
    /// file exists comes from the query.
    pub(super) fn open_port_help(&mut self) {
        let Some(key) = self.selected_key() else {
            return;
        };
        let Some(path) = self.session.ports[&key].pkg_help.clone() else {
            self.flash("this port ships no pkg-help", false);
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let lines = text.lines().map(str::to_string).collect();
                self.port_help = Some(PortHelp {
                    title: "pkg-help",
                    key,
                    lines,
                    scroll: 0,
                });
            }
            Err(e) => self.flash(&format!("cannot read {path}: {e}"), true),
        }
    }

    /// Whether the description panel shows: as chosen with 'V', else only
    /// on a terminal of ABOUT_MIN_ROWS rows or more, where it leaves the
    /// option rows room enough.
    pub fn about_shown(&self) -> bool {
        self.about_choice
            .unwrap_or(self.screen_rows >= ABOUT_MIN_ROWS)
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
    pub(super) fn sync_about(&mut self) {
        if !self.about_shown() {
            return;
        }
        let Some(key) = self.selected_key() else {
            return;
        };
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
        let Some(path) = &self.session.ports[key].about.pkg_descr else {
            return Ok(None);
        };
        std::fs::read_to_string(path)
            .map(Some)
            .map_err(|e| format!("cannot read {path}: {e}"))
    }

    /// Open the selected port's description: its COMMENT and WWW, then its
    /// pkg-descr as written. A port whose pkg-descr cannot be read still
    /// shows the rest.
    pub(super) fn open_port_about(&mut self) {
        let Some(key) = self.selected_key() else {
            return;
        };
        let about = &self.session.ports[&key].about;
        let mut lines: Vec<String> = Some(about.comment.clone())
            .filter(|c| !c.is_empty())
            .into_iter()
            .collect();
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
        self.port_help = Some(PortHelp {
            title: "about",
            key,
            lines,
            scroll: 0,
        });
    }

    /// Open the bulk-decision prompt, pre-filled with the option under the
    /// editor cursor when the editor has one selected.
    pub(super) fn open_bulk(&mut self) {
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
        let mut out = format!(
            "bulk over the {n} listed port{}",
            if n == 1 { "" } else { "s" }
        );
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
    pub(super) fn run_bulk(&mut self, input: &str) {
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
}
