//! Writing options files: which files a pass writes, from whose view when
//! flavors share one, and which it removes; then the commit, which renames
//! them into place only once every one is written.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::model::options::str_match;
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
        // Read as make reads it, as scan does: an option the file decides
        // has a SET or UNSET entry matching it, patterns included
        // (SavedOptionsFile::decides), and is on only when a SET entry
        // names it and no UNSET entry matches it (SavedOptionsFile::value).
        let was_on = |o: &str| old.value(o) == Some(true);
        let mentioned: BTreeSet<&str> = old
            .complete
            .iter()
            .chain(old.recorded())
            .map(String::as_str)
            .collect();
        Changes {
            turned_on: now_known
                .iter()
                .filter(|o| old.decides(o) && cur.contains(**o) && !was_on(o))
                .map(ToString::to_string)
                .collect(),
            turned_off: now_known
                .iter()
                .filter(|o| old.decides(o) && was_on(o) && !cur.contains(**o))
                .map(ToString::to_string)
                .collect(),
            // Kept in the port's own option order, like the file itself.
            adopted: self
                .complete
                .iter()
                .filter(|o| !old.decides(o))
                .map(|o| (o.clone(), cur.contains(o.as_str())))
                .collect(),
            // An entry is dropped once it matches no option of the port,
            // as scan judges an obsolete one (Session::stale_delta). One
            // options.local holds stays there: the write leaves it alone.
            dropped: mentioned
                .iter()
                .filter(|e| !now_known.iter().any(|c| str_match(c, e)))
                .filter(|e| !self.kept.iter().any(|k| k == *e))
                .filter(|e| !old.local_set.contains(**e) && !old.local_unset.contains(**e))
                .map(ToString::to_string)
                .collect(),
        }
    }

    /// Human-readable one-line change summary.
    pub fn describe(&self) -> String {
        if self.old.is_none() {
            // The file records every option, on or off.
            return format!(
                "new file ({} of {} on)",
                self.enabled.len(),
                self.complete.len()
            );
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
/// default/make.conf value. That value is make's before bsd.port.mk drops
/// the members of a conditional group whose option is off: PORT_OPTIONS
/// lacks such a member, but its helpers ran with it, so recording it off
/// would change the build (`session::built_with`).
pub fn sync_enabled_set(info: &PortInfo, saved: Option<&SavedOptionsFile>) -> BTreeSet<String> {
    let dropped: BTreeSet<String> = crate::session::built_with(info, saved)
        .difference(&crate::session::effective_with(info, saved))
        .cloned()
        .collect();
    let mut enabled = BTreeSet::new();
    for opt in &info.options.complete {
        let on = saved
            .and_then(|s| s.value(opt))
            .unwrap_or_else(|| info.options.effective.contains(opt) || dropped.contains(opt));
        if on {
            enabled.insert(opt.clone());
        }
    }
    enabled
}

/// [`sync_enabled_set`] with the options `saved` gives no line taken from
/// optique's own reading of the layers ([`crate::session::built_with`])
/// rather than from the port's queried PORT_OPTIONS, which in the TUI a
/// refresh has queried with the staged edits in place.
pub fn file_enabled_set(info: &PortInfo, saved: Option<&SavedOptionsFile>) -> BTreeSet<String> {
    let built = crate::session::built_with(info, saved);
    info.options
        .complete
        .iter()
        .filter(|opt| {
            saved
                .and_then(|s| s.value(opt))
                .unwrap_or_else(|| built.contains(*opt))
        })
        .cloned()
        .collect()
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

/// What a write pass does with a port that has no options file, whose
/// options make.conf fully decides (none undecided,
/// `session::undecided_options`), and no edit staged. A file already
/// there is rewritten as before: it has outranked make.conf for every
/// option it records since it was written, and a stale one left as it is
/// would keep options the port lacks, which synth refuses
/// (`synth::refuses`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Covered<'e> {
    /// Write it as any other (`decide`, whose plan names its ports).
    Write,
    /// Leave it, as `make config-conditional` does when NEW_OPTIONS is
    /// empty (`sync`, the TUI's apply): a file would record make.conf's
    /// choices and outrank them from then on (bsd.options.mk applies the
    /// file after OPTIONS_SET/UNSET), so the port would stop following a
    /// later make.conf edit.
    ///
    /// `edited` names the options files whose staged values the user
    /// changed, which are written all the same; `sync` stages none.
    Leave { edited: &'e BTreeSet<String> },
}

/// [`plan_writes_for`] writing every file, those make.conf fully decides
/// included ([`Covered::Write`]).
pub fn plan_writes<'a>(
    ports: impl Iterator<Item = (&'a PortKey, &'a PortInfo, BTreeSet<String>)>,
    files: &OptionsFiles,
    minimal: bool,
) -> PlannedWrites {
    plan_writes_for(ports, files, minimal, Covered::Write)
}

/// Build the list of files that actually need (re)writing.
///
/// A file is skipped when it is semantically identical: same complete option
/// list and same IMPLIES-closed enabled set (pkgname-only changes don't force a rewrite,
/// matching `make config-conditional` behavior).
///
/// Flavors of one origin share a single options file, and their computed
/// option sets may legitimately differ (a flavor can change defaults or
/// exclude options — e.g. devel/git@lite). The file is written from its
/// owner's point of view — the default flavor, exactly like `make config` on
/// the plain origin would, else the widest view present, the session's rule
/// (`session::outranks`); divergent other flavors are reported as warnings.
/// In `minimal` mode a port whose final enabled set equals the no-file
/// outcome (defaults + make.conf layers, IMPLIES-closed) is not persisted:
/// an existing file becomes a removal, a missing one is left missing.
pub fn plan_writes_for<'a>(
    ports: impl Iterator<Item = (&'a PortKey, &'a PortInfo, BTreeSet<String>)>,
    files: &OptionsFiles,
    minimal: bool,
    covered: Covered<'_>,
) -> PlannedWrites {
    let mut groups: std::collections::BTreeMap<String, Vec<Candidate<'a>>> = Default::default();
    for (key, info, enabled) in ports {
        // A flavor make.conf moves OPTIONS_FILE for reads none of the
        // shared file (`session::moved`): it neither writes nor judges it.
        if !info.options.has_options() || crate::session::moved(info).is_some() {
            continue;
        }
        let preferred = crate::session::is_preferred(info);
        groups
            .entry(info.options_name.clone())
            .or_default()
            .push(Candidate {
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
        // A file optique cannot model (`session::unmanaged_file`: a line it
        // cannot read, an options.local resetting a list or assigning one
        // with `?=`): no write or removal of it can say what make ends up
        // with, and a write would drop its lines (the startup warning names
        // it).
        if crate::session::unmanaged_file(&files.combined(options_name)).is_some() {
            continue;
        }
        // The file's owner writes it, by the session's rule
        // (`session::outranks`): the default flavor, else the widest view,
        // the first on ties.
        let chosen = cands
            .iter()
            .reduce(|owner, c| {
                if crate::session::outranks(c.info, owner.info) {
                    c
                } else {
                    owner
                }
            })
            .expect("a group holds at least one candidate");
        // Read with its options.local, as make reads it; what a write
        // carries over comes from the file alone, which is all it writes.
        // Without an options file there is no old file, options.local or
        // not: the write is a new one.
        let old = files
            .own(options_name)
            .map(|own| files.under_local(options_name, own));
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
                    crate::session::built_with(c.info, Some(&with_file))
                        == crate::session::built_with(c.info, Some(&without))
                });
            if redundant {
                if files.bytes(options_name).is_some() {
                    removals.push(crate::clean::Removal {
                        options_name: options_name.clone(),
                        dir: files.entry_dir(options_name),
                        reason: "defaults + make.conf produce this exact configuration".to_string(),
                    });
                }
                continue;
            }
        }
        // No file yet, make.conf decides every option of every flavor that
        // would read one, and nobody edited it: as config-conditional, none
        // is created. Edits are the caller's to name: in the TUI a refresh
        // queries them into the port's effective options, so no set
        // compared here would tell them from what sync records.
        if let Covered::Leave { edited } = covered
            && !edited.contains(options_name)
            && files.hash(options_name) == crate::optfiles::ABSENT
        {
            let saved = files.saved(options_name);
            let settled = |c: &Candidate| {
                crate::session::undecided_options(c.info, saved.as_ref(), false).is_empty()
            };
            // A default flavor writes only its own options (`planned_file`):
            // a file would settle nothing another flavor alone has, so only
            // the owner's count, as `current` judges below.
            let decided = if chosen.preferred {
                settled(chosen)
            } else {
                cands.iter().all(settled)
            };
            if decided {
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
        // Both sides read with options.local, as make reads either.
        let local = |file| files.under_local(options_name, file);
        let current = |old: &SavedOptionsFile| {
            if chosen.preferred {
                up_to_date(old, info, enabled, &local)
            } else {
                cands
                    .iter()
                    .all(|c| up_to_date(old, c.info, &c.enabled, &local))
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
    PlannedWrites {
        writes,
        removals,
        warnings,
    }
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
    let why = if chosen.preferred {
        "default flavor"
    } else {
        "widest view"
    };
    Some(format!(
        "{}: flavors disagree on shared options file {} — using {} ({}); \
         differing shared options: {}",
        other.key,
        options_name,
        chosen.key,
        why,
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
///
/// `old` is read with its options.local, and `local` reads the would-be
/// file the same way: a SET glob there (`OPTIONS_FILE_SET+=C*`) changes
/// both sides alike, and only one side read with it would never match.
fn up_to_date(
    old: &SavedOptionsFile,
    info: &PortInfo,
    enabled: &BTreeSet<String>,
    local: &dyn Fn(SavedOptionsFile) -> SavedOptionsFile,
) -> bool {
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
    let would = local(written_file(info, enabled));
    crate::session::built_with(info, Some(old)) == crate::session::built_with(info, Some(&would))
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
fn planned_file(
    chosen: &Candidate,
    cands: &[Candidate],
    old: Option<&SavedOptionsFile>,
) -> Planned {
    let mut file = written_file(chosen.info, &chosen.enabled);
    let mut kept: Vec<String> = Vec::new();
    let mut foreign = false;
    if chosen.preferred {
        return Planned {
            file,
            kept,
            foreign,
        };
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
        // A pattern (`OPTIONS_FILE_UNSET+=A*`) is no option another flavor
        // lacks: kept, make would apply it after every SET line, and take
        // off the options of this view it matches, edited on or not.
        let view = file.complete.clone();
        let covers = |o: &String| view.iter().any(|c| str_match(c, o));
        for o in old.complete.iter().chain(old.recorded()) {
            if !file.complete.contains(o) && !covers(o) {
                file.complete.push(o.clone());
                if old.set.contains(o) {
                    file.set.insert(o.clone());
                }
                // Off by its own line, or by a pattern dropped above (which
                // matched an option of the view too): a line of its own
                // keeps it off once the pattern is gone.
                if old.unset.contains(o) || old.value(o) == Some(false) {
                    file.unset.insert(o.clone());
                }
                kept.push(o.clone());
                foreign = true;
            }
        }
    }
    Planned {
        file,
        kept,
        foreign,
    }
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
        unset: info
            .options
            .complete
            .iter()
            .filter(|o| !enabled.contains(*o))
            .cloned()
            .collect(),
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
    // Nor can one make.conf moves OPTIONS_FILE for, which reads another file.
    let optionless: BTreeSet<&str> = ports
        .values()
        .filter(|i| crate::session::is_preferred(i) && crate::session::moved(i).is_none())
        .map(|i| i.options_name.as_str())
        .filter(|name| !with_options.contains(name))
        .collect();
    optionless
        .into_iter()
        .filter(|name| files.bytes(name).is_some())
        // Nor one optique cannot model, which is not its to remove.
        .filter(|name| crate::session::unmanaged_file(&files.combined(name)).is_none())
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
/// leaves the options dir as it was. A rename that fails (its place taken by
/// a directory) stops the pass there: the files renamed before it stay, as
/// `written` counts, and the rest are discarded, each failed.
pub fn apply(writes: &[PendingWrite]) -> ApplySummary {
    let mut summary = ApplySummary {
        written: 0,
        failed: Vec::new(),
    };
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
    let mut staged = staged.into_iter();
    for (w, s) in staged.by_ref() {
        match fs::rename(&s.tmp, &w.path) {
            Ok(()) => summary.written += 1,
            Err(e) => {
                summary.failed.push((
                    w.key.clone(),
                    format!("renaming into place: {}: {e}", w.path.display()),
                ));
                s.discard();
                break;
            }
        }
    }
    for (w, s) in staged {
        summary.failed.push((
            w.key.clone(),
            "not renamed into place: an earlier rename failed".to_string(),
        ));
        s.discard();
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
    // Named after the file: two files of one entry (`options`, a flavor's
    // `lite-options`) are written in one pass, each through its own.
    let name = w
        .path
        .file_name()
        .context("options path has no file name")?;
    let tmp = dir.join(format!(".{}.optique.tmp", name.to_string_lossy()));
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
    if let Err(e) = f
        .write_all(w.content.as_bytes())
        .and_then(|()| f.sync_all())
    {
        let tmp = staged.tmp.display().to_string();
        staged.discard();
        return Err(e).with_context(|| format!("writing {tmp}"));
    }
    Ok(staged)
}

#[cfg(test)]
mod tests;
