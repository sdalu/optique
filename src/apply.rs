use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::model::origin::PortKey;
use crate::model::port::PortInfo;
use crate::optfiles::OptionsFiles;
use crate::optionsfile::{self, SavedOptionsFile};

/// One options file the apply pass wants to write.
#[derive(Debug)]
pub struct PendingWrite {
    pub key: PortKey,
    pub options_name: String,
    pub path: PathBuf,
    pub old: Option<SavedOptionsFile>,
    /// Final enabled set the file will record.
    pub enabled: BTreeSet<String>,
    /// Current complete option list (display/diff aid).
    pub complete: Vec<String>,
    /// Options of the old file that a non-default flavor writing it lacks,
    /// carried over with their list entry and SET/UNSET lines as they were:
    /// the default flavor may still have them (see `kept_options`).
    pub kept: Vec<String>,
    pub content: String,
}

/// How a write changes the options file, as sets rather than prose: the text
/// summary and the JSON report are both rendered from this, so the two cannot
/// drift apart.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Options the file already knew, now enabled.
    pub turned_on: Vec<String>,
    /// Options the file already knew, now disabled.
    pub turned_off: Vec<String>,
    /// Options the port gained since the file was written, with the value
    /// they are being recorded at.
    pub adopted: Vec<(String, bool)>,
    /// Options the port no longer has; the file stops mentioning them.
    pub dropped: Vec<String>,
}

impl PendingWrite {
    /// What this write changes. Every option of a brand-new file counts as
    /// adopted: the file knew nothing before.
    pub fn changes(&self) -> Changes {
        let cur: BTreeSet<&str> = self.enabled.iter().map(String::as_str).collect();
        let now_known: BTreeSet<&str> = self.complete.iter().map(String::as_str).collect();
        let Some(old) = &self.old else {
            return Changes {
                adopted: self
                    .complete
                    .iter()
                    .map(|o| (o.clone(), cur.contains(o.as_str())))
                    .collect(),
                ..Default::default()
            };
        };
        // Read as make reads it (SavedOptionsFile::value): a value only for an
        // option with a SET or UNSET line, UNSET winning.
        let file_known: BTreeSet<&str> = old.recorded().map(String::as_str).collect();
        let was: BTreeSet<&str> =
            file_known.iter().copied().filter(|o| old.value(o) == Some(true)).collect();
        let mentioned: BTreeSet<&str> =
            old.complete.iter().map(String::as_str).chain(file_known.iter().copied()).collect();
        Changes {
            turned_on: cur
                .difference(&was)
                .filter(|o| file_known.contains(**o))
                .map(|o| o.to_string())
                .collect(),
            turned_off: was
                .iter()
                .filter(|o| now_known.contains(**o) && !cur.contains(**o))
                .map(|o| o.to_string())
                .collect(),
            // Kept in the port's own option order, like the file itself.
            adopted: self
                .complete
                .iter()
                .filter(|o| !file_known.contains(o.as_str()))
                .map(|o| (o.clone(), cur.contains(o.as_str())))
                .collect(),
            dropped: mentioned
                .iter()
                .filter(|o| !now_known.contains(**o) && !self.kept.iter().any(|k| k == *o))
                .map(|o| o.to_string())
                .collect(),
        }
    }

    /// Human-readable one-line change summary.
    pub fn describe(&self) -> String {
        if self.old.is_none() {
            // The file records every option, on or off.
            return format!("new file ({} of {} on)", self.enabled.len(), self.complete.len());
        }
        let c = self.changes();
        let mut parts = Vec::new();
        if !c.turned_on.is_empty() {
            parts.push(format!("+{}", c.turned_on.join(" +")));
        }
        if !c.turned_off.is_empty() {
            parts.push(format!("-{}", c.turned_off.join(" -")));
        }
        if !c.adopted.is_empty() {
            parts.push(format!(
                "new: {}",
                c.adopted
                    .iter()
                    .map(|(o, on)| format!("{o}({})", crate::model::options::on_off(*on)))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
        if !c.dropped.is_empty() {
            parts.push(format!("dropped: {}", c.dropped.join(" ")));
        }
        if parts.is_empty() {
            parts.push("option list refresh".to_string());
        }
        parts.join(" · ")
    }
}

/// Decide the final enabled set for a port when syncing non-interactively:
/// keep the saved file's choices for options that still exist, take the
/// effective state (defaults + make.conf) for options the file doesn't know,
/// and drop options that no longer exist.
///
/// The file is read the way bsd.options.mk reads it: OPTIONS_FILE_UNSET runs
/// after OPTIONS_FILE_SET, so an option on both lines is off, and an option
/// on neither (listed only in `_FILE_COMPLETE_OPTIONS_LIST`) keeps its
/// default/make.conf value.
pub fn sync_enabled_set(info: &PortInfo, saved: Option<&SavedOptionsFile>) -> BTreeSet<String> {
    let mut enabled = BTreeSet::new();
    for opt in &info.options.complete {
        let on = saved
            .and_then(|s| s.value(opt))
            .unwrap_or_else(|| info.options.effective.contains(opt));
        if on {
            enabled.insert(opt.clone());
        }
    }
    enabled
}

/// Result of planning: the files to write, files to remove (minimal mode),
/// plus non-fatal notes.
pub struct PlannedWrites {
    pub writes: Vec<PendingWrite>,
    /// Existing files whose content defaults + make.conf already produce;
    /// only populated in minimal mode.
    pub removals: Vec<crate::clean::Removal>,
    pub warnings: Vec<String>,
}

impl PlannedWrites {
    /// The options files the plan changes on disk, written or removed.
    pub fn touched(&self) -> impl Iterator<Item = &str> {
        self.writes
            .iter()
            .map(|w| w.options_name.as_str())
            .chain(self.removals.iter().map(|r| r.options_name.as_str()))
    }

    /// The plan without the files named in `skip`: neither written nor
    /// removed, they stay on disk as they are.
    pub fn without(mut self, skip: &BTreeSet<String>) -> PlannedWrites {
        self.writes.retain(|w| !skip.contains(&w.options_name));
        self.removals.retain(|r| !skip.contains(&r.options_name));
        self
    }
}

/// Build the list of files that actually need (re)writing.
///
/// A file is skipped when it is semantically identical: same complete option
/// list and same IMPLIES-closed enabled set (pkgname-only changes don't force a rewrite,
/// matching `make config-conditional` behavior).
///
/// Flavors of one origin share a single options file, and their computed
/// option sets may legitimately differ (a flavor can change defaults or
/// exclude options — e.g. devel/git@lite). The file is written from the
/// default flavor's point of view, exactly like `make config` on the plain
/// origin would; divergent non-default flavors are reported as warnings.
/// In `minimal` mode a port whose final enabled set equals the no-file
/// outcome (defaults + make.conf layers, IMPLIES-closed) is not persisted:
/// an existing file becomes a removal, a missing one is left missing.
pub fn plan_writes<'a>(
    ports: impl Iterator<Item = (&'a PortKey, &'a PortInfo, BTreeSet<String>)>,
    files: &OptionsFiles,
    minimal: bool,
) -> PlannedWrites {
    let mut groups: std::collections::BTreeMap<String, Vec<Candidate<'a>>> = Default::default();
    for (key, info, enabled) in ports {
        if !info.options.has_options() {
            continue;
        }
        let preferred = crate::session::is_preferred(info);
        groups.entry(info.options_name.clone()).or_default().push(Candidate {
            key: key.clone(),
            info,
            enabled,
            preferred,
        });
    }

    let mut writes = Vec::new();
    let mut removals = Vec::new();
    let mut warnings = Vec::new();
    for (options_name, cands) in &groups {
        // bsd.options.mk:300-304: a port with an OPTIONS_OVERRIDE never
        // reads its options file; one candidate having it is enough to
        // leave the whole shared file alone, written or removed.
        if cands.iter().any(|c| c.info.options.r#override.is_some()) {
            continue;
        }
        // bsd.options.mk:341-363: an options.local assigning a SET or UNSET
        // list afresh drops what the file put in it; no write or removal
        // of the file can say what make ends up with (the startup warning
        // names it).
        if files.combined(options_name).local_reset().is_some() {
            continue;
        }
        let chosen = cands.iter().find(|c| c.preferred).unwrap_or(&cands[0]);
        // Read with its options.local, as make reads it; what a write
        // carries over comes from the file alone, which is all it writes.
        let old = files.saved(options_name);
        let planned = planned_file(chosen, cands, files.own(options_name).as_ref());
        if minimal {
            // The file decides every flavor sharing it: it goes only when
            // none of them ends up elsewhere without it (a flavor can set its
            // own OPTIONS_DEFAULT, as www/sogo@pgsql does), judged on the
            // file as it would be written, so OPTIONS_SLAVE and conditional
            // groups count on both sides. Lines no flavor here can see are
            // another flavor's choices, and keep the file.
            // make reads options.local whether or not the file is there
            // (bsd.options.mk:342), so both sides read it: the file is
            // redundant only when options.local alone ends up the same.
            let with_file = files.under_local(options_name, planned.file.clone());
            let without = files.under_local(options_name, SavedOptionsFile::default());
            let redundant = !planned.foreign
                && cands.iter().all(|c| {
                    crate::session::effective_with(c.info, Some(&with_file))
                        == crate::session::effective_with(c.info, Some(&without))
                });
            if redundant {
                if files.bytes(options_name).is_some() {
                    removals.push(crate::clean::Removal {
                        options_name: options_name.clone(),
                        dir: files.entry_dir(options_name),
                        reason: "defaults + make.conf produce this exact configuration"
                            .to_string(),
                    });
                }
                continue;
            }
        }
        for other in cands.iter().filter(|c| !std::ptr::eq(*c, chosen)) {
            warnings.extend(divergence_warning(options_name, chosen, other));
        }
        let info = chosen.info;
        let enabled = &chosen.enabled;
        // The default flavor answers for the file; without it, every flavor
        // here must find it up to date.
        let current = |old: &SavedOptionsFile| {
            if chosen.preferred {
                up_to_date(old, info, enabled)
            } else {
                cands.iter().all(|c| up_to_date(old, c.info, &c.enabled))
            }
        };
        if old.as_ref().is_some_and(current) {
            continue;
        }
        let mut content = optionsfile::render(&info.pkgname, &info.options.complete, enabled);
        if !planned.kept.is_empty() {
            content = carry_over(&content, &planned.file, &planned.kept);
        }
        writes.push(PendingWrite {
            key: chosen.key.clone(),
            options_name: options_name.clone(),
            path: files.path(options_name),
            old,
            enabled: enabled.clone(),
            complete: info.options.complete.clone(),
            kept: planned.kept,
            content,
        });
    }
    PlannedWrites { writes, removals, warnings }
}

/// A port wanting to write an options file, which the other flavors of its
/// origin share.
struct Candidate<'a> {
    key: PortKey,
    info: &'a PortInfo,
    enabled: BTreeSet<String>,
    /// Default flavor of its origin (or unflavored) — owns the file.
    preferred: bool,
}

/// The warning for a flavor whose choices differ from the owner's on the
/// shared file. Differences confined to options one flavor doesn't even have
/// (OPTIONS_EXCLUDE etc.) are structural and harmless: the file records the
/// default flavor's view, the other flavor ignores options it excludes. Only
/// disagreement on options BOTH flavors carry is a real decision worth
/// surfacing.
fn divergence_warning(options_name: &str, chosen: &Candidate, other: &Candidate) -> Option<String> {
    if other.enabled == chosen.enabled {
        return None;
    }
    let has = |c: &Candidate, o: &str| c.info.options.complete.iter().any(|x| x == o);
    let shared_diff: Vec<&str> = other
        .enabled
        .symmetric_difference(&chosen.enabled)
        .map(String::as_str)
        .filter(|o| has(chosen, o) && has(other, o))
        .collect();
    if shared_diff.is_empty() {
        return None;
    }
    Some(format!(
        "{}: flavors disagree on shared options file {} — using {} (default flavor); \
         differing shared options: {}",
        other.key,
        options_name,
        chosen.key,
        shared_diff.join(" ")
    ))
}

/// Whether the saved file already says what writing `enabled` would: the same
/// option list, a line for every option of it (a file missing one is
/// completed, as `make config` would write it), and make ending up with the
/// same options from either file
/// (pkgname-only changes don't count, like `make config-conditional`). Both
/// files are run through the framework's own layering (`effective_with`):
/// UNSET wins over SET, an option with neither line is at its defaults +
/// make.conf value, FORCE overrides the file, IMPLIES applies last. The raw
/// SET lines miss the first two; the port's queried `effective` cannot stand
/// in for the old file either, since in the TUI a background refresh queries
/// make with the staged edits in place.
///
/// A non-default flavor writing the file sees only its own options: the
/// file's list may be a superset of them (the default flavor's view), and
/// only its own options need a line; the rest is not its to judge.
fn up_to_date(old: &SavedOptionsFile, info: &PortInfo, enabled: &BTreeSet<String>) -> bool {
    let list_ok = if crate::session::is_preferred(info) {
        old.complete.iter().collect::<BTreeSet<_>>()
            == info.options.complete.iter().collect::<BTreeSet<_>>()
            && old.unlined().is_empty()
    } else {
        info.options
            .complete
            .iter()
            .all(|o| old.complete.contains(o) && old.value(o).is_some())
    };
    if !list_ok {
        return false;
    }
    let would = written_file(info, enabled);
    crate::session::effective_with(info, Some(old)) == crate::session::effective_with(info, Some(&would))
}

/// The file `chosen` writes for the flavors `cands` sharing it, as make
/// reads it, with what goes beyond `chosen`'s own view.
struct Planned {
    file: SavedOptionsFile,
    /// Options outside `chosen`'s list the file still records, in order.
    kept: Vec<String>,
    /// Some of them come from `old` alone: no flavor here has them.
    foreign: bool,
}

/// What writing the shared file from `chosen` produces. The default flavor's
/// view is the whole file. Without it, a non-default flavor cannot tell an
/// option it merely lacks (the default flavor's, OPTIONS_EXCLUDE) from one
/// the port removed, and dropping the first loses the user's choice for
/// good while keeping the second is harmless (make ignores it, the default
/// flavor's next write drops it): so the options another flavor in the
/// closure has go in with that flavor's staged value, and the rest of
/// `old`'s with the lines they had.
fn planned_file(chosen: &Candidate, cands: &[Candidate], old: Option<&SavedOptionsFile>) -> Planned {
    let mut file = written_file(chosen.info, &chosen.enabled);
    let mut kept: Vec<String> = Vec::new();
    let mut foreign = false;
    if chosen.preferred {
        return Planned { file, kept, foreign };
    }
    for c in cands {
        for o in &c.info.options.complete {
            if !file.complete.contains(o) {
                file.complete.push(o.clone());
                if c.enabled.contains(o) {
                    file.set.insert(o.clone());
                } else {
                    file.unset.insert(o.clone());
                }
                kept.push(o.clone());
            }
        }
    }
    if let Some(old) = old {
        for o in old.complete.iter().chain(old.recorded()) {
            if !file.complete.contains(o) {
                file.complete.push(o.clone());
                if old.set.contains(o) {
                    file.set.insert(o.clone());
                }
                if old.unset.contains(o) {
                    file.unset.insert(o.clone());
                }
                kept.push(o.clone());
                foreign = true;
            }
        }
    }
    Planned { file, kept, foreign }
}

/// `content` (a rendered file) with the `kept` options of `file` added:
/// each in the complete list, and with the SET/UNSET lines `file` gives it.
fn carry_over(content: &str, file: &SavedOptionsFile, kept: &[String]) -> String {
    let mut out = String::new();
    for line in content.lines() {
        out.push_str(line);
        if line.starts_with("_FILE_COMPLETE_OPTIONS_LIST=") {
            for o in kept {
                out.push(' ');
                out.push_str(o);
            }
        }
        out.push('\n');
    }
    for o in kept {
        if file.set.contains(o) {
            out.push_str(&format!("OPTIONS_FILE_SET+={o}\n"));
        }
        if file.unset.contains(o) {
            out.push_str(&format!("OPTIONS_FILE_UNSET+={o}\n"));
        }
    }
    out
}

/// The file writing `enabled` from `info`'s view produces, as make reads it.
fn written_file(info: &PortInfo, enabled: &BTreeSet<String>) -> SavedOptionsFile {
    SavedOptionsFile {
        complete: info.options.complete.clone(),
        set: enabled.clone(),
        unset: info.options.complete.iter().filter(|o| !enabled.contains(*o)).cloned().collect(),
        ..Default::default()
    }
}

/// Leftover options files owned by ports that no longer have ANY options.
/// A file is only removable when NO port in the closure sharing that
/// OPTIONS_NAME still has options — a flavor with an empty option list
/// (e.g. glib20@bootstrap) must not delete the default flavor's file.
pub fn plan_stale_removals(
    ports: &std::collections::BTreeMap<PortKey, PortInfo>,
    files: &OptionsFiles,
) -> Vec<crate::clean::Removal> {
    let with_options: BTreeSet<&str> = ports
        .values()
        .filter(|i| i.options.has_options())
        .map(|i| i.options_name.as_str())
        .collect();
    // Only the view that owns a shared file can say it has no options: a
    // non-default flavor lacking them (glib20@bootstrap, gobject-introspection
    // @bootstrap) says nothing of the default flavor, which may still read
    // the file when it is not in the closure.
    let optionless: BTreeSet<&str> = ports
        .values()
        .filter(|i| crate::session::is_preferred(i))
        .map(|i| i.options_name.as_str())
        .filter(|name| !with_options.contains(name))
        .collect();
    optionless
        .into_iter()
        .filter(|name| files.bytes(name).is_some())
        .map(|name| crate::clean::Removal {
            options_name: name.to_string(),
            dir: files.entry_dir(name),
            reason: "port has no options anymore".to_string(),
        })
        .collect()
}

pub struct ApplySummary {
    pub written: usize,
    pub failed: Vec<(PortKey, String)>,
}

/// Write every pending file atomically, and the pass as a whole: every
/// temporary file is written (tmp + fsync) first, and only once all of them
/// are does any rename into place. When one cannot be written, the temporary
/// files already written are removed and nothing is renamed, so a failed pass
/// leaves the options dir as it was.
pub fn apply(writes: &[PendingWrite]) -> ApplySummary {
    let mut summary = ApplySummary { written: 0, failed: Vec::new() };
    let mut staged: Vec<(&PendingWrite, Staged)> = Vec::new();
    for w in writes {
        match write_temp(w) {
            Ok(s) => staged.push((w, s)),
            Err(e) => summary.failed.push((w.key.clone(), format!("{e:#}"))),
        }
    }
    if !summary.failed.is_empty() {
        for (_, s) in staged {
            s.discard();
        }
        return summary;
    }
    for (w, s) in staged {
        match fs::rename(&s.tmp, &w.path) {
            Ok(()) => summary.written += 1,
            Err(e) => {
                summary
                    .failed
                    .push((w.key.clone(), format!("renaming into place: {}: {e}", w.path.display())));
                s.discard();
            }
        }
    }
    summary
}

/// What committing a plan did.
pub struct Committed {
    pub summary: ApplySummary,
    pub removed: crate::clean::Removed,
}

/// Write every pending file (see `apply`), then remove the planned entries —
/// only once every write landed: a pass that failed to write leaves the
/// options dir as it found it. `sync`, `decide` and the TUI all commit here.
pub fn commit(writes: &[PendingWrite], removals: &[crate::clean::Removal]) -> Committed {
    let summary = apply(writes);
    let removed = if summary.failed.is_empty() {
        crate::clean::remove_all(removals)
    } else {
        crate::clean::Removed::default()
    };
    Committed { summary, removed }
}

/// A temporary file written and synced, waiting to be renamed into place.
struct Staged {
    tmp: PathBuf,
    /// The options-dir entry did not exist before: discarding removes it
    /// again (when it is empty).
    new_dir: Option<PathBuf>,
}

impl Staged {
    fn discard(self) {
        let _ = fs::remove_file(&self.tmp);
        if let Some(dir) = self.new_dir {
            let _ = fs::remove_dir(dir);
        }
    }
}

fn write_temp(w: &PendingWrite) -> Result<Staged> {
    let dir = w.path.parent().context("options path has no parent")?;
    let new_dir = (!dir.exists()).then(|| dir.to_path_buf());
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join(".options.optique.tmp");
    let staged = Staged { tmp, new_dir };
    let mut f = match fs::File::create(&staged.tmp) {
        Ok(f) => f,
        Err(e) => {
            // Never ours to remove: whatever holds the name was there before.
            if let Some(dir) = &staged.new_dir {
                let _ = fs::remove_dir(dir);
            }
            return Err(e).with_context(|| format!("creating {}", staged.tmp.display()));
        }
    };
    if let Err(e) = f.write_all(w.content.as_bytes()).and_then(|()| f.sync_all()) {
        let tmp = staged.tmp.display().to_string();
        staged.discard();
        return Err(e).with_context(|| format!("writing {tmp}"));
    }
    Ok(staged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::options::PortOptions;

    fn mk_info(complete: &[&str], effective: &[&str]) -> PortInfo {
        PortInfo {
            pkgname: "port-1.0".into(),
            options_name: "cat_port".into(),
            options: PortOptions {
                complete: complete.iter().map(|s| s.to_string()).collect(),
                effective: effective.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            ..PortInfo::stub("cat/port")
        }
    }

    #[test]
    fn sync_keeps_choices_and_adopts_new() {
        // Saved file: A on, B off. Tree update added C (effective: on).
        let info = mk_info(&["A", "B", "C"], &["A", "C"]);
        let saved = SavedOptionsFile::parse(
            "_OPTIONS_READ=port-0.9\n_FILE_COMPLETE_OPTIONS_LIST=A B D\n\
             OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n",
        );
        let enabled = sync_enabled_set(&info, Some(&saved));
        // A kept on, B kept off (even though effective would differ), C adopted, D dropped.
        assert_eq!(enabled, ["A", "C"].iter().map(|s| s.to_string()).collect());
    }

    /// A file written before `_FILE_COMPLETE_OPTIONS_LIST` existed knows
    /// its options only from the SET/UNSET lines: those choices still hold.
    #[test]
    fn sync_keeps_choices_a_file_knows_only_from_its_set_lines() {
        let info = mk_info(&["A", "B"], &["B"]);
        let saved = SavedOptionsFile::parse("OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n");
        let enabled = sync_enabled_set(&info, Some(&saved));
        assert_eq!(enabled, ["A"].iter().map(|s| s.to_string()).collect());
    }

    /// bsd.options.mk applies OPTIONS_FILE_SET then OPTIONS_FILE_UNSET, so
    /// an option on both lines is off; an option the file only lists in
    /// `_FILE_COMPLETE_OPTIONS_LIST` is left at its default/make.conf value.
    /// Sync must read a hand-edited file the way make does, or its rewrite
    /// flips options the reported diff never mentions.
    #[test]
    fn sync_lets_unset_win_and_leaves_complete_only_options_to_make() {
        // What make computes with this file in place: A off (UNSET wins), B
        // on, C at its default (on), D at its default (off).
        let info = mk_info(&["A", "B", "C", "D"], &["B", "C"]);
        let saved = SavedOptionsFile::parse(
            "_FILE_COMPLETE_OPTIONS_LIST=A B C D\n\
             OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_SET+=B\n",
        );
        let enabled = sync_enabled_set(&info, Some(&saved));
        assert_eq!(enabled, ["B", "C"].iter().map(|s| s.to_string()).collect());
        // Nobody decided C or D: the file says nothing about them.
        assert_eq!(crate::session::undecided_options(&info, Some(&saved), false), ["C", "D"]);
    }

    fn pending(old: Option<&str>, complete: &[&str], enabled: &[&str]) -> PendingWrite {
        PendingWrite {
            key: PortKey::parse("cat/port").unwrap(),
            options_name: "cat_port".into(),
            path: PathBuf::from("/nonexistent/cat_port/options"),
            old: old.map(SavedOptionsFile::parse),
            enabled: enabled.iter().map(|s| s.to_string()).collect(),
            complete: complete.iter().map(|s| s.to_string()).collect(),
            kept: vec![],
            content: String::new(),
        }
    }

    /// The old file is read as make reads it: UNSET wins over SET, and an
    /// option only in the complete list holds no value of its own, so a
    /// rewrite that settles either is reported, never passed over.
    #[test]
    fn changes_read_the_old_file_as_make_does() {
        let old = "_FILE_COMPLETE_OPTIONS_LIST=A B C\n\
                   OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_SET+=C\n";
        let c = pending(Some(old), &["A", "B", "C"], &["A", "B", "C"]).changes();
        assert_eq!(c.turned_on, ["A"], "A was off for make");
        assert_eq!(c.adopted, [("B".to_string(), true)], "B had no value in the file");
        assert!(c.turned_off.is_empty() && c.dropped.is_empty(), "{c:?}");
    }

    /// A brand-new file adopts every option, at the value it is written with.
    #[test]
    fn a_new_file_adopts_every_option() {
        let c = pending(None, &["A", "B"], &["B"]).changes();
        assert_eq!(c.adopted, [("A".to_string(), false), ("B".to_string(), true)]);
        assert!(c.turned_on.is_empty() && c.turned_off.is_empty() && c.dropped.is_empty());
    }

    /// An option the port no longer has is dropped, not turned off.
    #[test]
    fn a_dropped_option_is_not_turned_off() {
        let old = "_FILE_COMPLETE_OPTIONS_LIST=A Z\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_SET+=Z\n";
        let c = pending(Some(old), &["A"], &[]).changes();
        assert_eq!(c.turned_off, ["A"]);
        assert_eq!(c.dropped, ["Z"]);
    }

    #[test]
    fn unconfigured_takes_effective() {
        let info = mk_info(&["A", "B"], &["B"]);
        let enabled = sync_enabled_set(&info, None);
        assert_eq!(enabled, ["B"].iter().map(|s| s.to_string()).collect());
    }

    /// Same option list, different choices: the file is rewritten.
    #[test]
    fn plan_writes_a_changed_choice_on_an_unchanged_list() {
        let tmp = tempfile::tempdir().unwrap();
        let info = mk_info(&["A", "B"], &["A"]);
        fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
        fs::write(
            tmp.path().join("cat_port/options"),
            "_FILE_COMPLETE_OPTIONS_LIST=A B\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n",
        )
        .unwrap();
        let enabled: BTreeSet<String> = ["B".to_string()].into();
        let planned = plan_writes(
            std::iter::once((&info.key.clone(), &info, enabled)),
            &OptionsFiles::new(tmp.path()),
            false,
        );
        assert_eq!(planned.writes.len(), 1);
    }

    /// bsd.options.mk:300-304: a port with an OPTIONS_OVERRIDE never reads
    /// its options file, so a disagreeing one already on disk must survive
    /// untouched: no write, whatever `enabled` is handed in.
    #[test]
    fn plan_writes_leaves_an_overridden_ports_file_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let mut info = mk_info(&["A", "B"], &["A"]);
        info.options.r#override = Some(["A".to_string()].into());
        fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
        fs::write(
            tmp.path().join("cat_port/options"),
            "_FILE_COMPLETE_OPTIONS_LIST=A B\nOPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_SET+=B\n",
        )
        .unwrap();
        // Whatever the caller computed as "enabled" (A, the override's own
        // outcome, disagreeing with the file's B) must not trigger a write.
        let enabled: BTreeSet<String> = ["A".to_string()].into();
        let planned = plan_writes(
            std::iter::once((&info.key.clone(), &info, enabled)),
            &OptionsFiles::new(tmp.path()),
            false,
        );
        assert!(planned.writes.is_empty(), "{:?}", planned.writes);
        assert!(planned.removals.is_empty(), "{:?}", planned.removals);
    }

    /// The same, under `-m`: without the OPTIONS_OVERRIDE guard, minimal
    /// mode would see the override's outcome equal defaults + make.conf
    /// (both computed the same way with the file ignored) and queue the
    /// file for removal as redundant. It must not: the file is left alone,
    /// not merely rewritten as absent.
    #[test]
    fn plan_writes_leaves_an_overridden_ports_file_alone_under_minimal() {
        let tmp = tempfile::tempdir().unwrap();
        let mut info = mk_info(&["A", "B"], &["A"]);
        info.options.r#override = Some(["A".to_string()].into());
        fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
        fs::write(
            tmp.path().join("cat_port/options"),
            "_FILE_COMPLETE_OPTIONS_LIST=A B\nOPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_SET+=B\n",
        )
        .unwrap();
        let enabled: BTreeSet<String> = ["A".to_string()].into();
        let planned = plan_writes(
            std::iter::once((&info.key.clone(), &info, enabled)),
            &OptionsFiles::new(tmp.path()),
            true,
        );
        assert!(planned.writes.is_empty(), "{:?}", planned.writes);
        assert!(planned.removals.is_empty(), "{:?}", planned.removals);
    }

    /// bsd.options.mk:341-363: options.local is read after the file and
    /// joins its lists, UNSET winning. With the file setting A and
    /// options.local unsetting it, make has A off: so does the session,
    /// the port is configured, and sync finds nothing to write, leaving
    /// options.local alone.
    #[test]
    fn options_local_is_read_with_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let info = mk_info(&["A", "B", "C"], &[]);
        fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
        fs::write(
            tmp.path().join("cat_port/options"),
            "_FILE_COMPLETE_OPTIONS_LIST=A B C\n\
             OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\nOPTIONS_FILE_UNSET+=C\n",
        )
        .unwrap();
        fs::write(tmp.path().join("cat_port/options.local"), "OPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_SET+=B\n")
            .unwrap();
        let files = OptionsFiles::new(tmp.path());
        let mut ports = std::collections::BTreeMap::new();
        ports.insert(info.key.clone(), info.clone());
        let roots: Vec<PortKey> = ports.keys().cloned().collect();
        let sess = crate::session::Session::new(ports, Default::default(), &roots, &files, false);
        let staged = sess.state(&info).unwrap().staged.clone();
        assert!(staged.is_empty(), "{staged:?}");
        assert_eq!(sess.status(&info), crate::session::UiStatus::Ok);
        let planned = plan_writes(std::iter::once((&info.key.clone(), &info, staged)), &files, false);
        assert!(planned.writes.is_empty(), "{:?}", planned.writes);
    }

    /// bsd.options.mk:342 reads options.local whether or not the options
    /// file is there. The file says the defaults (A on, B off) and
    /// options.local sets B: with the file make has A alone (its UNSET of
    /// B wins), without it A and B. The file is not redundant under -m.
    #[test]
    fn minimal_keeps_a_file_options_local_needs() {
        let tmp = tempfile::tempdir().unwrap();
        let mut info = mk_info(&["A", "B"], &["A"]);
        info.options.defaults = ["A".to_string()].into();
        fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
        fs::write(
            tmp.path().join("cat_port/options"),
            "_FILE_COMPLETE_OPTIONS_LIST=A B\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n",
        )
        .unwrap();
        fs::write(tmp.path().join("cat_port/options.local"), "OPTIONS_FILE_SET+=B\n").unwrap();
        let enabled: BTreeSet<String> = ["A".to_string()].into();
        let planned = plan_writes(
            std::iter::once((&info.key.clone(), &info, enabled)),
            &OptionsFiles::new(tmp.path()),
            true,
        );
        assert!(planned.removals.is_empty(), "{:?}", planned.removals);
        assert!(planned.writes.is_empty(), "{:?}", planned.writes);
    }

    /// A file `make config` wrote with LDAPS on and LDAP unset, LDAPS
    /// implying LDAP, is up to date: make turns LDAP on either way. Sync
    /// plans from the file as written, the session from its IMPLIES-closed
    /// baseline; both must find nothing to rewrite.
    #[test]
    fn sync_and_session_baseline_agree_under_implies() {
        let tmp = tempfile::tempdir().unwrap();
        let mut info = mk_info(&["LDAP", "LDAPS"], &["LDAP", "LDAPS"]);
        info.options.defs.insert(
            "LDAPS".into(),
            crate::model::options::OptionDef { implies: vec!["LDAP".into()], ..Default::default() },
        );
        fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
        fs::write(
            tmp.path().join("cat_port/options"),
            "_FILE_COMPLETE_OPTIONS_LIST=LDAP LDAPS\n\
             OPTIONS_FILE_UNSET+=LDAP\nOPTIONS_FILE_SET+=LDAPS\n",
        )
        .unwrap();
        let files = OptionsFiles::new(tmp.path());

        let synced = sync_enabled_set(&info, files.saved("cat_port").as_ref());
        let planned = plan_writes(std::iter::once((&info.key.clone(), &info, synced)), &files, false);
        assert!(planned.writes.is_empty(), "sync: {:?}", planned.writes);

        let mut ports = std::collections::BTreeMap::new();
        ports.insert(info.key.clone(), info.clone());
        let roots: Vec<PortKey> = ports.keys().cloned().collect();
        let sess = crate::session::Session::new(ports, Default::default(), &roots, &files, false);
        let staged = sess.state(&info).unwrap().staged.clone();
        let planned = plan_writes(std::iter::once((&info.key.clone(), &info, staged)), &files, false);
        assert!(planned.writes.is_empty(), "session: {:?}", planned.writes);
    }

    /// The up-to-date check reads the file as make does. A hand-edited file
    /// with A on both lines has A off (UNSET wins), so turning A on is a
    /// change; comparing the raw SET lines missed it. A file missing a line
    /// is completed even when make already reads it as wanted.
    #[test]
    fn plan_reads_the_file_as_make_does() {
        let tmp = tempfile::tempdir().unwrap();
        let mut info = mk_info(&["A", "B"], &["A"]);
        info.options.defaults = ["A".to_string()].into();
        let dir = tmp.path().join("cat_port");
        fs::create_dir_all(&dir).unwrap();
        let plan = |file: &str, enabled: &[&str]| {
            fs::write(dir.join("options"), file).unwrap();
            let enabled: BTreeSet<String> = enabled.iter().map(|s| s.to_string()).collect();
            let files = OptionsFiles::new(tmp.path());
            plan_writes(std::iter::once((&info.key.clone(), &info, enabled)), &files, false)
        };
        let both = "_FILE_COMPLETE_OPTIONS_LIST=A B\n\
                    OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_UNSET+=B\n";
        assert_eq!(plan(both, &["A"]).writes.len(), 1, "A was off, now on");
        assert!(plan(both, &[]).writes.is_empty(), "A was off, stays off");
        let no_line = "_FILE_COMPLETE_OPTIONS_LIST=A B\nOPTIONS_FILE_UNSET+=B\n";
        assert_eq!(plan(no_line, &[]).writes.len(), 1, "A was on by default, now off");
        let completed = plan(no_line, &["A"]);
        assert_eq!(completed.writes.len(), 1, "A at its default, but the file has no line for it");
        assert!(completed.writes[0].content.contains("OPTIONS_FILE_SET+=A\n"));
    }

    /// In the TUI a background refresh queries make with the staged edits in
    /// place, so `effective` already holds them. An option the file has no
    /// line for must still be judged at its no-file value (defaults +
    /// make.conf), or turning it on looks like nothing to write.
    #[test]
    fn plan_ignores_a_refreshed_effective_for_unlined_options() {
        let tmp = tempfile::tempdir().unwrap();
        // A default on, B default off; the refresh reports the staged A B.
        let mut info = mk_info(&["A", "B"], &["A", "B"]);
        info.options.defaults = ["A".to_string()].into();
        let dir = tmp.path().join("cat_port");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("options"), "_FILE_COMPLETE_OPTIONS_LIST=A B\nOPTIONS_FILE_SET+=A\n")
            .unwrap();
        let enabled: BTreeSet<String> = ["A".to_string(), "B".to_string()].into();
        let files = OptionsFiles::new(tmp.path());
        let planned = plan_writes(std::iter::once((&info.key.clone(), &info, enabled)), &files, false);
        assert_eq!(planned.writes.len(), 1, "B was off by default, now on");
    }

    /// make applies IMPLIES after the file: X on by default and implying Y,
    /// the file unsetting both, leaves Y off. Asking for Y must write; asking
    /// for nothing must not.
    #[test]
    fn plan_writes_an_option_implied_only_by_an_unset_default() {
        let tmp = tempfile::tempdir().unwrap();
        let mut info = mk_info(&["X", "Y"], &[]);
        info.options.defaults = ["X".to_string()].into();
        info.options.defs.insert(
            "X".into(),
            crate::model::options::OptionDef { implies: vec!["Y".into()], ..Default::default() },
        );
        let dir = tmp.path().join("cat_port");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("options"),
            "_FILE_COMPLETE_OPTIONS_LIST=X Y\nOPTIONS_FILE_UNSET+=X\nOPTIONS_FILE_UNSET+=Y\n",
        )
        .unwrap();
        let plan = |enabled: &[&str]| {
            let enabled: BTreeSet<String> = enabled.iter().map(|s| s.to_string()).collect();
            let files = OptionsFiles::new(tmp.path());
            plan_writes(std::iter::once((&info.key.clone(), &info, enabled)), &files, false)
        };
        assert_eq!(plan(&["Y"]).writes.len(), 1, "Y was off, now on");
        assert!(plan(&[]).writes.is_empty(), "nothing changes");
    }

    #[test]
    fn plan_skips_semantically_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let info = mk_info(&["A", "B"], &["A"]);
        let dir = tmp.path().join("cat_port");
        fs::create_dir_all(&dir).unwrap();
        // pkgname differs from info.pkgname but list+set match -> skip
        fs::write(
            dir.join("options"),
            "_OPTIONS_READ=port-0.9\n_FILE_COMPLETE_OPTIONS_LIST=A B\n\
             OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n",
        )
        .unwrap();
        let enabled: BTreeSet<String> = ["A".to_string()].into();
        let planned =
            plan_writes(std::iter::once((&info.key.clone(), &info, enabled)), &OptionsFiles::new(tmp.path()), false);
        assert!(planned.writes.is_empty());
        assert!(planned.warnings.is_empty());
    }

    #[test]
    fn shared_file_uses_default_flavor() {
        let tmp = tempfile::tempdir().unwrap();
        // Two flavors of one origin, default flavor first in FLAVORS.
        let mut a = mk_info(&["A", "B"], &["A"]);
        a.key = PortKey::parse("devel/git@default").unwrap();
        a.canonical = a.key.clone();
        a.flavors = vec!["default".into(), "lite".into()];
        let mut b = mk_info(&["A"], &[]);
        b.key = PortKey::parse("devel/git@lite").unwrap();
        b.canonical = b.key.clone();
        b.flavors = vec!["default".into(), "lite".into()];

        let ports = [
            (b.key.clone(), &b, BTreeSet::new()), // divergent non-default first
            (a.key.clone(), &a, ["A".to_string()].into()),
        ];
        let planned =
            plan_writes(ports.iter().map(|(k, i, e)| (k, *i, e.clone())), &OptionsFiles::new(tmp.path()), false);
        assert_eq!(planned.writes.len(), 1);
        assert_eq!(planned.writes[0].key.to_string(), "devel/git@default");
        assert!(planned.writes[0].enabled.contains("A"));
        // A exists in BOTH flavors and they disagree -> real warning.
        assert_eq!(planned.warnings.len(), 1);
        assert!(planned.warnings[0].contains("devel/git@lite"));
        assert!(planned.warnings[0].contains("A"));
    }

    #[test]
    fn flavor_excluded_options_do_not_warn() {
        let tmp = tempfile::tempdir().unwrap();
        // Default flavor has X11 (on); nox flavor excludes X11 entirely.
        // Both agree on the shared option A.
        let mut full = mk_info(&["A", "X11"], &["A", "X11"]);
        full.key = PortKey::parse("editors/emacs@full").unwrap();
        full.canonical = full.key.clone();
        full.flavors = vec!["full".into(), "nox".into()];
        let mut nox = mk_info(&["A"], &["A"]);
        nox.key = PortKey::parse("editors/emacs@nox").unwrap();
        nox.canonical = nox.key.clone();
        nox.flavors = vec!["full".into(), "nox".into()];

        let ports: Vec<(PortKey, &PortInfo, BTreeSet<String>)> = vec![
            (nox.key.clone(), &nox, ["A".to_string()].into()),
            (full.key.clone(), &full, ["A".to_string(), "X11".to_string()].into()),
        ];
        let planned =
            plan_writes(ports.iter().map(|(k, i, e)| (k, *i, e.clone())), &OptionsFiles::new(tmp.path()), false);
        assert_eq!(planned.writes.len(), 1);
        assert_eq!(planned.writes[0].key.to_string(), "editors/emacs@full");
        assert!(planned.warnings.is_empty(), "structural exclusion must not warn: {:?}", planned.warnings);
    }

    /// A non-default flavor alone sees only its own options: the default
    /// flavor's others in the shared file are not its to drop.
    #[test]
    fn a_non_default_flavor_keeps_the_options_it_lacks() {
        let tmp = tempfile::tempdir().unwrap();
        let mut nox = mk_info(&["A", "B"], &["A"]);
        nox.key = PortKey::parse("editors/emacs@nox").unwrap();
        nox.canonical = nox.key.clone();
        nox.flavors = vec!["full".into(), "nox".into()];
        nox.options_name = "editors_emacs".into();
        fs::create_dir_all(tmp.path().join("editors_emacs")).unwrap();
        fs::write(
            tmp.path().join("editors_emacs/options"),
            "_OPTIONS_READ=emacs-1.0\n_FILE_COMPLETE_OPTIONS_LIST=A B X11 GTK\n\
             OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\nOPTIONS_FILE_SET+=X11\nOPTIONS_FILE_UNSET+=GTK\n",
        )
        .unwrap();
        let set = |opts: &[&str]| opts.iter().map(|o| o.to_string()).collect::<BTreeSet<String>>();
        let plan = |enabled: BTreeSet<String>| {
            plan_writes(std::iter::once((&nox.key, &nox, enabled)), &OptionsFiles::new(tmp.path()), false)
        };

        // Nothing @nox sees changed: the file is up to date.
        assert!(plan(set(&["A"])).writes.is_empty(), "a superset list is up to date for @nox");

        // B turned on: written, X11 and GTK kept as they were, not dropped.
        let planned = plan(set(&["A", "B"]));
        assert_eq!(planned.writes.len(), 1);
        let w = &planned.writes[0];
        let c = w.changes();
        assert_eq!(c.turned_on, ["B"]);
        assert!(c.dropped.is_empty() && c.turned_off.is_empty(), "{c:?}");
        let written = SavedOptionsFile::parse(&w.content);
        assert_eq!(written.complete, ["A", "B", "X11", "GTK"]);
        assert_eq!(written.set, set(&["A", "B", "X11"]));
        assert_eq!(written.unset, set(&["GTK"]));
    }

    /// Under -m the file goes when the file it would write gives the no-file
    /// outcome, OPTIONS_SLAVE included: www/w3m-img with nothing chosen
    /// needs no file, and one repeating its defaults is removed.
    #[test]
    fn minimal_mode_judges_the_file_it_would_write() {
        let tmp = tempfile::tempdir().unwrap();
        let mut info = mk_info(&["A", "B"], &["A", "S"]);
        info.options.defaults = ["A".to_string()].into();
        info.options.slave = ["S".to_string()].into();
        let set = |opts: &[&str]| opts.iter().map(|o| o.to_string()).collect::<BTreeSet<String>>();
        let plan = |enabled: BTreeSet<String>| {
            plan_writes(std::iter::once((&info.key, &info, enabled)), &OptionsFiles::new(tmp.path()), true)
        };
        let p = plan(set(&["A"]));
        assert!(p.writes.is_empty() && p.removals.is_empty(), "no file needed");
        fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
        fs::write(
            tmp.path().join("cat_port/options"),
            "_FILE_COMPLETE_OPTIONS_LIST=A B\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n",
        )
        .unwrap();
        let p = plan(set(&["A"]));
        assert!(p.writes.is_empty());
        assert_eq!(p.removals.len(), 1, "a file repeating the defaults goes");
    }

    /// Under -m a non-default flavor alone never removes a shared file
    /// holding lines it cannot see: they are the default flavor's choices.
    #[test]
    fn minimal_mode_keeps_lines_a_lone_flavor_cannot_see() {
        let tmp = tempfile::tempdir().unwrap();
        let mut nox = mk_info(&["A"], &["A"]);
        nox.options.defaults = ["A".to_string()].into();
        nox.key = PortKey::parse("editors/emacs@nox").unwrap();
        nox.canonical = nox.key.clone();
        nox.flavors = vec!["full".into(), "nox".into()];
        nox.options_name = "editors_emacs".into();
        fs::create_dir_all(tmp.path().join("editors_emacs")).unwrap();
        fs::write(
            tmp.path().join("editors_emacs/options"),
            "_FILE_COMPLETE_OPTIONS_LIST=A X11\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=X11\n",
        )
        .unwrap();
        let enabled: BTreeSet<String> = ["A".to_string()].into();
        let p = plan_writes(std::iter::once((&nox.key, &nox, enabled)), &OptionsFiles::new(tmp.path()), true);
        assert!(p.removals.is_empty(), "X11 is @full's choice: {:?}", p.removals.len());
        assert!(p.writes.is_empty(), "nothing @nox sees changed");
    }

    /// Two non-default flavors and no default one: an edit to an option only
    /// one of them has is written, with that flavor's value.
    #[test]
    fn two_lone_flavors_each_write_what_they_have() {
        let tmp = tempfile::tempdir().unwrap();
        let flavor = |name: &str, complete: &[&str]| {
            let mut i = mk_info(complete, &[]);
            i.key = PortKey::parse(&format!("editors/emacs@{name}")).unwrap();
            i.canonical = i.key.clone();
            i.flavors = vec!["full".into(), "nox".into(), "pgtk".into()];
            i.options_name = "editors_emacs".into();
            i
        };
        let nox = flavor("nox", &["A"]);
        let pgtk = flavor("pgtk", &["A", "Y"]);
        fs::create_dir_all(tmp.path().join("editors_emacs")).unwrap();
        fs::write(
            tmp.path().join("editors_emacs/options"),
            "_FILE_COMPLETE_OPTIONS_LIST=A Y\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=Y\n",
        )
        .unwrap();
        let set = |opts: &[&str]| opts.iter().map(|o| o.to_string()).collect::<BTreeSet<String>>();
        let p = plan_writes(
            [(&nox.key, &nox, set(&["A"])), (&pgtk.key, &pgtk, set(&["A", "Y"]))].into_iter(),
            &OptionsFiles::new(tmp.path()),
            false,
        );
        assert_eq!(p.writes.len(), 1, "@pgtk turned Y on");
        let written = SavedOptionsFile::parse(&p.writes[0].content);
        assert_eq!(written.set, set(&["A", "Y"]));
        assert!(written.unset.is_empty());
    }

    #[test]
    fn describe_covers_all_change_kinds() {
        // Old file: knows A(on) B(off) GONE(off); current: A B NEWOPT.
        let old = SavedOptionsFile::parse(
            "_OPTIONS_READ=p-1\n_FILE_COMPLETE_OPTIONS_LIST=A B GONE\n\
             OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\nOPTIONS_FILE_UNSET+=GONE\n",
        );
        let w = PendingWrite {
            key: PortKey::parse("cat/port").unwrap(),
            options_name: "cat_port".into(),
            path: std::path::PathBuf::from("/nonexistent"),
            old: Some(old),
            // A turned off, B turned on, NEWOPT adopted on, GONE dropped.
            enabled: ["B".to_string(), "NEWOPT".to_string()].into(),
            complete: vec!["A".into(), "B".into(), "NEWOPT".into()],
            kept: vec![],
            content: String::new(),
        };
        let d = w.describe();
        assert!(d.contains("+B"), "{d}");
        assert!(d.contains("-A"), "{d}");
        assert!(d.contains("NEWOPT(on)"), "{d}");
        assert!(d.contains("dropped: GONE"), "{d}");
        // A brand-new file describes itself by option count.
        let w2 = PendingWrite { old: None, ..w };
        assert!(w2.describe().contains("new file"));
    }

    /// A new file records every option of the port, not just the enabled
    /// ones: the summary counts both.
    #[test]
    fn describe_new_file_counts_every_option() {
        let w = pending(None, &["A", "B", "C"], &["B"]);
        assert_eq!(w.describe(), "new file (1 of 3 on)");
    }

    #[test]
    fn stale_removal_spares_files_shared_with_optioned_flavors() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("devel_glib20")).unwrap();
        std::fs::write(tmp.path().join("devel_glib20/options"), "x\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("print_indexinfo")).unwrap();
        std::fs::write(tmp.path().join("print_indexinfo/options"), "x\n").unwrap();

        // glib20@default has options; glib20@bootstrap shares the file and
        // has none. indexinfo has none at all.
        let mut default = mk_info(&["DOCS"], &[]);
        default.key = PortKey::parse("devel/glib20@default").unwrap();
        default.canonical = default.key.clone();
        default.options_name = "devel_glib20".into();
        let mut bootstrap = mk_info(&[], &[]);
        bootstrap.key = PortKey::parse("devel/glib20@bootstrap").unwrap();
        bootstrap.canonical = bootstrap.key.clone();
        bootstrap.options_name = "devel_glib20".into();
        let mut indexinfo = mk_info(&[], &[]);
        indexinfo.key = PortKey::parse("print/indexinfo").unwrap();
        indexinfo.canonical = indexinfo.key.clone();
        indexinfo.options_name = "print_indexinfo".into();

        let mut ports = std::collections::BTreeMap::new();
        for i in [default, bootstrap, indexinfo] {
            ports.insert(i.key.clone(), i);
        }
        let removals = plan_stale_removals(&ports, &OptionsFiles::new(tmp.path()));
        let names: Vec<&str> = removals.iter().map(|r| r.options_name.as_str()).collect();
        assert_eq!(names, vec!["print_indexinfo"], "glib20's shared file must survive");
    }

    /// A closure holding only an optionless non-default flavor of a port
    /// (gobject-introspection@bootstrap, reached from editors/emacs@nox)
    /// proves nothing about the shared file its default flavor reads: kept.
    /// With the default flavor itself optionless, the file goes.
    #[test]
    fn stale_removal_needs_the_default_flavors_view() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("devel_gi")).unwrap();
        std::fs::write(tmp.path().join("devel_gi/options"), "x\n").unwrap();
        let flavored = |flavor: &str| {
            let mut i = mk_info(&[], &[]);
            i.key = PortKey::parse(&format!("devel/gi@{flavor}")).unwrap();
            i.canonical = i.key.clone();
            i.flavors = vec!["default".into(), "bootstrap".into()];
            i.options_name = "devel_gi".into();
            i
        };
        let plan = |infos: Vec<PortInfo>| {
            let ports: std::collections::BTreeMap<_, _> =
                infos.into_iter().map(|i| (i.key.clone(), i)).collect();
            plan_stale_removals(&ports, &OptionsFiles::new(tmp.path())).len()
        };
        assert_eq!(plan(vec![flavored("bootstrap")]), 0, "@bootstrap alone keeps the file");
        assert_eq!(plan(vec![flavored("default"), flavored("bootstrap")]), 1);
    }

    /// Planning a whole closure reads nothing the session already read, and
    /// judges each file as the session saw it.
    #[test]
    fn plans_against_the_store_without_reading_again() {
        let tmp = tempfile::tempdir().unwrap();
        let info = mk_info(&["A"], &[]);
        fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
        fs::write(
            tmp.path().join("cat_port/options"),
            "_FILE_COMPLETE_OPTIONS_LIST=A\nOPTIONS_FILE_SET+=A\n",
        )
        .unwrap();
        let files = OptionsFiles::new(tmp.path());
        assert!(files.saved("cat_port").is_some());
        let mut gone = mk_info(&[], &[]);
        gone.key = PortKey::parse("cat/gone").unwrap();
        gone.options_name = "cat_gone".into();
        assert!(files.bytes("cat_gone").is_none());
        let reads = files.disk_reads();

        // Changed on disk since: the plan still compares with what was read,
        // so a port nobody edited is left alone rather than rewritten.
        fs::write(tmp.path().join("cat_port/options"), "OPTIONS_FILE_UNSET+=A\n").unwrap();
        fs::create_dir_all(tmp.path().join("cat_gone")).unwrap();
        fs::write(tmp.path().join("cat_gone/options"), "x\n").unwrap();

        let enabled: BTreeSet<String> = ["A".to_string()].into();
        let planned = plan_writes(std::iter::once((&info.key.clone(), &info, enabled)), &files, false);
        assert!(planned.writes.is_empty());
        let mut ports = std::collections::BTreeMap::new();
        ports.insert(gone.key.clone(), gone);
        assert!(plan_stale_removals(&ports, &files).is_empty(), "absent when read");
        assert_eq!(files.disk_reads(), reads);
    }

    #[test]
    fn minimal_mode_persists_only_deviations() {
        let tmp = tempfile::tempdir().unwrap();
        // defaults {A}, no make.conf: the no-file outcome is {A}.
        let mut info = mk_info(&["A", "B"], &["A"]);
        info.options.defaults = ["A".to_string()].into();

        // Redundant state + existing file -> removal, no write.
        fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
        fs::write(tmp.path().join("cat_port/options"), "x\n").unwrap();
        let enabled: BTreeSet<String> = ["A".to_string()].into();
        let planned = plan_writes(
            std::iter::once((&info.key.clone(), &info, enabled.clone())),
            &OptionsFiles::new(tmp.path()),
            true,
        );
        assert!(planned.writes.is_empty());
        assert_eq!(planned.removals.len(), 1);
        assert_eq!(planned.removals[0].options_name, "cat_port");

        // Redundant state, no file -> nothing at all.
        fs::remove_dir_all(tmp.path().join("cat_port")).unwrap();
        let planned = plan_writes(
            std::iter::once((&info.key.clone(), &info, enabled)),
            &OptionsFiles::new(tmp.path()),
            true,
        );
        assert!(planned.writes.is_empty() && planned.removals.is_empty());

        // Deviating state -> written as usual even in minimal mode.
        let deviating: BTreeSet<String> = ["A".to_string(), "B".to_string()].into();
        let planned = plan_writes(
            std::iter::once((&info.key.clone(), &info, deviating)),
            &OptionsFiles::new(tmp.path()),
            true,
        );
        assert_eq!(planned.writes.len(), 1);
        assert!(planned.removals.is_empty());
    }

    /// A file the default flavor could drop still decides another flavor
    /// sharing it: www/sogo@pgsql defaults to PGSQL, the file says MYSQL.
    #[test]
    fn minimal_mode_keeps_a_file_another_flavor_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let flavor = |f: &str, default: &str| {
            let mut info = mk_info(&["MYSQL", "PGSQL"], &[default]);
            info.key = PortKey::parse(&format!("www/sogo@{f}")).unwrap();
            info.canonical = info.key.clone();
            info.flavors = vec!["mysql".into(), "pgsql".into()];
            info.options_name = "www_sogo".into();
            info.options.defaults = [default.to_string()].into();
            info
        };
        let (mysql, pgsql) = (flavor("mysql", "MYSQL"), flavor("pgsql", "PGSQL"));
        fs::create_dir_all(tmp.path().join("www_sogo")).unwrap();
        fs::write(
            tmp.path().join("www_sogo/options"),
            "_OPTIONS_READ=port-1.0\n_FILE_COMPLETE_OPTIONS_LIST=MYSQL PGSQL\n\
             OPTIONS_FILE_SET+=MYSQL\nOPTIONS_FILE_UNSET+=PGSQL\n",
        )
        .unwrap();
        let mysql_on: BTreeSet<String> = ["MYSQL".to_string()].into();
        let ports = [(&mysql, mysql_on.clone()), (&pgsql, mysql_on.clone())];
        let planned = plan_writes(
            ports.iter().map(|(i, e)| (&i.key, *i, e.clone())),
            &OptionsFiles::new(tmp.path()),
            true,
        );
        assert!(planned.removals.is_empty(), "@pgsql reads MYSQL from the file: {:?}", planned.removals);
        assert!(planned.writes.is_empty(), "the file is up to date");

        // Alone, the default flavor cannot see @pgsql: the file goes.
        let planned = plan_writes(
            std::iter::once((&mysql.key, &mysql, mysql_on)),
            &OptionsFiles::new(tmp.path()),
            true,
        );
        assert_eq!(planned.removals.len(), 1);
    }

    #[test]
    fn apply_reports_failures() {
        let w = PendingWrite {
            key: PortKey::parse("cat/port").unwrap(),
            options_name: "cat_port".into(),
            // Parent is a file, not a dir -> create_dir_all fails.
            path: std::path::PathBuf::from("/dev/null/cat_port/options"),
            old: None,
            enabled: BTreeSet::new(),
            complete: vec![],
            kept: vec![],
            content: "x\n".into(),
        };
        let summary = apply(&[w]);
        assert_eq!(summary.written, 0);
        assert_eq!(summary.failed.len(), 1);
    }

    /// `decide` is all-or-nothing: when one file of the pass cannot be
    /// written, none is, and no temporary file is left behind.
    #[test]
    fn decide_write_failure_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let w = |name: &str| PendingWrite {
            key: PortKey::parse(&name.replace('_', "/")).unwrap(),
            options_name: name.into(),
            path: tmp.path().join(name).join("options"),
            old: None,
            enabled: BTreeSet::new(),
            complete: vec![],
            kept: vec![],
            content: "x\n".into(),
        };
        // Sorted first, so it is the one that would land before the failure.
        let ok = w("sysutils_tmux");
        // A directory squatting the temp name: creating the temp file fails.
        let bad = w("textproc_expat2");
        fs::create_dir_all(tmp.path().join("textproc_expat2/.options.optique.tmp")).unwrap();
        let summary = apply(&[ok, bad]);
        assert_eq!(summary.written, 0);
        assert_eq!(summary.failed.len(), 1, "{:?}", summary.failed);
        assert_eq!(summary.failed[0].0.to_string(), "textproc/expat2");
        assert!(!tmp.path().join("sysutils_tmux/options").exists(), "nothing written");
        assert!(!tmp.path().join("sysutils_tmux/.options.optique.tmp").exists(), "temp removed");
    }

    #[test]
    fn apply_writes_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let info = mk_info(&["A"], &["A"]);
        let enabled: BTreeSet<String> = ["A".to_string()].into();
        let planned =
            plan_writes(std::iter::once((&info.key.clone(), &info, enabled)), &OptionsFiles::new(tmp.path()), false);
        let writes = planned.writes;
        assert_eq!(writes.len(), 1);
        let summary = apply(&writes);
        assert_eq!(summary.written, 1);
        assert!(summary.failed.is_empty());
        let saved = OptionsFiles::new(tmp.path()).saved("cat_port").unwrap();
        assert!(saved.set.contains("A"));
        assert_eq!(saved.options_read, "port-1.0");
    }

    /// Skipping a file drops its write and its removal, and nothing else.
    #[test]
    fn a_plan_without_skipped_files_keeps_the_others() {
        let tmp = tempfile::tempdir().unwrap();
        // Defaults {A}: {A} repeats them (a removal under -m), {A, B} does not.
        let mk = |origin: &str| {
            let mut options = mk_info(&["A", "B"], &["A"]).options;
            options.defaults = ["A".to_string()].into();
            PortInfo { options, ..PortInfo::stub(origin) }
        };
        let ports = [mk("cat/one"), mk("cat/two"), mk("cat/three")];
        for name in ["cat_one", "cat_three"] {
            fs::create_dir_all(tmp.path().join(name)).unwrap();
            fs::write(tmp.path().join(name).join("options"), "x\n").unwrap();
        }
        let set = |opts: &[&str]| opts.iter().map(|o| o.to_string()).collect::<BTreeSet<String>>();
        let enabled = [set(&["A", "B"]), set(&["A", "B"]), set(&["A"])];
        let plan = || {
            plan_writes(
                ports.iter().zip(&enabled).map(|(i, e)| (&i.key, i, e.clone())),
                &OptionsFiles::new(tmp.path()),
                true,
            )
        };
        let all = plan();
        assert_eq!(all.writes.len(), 2);
        assert_eq!(all.removals.len(), 1);
        assert_eq!(all.touched().collect::<BTreeSet<_>>(), ["cat_one", "cat_three", "cat_two"].into());
        let kept = all.without(&set(&["cat_one", "cat_three"]));
        assert_eq!(kept.touched().collect::<Vec<_>>(), ["cat_two"]);
        let kept = plan().without(&set(&["cat_two"]));
        assert_eq!(kept.writes.iter().map(|w| w.options_name.as_str()).collect::<Vec<_>>(), ["cat_one"]);
        assert_eq!(kept.removals[0].options_name, "cat_three");
    }
}
