use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

const POUDRIERE_D: &str = "/usr/local/etc/poudriere.d";

/// Resolved execution environment: where the ports tree lives, which options
/// dir is the write target, and the layered make.conf to query with.
#[derive(Debug)]
pub struct Settings {
    pub portsdir: PathBuf,
    /// The real options dir writes will target (PORT_DBDIR equivalent).
    pub options_dir: PathBuf,
    /// Layered make.conf (written into `staging_dir`), if any applies.
    pub make_conf: Option<PathBuf>,
    /// Hash identifying the make.conf layering (empty layering hashes too).
    pub conf_hash: String,
}

pub fn resolve(
    tree: &str,
    jail: Option<&str>,
    set: Option<&str>,
    options_dir_flag: Option<&Path>,
    staging_dir: &Path,
) -> Result<Settings> {
    let poudriere_d = Path::new(POUDRIERE_D);
    let portsdir = resolve_portsdir(poudriere_d, tree)?;

    let options_dir = match options_dir_flag {
        Some(d) => d.to_path_buf(),
        None => {
            if let Some(set) = set {
                let d = poudriere_d.join(format!("{set}-options"));
                if !d.is_dir() {
                    bail!(
                        "poudriere set '{set}' has no options dir at {} (create it or pass -o)",
                        d.display()
                    );
                }
                d
            } else if poudriere_d.join("options").is_dir() {
                poudriere_d.join("options")
            } else {
                PathBuf::from("/var/db/ports")
            }
        }
    };

    let (make_conf, conf_hash) = layer_make_conf(poudriere_d, jail, set, staging_dir)?;

    Ok(Settings { portsdir, options_dir, make_conf, conf_hash })
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

/// Concatenate poudriere's make.conf layers the way poudriere does when
/// populating a jail's /etc/make.conf: make.conf, <jail>-make.conf,
/// <set>-make.conf, <jail>-<set>-make.conf.
fn layer_make_conf(
    poudriere_d: &Path,
    jail: Option<&str>,
    set: Option<&str>,
    staging_dir: &Path,
) -> Result<(Option<PathBuf>, String)> {
    let mut names = vec!["make.conf".to_string()];
    if let Some(j) = jail {
        names.push(format!("{j}-make.conf"));
    }
    if let Some(s) = set {
        names.push(format!("{s}-make.conf"));
    }
    if let (Some(j), Some(s)) = (jail, set) {
        names.push(format!("{j}-{s}-make.conf"));
    }

    let mut combined = String::new();
    let mut found = false;
    for name in &names {
        let path = poudriere_d.join(name);
        if let Ok(text) = fs::read_to_string(&path) {
            found = true;
            combined.push_str(&format!("# --- {} ---\n", path.display()));
            combined.push_str(&text);
            if !text.ends_with('\n') {
                combined.push('\n');
            }
        }
    }

    let conf_hash = crate::cache::sha256_hex(combined.as_bytes());
    if !found {
        return Ok((None, conf_hash));
    }
    let path = staging_dir.join("make.conf");
    fs::write(&path, &combined)
        .with_context(|| format!("writing layered make.conf to {}", path.display()))?;
    Ok((Some(path), conf_hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layering_order() {
        let tmp = tempfile::tempdir().unwrap();
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        fs::write(pd.join("make.conf"), "A=1\n").unwrap();
        fs::write(pd.join("j1-make.conf"), "B=2\n").unwrap();
        fs::write(pd.join("s1-make.conf"), "C=3\n").unwrap();
        fs::write(pd.join("j1-s1-make.conf"), "D=4\n").unwrap();
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let (path, _) = layer_make_conf(&pd, Some("j1"), Some("s1"), &stage).unwrap();
        let text = fs::read_to_string(path.unwrap()).unwrap();
        let idx = |needle: &str| text.find(needle).unwrap();
        assert!(idx("A=1") < idx("B=2"));
        assert!(idx("B=2") < idx("C=3"));
        assert!(idx("C=3") < idx("D=4"));
    }
}
