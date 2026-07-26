use std::collections::BTreeSet;
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
    moved: &Moved,
) -> (Vec<Removal>, Vec<LiveEntry>, Vec<String>) {
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
        if portsdir.join(&origin).join("Makefile").is_file() {
            live.push(LiveEntry {
                options_name: name,
                dir,
                key: PortKey::new(origin, None),
            });
            continue;
        }
        let reason = match moved.resolve(&origin) {
            MovedResult::Removed { reason } if !reason.is_empty() => {
                format!("port removed from tree ({reason})")
            }
            MovedResult::Removed { .. } => "port removed from tree".to_string(),
            MovedResult::MovedTo { origin: new, .. } => {
                format!("port moved to {new} (its options live under another name)")
            }
            MovedResult::Unchanged => "port no longer exists in the tree".to_string(),
        };
        removals.push(Removal { options_name: name, dir, reason });
    }
    removals.sort_by(|a, b| a.options_name.cmp(&b.options_name));
    live.sort_by(|a, b| a.options_name.cmp(&b.options_name));
    (removals, live, warnings)
}

/// Effective options as they would be WITHOUT any options file: the
/// bsd.options.mk application order minus the options-file layer.
fn nofile_effective(info: &PortInfo) -> BTreeSet<String> {
    let o = &info.options;
    let complete: BTreeSet<&str> = o.complete.iter().map(String::as_str).collect();
    let mut set: BTreeSet<String> = o
        .defaults
        .iter()
        .filter(|d| complete.contains(d.as_str()))
        .cloned()
        .collect();
    for opt in o.mc_set.iter().chain(o.port_set.iter()) {
        if complete.contains(opt.as_str()) {
            set.insert(opt.clone());
        }
    }
    for opt in o.mc_unset.iter().chain(o.port_unset.iter()) {
        set.remove(opt);
    }
    for opt in &o.force_set {
        if complete.contains(opt.as_str()) {
            set.insert(opt.clone());
        }
    }
    for opt in &o.force_unset {
        set.remove(opt);
    }
    crate::session::close_implies(info, set)
}

/// How the options file changes the outcome versus defaults + make.conf:
/// `+OPT` = the file turns it on, `-OPT` = the file turns it off.
/// Empty means the file is redundant.
pub fn redundancy_diff(info: &PortInfo) -> Vec<String> {
    let nofile = nofile_effective(info);
    let effective = &info.options.effective;
    let mut out: Vec<String> =
        effective.difference(&nofile).map(|o| format!("+{o}")).collect();
    out.extend(nofile.difference(effective).map(|o| format!("-{o}")));
    out
}

/// Does the options file change nothing? True when the effective options
/// computed WITH the file (info.options.effective, queried against the real
/// options dir) equal what defaults + make.conf overlays alone would yield.
pub fn file_is_redundant(info: &PortInfo) -> bool {
    redundancy_diff(info).is_empty()
}

/// Delete the options file and (when empty) its directory.
/// A sibling `options.local` is never touched and keeps the directory.
pub fn remove_entry(removal: &Removal) -> std::io::Result<Option<String>> {
    fs::remove_file(removal.dir.join("options"))?;
    if removal.dir.join("options.local").is_file() {
        return Ok(Some(format!(
            "{}: kept options.local (user-managed)",
            removal.options_name
        )));
    }
    let _ = fs::remove_dir(&removal.dir); // non-empty dirs are left alone
    Ok(None)
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
        let key = PortKey::parse("cat/port").unwrap();
        PortInfo {
            key: key.clone(),
            canonical: key,
            pkgname: "port-1.0".into(),
            flavors: vec![],
            options_name: "cat_port".into(),
            options,
            deps: vec![],
            broken: None,
            ignore: None,
            deprecated: None,
            default_versions: vec![],
            warnings: vec![],
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
        for name in ["cat_alive", "cat_gone", "cat_renamed", "weird"] {
            std::fs::create_dir_all(optdir.join(name)).unwrap();
            std::fs::write(optdir.join(name).join("options"), "x\n").unwrap();
        }
        // A dir without an options file must be ignored entirely.
        std::fs::create_dir_all(optdir.join("cat_emptydir")).unwrap();

        let moved =
            Moved::parse("cat/gone||2026-01-01|expired\ncat/renamed|cat/alive|2026-01-01|renamed\n");
        let (removals, live, warnings) = classify_entries(&optdir, &portsdir, &moved);

        assert_eq!(live.len(), 1);
        assert_eq!(live[0].key.to_string(), "cat/alive");
        let names: Vec<&str> = removals.iter().map(|r| r.options_name.as_str()).collect();
        assert_eq!(names, vec!["cat_gone", "cat_renamed"]);
        assert!(removals[0].reason.contains("expired"));
        assert!(removals[1].reason.contains("cat/alive"), "rename target named");
        // "weird" has no '_' -> unmappable, warned about, untouched.
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("weird"));
    }

    #[test]
    fn redundancy_diff_directions() {
        // defaults {A}; file result: B on, A off -> +B -A.
        let mut o = PortOptions::default();
        o.complete = vec!["A".into(), "B".into()];
        o.defaults = ["A".to_string()].into();
        o.effective = ["B".to_string()].into();
        let d = redundancy_diff(&info_with(o));
        assert_eq!(d, vec!["+B".to_string(), "-A".to_string()]);
    }

    #[test]
    fn redundancy_check() {
        // defaults {A}, mc unsets B; file recorded exactly that -> effective {A}.
        let mut o = PortOptions::default();
        o.complete = vec!["A".into(), "B".into()];
        o.defaults = ["A".to_string()].into();
        o.mc_unset = ["B".to_string()].into();
        o.effective = ["A".to_string()].into();
        assert!(file_is_redundant(&info_with(o.clone())));

        // file deviates: B enabled although default+mc say off.
        o.effective = ["A".to_string(), "B".to_string()].into();
        assert!(!file_is_redundant(&info_with(o)));
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
