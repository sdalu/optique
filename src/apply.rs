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
    pub path: PathBuf,
    pub old: Option<SavedOptionsFile>,
    /// Final enabled set the file will record.
    pub enabled: BTreeSet<String>,
    /// Current complete option list (display/diff aid).
    pub complete: Vec<String>,
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
        let file_known: BTreeSet<&str> =
            old.set.iter().chain(old.unset.iter()).map(String::as_str).collect();
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
                .filter(|o| !now_known.contains(**o))
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
                    .map(|(o, on)| format!("{o}({})", if *on { "on" } else { "off" }))
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
    struct Candidate<'a> {
        key: PortKey,
        info: &'a PortInfo,
        enabled: BTreeSet<String>,
        /// Default flavor of its origin (or unflavored) — owns the file.
        preferred: bool,
    }
    let mut groups: std::collections::BTreeMap<String, Vec<Candidate<'a>>> = Default::default();
    for (key, info, enabled) in ports {
        if !info.options.has_options() {
            continue;
        }
        let preferred = match &info.canonical.flavor {
            None => true,
            Some(f) => info.flavors.first() == Some(f),
        };
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
        let chosen = cands.iter().find(|c| c.preferred).unwrap_or(&cands[0]);
        if minimal {
            // Compare IMPLIES-closed sets: the framework adds implied options
            // either way, so they must not count as a deviation.
            let closed =
                crate::session::close_implies(chosen.info, chosen.enabled.clone());
            if closed == crate::session::nofile_effective(chosen.info) {
                if files.bytes(options_name).is_some() {
                    removals.push(crate::clean::Removal {
                        options_name: options_name.clone(),
                        dir: files.dir().join(options_name),
                        reason: "defaults + make.conf produce this exact configuration"
                            .to_string(),
                    });
                }
                continue;
            }
        }
        let chosen_complete: BTreeSet<&str> =
            chosen.info.options.complete.iter().map(String::as_str).collect();
        for other in cands.iter().filter(|c| !std::ptr::eq(*c, chosen)) {
            if other.enabled == chosen.enabled {
                continue;
            }
            // Differences confined to options one flavor doesn't even have
            // (OPTIONS_EXCLUDE etc.) are structural and harmless: the file
            // records the default flavor's view, the other flavor ignores
            // options it excludes. Only disagreement on options BOTH
            // flavors carry is a real decision worth surfacing.
            let other_complete: BTreeSet<&str> =
                other.info.options.complete.iter().map(String::as_str).collect();
            let shared_diff: Vec<&str> = other
                .enabled
                .symmetric_difference(&chosen.enabled)
                .map(String::as_str)
                .filter(|o| chosen_complete.contains(o) && other_complete.contains(o))
                .collect();
            if shared_diff.is_empty() {
                continue;
            }
            warnings.push(format!(
                "{}: flavors disagree on shared options file {} — using {} (default flavor); \
                 differing shared options: {}",
                other.key,
                options_name,
                chosen.key,
                shared_diff.join(" ")
            ));
        }
        let info = chosen.info;
        let enabled = &chosen.enabled;
        let path = files.path(options_name);
        let old = files.saved(options_name);
        if let Some(old) = &old {
            let same_list = old.complete.iter().collect::<BTreeSet<_>>()
                == info.options.complete.iter().collect::<BTreeSet<_>>();
            // IMPLIES-closed on both sides: sync plans from the file as
            // written, the session from its closed baseline, and make adds
            // the implied options either way.
            let close = |set: &BTreeSet<String>| crate::session::close_implies(info, set.clone());
            let same_set = close(&old.set) == close(enabled);
            if same_list && same_set {
                continue;
            }
        }
        let content = optionsfile::render(&info.pkgname, &info.options.complete, enabled);
        writes.push(PendingWrite {
            key: chosen.key.clone(),
            path,
            old,
            enabled: enabled.clone(),
            complete: info.options.complete.clone(),
            content,
        });
    }
    PlannedWrites { writes, removals, warnings }
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
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut out = Vec::new();
    for info in ports.values() {
        if info.options.has_options()
            || with_options.contains(info.options_name.as_str())
            || !seen.insert(info.options_name.as_str())
        {
            continue;
        }
        if files.bytes(&info.options_name).is_some() {
            out.push(crate::clean::Removal {
                options_name: info.options_name.clone(),
                dir: files.dir().join(&info.options_name),
                reason: "port has no options anymore".to_string(),
            });
        }
    }
    out
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
        let key = PortKey::parse("cat/port").unwrap();
        PortInfo {
            key: key.clone(),
            canonical: key,
            pkgname: "port-1.0".into(),
            flavors: vec![],
            options_name: "cat_port".into(),
            options: PortOptions {
                complete: complete.iter().map(|s| s.to_string()).collect(),
                effective: effective.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            deps: vec![],
            broken: None,
            ignore: None,
            deprecated: None,
            pkg_help: None,
            about: Default::default(),
            default_versions: vec![],
            warnings: vec![],
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
            path: PathBuf::from("/nonexistent/cat_port/options"),
            old: old.map(SavedOptionsFile::parse),
            enabled: enabled.iter().map(|s| s.to_string()).collect(),
            complete: complete.iter().map(|s| s.to_string()).collect(),
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

        let ports = vec![
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

    #[test]
    fn describe_covers_all_change_kinds() {
        // Old file: knows A(on) B(off) GONE(off); current: A B NEWOPT.
        let old = SavedOptionsFile::parse(
            "_OPTIONS_READ=p-1\n_FILE_COMPLETE_OPTIONS_LIST=A B GONE\n\
             OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\nOPTIONS_FILE_UNSET+=GONE\n",
        );
        let w = PendingWrite {
            key: PortKey::parse("cat/port").unwrap(),
            path: std::path::PathBuf::from("/nonexistent"),
            old: Some(old),
            // A turned off, B turned on, NEWOPT adopted on, GONE dropped.
            enabled: ["B".to_string(), "NEWOPT".to_string()].into(),
            complete: vec!["A".into(), "B".into(), "NEWOPT".into()],
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

    #[test]
    fn apply_reports_failures() {
        let w = PendingWrite {
            key: PortKey::parse("cat/port").unwrap(),
            // Parent is a file, not a dir -> create_dir_all fails.
            path: std::path::PathBuf::from("/dev/null/cat_port/options"),
            old: None,
            enabled: BTreeSet::new(),
            complete: vec![],
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
            path: tmp.path().join(name).join("options"),
            old: None,
            enabled: BTreeSet::new(),
            complete: vec![],
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
}
