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

/// Build the list of files that actually need (re)writing.
///
/// A file is skipped when it is semantically identical: same complete option
/// list and same enabled set (pkgname-only changes don't force a rewrite,
/// matching `make config-conditional` behavior).
pub fn plan_writes<'a>(
    ports: impl Iterator<Item = (&'a PortKey, &'a PortInfo, BTreeSet<String>)>,
    options_dir: &Path,
) -> Result<Vec<PendingWrite>> {
    let mut writes = Vec::new();
    let mut by_name: std::collections::HashMap<String, (PortKey, BTreeSet<String>)> =
        std::collections::HashMap::new();
    for (key, info, enabled) in ports {
        if !info.options.has_options() {
            continue;
        }
        // Flavors of one origin share an options file; identical states are
        // deduplicated, conflicting states are an error.
        if let Some((prev, prev_enabled)) = by_name.get(&info.options_name) {
            if *prev_enabled == enabled {
                continue;
            }
            anyhow::bail!(
                "{} and {} share options file {} but stage different option sets",
                prev,
                key,
                info.options_name
            );
        }
        by_name.insert(info.options_name.clone(), (key.clone(), enabled.clone()));

        let path = options_dir.join(&info.options_name).join("options");
        let old = SavedOptionsFile::load(&path);
        if let Some(old) = &old {
            let same_list = old.complete.iter().collect::<BTreeSet<_>>()
                == info.options.complete.iter().collect::<BTreeSet<_>>();
            let same_set = old.set == enabled;
            if same_list && same_set {
                continue;
            }
        }
        let content = optionsfile::render(&info.pkgname, &info.options.complete, &enabled);
        writes.push(PendingWrite {
            key: key.clone(),
            options_name: info.options_name.clone(),
            path,
            old,
            enabled,
            complete: info.options.complete.clone(),
            content,
        });
    }
    Ok(writes)
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
        let writes = plan_writes(
            std::iter::once((&info.key.clone(), &info, enabled)),
            tmp.path(),
        )
        .unwrap();
        assert!(writes.is_empty());
    }

    #[test]
    fn apply_writes_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let info = mk_info(&["A"], &["A"]);
        let enabled: BTreeSet<String> = ["A".to_string()].into();
        let writes =
            plan_writes(std::iter::once((&info.key.clone(), &info, enabled)), tmp.path()).unwrap();
        assert_eq!(writes.len(), 1);
        let summary = apply(&writes);
        assert_eq!(summary.written, 1);
        assert!(summary.failed.is_empty());
        let saved = SavedOptionsFile::load(&tmp.path().join("cat_port/options")).unwrap();
        assert!(saved.set.contains("A"));
        assert_eq!(saved.options_read, "port-1.0");
    }
}
