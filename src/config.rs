use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

const POUDRIERE_D: &str = "/usr/local/etc/poudriere.d";
const POUDRIERE_CONF: &str = "/usr/local/etc/poudriere.conf";

/// What poudriere(8) writes at the top of every jail's make.conf, ahead of
/// the poudriere.d layers (`setup_makeconf` in its common.sh), as far as it
/// bears on evaluating a Makefile.
const POUDRIERE_JAIL_PRELUDE: &str = "\
# --- poudriere jail environment (optique) ---
USE_PACKAGE_DEPENDS=yes
BATCH=yes
";

/// The part of that prelude poudriere leaves out when poudriere.conf sets
/// `NO_PACKAGE_BUILDING`: several framework probes are skipped under
/// `PACKAGE_BUILDING`, and some ports change option defaults with it
/// (math/openblas turns `DYNAMIC_ARCH` on).
const PACKAGE_BUILDING_LINES: &str = "\
PACKAGE_BUILDING=yes
PACKAGE_BUILDING_FLAVORS=yes
";

/// The environment queries evaluate Makefiles in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryEnv {
    /// The host's own, for a build on the host (plain /var/db/ports, synth).
    Host,
    /// A poudriere jail's: nothing installed, poudriere's make.conf prelude,
    /// `PACKAGE_BUILDING` unless poudriere.conf turns it off.
    Jail { package_building: bool },
}

/// A jail's LOCALBASE is empty when poudriere gathers ports, while the host
/// running optique has packages there that Makefiles probe with `exists()`
/// (www/apache24's apr-util checks, Mk/Uses/iconv.mk). Pointing LOCALBASE
/// where nothing is installed answers those probes as the jail does. It
/// comes after the layers so that one setting LOCALBASE cannot undo it.
const POUDRIERE_JAIL_TRAILER: &str = "\
# --- poudriere jail environment (optique) ---
LOCALBASE=/nonexistent
";
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
    /// Whether queries see a poudriere jail's environment or the host's
    /// (`POUDRIERE_JAIL_PRELUDE`, `POUDRIERE_JAIL_TRAILER`).
    pub env: QueryEnv,
    /// Ports poudriere(8) would refuse to build for this jail/tree/set.
    pub blacklist: Blacklist,
    /// Remarks about the resolution itself (e.g. a flag that does not apply
    /// to the selected backend), printed by the startup banner.
    pub notes: Vec<String>,
    /// Ports trees laid over `portsdir`, in lookup order (the first holding
    /// a port shadows the others and the main tree): the `OVERLAYS` make
    /// itself reports once the make.conf is settled.
    pub overlays: Vec<Overlay>,
    /// Whether an `OVERLAYS` in optique's environment reaches the queries:
    /// for a local build, where make reads the environment; never for
    /// poudriere or synth, whose builders do not.
    pub inherit_env_overlays: bool,
}

/// A ports tree laid over the main one (the framework's `OVERLAYS`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overlay {
    /// The poudriere tree name it was given as (-O), if any.
    pub name: Option<String>,
    pub path: PathBuf,
}

/// An overlay as the command line gives it: a registered poudriere tree
/// (-O name) or a directory (--overlay-dir).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverlaySpec {
    Name(String),
    Dir(PathBuf),
}

impl Settings {
    /// The overlays' paths, in lookup order.
    pub fn overlay_paths(&self) -> Vec<PathBuf> {
        self.overlays.iter().map(|o| o.path.clone()).collect()
    }

    /// Every path made absolute against optique's own directory. make runs
    /// with -C in the port's directory, so a relative PORT_DBDIR, PORTSDIR
    /// or __MAKE_CONF would be read from there while optique reads the
    /// same path from here: the saved choices of a relative -o were
    /// invisible to make, and `clean --redundant` took them for defaults.
    fn absolute(mut self) -> Result<Self> {
        let abs = |p: &Path| {
            std::path::absolute(p).with_context(|| format!("resolving {}", p.display()))
        };
        self.portsdir = abs(&self.portsdir)?;
        self.options_dir = abs(&self.options_dir)?;
        self.make_conf = self.make_conf.as_deref().map(abs).transpose()?;
        Ok(self)
    }
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

/// What the command line asks of the resolution.
#[derive(Debug, Default, Clone, Copy)]
pub struct Request<'a> {
    /// -p: None when not given ("default" is used for lookups, but — like
    /// `poudriere options` — the tree name is then left out of a
    /// newly-created options dir name).
    pub tree: Option<&'a str>,
    pub jail: Option<&'a str>,
    pub set: Option<&'a str>,
    /// -s: the synth(1) layout instead of poudriere's, with this profile
    /// (empty for synth.ini's selection).
    pub synth: Option<&'a str>,
    /// -l: a local build, poudriere.d ignored even when it exists.
    pub local: bool,
    /// -P: the ports tree, taken as given.
    pub portsdir: Option<&'a Path>,
    /// -o: the options dir, taken as given.
    pub options_dir: Option<&'a Path>,
    /// -O and --overlay-dir, in command-line order.
    pub overlays: &'a [OverlaySpec],
}

/// `Cli::check_globals` rejects the flag combinations that make no sense
/// (-s or -l with -z/-j/-p, -p with -P), so `req` is consistent here.
pub fn resolve(req: &Request, staging_dir: &Path) -> Result<Settings> {
    // pkg(8)'s own default, which the environment may move.
    let pkg_db = std::env::var_os("PKG_DBDIR")
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from("/var/db/pkg"), PathBuf::from)
        .join("local.sqlite");
    if let Some(profile) = req.synth {
        let settings = resolve_synth(
            Path::new(SYNTH_DIR),
            profile,
            req.portsdir,
            req.options_dir,
            staging_dir,
        )?
        .absolute()?;
        let settings = with_overlays(settings, Vec::new());
        return Ok(key_installed_packages(settings, &pkg_db));
    }
    let poudriere_d = Path::new(POUDRIERE_D);
    // Only a local build asks make for its defaults: a poudriere build
    // reads neither the environment's nor /etc/make.conf's.
    let local = req.local || !poudriere_d.is_dir();
    let (ports, db) = if local { make_defaults() } else { (None, None) };
    // make's own default, which the environment may move as for any make.
    let make_conf = std::env::var_os("__MAKE_CONF")
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from("/etc/make.conf"), PathBuf::from);
    let host = HostPaths {
        poudriere_conf: Path::new(POUDRIERE_CONF),
        poudriere_d,
        ports: ports.as_deref().unwrap_or(Path::new("/usr/ports")),
        db: db.as_deref().unwrap_or(Path::new("/var/db/ports")),
        make_conf: &make_conf,
        ports_from_make: ports.is_some(),
        db_from_make: db.is_some(),
        pkg_db: &pkg_db,
    };
    resolve_host(&host, req, staging_dir)
}

/// PORTSDIR and PORT_DBDIR as make(1) sees them on this host, from the
/// environment or /etc/make.conf (sys.mk reads it), None where neither
/// sets one. make is asked rather than the environment read, so that a
/// make.conf setting counts as it does for `make config`; any failure
/// leaves the built-in defaults.
fn make_defaults() -> (Option<PathBuf>, Option<PathBuf>) {
    let out = std::process::Command::new("/usr/bin/make")
        .current_dir("/")
        .args(["-f", "/dev/null", "-V", "PORTSDIR", "-V", "PORT_DBDIR"])
        .env_remove("MAKEFLAGS")
        .env_remove("MAKELEVEL")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(out) = out else { return (None, None) };
    if !out.status.success() {
        return (None, None);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines().map(|l| {
        let l = l.trim();
        (!l.is_empty()).then(|| PathBuf::from(l))
    });
    (lines.next().flatten(), lines.next().flatten())
}

/// The host paths the resolution reads, as one value so a test can point
/// them into a tempdir.
#[derive(Clone, Copy)]
struct HostPaths<'a> {
    /// poudriere's own configuration, read before poudriere.d's.
    poudriere_conf: &'a Path,
    poudriere_d: &'a Path,
    /// The ports tree of a local build, and of poudriere's default tree
    /// when poudriere.d names none.
    ports: &'a Path,
    /// The options dir of a local build.
    db: &'a Path,
    /// The make.conf of a local build: `__MAKE_CONF` from the environment,
    /// else make's own default, /etc/make.conf.
    make_conf: &'a Path,
    /// Whether `ports` and `db` are make's PORTSDIR and PORT_DBDIR rather
    /// than the built-in defaults (the startup banner says so).
    ports_from_make: bool,
    db_from_make: bool,
    /// pkg(8)'s database of installed packages (`local.sqlite`).
    pkg_db: &'a Path,
}

/// A poudriere build when poudriere.d exists and -l was not given, a local
/// one otherwise.
fn resolve_host(host: &HostPaths, req: &Request, staging_dir: &Path) -> Result<Settings> {
    let local = req.local || !host.poudriere_d.is_dir();
    let given = given_overlays(host.poudriere_d, req.overlays, local)?;
    let settings = if local {
        resolve_local(host, req, &given, staging_dir)?.absolute()?
    } else {
        resolve_poudriere(host, req, &given, staging_dir)?.absolute()?
    };
    let settings = with_overlays(settings, given);
    Ok(key_installed_packages(settings, host.pkg_db))
}

/// The overlays the command line names, in its order: a -O name read from
/// `poudriere.d/ports/<name>/mnt` as poudriere's `bulk -O` does (an overlay
/// need not be a whole tree, so no `Mk/` is asked of it), a --overlay-dir
/// as given. Paths are canonical, as make's `:tA` would make them.
fn given_overlays(poudriere_d: &Path, specs: &[OverlaySpec], local: bool) -> Result<Vec<Overlay>> {
    let mut out = Vec::new();
    for spec in specs {
        let (name, path) = match spec {
            OverlaySpec::Name(name) => {
                if local {
                    bail!(
                        "-O {name} names a poudriere tree, but {} does not exist",
                        poudriere_d.display()
                    );
                }
                let dir = poudriere_d.join("ports").join(name);
                if !dir.is_dir() {
                    bail!("no poudriere overlay '{name}' ({} does not exist)", dir.display());
                }
                let Ok(mnt) = fs::read_to_string(dir.join("mnt")) else {
                    bail!("poudriere overlay '{name}' has no mnt file in {}", dir.display());
                };
                (Some(name.clone()), PathBuf::from(mnt.trim()))
            }
            OverlaySpec::Dir(dir) => (None, dir.clone()),
        };
        let what = name.as_deref().map_or_else(
            || format!("--overlay-dir {}", path.display()),
            |n| format!("poudriere overlay '{n}' points to {}, which", path.display()),
        );
        if !path.is_dir() {
            bail!("{what} is not a directory");
        }
        let path = path.canonicalize().with_context(|| format!("resolving {}", path.display()))?;
        out.push(Overlay { name, path });
    }
    Ok(out)
}

/// The `OVERLAYS+=` lines the given overlays add to a make.conf, in order.
fn overlay_lines(overlays: &[Overlay]) -> String {
    overlays.iter().map(|o| format!("OVERLAYS+={}\n", o.path.display())).collect()
}

/// Settle `settings.overlays` on what make reports for the staged make.conf
/// (and, for a local build, the environment): -O and --overlay-dir lines,
/// a poudriere.d layer, /etc/make.conf or a synth profile alike, so the
/// lookup follows exactly the overlays make will use. Names come back from
/// the command line by path; if make cannot be asked, the given ones stand.
fn with_overlays(mut settings: Settings, given: Vec<Overlay>) -> Settings {
    let reported = make_overlays(settings.make_conf.as_deref(), settings.inherit_env_overlays);
    settings.overlays = match reported {
        None => given,
        Some(paths) => paths
            .into_iter()
            .map(|path| {
                let path = path.canonicalize().unwrap_or(path);
                let name = given.iter().find(|o| o.path == path).and_then(|o| o.name.clone());
                if name.is_none() && !given.iter().any(|o| o.path == path) {
                    let what = if path.is_dir() {
                        "set by make.conf or the environment"
                    } else {
                        "does not exist"
                    };
                    settings.notes.push(format!("overlay {} {what}", path.display()));
                }
                Overlay { name, path }
            })
            .collect(),
    };
    settings
}

/// `OVERLAYS` as make sees it under `make_conf` (make's own default when
/// None), the environment's left out unless `inherit_env`. None when make
/// cannot be asked.
fn make_overlays(make_conf: Option<&Path>, inherit_env: bool) -> Option<Vec<PathBuf>> {
    let mut cmd = std::process::Command::new("/usr/bin/make");
    cmd.current_dir("/")
        .args(["-f", "/dev/null", "-V", "OVERLAYS"])
        .env_remove("MAKEFLAGS")
        .env_remove("MAKELEVEL")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if !inherit_env {
        cmd.env_remove("OVERLAYS");
    }
    if let Some(conf) = make_conf {
        cmd.arg(format!("__MAKE_CONF={}", conf.display()));
    }
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).split_whitespace().map(PathBuf::from).collect())
}

/// A build on the host sees what is installed there: Makefiles probe
/// /usr/local (Mk/Uses/iconv.mk, www/apache24's apr-util checks), so a
/// package installed or deleted can change their answers. The size and
/// mtime of pkg's database go into the hash for such a build, making each
/// change to it a new cache generation; a missing database adds nothing.
fn key_installed_packages(mut settings: Settings, pkg_db: &Path) -> Settings {
    if settings.env != QueryEnv::Host {
        return settings;
    }
    let Ok(meta) = fs::metadata(pkg_db) else { return settings };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let keyed = format!("{}\n{} {} {mtime}\n", settings.conf_hash, pkg_db.display(), meta.len());
    settings.conf_hash = crate::cache::sha256_hex(keyed.as_bytes());
    settings
}

/// A local build: `make config` on this host, built here. What is
/// installed is then the truth, not a leak. The make.conf goes through
/// the staging like any layer, so that the cache keys its content and the
/// banner names it, but make reads the original: a copy elsewhere would
/// lose the directory its relative `.include`s are found from.
fn resolve_local(
    host: &HostPaths,
    req: &Request,
    overlays: &[Overlay],
    staging_dir: &Path,
) -> Result<Settings> {
    if req.jail.is_some() || req.set.is_some() || req.tree.is_some() {
        bail!(
            "-j/-z/-p name poudriere's configuration, but {} does not exist",
            host.poudriere_d.display()
        );
    }
    let mut notes = Vec::new();
    let portsdir = match req.portsdir {
        Some(dir) => checked_portsdir(dir)?,
        None if is_ports_tree(host.ports) => {
            if host.ports_from_make {
                notes.push(format!("ports tree {} from PORTSDIR", host.ports.display()));
            }
            host.ports.to_path_buf()
        }
        None => bail!("no ports tree at {}; pass -P", host.ports.display()),
    };
    let options_dir = match req.options_dir {
        Some(d) => d.to_path_buf(),
        None => {
            if host.db_from_make {
                notes.push(format!("options dir {} from PORT_DBDIR", host.db.display()));
            }
            host.db.to_path_buf()
        }
    };
    let options_dir_is_new = !options_dir.is_dir();
    let (staged, mut conf_hash, make_conf_sources) =
        concat_make_conf(&[host.make_conf.to_path_buf()], QueryEnv::Host, &[], staging_dir)?;
    let mut make_conf = staged.map(|_| host.make_conf.to_path_buf());
    // --overlay-dir: a small make.conf naming the overlays first, then
    // including the real one, still read in place for its own .includes.
    if !overlays.is_empty() {
        let text = format!(
            "{}.sinclude \"{}\"\n",
            overlay_lines(overlays),
            std::path::absolute(host.make_conf)?.display()
        );
        let path = staging_dir.join("local-make.conf");
        fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;
        conf_hash = crate::cache::sha256_hex(format!("{conf_hash}\n{text}").as_bytes());
        make_conf = Some(path);
    }
    Ok(Settings {
        portsdir,
        options_dir,
        options_dir_is_new,
        make_conf,
        make_conf_sources,
        conf_hash,
        env: QueryEnv::Host,
        blacklist: Blacklist::default(),
        notes,
        overlays: Vec::new(),
        inherit_env_overlays: true,
    })
}

fn resolve_poudriere(
    host: &HostPaths,
    req: &Request,
    overlays: &[Overlay],
    staging_dir: &Path,
) -> Result<Settings> {
    let poudriere_d = host.poudriere_d;
    let (tree, jail, set) = (req.tree, req.jail, req.set);
    let tree_name = tree.unwrap_or("default");
    let portsdir = match req.portsdir {
        Some(dir) => checked_portsdir(dir)?,
        None => resolve_portsdir(poudriere_d, host.ports, tree_name, tree.is_some())?,
    };

    let (options_dir, options_dir_is_new) = match req.options_dir {
        Some(d) => (d.to_path_buf(), !d.is_dir()),
        None => resolve_options_dir(poudriere_d, jail, tree_name, tree.is_some(), set),
    };

    let off = no_package_building(host.poudriere_conf, poudriere_d, jail, tree, set);
    let env = QueryEnv::Jail { package_building: !off };
    let (make_conf, conf_hash, make_conf_sources) =
        layer_make_conf(poudriere_d, jail, tree_name, set, env, overlays, staging_dir)?;

    let blacklist = Blacklist::load(poudriere_d, jail, tree_name, set);

    Ok(Settings {
        portsdir,
        options_dir,
        options_dir_is_new,
        make_conf,
        make_conf_sources,
        conf_hash,
        env,
        blacklist,
        notes: Vec::new(),
        overlays: Vec::new(),
        inherit_env_overlays: false,
    })
}

/// A directory is a usable ports tree when it carries the ports framework.
fn is_ports_tree(dir: &Path) -> bool {
    dir.join("Mk/bsd.port.mk").is_file()
}

/// -P: taken as given, but it has to be a ports tree.
fn checked_portsdir(dir: &Path) -> Result<PathBuf> {
    if !is_ports_tree(dir) {
        bail!("-P {} is not a ports tree (no Mk/bsd.port.mk)", dir.display());
    }
    Ok(dir.to_path_buf())
}

/// The tree poudriere.d names. Only the implicit `default` may fall back to
/// `fallback` when poudriere.d names none: a tree given with -p that
/// poudriere does not know is a typo, not /usr/ports.
fn resolve_portsdir(
    poudriere_d: &Path,
    fallback: &Path,
    tree: &str,
    tree_explicit: bool,
) -> Result<PathBuf> {
    let mnt = poudriere_d.join("ports").join(tree).join("mnt");
    if let Ok(path) = fs::read_to_string(&mnt) {
        let p = PathBuf::from(path.trim());
        if is_ports_tree(&p) {
            return Ok(p);
        }
        bail!("poudriere tree '{tree}' points to {} which is not a ports tree", p.display());
    }
    if tree_explicit {
        bail!("no poudriere tree '{tree}' ({} does not exist)", mnt.display());
    }
    if is_ports_tree(fallback) {
        Ok(fallback.to_path_buf())
    } else {
        bail!(
            "no poudriere tree '{tree}' and no {}; pass -p or -P",
            fallback.display()
        )
    }
}

/// synth(1) layout: one `synth.ini` describing every profile, the profile's
/// own `<profile>-make.conf` (no layering), and no blacklist at all. The
/// options dir is a documented synth setting (`Directory_options`), so unlike
/// poudriere there is nothing to probe for.
fn resolve_synth(
    synth_dir: &Path,
    profile: &str,
    portsdir_flag: Option<&Path>,
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
    // A named profile that synth.ini doesn't know silently gets built-in
    // defaults — say so, it is usually a typo.
    if !ini.is_empty() && !synth_ini_has_section(&ini, profile) {
        notes.push(format!(
            "profile '{profile}' has no section in synth.ini; using built-in defaults"
        ));
    }

    let portsdir = match (portsdir_flag, setting("Directory_portsdir")) {
        (Some(dir), _) => checked_portsdir(dir)?,
        (None, Some(dir)) => {
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
        (None, None) => {
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

    // synth builds on the host it configures, so the host's view is the
    // right one: no jail environment, as for a plain /var/db/ports.
    let (make_conf, conf_hash, make_conf_sources) = concat_make_conf(
        &[synth_dir.join(format!("{profile}-make.conf"))],
        QueryEnv::Host,
        &[],
        staging_dir,
    )?;
    // A profile without a make.conf still names one: left unset, make would
    // read the host's (/etc/make.conf or the environment's __MAKE_CONF),
    // which a synth builder never sees and the empty hash does not key.
    let make_conf = make_conf.or_else(|| Some(PathBuf::from("/dev/null")));

    Ok(Settings {
        portsdir,
        options_dir,
        options_dir_is_new,
        make_conf,
        make_conf_sources,
        conf_hash,
        env: QueryEnv::Host,
        blacklist: Blacklist::default(),
        notes,
        overlays: Vec::new(),
        inherit_env_overlays: false,
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

/// Does poudriere.conf turn `PACKAGE_BUILDING` off? The files are read in
/// poudriere(8)'s own order (`include_poudriere_confs` in its common.sh): its
/// poudriere.conf, poudriere.d's, then the set, tree and jail ones of
/// poudriere.d for what the command line names. A tree counts only when `-p`
/// gave it: poudriere takes the names from its arguments, not from defaults.
///
/// The files are shell, sourced in turn, so the last assignment wins and an
/// empty value unsets. Only plain assignment lines are read (`export` and
/// quotes allowed); a value set by shell logic is not seen, but nothing runs.
fn no_package_building(
    poudriere_conf: &Path,
    poudriere_d: &Path,
    jail: Option<&str>,
    tree: Option<&str>,
    set: Option<&str>,
) -> bool {
    let mut files = vec![poudriere_conf.to_path_buf(), poudriere_d.join("poudriere.conf")];
    files.extend(
        poudriere_conf_layers(jail, tree, set)
            .into_iter()
            .map(|name| poudriere_d.join(format!("{name}-poudriere.conf"))),
    );
    let mut set_on = false;
    for file in files {
        let Ok(text) = fs::read_to_string(&file) else { continue };
        for line in text.lines() {
            if let Some(value) = shell_assignment(line, "NO_PACKAGE_BUILDING") {
                set_on = !value.is_empty();
            }
        }
    }
    set_on
}

/// The per-name poudriere.conf layers, in poudriere(8)'s reading order:
/// `<set>`, `<tree>`, `<jail>`, `<tree>-<set>`, `<jail>-<tree>`,
/// `<jail>-<set>`, `<jail>-<tree>-<set>`, each only when its names are given.
fn poudriere_conf_layers(jail: Option<&str>, tree: Option<&str>, set: Option<&str>) -> Vec<String> {
    let mut v: Vec<String> = [set, tree, jail].into_iter().flatten().map(str::to_string).collect();
    if let (Some(t), Some(s)) = (tree, set) {
        v.push(format!("{t}-{s}"));
    }
    if let (Some(j), Some(t)) = (jail, tree) {
        v.push(format!("{j}-{t}"));
    }
    if let (Some(j), Some(s)) = (jail, set) {
        v.push(format!("{j}-{s}"));
    }
    if let (Some(j), Some(t), Some(s)) = (jail, tree, set) {
        v.push(format!("{j}-{t}-{s}"));
    }
    v
}

/// The value a plain shell assignment line gives `name`, if it is one:
/// `NAME=value`, `export NAME=value`, quoted or not, a trailing ` # comment`
/// dropped. A commented-out line is no assignment.
fn shell_assignment<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let line = line.trim();
    let line = line.strip_prefix("export ").map(str::trim_start).unwrap_or(line);
    let value = line.strip_prefix(name)?.strip_prefix('=')?;
    let value = match value.find(" #") {
        Some(i) => &value[..i],
        None => value,
    }
    .trim();
    let unquoted = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(value);
    Some(unquoted)
}

fn layer_make_conf(
    poudriere_d: &Path,
    jail: Option<&str>,
    tree: &str,
    set: Option<&str>,
    env: QueryEnv,
    overlays: &[Overlay],
    staging_dir: &Path,
) -> Result<(Option<PathBuf>, String, Vec<PathBuf>)> {
    let paths: Vec<PathBuf> =
        make_conf_layers(jail, tree, set).iter().map(|n| poudriere_d.join(n)).collect();
    concat_make_conf(&paths, env, overlays, staging_dir)
}

/// Stage a make.conf holding the jail environment alone, no layers.
#[cfg(test)]
pub(crate) fn stage_jail_make_conf(staging_dir: &Path) -> Result<PathBuf> {
    let (path, _, _) =
        concat_make_conf(&[], QueryEnv::Jail { package_building: true }, &[], staging_dir)?;
    Ok(path.expect("the jail environment is never empty"))
}

/// Concatenate every existing candidate, in order, into one staged
/// `make.conf`, between the jail prelude and trailer in a jail's `env`, the
/// overlays' `OVERLAYS+=` lines where poudriere writes them in the jail's
/// make.conf: after the prelude, before the layers (host paths, as the
/// jail's `/overlays/<name>` does not exist here). The
/// hash covers the combined text (so an empty layering hashes too, and the
/// jail environment keys a cache generation of its own); nothing is written
/// when there is nothing to write.
fn concat_make_conf(
    candidates: &[PathBuf],
    env: QueryEnv,
    overlays: &[Overlay],
    staging_dir: &Path,
) -> Result<(Option<PathBuf>, String, Vec<PathBuf>)> {
    let mut combined = String::new();
    let mut sources = Vec::new();
    if let QueryEnv::Jail { package_building } = env {
        combined.push_str(POUDRIERE_JAIL_PRELUDE);
        combined.push_str(&overlay_lines(overlays));
        if package_building {
            combined.push_str(PACKAGE_BUILDING_LINES);
        }
    }
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
    if env != QueryEnv::Host {
        combined.push_str(POUDRIERE_JAIL_TRAILER);
    }

    let conf_hash = crate::cache::sha256_hex(combined.as_bytes());
    if combined.is_empty() {
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

    /// A fake ports tree at `dir`.
    fn fake_tree(dir: &Path) {
        fs::create_dir_all(dir.join("Mk")).unwrap();
        fs::write(dir.join("Mk/bsd.port.mk"), "# fake\n").unwrap();
    }

    fn host<'a>(tmp: &'a Path, poudriere_d: &'a Path, ports: &'a Path, db: &'a Path) -> HostPaths<'a> {
        HostPaths {
            poudriere_conf: tmp,
            poudriere_d,
            ports,
            db,
            make_conf: Path::new("/nonexistent/make.conf"),
            ports_from_make: false,
            db_from_make: false,
            pkg_db: Path::new("/nonexistent/local.sqlite"),
        }
    }

    /// A local build sees the installed packages (Makefiles probe
    /// /usr/local), so installing or deleting one starts a new cache
    /// generation; a missing database adds nothing, and a poudriere jail,
    /// which sees no package, does not follow it.
    #[test]
    fn host_env_generation_follows_the_pkg_db() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let db = tmp.path().join("db");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let absent = tmp.path().join("no-poudriere.d");
        let pkg_db = tmp.path().join("local.sqlite");
        let host = HostPaths { pkg_db: &pkg_db, ..host(tmp.path(), &absent, &ports, &db) };
        let none = Request::default();
        let hash = |host: &HostPaths| resolve_host(host, &none, &stage).unwrap().conf_hash;

        let (_, bare, _) = concat_make_conf(&[], QueryEnv::Host, &[], &stage).unwrap();
        assert_eq!(hash(&host), bare, "no database, nothing to key");
        fs::write(&pkg_db, "one package").unwrap();
        let one = hash(&host);
        assert_ne!(one, bare);
        fs::write(&pkg_db, "one package, then another").unwrap();
        assert_ne!(hash(&host), one, "an install is a new generation");

        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        let jail = HostPaths { poudriere_d: &pd, ..host };
        let before = hash(&jail);
        fs::remove_file(&pkg_db).unwrap();
        assert_eq!(hash(&jail), before, "a jail does not see the host's packages");
    }

    /// Resolution with and without a poudriere.d, against a fake host.
    #[test]
    fn resolve_follows_poudriere_d_or_falls_back_to_the_host() {
        let tmp = tempfile::tempdir().unwrap();
        let conf = tmp.path().join("poudriere.conf");
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let db = tmp.path().join("db");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let absent = tmp.path().join("no-poudriere.d");
        let host = host(&conf, &absent, &ports, &db);
        let none = Request::default();

        // No poudriere.d: plain make config on this host.
        let s = resolve_host(&host, &none, &stage).unwrap();
        assert_eq!((s.portsdir.as_path(), s.options_dir.as_path()), (ports.as_path(), db.as_path()));
        assert!(s.options_dir_is_new, "{} does not exist yet", db.display());
        assert!(s.env == QueryEnv::Host && s.make_conf.is_none());
        assert!(s.notes.is_empty(), "built-in defaults earn no note: {:?}", s.notes);
        fs::create_dir_all(&db).unwrap();
        assert!(!resolve_host(&host, &none, &stage).unwrap().options_dir_is_new);
        // A set, jail or tree means poudriere, which is not there.
        for req in [
            Request { jail: Some("j"), ..none },
            Request { set: Some("s"), ..none },
            Request { tree: Some("t"), ..none },
        ] {
            let err = resolve_host(&host, &req, &stage).unwrap_err().to_string();
            assert!(err.starts_with("-j/-z/-p name poudriere's"), "{err}");
        }
        // -o is taken as given either way.
        let mine = tmp.path().join("mine");
        let s = resolve_host(&host, &Request { options_dir: Some(&mine), ..none }, &stage).unwrap();
        assert_eq!(s.options_dir, mine);
        assert!(s.options_dir_is_new, "created on apply");
        let s = resolve_host(&host, &Request { options_dir: Some(&db), ..none }, &stage).unwrap();
        assert!(!s.options_dir_is_new);
        // No ports tree anywhere: say where it was looked for.
        let bare = tmp.path().join("bare");
        let nohost = HostPaths { ports: &bare, ..host };
        let err = resolve_host(&nohost, &none, &stage).unwrap_err().to_string();
        assert!(err.contains(&bare.display().to_string()), "{err}");

        // A poudriere.d: its options dir naming, a jail's environment.
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        let host = HostPaths { poudriere_d: &pd, ..host };
        let s = resolve_host(&host, &Request { set: Some("s"), ..none }, &stage).unwrap();
        assert_eq!(s.options_dir, pd.join("s-options"));
        assert!(s.options_dir_is_new);
        assert_eq!(s.env, QueryEnv::Jail { package_building: true });
        let conf = fs::read_to_string(s.make_conf.unwrap()).unwrap();
        assert!(conf.contains("PACKAGE_BUILDING=yes"), "{conf}");
    }

    /// make runs with -C in the port's directory, so a relative path handed
    /// to it would be read from there while optique reads it from its own
    /// directory: every path leaves the resolution absolute.
    #[test]
    fn resolved_paths_are_absolute() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let absent = tmp.path().join("no-poudriere.d");
        let etc = Path::new("relative-make.conf");
        let host = HostPaths {
            make_conf: etc,
            ..host(tmp.path(), &absent, &ports, Path::new("relative-db"))
        };
        let cwd = std::env::current_dir().unwrap();
        let rel = Path::new("relative-options");
        for req in [Request::default(), Request { options_dir: Some(rel), ..Request::default() }] {
            let s = resolve_host(&host, &req, &stage).unwrap();
            assert!(s.options_dir.is_absolute(), "{}", s.options_dir.display());
            assert!(s.options_dir.starts_with(&cwd), "{}", s.options_dir.display());
        }
        // A make.conf found relative to optique is handed to make absolute.
        fs::write(cwd.join(etc), "# test\n").ok();
        let s = resolve_host(&host, &Request::default(), &stage);
        fs::remove_file(cwd.join(etc)).ok();
        let make_conf = s.unwrap().make_conf.unwrap();
        assert_eq!(make_conf, cwd.join(etc));
    }

    /// -O names a poudriere tree (its mnt), --overlay-dir a directory; both
    /// keep command-line order, reach make as OVERLAYS+= after the jail
    /// prelude, and come back from make named.
    #[test]
    fn overlays_resolve_in_order_and_reach_make() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let pd = tmp.path().join("poudriere.d");
        let named = tmp.path().join("named");
        let dir = tmp.path().join("dir");
        fs::create_dir_all(&named).unwrap(); // no Mk/: an overlay need not be a tree
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(pd.join("ports/ov")).unwrap();
        fs::write(pd.join("ports/ov/mnt"), format!("{}\n", named.display())).unwrap();
        let host = host(tmp.path(), &pd, &ports, tmp.path());
        let specs = [OverlaySpec::Dir(dir.clone()), OverlaySpec::Name("ov".into())];
        let req = Request { overlays: &specs, ..Request::default() };

        let s = resolve_host(&host, &req, &stage).unwrap();
        let (dir, named) = (dir.canonicalize().unwrap(), named.canonicalize().unwrap());
        assert_eq!(
            s.overlays,
            [Overlay { name: None, path: dir.clone() }, Overlay { name: Some("ov".into()), path: named.clone() }]
        );
        assert!(!s.inherit_env_overlays, "a jail does not see optique's environment");
        let conf = fs::read_to_string(s.make_conf.as_ref().unwrap()).unwrap();
        let lines = format!("OVERLAYS+={}\nOVERLAYS+={}\n", dir.display(), named.display());
        let at = conf.find(&lines).expect("the OVERLAYS lines are staged");
        assert!(at > conf.find("BATCH=yes").unwrap() && at < conf.find("PACKAGE_BUILDING").unwrap(), "{conf}");
        let bare = resolve_host(&host, &Request::default(), &stage).unwrap();
        assert_ne!(s.conf_hash, bare.conf_hash, "the overlays key their own generation");
        assert!(bare.overlays.is_empty());

        // The errors, in poudriere's terms.
        let err = |specs: &[OverlaySpec]| {
            resolve_host(&host, &Request { overlays: specs, ..Request::default() }, &stage)
                .unwrap_err()
                .to_string()
        };
        assert!(err(&[OverlaySpec::Name("nope".into())]).starts_with("no poudriere overlay 'nope'"));
        fs::create_dir_all(pd.join("ports/nomnt")).unwrap();
        assert!(err(&[OverlaySpec::Name("nomnt".into())]).contains("has no mnt file"));
        fs::create_dir_all(pd.join("ports/gone")).unwrap();
        fs::write(pd.join("ports/gone/mnt"), "/nonexistent/overlay\n").unwrap();
        assert!(err(&[OverlaySpec::Name("gone".into())]).ends_with("is not a directory"));
        assert!(err(&[OverlaySpec::Dir("/nonexistent/dir".into())]).starts_with("--overlay-dir"));

        // No poudriere.d: a name has nothing to be read from.
        let local = HostPaths { poudriere_d: Path::new("/nonexistent/poudriere.d"), ..host };
        let e = resolve_host(&local, &Request { overlays: &[OverlaySpec::Name("ov".into())], ..Request::default() }, &stage)
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("-O ov names a poudriere tree"), "{e}");
    }

    /// A local build reads its make.conf in place; --overlay-dir comes first
    /// through a small make.conf that includes it, and an overlay that
    /// make.conf sets itself follows, unnamed and noted.
    #[test]
    fn local_overlay_dirs_go_before_the_make_confs_own() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let (mine, theirs) = (tmp.path().join("mine"), tmp.path().join("theirs"));
        fs::create_dir_all(&mine).unwrap();
        fs::create_dir_all(&theirs).unwrap();
        let etc = tmp.path().join("etc");
        fs::create_dir_all(&etc).unwrap();
        fs::write(etc.join("make.conf"), ".include \"overlays.conf\"\n").unwrap();
        fs::write(etc.join("overlays.conf"), format!("OVERLAYS+={}\n", theirs.display())).unwrap();
        let absent = tmp.path().join("no-poudriere.d");
        let conf = etc.join("make.conf");
        let host = HostPaths { make_conf: &conf, ..host(tmp.path(), &absent, &ports, tmp.path()) };
        let specs = [OverlaySpec::Dir(mine.clone())];

        let s = resolve_host(&host, &Request { overlays: &specs, ..Request::default() }, &stage).unwrap();
        let paths: Vec<_> = s.overlays.iter().map(|o| o.path.clone()).collect();
        assert_eq!(paths, [mine.canonicalize().unwrap(), theirs.canonicalize().unwrap()]);
        assert!(s.inherit_env_overlays);
        assert!(s.notes.iter().any(|n| n.contains("set by make.conf")), "{:?}", s.notes);
        let plain = resolve_host(&host, &Request::default(), &stage).unwrap();
        assert_eq!(plain.make_conf.as_deref(), Some(conf.as_path()), "read in place without one");
        assert_ne!(s.conf_hash, plain.conf_hash);
    }

    /// make's PORTSDIR and PORT_DBDIR are a local build's defaults, and the
    /// banner says where they came from.
    #[test]
    fn local_defaults_from_make_are_noted() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let db = tmp.path().join("db");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let absent = tmp.path().join("no-poudriere.d");
        let host = HostPaths {
            ports_from_make: true,
            db_from_make: true,
            ..host(tmp.path(), &absent, &ports, &db)
        };
        let s = resolve_host(&host, &Request::default(), &stage).unwrap();
        assert_eq!((s.portsdir.as_path(), s.options_dir.as_path()), (ports.as_path(), db.as_path()));
        assert!(s.notes.iter().any(|n| n.ends_with("from PORTSDIR")), "{:?}", s.notes);
        assert!(s.notes.iter().any(|n| n.ends_with("from PORT_DBDIR")), "{:?}", s.notes);
        // -P and -o win, and then there is nothing to note.
        let other = tmp.path().join("other");
        fake_tree(&other);
        let req = Request { portsdir: Some(&other), options_dir: Some(&other), ..Request::default() };
        let s = resolve_host(&host, &req, &stage).unwrap();
        assert_eq!((s.portsdir.as_path(), s.options_dir.as_path()), (other.as_path(), other.as_path()));
        assert!(s.notes.is_empty(), "{:?}", s.notes);
    }

    /// -l ignores a poudriere.d that exists: the host's tree, options dir,
    /// environment, and no poudriere layer or blacklist.
    #[test]
    fn local_ignores_poudriere_d() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let db = tmp.path().join("db");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(pd.join("options")).unwrap();
        fs::write(pd.join("make.conf"), "WITH_DEBUG=yes\n").unwrap();
        fs::write(pd.join("blacklist"), "www/*\n").unwrap();
        let host = host(tmp.path(), &pd, &ports, &db);

        let local = Request { local: true, ..Request::default() };
        let s = resolve_host(&host, &local, &stage).unwrap();
        assert_eq!((s.portsdir.as_path(), s.options_dir.as_path()), (ports.as_path(), db.as_path()));
        assert_eq!(s.env, QueryEnv::Host);
        assert!(s.make_conf.is_none() && s.make_conf_sources.is_empty());
        assert!(s.blacklist.is_empty());
        // The host's make.conf, not poudriere.d's, staged and keyed.
        let etc = tmp.path().join("etc-make.conf");
        fs::write(&etc, "DEFAULT_VERSIONS+=ssl=base\n").unwrap();
        let s = resolve_host(&HostPaths { make_conf: &etc, ..host }, &local, &stage).unwrap();
        assert_eq!(s.make_conf_sources, vec![etc.clone()]);
        // Read in place, for its relative .includes, and keyed all the same.
        assert_eq!(s.make_conf.as_ref(), Some(&etc));
        let (_, empty_hash, _) = concat_make_conf(&[], QueryEnv::Host, &[], &stage).unwrap();
        assert_ne!(s.conf_hash, empty_hash);
        // The same host without -l is a poudriere build.
        let s = resolve_host(&host, &Request::default(), &stage).unwrap();
        assert_eq!(s.options_dir, pd.join("options"));
        assert!(!s.blacklist.is_empty());
    }

    /// Under a poudriere.d, -p must name a tree poudriere knows; only the
    /// implicit default may fall back. -P bypasses the lookup.
    #[test]
    fn poudriere_tree_is_strict_and_portsdir_overrides_it() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let named = tmp.path().join("named");
        fake_tree(&named);
        let db = tmp.path().join("db");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(pd.join("ports/main")).unwrap();
        fs::write(pd.join("ports/main/mnt"), format!("{}\n", named.display())).unwrap();
        let host = host(tmp.path(), &pd, &ports, &db);
        let none = Request::default();

        let s = resolve_host(&host, &Request { tree: Some("main"), ..none }, &stage).unwrap();
        assert_eq!(s.portsdir, named);
        let err = resolve_host(&host, &Request { tree: Some("mian"), ..none }, &stage)
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("no poudriere tree 'mian'"), "{err}");
        // No -p and no default tree: the fallback.
        assert_eq!(resolve_host(&host, &none, &stage).unwrap().portsdir, ports);

        // -P: taken as given, but checked.
        let s = resolve_host(&host, &Request { portsdir: Some(&named), ..none }, &stage).unwrap();
        assert_eq!(s.portsdir, named);
        assert!(matches!(s.env, QueryEnv::Jail { .. }), "still a poudriere build");
        let err = resolve_host(&host, &Request { portsdir: Some(&db), ..none }, &stage)
            .unwrap_err()
            .to_string();
        assert!(err.contains("is not a ports tree"), "{err}");
    }

    #[test]
    fn shell_assignment_reads_plain_assignments_only() {
        fn v(l: &str) -> Option<&str> {
            shell_assignment(l, "NO_PACKAGE_BUILDING")
        }
        assert_eq!(v("NO_PACKAGE_BUILDING=yes"), Some("yes"));
        assert_eq!(v("  export NO_PACKAGE_BUILDING=yes"), Some("yes"));
        assert_eq!(v("NO_PACKAGE_BUILDING=\"yes\""), Some("yes"));
        assert_eq!(v("NO_PACKAGE_BUILDING='yes'  # for this box"), Some("yes"));
        assert_eq!(v("NO_PACKAGE_BUILDING="), Some(""), "an empty value unsets");
        assert_eq!(v("NO_PACKAGE_BUILDING=\"\""), Some(""));
        assert_eq!(v("# NO_PACKAGE_BUILDING=yes"), None, "the sample's commented line");
        assert_eq!(v("NO_PACKAGE_BUILDING_X=yes"), None);
        assert_eq!(v("XNO_PACKAGE_BUILDING=yes"), None);
    }

    /// poudriere's order (include_poudriere_confs): names alone, then pairs,
    /// then all three; only the names given.
    #[test]
    fn poudriere_conf_layers_follow_poudriere() {
        assert_eq!(
            poudriere_conf_layers(Some("j"), Some("t"), Some("s")),
            ["s", "t", "j", "t-s", "j-t", "j-s", "j-t-s"]
        );
        assert_eq!(poudriere_conf_layers(Some("j"), None, Some("s")), ["s", "j", "j-s"]);
        assert!(poudriere_conf_layers(None, None, None).is_empty());
    }

    /// Sourced in turn: the last assignment wins, an empty one unsets, and a
    /// tree's file counts only when the tree was named.
    #[test]
    fn no_package_building_reads_every_layer_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let conf = tmp.path().join("poudriere.conf");
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        let npb = |jail, tree, set| no_package_building(&conf, &pd, jail, tree, set);
        assert!(!npb(None, None, None), "no file at all");
        fs::write(&conf, "# NO_PACKAGE_BUILDING=yes\nBASEFS=/usr/local/poudriere\n").unwrap();
        assert!(!npb(None, None, None));
        fs::write(&conf, "NO_PACKAGE_BUILDING=yes\n").unwrap();
        assert!(npb(None, None, None));
        fs::write(pd.join("poudriere.conf"), "NO_PACKAGE_BUILDING=\n").unwrap();
        assert!(!npb(None, None, None), "poudriere.d's unsets it");
        fs::write(pd.join("s-poudriere.conf"), "NO_PACKAGE_BUILDING=yes\n").unwrap();
        assert!(npb(None, None, Some("s")));
        assert!(!npb(None, None, Some("other")));
        fs::write(pd.join("j-s-poudriere.conf"), "NO_PACKAGE_BUILDING=\n").unwrap();
        assert!(!npb(Some("j"), None, Some("s")), "the jail-set file comes last");
        fs::write(pd.join("default-poudriere.conf"), "NO_PACKAGE_BUILDING=yes\n").unwrap();
        assert!(!npb(None, None, None), "no -p, no tree file");
        assert!(npb(None, Some("default"), None));
    }

    /// End to end: with NO_PACKAGE_BUILDING set, the staged make.conf keeps
    /// the rest of the jail's environment but not PACKAGE_BUILDING, and the
    /// cache keys the difference.
    #[test]
    fn no_package_building_drops_package_building_from_the_queries() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fs::create_dir_all(ports.join("Mk")).unwrap();
        fs::write(ports.join("Mk/bsd.port.mk"), "# fake\n").unwrap();
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        let conf = tmp.path().join("poudriere.conf");
        let db = tmp.path().join("db");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let host = HostPaths { poudriere_conf: &conf, ..host(tmp.path(), &pd, &ports, &db) };

        let on = resolve_poudriere(&host, &Request::default(), &[], &stage).unwrap();
        assert_eq!(on.env, QueryEnv::Jail { package_building: true });
        fs::write(&conf, "NO_PACKAGE_BUILDING=yes\n").unwrap();
        let off = resolve_poudriere(&host, &Request::default(), &[], &stage).unwrap();
        assert_eq!(off.env, QueryEnv::Jail { package_building: false });
        let text = fs::read_to_string(off.make_conf.unwrap()).unwrap();
        assert!(!text.contains("PACKAGE_BUILDING"), "{text}");
        assert!(text.contains("BATCH=yes") && text.contains("LOCALBASE=/nonexistent"), "{text}");
        assert_ne!(on.conf_hash, off.conf_hash);
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
        assert_eq!(resolve_portsdir(&pd, Path::new("/nonexistent"), "mytree", true).unwrap(), tree);
        // A tree pointing at a non-ports dir is an error, not a fallback.
        fs::write(pd.join("ports/mytree/mnt"), "/nonexistent\n").unwrap();
        assert!(resolve_portsdir(&pd, Path::new("/nonexistent"), "mytree", true).is_err());
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
            layer_make_conf(&pd, Some("j1"), "t1", Some("s1"), QueryEnv::Host, &[], &stage).unwrap();
        assert_eq!(sources.len(), 8);
        let text = fs::read_to_string(path.unwrap()).unwrap();
        let idx = |needle: &str| text.find(needle).unwrap();
        let order: Vec<usize> = layers.iter().map(|(_, c)| idx(c)).collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "layers out of order: {text}");
        assert!(!text.contains("PACKAGE_BUILDING") && !text.contains("LOCALBASE"), "{text}");
    }

    /// A layer without a final newline does not run into the next one.
    #[test]
    fn layers_without_a_final_newline_stay_apart() {
        let tmp = tempfile::tempdir().unwrap();
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        fs::write(pd.join("make.conf"), "A=1").unwrap();
        fs::write(pd.join("s1-make.conf"), "B=2\n").unwrap();
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let (path, _, _) = layer_make_conf(&pd, None, "t1", Some("s1"), QueryEnv::Host, &[], &stage).unwrap();
        let text = fs::read_to_string(path.unwrap()).unwrap();
        assert!(text.lines().any(|l| l == "A=1"), "{text}");
        assert!(text.lines().any(|l| l == "B=2"), "{text}");
    }

    #[test]
    fn jail_env_wraps_the_layers() {
        let tmp = tempfile::tempdir().unwrap();
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        fs::write(pd.join("make.conf"), "LOCALBASE=/opt/local\n").unwrap();
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let (path, jail_hash, _) =
            layer_make_conf(&pd, None, "t1", None, QueryEnv::Jail { package_building: true }, &[], &stage).unwrap();
        let text = fs::read_to_string(path.unwrap()).unwrap();
        // poudriere's own lines come before the layers, as in a jail...
        assert!(text.find("PACKAGE_BUILDING=yes").unwrap() < text.find("/opt/local").unwrap());
        // ...and the hidden LOCALBASE after them, so no layer can undo it.
        assert!(text.trim_end().ends_with("LOCALBASE=/nonexistent"), "{text}");

        // The environment keys its own cache generation.
        let (_, host_hash, _) = layer_make_conf(&pd, None, "t1", None, QueryEnv::Host, &[], &stage).unwrap();
        assert_ne!(jail_hash, host_hash);

        // An empty layering still stages a make.conf in a jail's environment.
        let empty = tmp.path().join("empty.d");
        fs::create_dir_all(&empty).unwrap();
        let (path, _, sources) = layer_make_conf(&empty, None, "t1", None, QueryEnv::Jail { package_building: true }, &[], &stage).unwrap();
        assert!(sources.is_empty());
        assert!(path.is_some());
        let (path, _, _) = layer_make_conf(&empty, None, "t1", None, QueryEnv::Host, &[], &stage).unwrap();
        assert!(path.is_none());
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
        let s = resolve_synth(&synth, "typo", None, None, &stage).unwrap();
        assert!(s.notes.iter().any(|n| n.contains("'typo' has no section")), "{:?}", s.notes);
        assert!(s.notes.iter().any(|n| n.contains("live /var/db/ports")), "{:?}", s.notes);

        // Bare -s honours profile_selected; known section → no missing note.
        let s = resolve_synth(&synth, "", None, None, &stage).unwrap();
        assert!(s.notes.iter().any(|n| n.contains("profile Live (synth.ini")), "{:?}", s.notes);
        assert!(!s.notes.iter().any(|n| n.contains("no section")), "{:?}", s.notes);

        // -o override: no live-dir note.
        let o = tmp.path().join("other");
        let s = resolve_synth(&synth, "Live", None, Some(&o), &stage).unwrap();
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

        let s = resolve_synth(&synth, "LiveSystem", None, None, &stage).unwrap();
        assert_eq!(s.portsdir, tree);
        assert_eq!(s.options_dir, opts);
        assert!(!s.options_dir_is_new);
        assert_eq!(s.make_conf_sources, vec![synth.join("LiveSystem-make.conf")]);
        let staged = fs::read_to_string(s.make_conf.as_ref().unwrap()).unwrap();
        assert!(staged.contains("ssl=openssl"), "{staged}");
        assert!(!staged.contains("A=1"), "other profile leaked in: {staged}");
        // synth has no blacklist, and nothing to note.
        assert!(s.blacklist.is_empty());
        assert!(s.notes.is_empty());
        assert!(!s.conf_hash.is_empty());

        // -o wins over Directory_options, -P over Directory_portsdir.
        let elsewhere = tmp.path().join("elsewhere");
        fake_tree(&elsewhere);
        let s = resolve_synth(&synth, "LiveSystem", Some(&elsewhere), Some(&elsewhere), &stage)
            .unwrap();
        assert_eq!((s.portsdir.as_path(), s.options_dir.as_path()), (elsewhere.as_path(), elsewhere.as_path()));
        assert!(s.notes.is_empty(), "{:?}", s.notes);
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

        let s = resolve_synth(&synth, "LiveSystem", None, None, &stage).unwrap();
        assert_eq!(s.portsdir, tree);
        // Nonexistent Directory_options is still the target, flagged as new.
        assert_eq!(s.options_dir, opts);
        assert!(s.options_dir_is_new);
        // No make.conf is still one: left unset, make would read the host's
        // (/etc/make.conf or the environment's __MAKE_CONF), which a synth
        // builder never does, and which the empty hash would not key.
        assert_eq!(s.make_conf.as_deref(), Some(Path::new("/dev/null")));
        assert!(s.make_conf_sources.is_empty());
        assert!(!stage.join("make.conf").exists(), "nothing to stage");

        // Directory_portsdir naming a non-tree is an error, not a fallback.
        fs::write(
            synth.join("synth.ini"),
            format!("[LiveSystem]\nDirectory_portsdir={}/nope\n", tmp.path().display()),
        )
        .unwrap();
        let err = resolve_synth(&synth, "LiveSystem", None, Some(&opts), &stage)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a ports tree"), "{err}");

        // No synth.ini: options dir falls back to /var/db/ports (the portsdir
        // fallback is /usr/ports, whose presence is host-dependent, so it is
        // only exercised through -o here).
        fs::remove_file(synth.join("synth.ini")).unwrap();
        if is_ports_tree(Path::new("/usr/ports")) {
            let s = resolve_synth(&synth, "LiveSystem", None, None, &stage).unwrap();
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
        // A middle literal consumes what it matched: two of them need two.
        assert!(glob_match("*ab*ab", "abab"));
        assert!(!glob_match("*ab*ab", "ab"));
        assert!(glob_match("x*-*y", "xa-by"));
        assert!(!glob_match("x*-*y", "xy"));
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
