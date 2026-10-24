//! The undo stack, and the draft a session left behind.

use std::collections::BTreeSet;

use crate::draft;
use crate::model::origin::PortKey;

use super::App;

/// How many option-set snapshots the undo stack keeps (a session's worth of
/// mistakes; the oldest entries are dropped past this).
pub(super) const UNDO_MAX: usize = 100;

/// Push onto the undo stack, dropping the oldest entry once it is full (a
/// Vec shift at 100 entries costs nothing and keeps the newest undo on top).
pub(super) fn push_capped(
    stack: &mut Vec<(String, BTreeSet<String>)>,
    entry: (String, BTreeSet<String>),
) {
    if stack.len() >= UNDO_MAX {
        stack.remove(0);
    }
    stack.push(entry);
}

impl App {
    /// Adopt the draft saved by a previous session on this options dir, if
    /// any. Restored ports are marked pending so the background re-query
    /// picks up the dependencies their staged options pull in.
    /// Returns how many ports it restored and how old it is, for the
    /// first-run message.
    pub(super) fn restore_draft_if_any(&mut self) -> Option<(usize, String)> {
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
            if self
                .session
                .states
                .get(&name)
                .map(|s| s.staged != staged)
                .unwrap_or(false)
            {
                self.push_undo_entry(name, staged);
            }
        }
        if restored == 0 {
            // Nothing in it still applies (ports gone, or the edits already
            // landed) — a stale draft would only nag on every launch.
            draft::discard(&self.options_dir);
            return None;
        }
        let owners: Vec<PortKey> = draft
            .staged
            .keys()
            .filter_map(|name| self.session.owners.get(name).cloned())
            .collect();
        for key in owners {
            self.mark_pending(&key);
        }
        Some((restored, draft::age_label(draft.age_secs())))
    }

    /// Record `staged` as the state of `options_name` before a change, for 'u'.
    pub(super) fn push_undo_entry(&mut self, options_name: String, staged: BTreeSet<String>) {
        push_capped(&mut self.undo, (options_name, staged));
    }

    /// Snapshot the current staged set of the state a port edits (its
    /// OPTIONS_NAME, shared by the flavors using the same file). Call this
    /// *before* mutating it.
    pub(super) fn push_undo(&mut self, key: &PortKey) {
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
    pub(super) fn undo_last(&mut self) {
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
}
