
use std::fs;
use std::path::{Path, PathBuf};

use crate::model::origin::{valid_origin, PortKey};
use crate::model::port::PortInfo;
use crate::moved::{Moved, MovedResult};

/// One options-dir entry scheduled for removal.
#[derive(Debug)]
pub struct Removal {
    pub options_name: String,
    pub dir: PathBuf,
    pub reason: String,
}

/// An options-dir entry that still corresponds to a live port.
#[derive(Debug)]
pub struct LiveEntry {
    pub options_name: String,
    pub dir: PathBuf,
    pub key: PortKey,
}

/// Map an OPTIONS_NAME directory back to a port origin. Categories never
/// contain '_', so the first '_' splits category from port name.
pub fn origin_from_options_name(name: &str) -> Option<String> {
    let (cat, port) = name.split_once('_')?;
    let origin = format!("{cat}/{port}");
    valid_origin(&origin).then_some(origin)
}

/// Classify every entry of the options dir: entries whose port vanished from
/// the tree (directly or through MOVED) become removals, the rest are live.
pub fn classify_entries(
    options_dir: &Path,
    portsdir: &Path,
    overlays: &[std::path::PathBuf],
    moved: &Moved,
) -> (Vec<Removal>, Vec<LiveEntry>, Vec<String>) {
    // A port lives in the main tree or any overlay; only absent from all of
    // them has it vanished.
    let exists = |o: &str| crate::query::makerunner::locate(portsdir, overlays, o).is_some();
    let mut removals = Vec::new();
    let mut live = Vec::new();
    let mut warnings = Vec::new();

    let Ok(entries) = fs::read_dir(options_dir) else {
        return (removals, live, warnings);
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() || !dir.join("options").is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(origin) = origin_from_options_name(&name) else {
            warnings.push(format!("{name}: cannot map back to a port origin, left alone"));
            continue;
        };
        if exists(&origin) {
            live.push(LiveEntry {
                options_name: name,
                dir,
                key: PortKey::new(origin, None),
            });
            continue;
        }
        let reason = match moved.resolve(&origin, exists) {
            MovedResult::MovedTo { origin: new, .. } => {
                format!("port moved to {new} (its options live under another name)")
            }
            MovedResult::Unchanged => "port no longer exists in the tree".to_string(),
            gone => gone.gone_reason().expect("removed and cycle both have a reason"),
        };
        removals.push(Removal { options_name: name, dir, reason });
    }
    removals.sort_by(|a, b| a.options_name.cmp(&b.options_name));
    live.sort_by(|a, b| a.options_name.cmp(&b.options_name));
    (removals, live, warnings)
}

/// Split live entries against the set of OPTIONS_NAMEs actually reached by a
/// dependency closure: entries nobody in the closure reads become removals.
/// Both halves come back sorted by options name.
pub fn split_unused(
    live: Vec<LiveEntry>,
    used: &std::collections::HashSet<String>,
) -> (Vec<LiveEntry>, Vec<Removal>) {
    let (mut kept, unused): (Vec<_>, Vec<_>) =
        live.into_iter().partition(|e| used.contains(&e.options_name));
    let mut removals: Vec<Removal> = unused
        .into_iter()
        .map(|e| Removal {
            options_name: e.options_name,
            dir: e.dir,
            reason: "not needed by the given list".to_string(),
        })
        .collect();
    kept.sort_by(|a, b| a.options_name.cmp(&b.options_name));
    removals.sort_by(|a, b| a.options_name.cmp(&b.options_name));
    (kept, removals)
}

/// How the options file changes the outcome versus defaults + make.conf
/// and `local`, the entry's options.local read with no options file (an
/// empty file without one): make reads options.local either way
/// (bsd.options.mk:342), so the file is redundant only when it alone
/// changes nothing. `+OPT` = the file turns it on, `-OPT` = the file turns
/// it off. Empty means the file is redundant.
pub fn redundancy_diff(info: &PortInfo, local: &crate::optionsfile::SavedOptionsFile) -> Vec<String> {
    let nofile = crate::session::effective_with(info, Some(local));
    let effective = &info.options.effective;
    let mut out: Vec<String> =
        effective.difference(&nofile).map(|o| format!("+{o}")).collect();
    out.extend(nofile.difference(effective).map(|o| format!("-{o}")));
    out
}

/// Delete the options file and (when empty) its directory.
/// A sibling `options.local` is never touched and keeps the directory.
///
/// A file that cannot be read is left in place, with the reason as the
/// error: deleting needs only the directory's write permission, so nothing
/// else would stop the removal of a file whose content nobody has seen.
pub fn remove_entry(removal: &Removal) -> std::io::Result<Option<String>> {
    let file = removal.dir.join("options");
    if let Err(e) = fs::File::open(&file) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(std::io::Error::new(e.kind(), format!("cannot read {}, left in place: {e}", file.display())));
        }
    }
    fs::remove_file(&file)?;
    if removal.dir.join("options.local").is_file() {
        return Ok(Some(format!(
            "{}: kept options.local (user-managed)",
            removal.options_name
        )));
    }
    let _ = fs::remove_dir(&removal.dir); // non-empty dirs are left alone
    Ok(None)
}

/// What a removal pass did.
#[derive(Debug, Default)]
pub struct Removed {
    pub count: usize,
    /// Notes from removals that left something behind (an `options.local`).
    pub notes: Vec<String>,
    /// `(options_name, message)` for each entry that could not be removed.
    pub failed: Vec<(String, String)>,
}

/// Remove every entry; a failure does not stop the rest.
pub fn remove_all(removals: &[Removal]) -> Removed {
    let mut out = Removed::default();
    for r in removals {
        match remove_entry(r) {
            Ok(note) => {
                out.count += 1;
                out.notes.extend(note);
            }
            Err(e) => out.failed.push((r.options_name.clone(), e.to_string())),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::options::PortOptions;

    #[test]
    fn options_name_mapping() {
        assert_eq!(origin_from_options_name("www_nginx").as_deref(), Some("www/nginx"));
        assert_eq!(
            origin_from_options_name("devel_py-Automat").as_deref(),
            Some("devel/py-Automat")
        );
        // port names keep their own underscores
        assert_eq!(
            origin_from_options_name("audio_baresip_gtk2").as_deref(),
            Some("audio/baresip_gtk2")
        );
        assert_eq!(origin_from_options_name("nounderscore"), None);
    }

    fn info_with(options: PortOptions) -> PortInfo {
        PortInfo {
            pkgname: "port-1.0".into(),
            options_name: "cat_port".into(),
            options,
            ..PortInfo::stub("cat/port")
        }
    }

    #[test]
    fn classify_against_fake_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let portsdir = tmp.path().join("ports");
        let optdir = tmp.path().join("options");
        // Live port: cat/alive.
        std::fs::create_dir_all(portsdir.join("cat/alive")).unwrap();
        std::fs::write(portsdir.join("cat/alive/Makefile"), "# port\n").unwrap();
        for name in ["cat_alive", "cat_gone", "cat_plain", "cat_renamed", "weird"] {
            std::fs::create_dir_all(optdir.join(name)).unwrap();
            std::fs::write(optdir.join(name).join("options"), "x\n").unwrap();
        }
        // A dir without an options file must be ignored entirely.
        std::fs::create_dir_all(optdir.join("cat_emptydir")).unwrap();

        let moved = Moved::parse(
            "cat/gone||2026-01-01|expired\ncat/plain||2026-01-01|\n\
             cat/renamed|cat/alive|2026-01-01|renamed\n",
        );
        let (removals, live, warnings) = classify_entries(&optdir, &portsdir, &[], &moved);

        assert_eq!(live.len(), 1);
        assert_eq!(live[0].key.to_string(), "cat/alive");
        let names: Vec<&str> = removals.iter().map(|r| r.options_name.as_str()).collect();
        assert_eq!(names, vec!["cat_gone", "cat_plain", "cat_renamed"]);
        assert!(removals[0].reason.contains("expired"));
        assert_eq!(removals[1].reason, "port removed from tree", "no empty reason");
        assert!(removals[2].reason.contains("cat/alive"), "rename target named");
        // "weird" has no '_' -> unmappable, warned about, untouched.
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("weird"));

        // cat/plain lives in an overlay: not vanished.
        let overlay = tmp.path().join("overlay");
        std::fs::create_dir_all(overlay.join("cat/plain")).unwrap();
        std::fs::write(overlay.join("cat/plain/Makefile"), "# port\n").unwrap();
        let (removals, live, _) = classify_entries(&optdir, &portsdir, &[overlay], &moved);
        let live: Vec<String> = live.iter().map(|l| l.key.to_string()).collect();
        assert_eq!(live, ["cat/alive", "cat/plain"]);
        assert!(!removals.iter().any(|r| r.options_name == "cat_plain"));
    }

    #[test]
    fn split_unused_partitions() {
        let entry = |name: &str, origin: &str| LiveEntry {
            options_name: name.to_string(),
            dir: PathBuf::from("/opt").join(name),
            key: PortKey::parse(origin).unwrap(),
        };
        // Deliberately unsorted input, only www/nginx is in the closure.
        let live = vec![
            entry("www_nginx", "www/nginx"),
            entry("mail_dovecot", "mail/dovecot"),
            entry("cat_alive", "cat/alive"),
        ];
        let used: std::collections::HashSet<String> = ["www_nginx".to_string()].into();

        let (kept, removals) = split_unused(live, &used);
        let kept_names: Vec<&str> = kept.iter().map(|e| e.options_name.as_str()).collect();
        assert_eq!(kept_names, vec!["www_nginx"]);
        let gone: Vec<&str> = removals.iter().map(|r| r.options_name.as_str()).collect();
        assert_eq!(gone, vec!["cat_alive", "mail_dovecot"], "removals come back sorted");
        assert!(removals.iter().all(|r| r.reason == "not needed by the given list"));
        assert_eq!(removals[0].dir, PathBuf::from("/opt/cat_alive"));
    }

    #[test]
    fn redundancy_diff_directions() {
        // defaults {A}; file result: B on, A off -> +B -A.
        let o = PortOptions {
            complete: vec!["A".into(), "B".into()],
            defaults: ["A".to_string()].into(),
            effective: ["B".to_string()].into(),
            ..Default::default()
        };
        let d = redundancy_diff(&info_with(o), &Default::default());
        assert_eq!(d, vec!["+B".to_string(), "-A".to_string()]);
    }

    #[test]
    fn redundancy_check() {
        // defaults {A}, mc unsets B; file recorded exactly that -> effective {A}.
        let mut o = PortOptions {
            complete: vec!["A".into(), "B".into()],
            defaults: ["A".to_string()].into(),
            mc_unset: ["B".to_string()].into(),
            effective: ["A".to_string()].into(),
            ..Default::default()
        };
        assert!(redundancy_diff(&info_with(o.clone()), &Default::default()).is_empty());

        // file deviates: B enabled although default+mc say off.
        o.effective = ["A".to_string(), "B".to_string()].into();
        assert!(!redundancy_diff(&info_with(o), &Default::default()).is_empty());
    }

    /// bsd.options.mk:342 reads options.local with or without the options
    /// file: a file saying the defaults (A on, B off) is not redundant when
    /// options.local sets B, which the file's UNSET line alone keeps off.
    #[test]
    fn redundancy_is_judged_with_options_local() {
        let o = PortOptions {
            complete: vec!["A".into(), "B".into()],
            defaults: ["A".to_string()].into(),
            // Queried with the file and options.local in place.
            effective: ["A".to_string()].into(),
            ..Default::default()
        };
        let local = crate::optionsfile::SavedOptionsFile::default().with_local("OPTIONS_FILE_SET+=B\n");
        assert_eq!(redundancy_diff(&info_with(o), &local), ["-B"]);
    }

    /// A file nobody could read is not deleted: removing it needs only the
    /// directory's permission, so nothing else would stop it.
    #[test]
    fn removal_leaves_an_unreadable_file_in_place() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("cat_port");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("options");
        std::fs::write(&file, "OPTIONS_FILE_SET+=A\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unreadable = std::fs::read(&file).is_err(); // root reads anyway
        let removal =
            Removal { options_name: "cat_port".into(), dir: dir.clone(), reason: "test".into() };
        let result = remove_entry(&removal);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).ok();
        if unreadable {
            let err = result.unwrap_err().to_string();
            assert!(err.contains("left in place"), "{err}");
            assert!(file.is_file(), "still there");
        }
    }

    #[test]
    fn removal_keeps_options_local() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("cat_port");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("options"), "x\n").unwrap();
        fs::write(dir.join("options.local"), "y\n").unwrap();
        let r = Removal { options_name: "cat_port".into(), dir: dir.clone(), reason: String::new() };
        let note = remove_entry(&r).unwrap();
        assert!(note.is_some());
        assert!(!dir.join("options").exists());
        assert!(dir.join("options.local").exists());

        let dir2 = tmp.path().join("cat_other");
        fs::create_dir_all(&dir2).unwrap();
        fs::write(dir2.join("options"), "x\n").unwrap();
        let r2 = Removal { options_name: "cat_other".into(), dir: dir2.clone(), reason: String::new() };
        assert!(remove_entry(&r2).unwrap().is_none());
        assert!(!dir2.exists());
    }
}
