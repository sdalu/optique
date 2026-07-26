use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

const POUDRIERE_D: &str = "/usr/local/etc/poudriere.d";
const SYNTH_DIR: &str = "/usr/local/etc/synth";
/// Section of synth.ini whose settings apply to every profile.
const SYNTH_GLOBAL_SECTION: &str = "Global Configuration";

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
    /// Ports poudriere(8) would refuse to build for this jail/tree/set.
    pub blacklist: Blacklist,
    /// Remarks about the resolution itself (e.g. a flag that does not apply
    /// to the selected backend), printed by the startup banner.
    pub notes: Vec<String>,
}

/// Compiled blacklist: patterns from every existing poudriere(8)
/// blacklist layer for this jail/tree/set.
#[derive(Debug, Default)]
pub struct Blacklist {
    patterns: Vec<String>,
    pub sources: Vec<PathBuf>,
}

impl Blacklist {
    /// Read every existing layer, in poudriere(8) order. Unlike the options
    /// dir (first match wins) all files contribute entries.
    pub fn load(
        poudriere_d: &Path,
        jail: Option<&str>,
        tree: &str,
        set: Option<&str>,
    ) -> Self {
        let mut bl = Blacklist::default();
        for name in blacklist_layers(jail, tree, set) {
            let path = poudriere_d.join(&name);
            let Ok(text) = fs::read_to_string(&path) else { continue };
            for line in text.lines() {
                let entry = match line.split_once('#') {
                    Some((before, _)) => before,
                    None => line,
                }
                .trim();
                if !entry.is_empty() {
                    bl.patterns.push(entry.to_string());
                }
            }
            bl.sources.push(path);
        }
        bl
    }

    /// Is this port origin blacklisted? Entries are exact origins or
    /// shell-style `*` globs, as poudriere(8) allows.
    pub fn matches(&self, origin: &str) -> bool {
        self.patterns.iter().any(|p| glob_match(p, origin))
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }
}

/// blacklist layer names in poudriere(8) read order; every existing file
/// contributes:
///
///   blacklist, <set>-blacklist, <tree>-blacklist, <jail>-blacklist,
///   <tree>-<set>-blacklist, <jail>-<tree>-blacklist, <jail>-<set>-blacklist,
///   <jail>-<tree>-<set>-blacklist
fn blacklist_layers(jail: Option<&str>, tree: &str, set: Option<&str>) -> Vec<String> {
    let mut v = vec!["blacklist".to_string()];
    if let Some(s) = set {
        v.push(format!("{s}-blacklist"));
    }
    v.push(format!("{tree}-blacklist"));
    if let Some(j) = jail {
        v.push(format!("{j}-blacklist"));
    }
    if let Some(s) = set {
        v.push(format!("{tree}-{s}-blacklist"));
    }
    if let Some(j) = jail {
        v.push(format!("{j}-{tree}-blacklist"));
        if let Some(s) = set {
            v.push(format!("{j}-{s}-blacklist"));
            v.push(format!("{j}-{tree}-{s}-blacklist"));
        }
    }
    v
}

/// Minimal glob: `*` matches any substring (including empty), everything
/// else is literal. A pattern without `*` is an exact match.
fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    // First literal anchors the start, last one the end, the rest must occur
    // in order in between (searching left to right can't backtrack wrongly:
    // every literal is separated by an unbounded '*').
    let Some(mut rest) = text.strip_prefix(parts[0]) else { return false };
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(parts[parts.len() - 1])
}

/// `tree` is None when -p wasn't given ("default" is used for lookups, but —
/// like `poudriere options` — the tree name is then left out of a
/// newly-created options dir name).
///
/// `synth` selects the synth(1) layout instead of the poudriere one; it is
/// mutually exclusive with `jail`/`set` (clap rejects the combination).
pub fn resolve(
    tree: Option<&str>,
    jail: Option<&str>,
    set: Option<&str>,
    synth: Option<&str>,
    options_dir_flag: Option<&Path>,
    staging_dir: &Path,
) -> Result<Settings> {
    if let Some(profile) = synth {
        return resolve_synth(
            Path::new(SYNTH_DIR),
            profile,
            tree.is_some(),
            options_dir_flag,
            staging_dir,
        );
    }
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

    let blacklist = Blacklist::load(poudriere_d, jail, tree_name, set);

    Ok(Settings {
        portsdir,
        options_dir,
        options_dir_is_new,
        make_conf,
        make_conf_sources,
        conf_hash,
        blacklist,
        notes: Vec::new(),
    })
}

/// A directory is a usable ports tree when it carries the ports framework.
fn is_ports_tree(dir: &Path) -> bool {
    dir.join("Mk/bsd.port.mk").is_file()
}

fn resolve_portsdir(poudriere_d: &Path, tree: &str) -> Result<PathBuf> {
    let mnt = poudriere_d.join("ports").join(tree).join("mnt");
    if let Ok(path) = fs::read_to_string(&mnt) {
        let p = PathBuf::from(path.trim());
        if is_ports_tree(&p) {
            return Ok(p);
        }
        bail!("poudriere tree '{tree}' points to {} which is not a ports tree", p.display());
    }
    let fallback = PathBuf::from("/usr/ports");
    if is_ports_tree(&fallback) {
        Ok(fallback)
    } else {
        bail!("no poudriere tree '{tree}' and no /usr/ports; pass -p or mount a ports tree")
    }
}

/// synth(1) layout: one `synth.ini` describing every profile, the profile's
/// own `<profile>-make.conf` (no layering), and no blacklist at all. The
/// options dir is a documented synth setting (`Directory_options`), so unlike
/// poudriere there is nothing to probe for.
fn resolve_synth(
    synth_dir: &Path,
    profile: &str,
    tree_explicit: bool,
    options_dir_flag: Option<&Path>,
    staging_dir: &Path,
) -> Result<Settings> {
    let ini = fs::read_to_string(synth_dir.join("synth.ini")).unwrap_or_default();

    let mut notes = Vec::new();
    // A bare -s (empty profile) follows synth's own selection.
    let selected;
    let profile = if profile.is_empty() {
        selected = synth_ini_lookup(&ini, "Global Configuration", "profile_selected")
            .unwrap_or_else(|| "LiveSystem".to_string());
        notes.push(format!("synth profile {selected} (synth.ini profile_selected)"));
        selected.as_str()
    } else {
        profile
    };
    let setting = |key: &str| synth_ini_lookup(&ini, profile, key);
    if tree_explicit {
        notes.push(
            "-p is a poudriere tree name; synth uses Directory_portsdir".to_string(),
        );
    }
    // A named profile that synth.ini doesn't know silently gets built-in
    // defaults — say so, it is usually a typo.
    if !ini.is_empty() && !synth_ini_has_section(&ini, profile) {
        notes.push(format!(
            "profile '{profile}' has no section in synth.ini; using built-in defaults"
        ));
    }

    let portsdir = match setting("Directory_portsdir") {
        Some(dir) => {
            let p = PathBuf::from(dir);
            if !is_ports_tree(&p) {
                bail!(
                    "synth profile '{profile}' points Directory_portsdir at {} \
                     which is not a ports tree",
                    p.display()
                );
            }
            p
        }
        None => {
            let fallback = PathBuf::from("/usr/ports");
            if !is_ports_tree(&fallback) {
                bail!(
                    "synth profile '{profile}' sets no Directory_portsdir and \
                     /usr/ports is not a ports tree"
                );
            }
            fallback
        }
    };

    let options_dir = match options_dir_flag {
        Some(d) => d.to_path_buf(),
        None => setting("Directory_options")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/var/db/ports")),
    };
    if options_dir == Path::new("/var/db/ports") {
        notes.push(
            "options dir is the live /var/db/ports: minimal/redundant cleanups \
             also affect plain `make config` workflows"
                .to_string(),
        );
    }
    let options_dir_is_new = !options_dir.is_dir();

    let (make_conf, conf_hash, make_conf_sources) =
        concat_make_conf(&[synth_dir.join(format!("{profile}-make.conf"))], staging_dir)?;

    Ok(Settings {
        portsdir,
        options_dir,
        options_dir_is_new,
        make_conf,
        make_conf_sources,
        conf_hash,
        blacklist: Blacklist::default(),
        notes,
    })
}

/// Does synth.ini contain a `[name]` section header?
fn synth_ini_has_section(text: &str, name: &str) -> bool {
    text.lines().any(|line| {
        let line = line.split([';', '#']).next().unwrap_or("").trim();
        line.strip_prefix('[')
            .and_then(|r| r.strip_suffix(']'))
            .map(|s| s.trim() == name)
            .unwrap_or(false)
    })
}

/// Minimal INI reader for synth.ini: the value of `key` in section
/// `[profile]`, falling back to the `[Global Configuration]` section. Keys are
/// case-sensitive, `;` and `#` start a comment, and whitespace around both
/// sides of `=` is stripped. First occurrence in a section wins; an empty
/// value counts as unset.
fn synth_ini_lookup(text: &str, profile: &str, key: &str) -> Option<String> {
    let mut section = String::new();
    let mut in_profile = None;
    let mut in_global = None;
    for line in text.lines() {
        let line = match line.find([';', '#']) {
            Some(i) => &line[..i],
            None => line,
        }
        .trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_string();
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        if k.trim() != key {
            continue;
        }
        let v = v.trim();
        if v.is_empty() {
            continue;
        }
        if section == profile {
            in_profile.get_or_insert_with(|| v.to_string());
        } else if section == SYNTH_GLOBAL_SECTION {
            in_global.get_or_insert_with(|| v.to_string());
        }
    }
    in_profile.or(in_global)
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
    let paths: Vec<PathBuf> =
        make_conf_layers(jail, tree, set).iter().map(|n| poudriere_d.join(n)).collect();
    concat_make_conf(&paths, staging_dir)
}

/// Concatenate every existing candidate, in order, into one staged
/// `make.conf`. The hash covers the combined text (so an empty layering hashes
/// too); nothing is written when no candidate exists.
fn concat_make_conf(
    candidates: &[PathBuf],
    staging_dir: &Path,
) -> Result<(Option<PathBuf>, String, Vec<PathBuf>)> {
    let mut combined = String::new();
    let mut sources = Vec::new();
    for path in candidates {
        if let Ok(text) = fs::read_to_string(path) {
            combined.push_str(&format!("# --- {} ---\n", path.display()));
            combined.push_str(&text);
            if !text.ends_with('\n') {
                combined.push('\n');
            }
            sources.push(path.clone());
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
    fn portsdir_resolution_via_poudriere_mnt() {
        let tmp = tempfile::tempdir().unwrap();
        let pd = tmp.path().join("poudriere.d");
        let tree = tmp.path().join("tree");
        fs::create_dir_all(tree.join("Mk")).unwrap();
        fs::write(tree.join("Mk/bsd.port.mk"), "# fake\n").unwrap();
        fs::create_dir_all(pd.join("ports/mytree")).unwrap();
        fs::write(pd.join("ports/mytree/mnt"), format!("{}\n", tree.display())).unwrap();
        assert_eq!(resolve_portsdir(&pd, "mytree").unwrap(), tree);
        // A tree pointing at a non-ports dir is an error, not a fallback.
        fs::write(pd.join("ports/mytree/mnt"), "/nonexistent\n").unwrap();
        assert!(resolve_portsdir(&pd, "mytree").is_err());
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

    #[test]
    fn synth_ini_lookup_picks_section_then_global() {
        let ini = "\
; leading comment
[Global Configuration]
Directory_portsdir= /global/ports
Directory_options = /global/options   # trailing comment

[LiveSystem]
Directory_portsdir=/live/ports
Operating_system= FreeBSD

[Other]
Directory_portsdir = /other/ports
";
        // Profile section wins over the global one.
        assert_eq!(
            synth_ini_lookup(ini, "LiveSystem", "Directory_portsdir").as_deref(),
            Some("/live/ports")
        );
        // Key absent from the profile: the global section answers.
        assert_eq!(
            synth_ini_lookup(ini, "LiveSystem", "Directory_options").as_deref(),
            Some("/global/options")
        );
        // Sections are not confused with each other.
        assert_eq!(
            synth_ini_lookup(ini, "Other", "Directory_portsdir").as_deref(),
            Some("/other/ports")
        );
        // Unknown profile still sees the global fallback...
        assert_eq!(
            synth_ini_lookup(ini, "Nope", "Directory_portsdir").as_deref(),
            Some("/global/ports")
        );
        // ...but an unknown key is None, and keys are case-sensitive.
        assert_eq!(synth_ini_lookup(ini, "LiveSystem", "Directory_packages"), None);
        assert_eq!(synth_ini_lookup(ini, "LiveSystem", "directory_portsdir"), None);
    }

    #[test]
    fn synth_ini_lookup_ignores_comments_and_junk() {
        let ini = "\
#[LiveSystem]
#Directory_portsdir=/commented/out
[LiveSystem]
; Directory_options=/also/commented
Directory_options=
   Directory_portsdir   =   /spaced/ports
not a key value line
";
        // A commented-out section header never opens a section, and a
        // commented-out or empty assignment counts as unset.
        assert_eq!(
            synth_ini_lookup(ini, "LiveSystem", "Directory_portsdir").as_deref(),
            Some("/spaced/ports")
        );
        assert_eq!(synth_ini_lookup(ini, "LiveSystem", "Directory_options"), None);
        // Nothing at all: no panic, no value.
        assert_eq!(synth_ini_lookup("", "LiveSystem", "Directory_portsdir"), None);
    }

    #[test]
    fn synth_notes_flag_missing_profile_and_live_options_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let synth = tmp.path().join("synth");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(
            synth.join("synth.ini"),
            "[Global Configuration]\nprofile_selected= Live\n[Live]\n",
        )
        .unwrap();

        // Unknown named profile: noted, defaults still apply (/var/db/ports
        // triggers the live-dir note too). /usr/ports exists on this host.
        let s = resolve_synth(&synth, "typo", false, None, &stage).unwrap();
        assert!(s.notes.iter().any(|n| n.contains("'typo' has no section")), "{:?}", s.notes);
        assert!(s.notes.iter().any(|n| n.contains("live /var/db/ports")), "{:?}", s.notes);

        // Bare -s honours profile_selected; known section → no missing note.
        let s = resolve_synth(&synth, "", false, None, &stage).unwrap();
        assert!(s.notes.iter().any(|n| n.contains("profile Live (synth.ini")), "{:?}", s.notes);
        assert!(!s.notes.iter().any(|n| n.contains("no section")), "{:?}", s.notes);

        // -o override: no live-dir note.
        let o = tmp.path().join("other");
        let s = resolve_synth(&synth, "Live", false, Some(&o), &stage).unwrap();
        assert!(!s.notes.iter().any(|n| n.contains("live /var/db/ports")), "{:?}", s.notes);
    }

    #[test]
    fn synth_resolution_follows_the_ini() {
        let tmp = tempfile::tempdir().unwrap();
        let synth = tmp.path().join("synth");
        let tree = tmp.path().join("ports");
        let opts = tmp.path().join("db-ports");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(tree.join("Mk")).unwrap();
        fs::write(tree.join("Mk/bsd.port.mk"), "# fake\n").unwrap();
        fs::create_dir_all(&opts).unwrap();
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(
            synth.join("synth.ini"),
            format!(
                "[Global Configuration]\nprofile_selected= LiveSystem\n\n\
                 [LiveSystem]\nDirectory_portsdir= {}\nDirectory_options={}\n",
                tree.display(),
                opts.display()
            ),
        )
        .unwrap();
        fs::write(synth.join("LiveSystem-make.conf"), "DEFAULT_VERSIONS+=ssl=openssl\n").unwrap();
        // A second profile's make.conf must not be picked up.
        fs::write(synth.join("Other-make.conf"), "A=1\n").unwrap();

        let s = resolve_synth(&synth, "LiveSystem", false, None, &stage).unwrap();
        assert_eq!(s.portsdir, tree);
        assert_eq!(s.options_dir, opts);
        assert!(!s.options_dir_is_new);
        assert_eq!(s.make_conf_sources, vec![synth.join("LiveSystem-make.conf")]);
        let staged = fs::read_to_string(s.make_conf.as_ref().unwrap()).unwrap();
        assert!(staged.contains("ssl=openssl"), "{staged}");
        assert!(!staged.contains("A=1"), "other profile leaked in: {staged}");
        // synth has no blacklist, and -p was not given so no note.
        assert!(s.blacklist.is_empty());
        assert!(s.notes.is_empty());
        assert!(!s.conf_hash.is_empty());

        // -o wins over Directory_options; -p only earns a note.
        let elsewhere = tmp.path().join("elsewhere");
        let s = resolve_synth(&synth, "LiveSystem", true, Some(&elsewhere), &stage).unwrap();
        assert_eq!(s.options_dir, elsewhere);
        assert!(s.options_dir_is_new);
        assert_eq!(s.notes.len(), 1);
        assert!(s.notes[0].contains("Directory_portsdir"), "{:?}", s.notes);
    }

    #[test]
    fn synth_resolution_without_profile_files() {
        let tmp = tempfile::tempdir().unwrap();
        let synth = tmp.path().join("synth");
        let tree = tmp.path().join("ports");
        let opts = tmp.path().join("db-ports");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(tree.join("Mk")).unwrap();
        fs::write(tree.join("Mk/bsd.port.mk"), "# fake\n").unwrap();
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        // Only the global section, and no <profile>-make.conf at all.
        fs::write(
            synth.join("synth.ini"),
            format!(
                "[Global Configuration]\nDirectory_portsdir={}\nDirectory_options={}\n",
                tree.display(),
                opts.display()
            ),
        )
        .unwrap();

        let s = resolve_synth(&synth, "LiveSystem", false, None, &stage).unwrap();
        assert_eq!(s.portsdir, tree);
        // Nonexistent Directory_options is still the target, flagged as new.
        assert_eq!(s.options_dir, opts);
        assert!(s.options_dir_is_new);
        assert!(s.make_conf.is_none());
        assert!(s.make_conf_sources.is_empty());
        assert!(!stage.join("make.conf").exists(), "nothing to stage");

        // Directory_portsdir naming a non-tree is an error, not a fallback.
        fs::write(
            synth.join("synth.ini"),
            format!("[LiveSystem]\nDirectory_portsdir={}/nope\n", tmp.path().display()),
        )
        .unwrap();
        let err = resolve_synth(&synth, "LiveSystem", false, Some(&opts), &stage)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a ports tree"), "{err}");

        // No synth.ini: options dir falls back to /var/db/ports (the portsdir
        // fallback is /usr/ports, whose presence is host-dependent, so it is
        // only exercised through -o here).
        fs::remove_file(synth.join("synth.ini")).unwrap();
        if is_ports_tree(Path::new("/usr/ports")) {
            let s = resolve_synth(&synth, "LiveSystem", false, None, &stage).unwrap();
            assert_eq!(s.options_dir, Path::new("/var/db/ports"));
        }
    }

    #[test]
    fn glob_match_handles_poudriere_style_entries() {
        // A pattern without '*' is an exact match, nothing else.
        assert!(glob_match("www/nginx", "www/nginx"));
        assert!(!glob_match("www/nginx", "www/nginx-devel"));
        assert!(!glob_match("www/nginx", "www/ngin"));
        // Trailing '*' matches any tail, including the empty one.
        assert!(glob_match("www/*", "www/nginx"));
        assert!(glob_match("www/nginx*", "www/nginx-devel"));
        assert!(glob_match("www/nginx*", "www/nginx"));
        assert!(!glob_match("www/*", "mail/dovecot"));
        // '*' alone matches everything, interior globs anchor both ends.
        assert!(glob_match("*", "anything/at-all"));
        assert!(glob_match("*", ""));
        assert!(glob_match("*/nginx", "www/nginx"));
        assert!(glob_match("www/*devel", "www/nginx-devel"));
        assert!(!glob_match("www/*devel", "www/nginx"));
    }

    #[test]
    fn blacklist_layers_and_matching() {
        let tmp = tempfile::tempdir().unwrap();
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        fs::write(pd.join("blacklist"), "# comment\n\nwww/nginx\n").unwrap();
        fs::write(pd.join("s1-blacklist"), "devel/*   # globbed\n").unwrap();

        let bl = Blacklist::load(&pd, None, "default", Some("s1"));
        assert_eq!(bl.sources.len(), 2);
        assert!(!bl.is_empty());
        assert!(bl.matches("www/nginx"));
        assert!(bl.matches("devel/anything"));
        assert!(!bl.matches("mail/dovecot"));

        // Without -z the set layer is not even looked at.
        let bl = Blacklist::load(&pd, None, "default", None);
        assert_eq!(bl.sources, vec![pd.join("blacklist")]);
        assert!(bl.matches("www/nginx"));
        assert!(!bl.matches("devel/anything"));

        // No poudriere.d at all: empty, matches nothing.
        let bl = Blacklist::load(&tmp.path().join("nope"), Some("j1"), "t1", Some("s1"));
        assert!(bl.is_empty());
        assert!(!bl.matches("www/nginx"));
    }

    #[test]
    fn blacklist_layer_order_matches_manpage() {
        assert_eq!(
            blacklist_layers(Some("j"), "t", Some("s")),
            vec![
                "blacklist",
                "s-blacklist",
                "t-blacklist",
                "j-blacklist",
                "t-s-blacklist",
                "j-t-blacklist",
                "j-s-blacklist",
                "j-t-s-blacklist"
            ]
        );
        assert_eq!(blacklist_layers(None, "t", None), vec!["blacklist", "t-blacklist"]);
    }
}
