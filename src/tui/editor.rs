//! The option editor's rows, its cursor and the toggle.

use std::collections::BTreeSet;

use super::keys::page_step;
use super::{App, EditorRow, Focus};

impl App {
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
    pub(super) fn editor_select_first(&mut self) {
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
    pub(super) fn editor_steps(&mut self, dir: isize, n: usize) {
        for _ in 0..n.min(self.editor_rows.len()) {
            self.editor_move(dir);
        }
    }

    /// A page of rows in direction `dir`: the option at most a page away,
    /// or the next one when the page holds none, stopping at the edges.
    pub(super) fn editor_page(&mut self, dir: isize) {
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

    pub(super) fn editor_move(&mut self, dir: isize) {
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

    pub(super) fn toggle_current(&mut self) {
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
    pub(super) fn jump_flavor(&mut self) {
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
}
