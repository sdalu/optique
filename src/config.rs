use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

const POUDRIERE_D: &str = "/usr/local/etc/poudriere.d";

/// Resolved execution environment: where the ports tree lives, which options
/// dir is the read/write target, and the layered make.conf to query with.
#[derive(Debug)]
pub struct Settings {
    pub portsdir: PathBuf,
    /// The real options dir writes will target (PORT_DBDIR equivalent).
    pub options_dir: PathBuf,
    /// True when `options_dir` doesn't exist yet (created on apply).
    pub options_dir_is_new: bool,
    /// Layered make.conf (written into `staging_dir`), if any applies.
    pub make_conf: Option<PathBuf>,
    /// The poudriere.d make.conf files that were concatenated, in inclusion
    /// order (empty when none exists).
    pub make_conf_sources: Vec<PathBuf>,
    /// Hash identifying the make.conf layering (empty layering hashes too).
    pub conf_hash: String,
}

/// `tree` is None when -p wasn't given ("default" is used for lookups, but —
/// like `poudriere options` — the tree name is then left out of a
/// newly-created options dir name).
pub fn resolve(
    tree: Option<&str>,
    jail: Option<&str>,
    set: Option<&str>,
    options_dir_flag: Option<&Path>,
    staging_dir: &Path,
) -> Result<Settings> {
    let poudriere_d = Path::new(POUDRIERE_D);
    let tree_name = tree.unwrap_or("default");
    let portsdir = resolve_portsdir(poudriere_d, tree_name)?;

    let (options_dir, options_dir_is_new) = match options_dir_flag {
        Some(d) => (d.to_path_buf(), !d.is_dir()),
        None if poudriere_d.is_dir() => {
            resolve_options_dir(poudriere_d, jail, tree_name, tree.is_some(), set)
        }
        None => {
            if jail.is_some() || set.is_some() {
                bail!("-j/-z given but {} does not exist", poudriere_d.display());
            }
            let d = PathBuf::from("/var/db/ports");
            let is_new = !d.is_dir();
            (d, is_new)
        }
    };

    let (make_conf, conf_hash, make_conf_sources) =
        layer_make_conf(poudriere_d, jail, tree_name, set, staging_dir)?;

    Ok(Settings {
        portsdir,
        options_dir,
        options_dir_is_new,
        make_conf,
        make_conf_sources,
        conf_hash,
    })
}

fn resolve_portsdir(poudriere_d: &Path, tree: &str) -> Result<PathBuf> {
    let mnt = poudriere_d.join("ports").join(tree).join("mnt");
    if let Ok(path) = fs::read_to_string(&mnt) {
        let p = PathBuf::from(path.trim());
        if p.join("Mk/bsd.port.mk").is_file() {
            return Ok(p);
        }
        bail!("poudriere tree '{tree}' points to {} which is not a ports tree", p.display());
    }
    let fallback = PathBuf::from("/usr/ports");
    if fallback.join("Mk/bsd.port.mk").is_file() {
        Ok(fallback)
    } else {
        bail!("no poudriere tree '{tree}' and no /usr/ports; pass -p or mount a ports tree")
    }
}

/// Candidate options dir names, most specific first, exactly as poudriere(8)
/// probes them before null-mounting one over the jail's /var/db/ports:
///
///   <jail>-<tree>-<set>-options, <jail>-<set>-options, <jail>-<tree>-options,
///   <tree>-<set>-options, <set>-options, <tree>-options, <jail>-options,
///   options
fn options_dir_candidates(jail: Option<&str>, tree: &str, set: Option<&str>) -> Vec<String> {
    let mut v = Vec::new();
    if let (Some(j), Some(s)) = (jail, set) {
        v.push(format!("{j}-{tree}-{s}-options"));
        v.push(format!("{j}-{s}-options"));
    }
    if let Some(j) = jail {
        v.push(format!("{j}-{tree}-options"));
    }
    if let Some(s) = set {
        v.push(format!("{tree}-{s}-options"));
        v.push(format!("{s}-options"));
    }
    v.push(format!("{tree}-options"));
    if let Some(j) = jail {
        v.push(format!("{j}-options"));
    }
    v.push("options".to_string());
    v
}

/// First existing candidate wins (that's the dir the build will actually
/// see). When none exists, fall back to the name `poudriere options` would
/// create for these flags: [<jail>-][<tree>-][<set>-]options, the tree part
/// only when -p was explicit.
fn resolve_options_dir(
    poudriere_d: &Path,
    jail: Option<&str>,
    tree: &str,
    tree_explicit: bool,
    set: Option<&str>,
) -> (PathBuf, bool) {
    for name in options_dir_candidates(jail, tree, set) {
        let dir = poudriere_d.join(&name);
        if dir.is_dir() {
            return (dir, false);
        }
    }
    let mut name = String::new();
    if let Some(j) = jail {
        name.push_str(j);
        name.push('-');
    }
    if tree_explicit {
        name.push_str(tree);
        name.push('-');
    }
    if let Some(s) = set {
        name.push_str(s);
        name.push('-');
    }
    name.push_str("options");
    (poudriere_d.join(name), true)
}

/// make.conf layer names in poudriere(8) inclusion order (least to most
/// specific); every existing file is concatenated:
///
///   make.conf, <set>-make.conf, <tree>-make.conf, <jail>-make.conf,
///   <tree>-<set>-make.conf, <jail>-<tree>-make.conf, <jail>-<set>-make.conf,
///   <jail>-<tree>-<set>-make.conf
fn make_conf_layers(jail: Option<&str>, tree: &str, set: Option<&str>) -> Vec<String> {
    let mut v = vec!["make.conf".to_string()];
    if let Some(s) = set {
        v.push(format!("{s}-make.conf"));
    }
    v.push(format!("{tree}-make.conf"));
    if let Some(j) = jail {
        v.push(format!("{j}-make.conf"));
    }
    if let Some(s) = set {
        v.push(format!("{tree}-{s}-make.conf"));
    }
    if let Some(j) = jail {
        v.push(format!("{j}-{tree}-make.conf"));
        if let Some(s) = set {
            v.push(format!("{j}-{s}-make.conf"));
            v.push(format!("{j}-{tree}-{s}-make.conf"));
        }
    }
    v
}

fn layer_make_conf(
    poudriere_d: &Path,
    jail: Option<&str>,
    tree: &str,
    set: Option<&str>,
    staging_dir: &Path,
) -> Result<(Option<PathBuf>, String, Vec<PathBuf>)> {
    let mut combined = String::new();
    let mut sources = Vec::new();
    for name in make_conf_layers(jail, tree, set) {
        let path = poudriere_d.join(&name);
        if let Ok(text) = fs::read_to_string(&path) {
            combined.push_str(&format!("# --- {} ---\n", path.display()));
            combined.push_str(&text);
            if !text.ends_with('\n') {
                combined.push('\n');
            }
            sources.push(path);
        }
    }

    let conf_hash = crate::cache::sha256_hex(combined.as_bytes());
    if sources.is_empty() {
        return Ok((None, conf_hash, sources));
    }
    let path = staging_dir.join("make.conf");
    fs::write(&path, &combined)
        .with_context(|| format!("writing layered make.conf to {}", path.display()))?;
    Ok((Some(path), conf_hash, sources))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_candidates_match_manpage_order() {
        let c = options_dir_candidates(Some("j"), "t", Some("s"));
        assert_eq!(
            c,
            vec![
                "j-t-s-options",
                "j-s-options",
                "j-t-options",
                "t-s-options",
                "s-options",
                "t-options",
                "j-options",
                "options"
            ]
        );
        assert_eq!(
            options_dir_candidates(None, "t", Some("s")),
            vec!["t-s-options", "s-options", "t-options", "options"]
        );
        assert_eq!(options_dir_candidates(None, "t", None), vec!["t-options", "options"]);
    }

    #[test]
    fn options_dir_first_match_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let pd = tmp.path();
        fs::create_dir_all(pd.join("options")).unwrap();
        fs::create_dir_all(pd.join("s-options")).unwrap();
        // set-specific beats the generic one
        let (dir, is_new) = resolve_options_dir(pd, None, "default", false, Some("s"));
        assert_eq!(dir, pd.join("s-options"));
        assert!(!is_new);
        // jail-specific dirs missing: falls through to the set dir
        let (dir, _) = resolve_options_dir(pd, Some("j"), "default", false, Some("s"));
        assert_eq!(dir, pd.join("s-options"));
        // no flags: plain options
        let (dir, _) = resolve_options_dir(pd, None, "default", false, None);
        assert_eq!(dir, pd.join("options"));
    }

    #[test]
    fn options_dir_created_name_mirrors_poudriere_options() {
        let tmp = tempfile::tempdir().unwrap();
        let pd = tmp.path();
        let (dir, is_new) = resolve_options_dir(pd, Some("j"), "t", true, Some("s"));
        assert_eq!(dir, pd.join("j-t-s-options"));
        assert!(is_new);
        // tree not explicit -> left out of the created name
        let (dir, _) = resolve_options_dir(pd, None, "default", false, Some("s"));
        assert_eq!(dir, pd.join("s-options"));
    }

    #[test]
    fn make_conf_layering_order_matches_manpage() {
        let tmp = tempfile::tempdir().unwrap();
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        let layers = [
            ("make.conf", "A=1"),
            ("s1-make.conf", "B=2"),
            ("t1-make.conf", "C=3"),
            ("j1-make.conf", "D=4"),
            ("t1-s1-make.conf", "E=5"),
            ("j1-t1-make.conf", "F=6"),
            ("j1-s1-make.conf", "G=7"),
            ("j1-t1-s1-make.conf", "H=8"),
        ];
        for (name, content) in &layers {
            fs::write(pd.join(name), format!("{content}\n")).unwrap();
        }
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let (path, _, sources) =
            layer_make_conf(&pd, Some("j1"), "t1", Some("s1"), &stage).unwrap();
        assert_eq!(sources.len(), 8);
        let text = fs::read_to_string(path.unwrap()).unwrap();
        let idx = |needle: &str| text.find(needle).unwrap();
        let order: Vec<usize> = layers.iter().map(|(_, c)| idx(c)).collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "layers out of order: {text}");
    }
}
