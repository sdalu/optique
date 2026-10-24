//! The apply modal: what would be written, and writing it.

use std::collections::BTreeSet;

use crate::apply::{self, PendingWrite};
use crate::draft;
use crate::model::origin::PortKey;
use crate::session::UiStatus;

use super::refresh::trim_reason;
use super::{App, ui};

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
            ws.iter()
                .map(|w| (w.options_name.clone(), w.content.clone()))
                .collect()
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
    /// Ports whose options file could not be written, and why. With any,
    /// nothing was removed, and nothing written unless a rename failed
    /// after others had landed (`written`).
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
            let mut lines = vec![self.failed_head(dir)];
            lines.extend(self.failed.iter().map(|(key, msg)| format!("{key}: {msg}")));
            return lines;
        }
        let mut lines = vec![format!("{} file(s) written to {dir}", self.written)];
        if self.skipped > 0 {
            lines.push(format!(
                "{} port(s) skipped: conflicting options",
                self.skipped
            ));
        }
        lines.extend(
            self.rm_failed
                .iter()
                .map(|(name, e)| format!("FAILED rm {name}: {e}")),
        );
        if self.removed > 0 {
            lines.push(format!(
                "{} redundant file(s) removed (--minimal)",
                self.removed
            ));
        }
        lines.extend(self.notes.iter().map(|note| format!("note: {note}")));
        lines
    }

    /// A failed apply's first line: nothing written to `dir`, or, when a
    /// rename failed after others had landed, how many were.
    pub fn failed_head(&self, dir: &str) -> String {
        match self.written {
            0 => format!("nothing written to {dir}"),
            n => format!("{n} file(s) written to {dir}, then apply stopped"),
        }
    }

    /// The status-bar message left once the result box closes, and whether
    /// it is an error: what was written, or why nothing was, its paths cut
    /// to their last component.
    pub fn message(&self) -> (String, bool) {
        if let Some((_, reason)) = self.failed.first() {
            let reason = trim_reason(&ui::elide_paths(reason, 0));
            let what = match self.written {
                0 => "nothing written".to_string(),
                n => format!("only {n} file(s) written"),
            };
            return (format!("apply failed: {what} — {reason}"), true);
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

/// Write the apply plan the preview shows. It is recomputed now: a
/// background refresh may have merged new ports, or changed some, since the
/// preview was opened, and then nothing is written — the preview shows the
/// new plan and asks again. `skip_conflicting` leaves the files of ports in
/// conflict as they are on disk, their edits staged.
pub(super) fn commit_apply(app: &mut App, skip_conflicting: bool) {
    let mut planned = app.compute_writes();
    let (conflicted, files) = app.conflicting(&planned);
    if app
        .modal
        .as_ref()
        .is_some_and(|m| !m.shows(&planned, &conflicted))
    {
        app.modal = None;
        app.open_apply_modal();
        if app.modal.is_some() {
            app.flash(
                "the plan changed since the preview (a re-query): check it and answer again",
                true,
            );
        } else {
            app.flash(
                "the plan changed since the preview (a re-query): nothing to write now",
                true,
            );
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
    // After a failed write nothing is on disk, so nothing is removed either,
    // and the edits stay staged (and in the draft) to be applied again. A
    // rename failing after others landed leaves those on disk (the first
    // `written`, apply renaming in order): they are read again, the rest
    // staged as before.
    if !result.failed.is_empty() && result.written > 0 {
        let landed: Vec<&str> = planned.writes[..result.written]
            .iter()
            .map(|w| w.options_name.as_str())
            .collect();
        app.options_files.forget(landed.iter().copied());
        app.session
            .reload_saved(&app.options_files, landed.iter().copied());
        app.blame.clear();
    }
    if result.failed.is_empty() {
        // Only the files apply touched are read again; the rest are as the
        // scan read them, and a skipped port keeps its edits staged.
        let touched: Vec<&str> = planned.touched().collect();
        app.options_files.forget(touched.iter().copied());
        app.session
            .reload_saved(&app.options_files, touched.iter().copied());
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
    pub(super) fn conflicting(
        &self,
        planned: &apply::PlannedWrites,
    ) -> (Vec<PortKey>, BTreeSet<String>) {
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

    pub(super) fn open_apply_modal(&mut self) {
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
}
