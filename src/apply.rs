use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::model::origin::PortKey;
use crate::model::port::PortInfo;
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
    pub content: String,
}

impl PendingWrite {
    /// Human-readable one-line change summary.
    pub fn describe(&self) -> String {
        match &self.old {
            None => format!("new file ({} options)", self.enabled.len()),
            Some(old) => {
                let cur: BTreeSet<&str> = self.enabled.iter().map(String::as_str).collect();
                let was: BTreeSet<&str> = old.set.iter().map(String::as_str).collect();
                let now_known: BTreeSet<&str> =
                    self.complete.iter().map(String::as_str).collect();
                let file_known: BTreeSet<&str> = old
                    .complete
                    .iter()
                    .chain(old.set.iter())
                    .chain(old.unset.iter())
                    .map(String::as_str)
                    .collect();
                let mut parts = Vec::new();
                let adopted: Vec<&str> = now_known
                    .iter()
                    .filter(|o| !file_known.contains(**o))
                    .map(|o| *o)
                    .collect();
                let dropped: Vec<&str> = file_known
                    .iter()
                    .filter(|o| !now_known.contains(**o))
                    .map(|o| *o)
                    .collect();
                let turned_on: Vec<&str> = cur
                    .difference(&was)
                    .filter(|o| file_known.contains(**o))
                    .map(|o| *o)
                    .collect();
                let turned_off: Vec<&str> = was
                    .iter()
                    .filter(|o| now_known.contains(**o) && !cur.contains(**o))
                    .map(|o| *o)
                    .collect();
                if !turned_on.is_empty() {
                    parts.push(format!("+{}", turned_on.join(" +")));
                }
                if !turned_off.is_empty() {
                    parts.push(format!("-{}", turned_off.join(" -")));
                }
                if !adopted.is_empty() {
                    parts.push(format!(
                        "new: {}",
                        adopted
                            .iter()
                            .map(|o| {
                                if cur.contains(*o) { format!("{o}(on)") } else { format!("{o}(off)") }
                            })
                            .collect::<Vec<_>>()
                            .join(" ")
                    ));
                }
                if !dropped.is_empty() {
                    parts.push(format!("dropped: {}", dropped.join(" ")));
                }
                if parts.is_empty() {
                    parts.push("option list refresh".to_string());
                }
                parts.join(" · ")
            }
        }
    }
}

/// Decide the final enabled set for a port when syncing non-interactively:
/// keep the saved file's choices for options that still exist, take the
/// effective state (defaults + make.conf) for options the file doesn't know,
/// and drop options that no longer exist.
pub fn sync_enabled_set(info: &PortInfo, saved: Option<&SavedOptionsFile>) -> BTreeSet<String> {
    let mut enabled = BTreeSet::new();
    for opt in &info.options.complete {
        let known_to_file = saved.map(|s| {
            s.complete.contains(opt) || s.set.contains(opt) || s.unset.contains(opt)
        });
        let on = match (saved, known_to_file) {
            (Some(s), Some(true)) => s.set.contains(opt),
            _ => info.options.effective.contains(opt),
        };
        if on {
            enabled.insert(opt.clone());
        }
    }
    enabled
}

/// Result of planning: the files to write plus non-fatal notes.
pub struct PlannedWrites {
    pub writes: Vec<PendingWrite>,
    pub warnings: Vec<String>,
}

/// Build the list of files that actually need (re)writing.
///
/// A file is skipped when it is semantically identical: same complete option
/// list and same enabled set (pkgname-only changes don't force a rewrite,
/// matching `make config-conditional` behavior).
///
/// Flavors of one origin share a single options file, and their computed
/// option sets may legitimately differ (a flavor can change defaults or
/// exclude options — e.g. devel/git@lite). The file is written from the
/// default flavor's point of view, exactly like `make config` on the plain
/// origin would; divergent non-default flavors are reported as warnings.
pub fn plan_writes<'a>(
    ports: impl Iterator<Item = (&'a PortKey, &'a PortInfo, BTreeSet<String>)>,
    options_dir: &Path,
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
    let mut warnings = Vec::new();
    for (options_name, cands) in &groups {
        let chosen = cands.iter().find(|c| c.preferred).unwrap_or(&cands[0]);
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
        let path = options_dir.join(options_name).join("options");
        let old = SavedOptionsFile::load(&path);
        if let Some(old) = &old {
            let same_list = old.complete.iter().collect::<BTreeSet<_>>()
                == info.options.complete.iter().collect::<BTreeSet<_>>();
            let same_set = old.set == *enabled;
            if same_list && same_set {
                continue;
            }
        }
        let content = optionsfile::render(&info.pkgname, &info.options.complete, enabled);
        writes.push(PendingWrite {
            key: chosen.key.clone(),
            options_name: options_name.clone(),
            path,
            old,
            enabled: enabled.clone(),
            complete: info.options.complete.clone(),
            content,
        });
    }
    PlannedWrites { writes, warnings }
}

pub struct ApplySummary {
    pub written: usize,
    pub failed: Vec<(PortKey, String)>,
}

/// Write every pending file atomically (tmp + fsync + rename).
pub fn apply(writes: &[PendingWrite]) -> ApplySummary {
    let mut summary = ApplySummary { written: 0, failed: Vec::new() };
    for w in writes {
        match write_atomic(w) {
            Ok(()) => summary.written += 1,
            Err(e) => summary.failed.push((w.key.clone(), format!("{e:#}"))),
        }
    }
    summary
}

fn write_atomic(w: &PendingWrite) -> Result<()> {
    let dir = w.path.parent().context("options path has no parent")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = dir.join(".options.optique.tmp");
    {
        let mut f = fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(w.content.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &w.path)
        .with_context(|| format!("renaming into place: {}", w.path.display()))?;
    Ok(())
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

    #[test]
    fn unconfigured_takes_effective() {
        let info = mk_info(&["A", "B"], &["B"]);
        let enabled = sync_enabled_set(&info, None);
        assert_eq!(enabled, ["B"].iter().map(|s| s.to_string()).collect());
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
            plan_writes(std::iter::once((&info.key.clone(), &info, enabled)), tmp.path());
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
            plan_writes(ports.iter().map(|(k, i, e)| (k, *i, e.clone())), tmp.path());
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
            plan_writes(ports.iter().map(|(k, i, e)| (k, *i, e.clone())), tmp.path());
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
            options_name: "cat_port".into(),
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
            content: "x\n".into(),
        };
        let summary = apply(&[w]);
        assert_eq!(summary.written, 0);
        assert_eq!(summary.failed.len(), 1);
    }

    #[test]
    fn apply_writes_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let info = mk_info(&["A"], &["A"]);
        let enabled: BTreeSet<String> = ["A".to_string()].into();
        let planned =
            plan_writes(std::iter::once((&info.key.clone(), &info, enabled)), tmp.path());
        let writes = planned.writes;
        assert_eq!(writes.len(), 1);
        let summary = apply(&writes);
        assert_eq!(summary.written, 1);
        assert!(summary.failed.is_empty());
        let saved = SavedOptionsFile::load(&tmp.path().join("cat_port/options")).unwrap();
        assert!(saved.set.contains("A"));
        assert_eq!(saved.options_read, "port-1.0");
    }
}
