//! Writing a pass of options files, removals with it, and saying what it did.

use std::path::Path;



use crate::{apply, clean, model, report};


/// One write pass, whatever assembled it: the files to write, the entries to
/// remove, and everything that stopped the pass from being complete.
pub(super) struct WritePass {
    pub(super) writes: Vec<apply::PendingWrite>,
    pub(super) removals: Vec<clean::Removal>,
    pub(super) warnings: Vec<String>,
    pub(super) rejected: Vec<report::RejectedEntry>,
    pub(super) conflicts: Vec<report::ConflictEntry>,
    pub(super) unknown: Vec<String>,
}

impl WritePass {
    pub(super) fn new(writes: Vec<apply::PendingWrite>, removals: Vec<clean::Removal>, warnings: Vec<String>) -> Self {
        WritePass {
            writes,
            removals,
            warnings,
            rejected: Vec::new(),
            conflicts: Vec::new(),
            unknown: Vec::new(),
        }
    }

    /// A pass that could not be carried out as asked writes nothing at all:
    /// a caller driving this from a script must never be left guessing which
    /// half of its plan landed.
    pub(super) fn blocked(&self) -> bool {
        !self.rejected.is_empty() || !self.conflicts.is_empty() || !self.unknown.is_empty()
    }

    pub(super) fn nothing_to_do(&self) -> bool {
        self.writes.is_empty() && self.removals.is_empty()
    }
}

/// Carry out a write pass — unless it is a dry run or blocked — and assemble
/// the report `sync` and `decide` both answer with. Failures are collected
/// rather than raised: a per-file problem must not hide the rest of the pass.
/// A file that cannot be written stops the whole pass (see `apply::apply`).
pub(super) fn commit_writes(
    pass: &WritePass,
    ports: &std::collections::BTreeMap<model::origin::PortKey, model::port::PortInfo>,
    options_dir: &Path,
    dry_run: bool,
) -> report::WriteReport {
    let act = !dry_run && !pass.blocked();
    let mut errors: Vec<report::ErrorEntry> = Vec::new();
    let mut written = 0usize;
    let mut removed = 0usize;
    let mut notes: Vec<String> = Vec::new();

    if act {
        let done = apply::commit(&pass.writes, &pass.removals);
        written = done.summary.written;
        removed = done.removed.count;
        notes = done.removed.notes;
        let failed = done.summary.failed.into_iter().map(|(key, m)| (key.to_string(), m));
        for (subject, message) in failed.chain(done.removed.failed) {
            errors.push(report::ErrorEntry { subject, message });
        }
    }

    let mut warnings = pass.warnings.clone();
    warnings.extend(notes);
    report::WriteReport {
        options_dir: options_dir.display().to_string(),
        dry_run,
        applied: written > 0 || removed > 0,
        writes: pass
            .writes
            .iter()
            .map(|w| report::write_entry(w, ports.get(&w.key)))
            .collect(),
        removals: pass
            .removals
            .iter()
            .map(report::RemovalEntry::from)
            .collect(),
        rejected: pass.rejected.clone(),
        conflicts: pass.conflicts.clone(),
        unknown: pass.unknown.clone(),
        warnings,
        summary: report::WriteSummary {
            written,
            removed,
            failed: errors.len(),
            rejected: pass.rejected.len(),
            conflicts: pass.conflicts.len(),
        },
        errors,
        moved: Vec::new(),
    }
}

/// The closing stderr line of a write pass, for sync and decide alike.
pub(super) fn print_write_summary(pass: &WritePass, report: &report::WriteReport, dir: &Path, dry_run: bool) {
    if dry_run {
        eprintln!(
            "dry run: {} file(s) would be written, {} removed in {}",
            pass.writes.len(),
            pass.removals.len(),
            dir.display()
        );
        return;
    }
    let failed = match report.summary.failed {
        0 => String::new(),
        n => format!(", {n} failed"),
    };
    eprintln!(
        "{} file(s) written, {} removed in {}{failed}",
        report.summary.written,
        report.summary.removed,
        dir.display()
    );
}

#[cfg(test)]
mod tests {
    /// A pass whose write failed removes nothing either: the options dir is
    /// left exactly as it was found.
    #[test]
    fn a_failed_write_removes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dead = tmp.path().join("cat_dead");
        std::fs::create_dir_all(&dead).unwrap();
        std::fs::write(dead.join("options"), "x\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("cat_port/.options.optique.tmp")).unwrap();
        let write = crate::apply::PendingWrite {
            key: crate::model::origin::PortKey::parse("cat/port").unwrap(),
            options_name: "cat_port".into(),
            path: tmp.path().join("cat_port/options"),
            old: None,
            enabled: Default::default(),
            complete: vec![],
            kept: vec![],
            content: "x\n".into(),
        };
        let removal = crate::clean::Removal {
            options_name: "cat_dead".into(),
            dir: dead.clone(),
            reason: "test".into(),
        };
        let pass = super::WritePass::new(vec![write], vec![removal], Vec::new());
        let report = super::commit_writes(&pass, &Default::default(), tmp.path(), false);
        assert_eq!(report.summary.failed, 1);
        assert_eq!(report.summary.removed, 0);
        assert!(dead.join("options").exists(), "the removal waited for the writes");
    }
}
