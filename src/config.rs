use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::model::origin::PortKey;
use crate::query::makerunner::Identity;

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
    /// The host's own, for a build on the host (plain /var/db/ports).
    Host,
    /// A poudriere jail's: nothing installed, poudriere's make.conf prelude,
    /// `PACKAGE_BUILDING` unless poudriere.conf turns it off.
    Jail { package_building: bool },
    /// synth's scan slave's: nothing installed, synth's make.conf header.
    Synth,
}

impl QueryEnv {
    /// Who queries run as: root in poudriere's jail (`jail_start` exports
    /// `USER=root` and `HOME=/root`, and `injail` keeps them) and in synth's
    /// scan slave (`scan_environment`), the invoking user on the host.
    pub fn identity(self) -> Identity {
        match self {
            QueryEnv::Host => Identity::Invoker,
            QueryEnv::Jail { .. } | QueryEnv::Synth => Identity::Root,
        }
    }
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

/// What poudriere(8) appends to the jail's make.conf after every other
/// layer: `setup_ccache` (common.sh:3591-3600), called from `jail_start`
/// (common.sh:4195) well after `setup_makeconf`'s own call there
/// (common.sh:4121) has already written the poudriere.d layers. Its heredoc
/// is unquoted, so `${HOME}` is expanded where it runs, not in the jail:
/// `jail_start` sets `export HOME=/root` (common.sh:4000) earlier in that
/// same function, no subshell between, so it is always `/root` here.
const POUDRIERE_JAIL_CCACHE: &str = "\
WITH_CCACHE_BUILD=yes
CCACHE_DIR=/root/.ccache
";

/// What synth(1) writes at the top of its scan slave's make.conf, ahead of
/// `<profile>-make.conf` (`create_make_conf` in its replicant.adb), as far as
/// it bears on evaluating a Makefile: the slave's PORTSDIR, PORT_DBDIR and
/// friends are chroot paths optique sets its own way. synth gives
/// `PACKAGE_BUILDING` alone, never `PACKAGE_BUILDING_FLAVORS`. The profile
/// line comes first and the ccache lines last, from [`synth_prelude`].
const SYNTH_SCAN_PRELUDE: &str = "\
USE_PACKAGE_DEPENDS_ONLY=yes
PACKAGE_BUILDING=yes
BATCH=yes
";

/// synth reads ports in a chroot whose /usr/local is an empty tmpfs or
/// nullfs (replicant.adb), so the host's packages answer no `exists()`
/// probe there: Mk/Uses/php.mk's php.conf and httpd checks, iconv.mk.
const SYNTH_SCAN_TRAILER: &str = "\
# --- synth scan environment (optique) ---
LOCALBASE=/nonexistent
";

/// What synth adds to that header when the profile's `Directory_ccache`
/// exists (replicant.adb:754-757): ports then gain devel/ccache as a build
/// dependency.
const SYNTH_SCAN_CCACHE: &str = "\
WITH_CCACHE_BUILD=yes
CCACHE_DIR=/ccache
";

/// synth's header for `profile`, as its scan slave's make.conf starts,
/// with the ccache lines when `ccache` is on.
fn synth_prelude(profile: &str, ccache: bool) -> String {
    let ccache = if ccache { SYNTH_SCAN_CCACHE } else { "" };
    format!(
        "# --- synth scan environment (optique) ---\nSYNTHPROFILE={profile}\n\
         {SYNTH_SCAN_PRELUDE}{ccache}"
    )
}

/// Does synth turn ccache on for a profile whose `Directory_ccache` is
/// `setting`? Only when that path exists; unset is synth's `disabled`.
fn synth_ccache(setting: Option<&str>) -> bool {
    setting.is_some_and(|dir| dir != "disabled" && Path::new(dir).exists())
}

const SYNTH_DIR: &str = "/usr/local/etc/synth";
/// The trees synth tries, in order, after `PORTSDIR` and make's answer
/// (`std_dports_loc`, `std_ports_loc` in its parameters.ads).
const SYNTH_STANDARD_PORTSDIRS: &[&str] = &["/usr/dports", "/usr/ports"];
/// Section of synth.ini whose settings apply to every profile.
const SYNTH_GLOBAL_SECTION: &str = "Global Configuration";

/// Resolved execution environment: where the ports tree lives, which options
/// dir is the read/write target, and the layered make.conf to query with.
#[derive(Debug)]
pub struct Settings {
    pub portsdir: PathBuf,
    /// The real options dir writes will target (PORT_DBDIR equivalent).
    pub options_dir: PathBuf,
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
    /// poudriere.conf's `FLAVOR_DEFAULT_ALL=yes`: a bare origin listed
    /// stands for every flavor of its port (poudriere builds only).
    pub flavor_default_all: bool,
    /// The framework's `_EXPORTED_VARS` as one probe of the settled tree,
    /// make.conf and environment found them, given to every query
    /// (`makerunner::framework_values`); empty when make could not say.
    pub framework: Vec<(String, String)>,
    /// Where make.conf alone puts options files, as one probe of the
    /// settled make.conf found it (`makerunner::options_place`); None when
    /// make could not say.
    pub options_place: Option<OptionsPlace>,
}

/// What make.conf sets of `PORT_DBDIR` and `OPTIONS_FILE`, each None when
/// it leaves the variable unset (or empty).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OptionsPlace {
    /// Expanded, as a build uses it.
    pub port_dbdir: Option<String>,
    /// As written: its `${OPTIONS_NAME}` is a port's.
    pub options_file: Option<String>,
}

/// bsd.options.mk's own `OPTIONS_FILE?=`: a make.conf spelling it out
/// moves nothing.
const DEFAULT_OPTIONS_FILE: &str = "${PORT_DBDIR}/${OPTIONS_NAME}/options";

/// Why clean cannot tell which options-dir entries make reads, so removes
/// none as vanished or unused, from `place`: make.conf relocates options
/// files, or make could not say whether it does. None when the entries are
/// named as bsd.options.mk names them.
pub fn relocated_options(place: Option<&OptionsPlace>) -> Option<String> {
    let tail = "optique cannot tell which options-dir entries make reads, \
                so none is removed as vanished or unused";
    match place {
        None => Some(format!("make could not say whether make.conf sets OPTIONS_FILE: {tail}")),
        Some(OptionsPlace { options_file: Some(file), .. }) if file != DEFAULT_OPTIONS_FILE => {
            Some(format!("make.conf sets OPTIONS_FILE ({file}): {tail}"))
        }
        Some(_) => None,
    }
}

/// Where the build reads options files from: a poudriere jail's
/// /var/db/ports, where poudriere puts the options dir and which it leaves
/// as the framework's default; synth's /options, which its own make.conf
/// header names; the options dir itself on the host.
fn build_dbdir(env: QueryEnv, options_dir: &Path) -> &Path {
    match env {
        QueryEnv::Jail { .. } => Path::new("/var/db/ports"),
        QueryEnv::Synth => Path::new("/options"),
        QueryEnv::Host => options_dir,
    }
}

/// The error when make.conf's `PORT_DBDIR` (`set`) sends the build away
/// from the options dir optique reads and writes: the command line
/// optique gives its queries overrides it, the build's does not. Paths are
/// compared as given, then canonicalised where both exist on the host.
fn port_dbdir_error(env: QueryEnv, options_dir: &Path, set: &str) -> Option<String> {
    let reads = build_dbdir(env, options_dir);
    let set_path = Path::new(set);
    let same = set_path == reads
        || env == QueryEnv::Host
            && matches!((set_path.canonicalize(), reads.canonicalize()), (Ok(a), Ok(b)) if a == b);
    (!same).then(|| {
        format!(
            "make.conf sets PORT_DBDIR={set}: the build reads that directory, not {}",
            options_dir.display()
        )
    })
}

impl Settings {
    /// The error a make.conf `PORT_DBDIR` moving the build's options
    /// directory away from `options_dir` deserves; None when it moves
    /// nothing, or make could not say.
    pub fn port_dbdir_error(&self) -> Option<String> {
        let set = self.options_place.as_ref()?.port_dbdir.as_deref()?;
        port_dbdir_error(self.env, &self.options_dir, set)
    }
}

/// A ports tree laid over the main one (the framework's `OVERLAYS`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overlay {
    /// The poudriere tree name it was given as (-O), if any.
    pub name: Option<String>,
    pub path: PathBuf,
}

impl Overlay {
    /// How the banner, the scan table and the TUI name it: its poudriere
    /// tree name, else its path.
    pub fn label(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.path.display().to_string())
    }
}

/// `Settings::overlay_of` for a bare list of overlays (the TUI keeps one).
pub fn overlay_of<'a>(overlays: &'a [Overlay], origin: &str) -> Option<&'a Overlay> {
    overlays.iter().find(|o| o.path.join(origin).join("Makefile").is_file())
}

/// An overlay as the command line gives it: a registered poudriere tree
/// (-O name) or a directory (--overlay-dir).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OverlaySpec {
    Name(String),
    Dir(PathBuf),
}

impl Settings {
    /// The overlay a port is taken from, if any: the first holding it, as
    /// the lookup goes; None when the main tree answers.
    pub fn overlay_of(&self, origin: &str) -> Option<&Overlay> {
        overlay_of(&self.overlays, origin)
    }

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
#[derive(Clone, Debug, Default)]
pub struct Blacklist {
    patterns: Vec<String>,
    pub sources: Vec<PathBuf>,
}

impl Blacklist {
    /// As [`Blacklist::load_emulating`] with `emulating` false — every
    /// production call site now knows whether -j's jail is emulated, so
    /// this convenience is a test-only shorthand for the common case.
    #[cfg(test)]
    pub(crate) fn load(poudriere_d: &Path, jail: Option<&str>, tree: &str, set: Option<&str>) -> Self {
        Self::load_emulating(poudriere_d, jail, tree, set, false)
    }

    /// As [`Blacklist::load`], also loading the `qemu-blacklist` layer last
    /// when `emulating` (poudriere would run this -j jail under qemu):
    /// `load_blacklist` in common.sh (~4248-4285) appends `qemu` to its own
    /// list of names after every jail/tree/set combination, right before
    /// reading them all in order, so it is always loaded last, if at all.
    pub fn load_emulating(
        poudriere_d: &Path,
        jail: Option<&str>,
        tree: &str,
        set: Option<&str>,
        emulating: bool,
    ) -> Self {
        let mut bl = Blacklist::default();
        for name in blacklist_layers(jail, tree, set, emulating) {
            let path = poudriere_d.join(&name);
            let Ok(text) = fs::read_to_string(&path) else { continue };
            for line in text.lines() {
                let entry = match line.split_once('#') {
                    Some((before, _)) => before,
                    None => line,
                };
                bl.patterns.extend(entry.split_whitespace().map(str::to_string));
            }
            bl.sources.push(path);
        }
        bl
    }

    /// Is this port blacklisted, matching as poudriere's `deps_fetch_vars` does
    /// (common.sh): entries are exact origins, never globs. A port with no
    /// flavors (`key.flavor` is `None`) matches only its bare origin. A
    /// flavored port always matches `origin` or `origin@all`, and
    /// `origin@<its flavor>`; the default flavor (first of `PortInfo::flavors`,
    /// told apart by `is_default_flavor`) additionally matches `origin@-`,
    /// poudriere's `FLAVOR_DEFAULT`.
    pub fn matches(&self, key: &PortKey, is_default_flavor: bool) -> bool {
        let has = |entry: &str| self.patterns.iter().any(|p| p == entry);
        let Some(flavor) = &key.flavor else {
            return has(&key.origin);
        };
        has(&key.origin)
            || has(&format!("{}@all", key.origin))
            || has(&format!("{}@{flavor}", key.origin))
            || (is_default_flavor && has(&format!("{}@-", key.origin)))
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }
}

/// The stems poudriere(8) builds every per-name layer from, in its own read
/// order: `set`, `tree`, `jail`, `tree-set`, `jail-tree`, `jail-set`,
/// `jail-tree-set` — each only when every name it needs was given. `tree` is
/// optional here because `poudriere_conf_layers` counts a tree only when -p
/// named it; `blacklist_layers` and `make_conf_layers` always have one, so
/// they pass `Some`.
fn layer_stems(jail: Option<&str>, tree: Option<&str>, set: Option<&str>) -> Vec<String> {
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

/// blacklist layer names in poudriere(8) read order; every existing file
/// contributes:
///
///   blacklist, <set>-blacklist, <tree>-blacklist, <jail>-blacklist,
///   <tree>-<set>-blacklist, <jail>-<tree>-blacklist, <jail>-<set>-blacklist,
///   <jail>-<tree>-<set>-blacklist, [qemu-blacklist, only when `emulating`]
fn blacklist_layers(jail: Option<&str>, tree: &str, set: Option<&str>, emulating: bool) -> Vec<String> {
    let mut v = vec!["blacklist".to_string()];
    v.extend(layer_stems(jail, Some(tree), set).into_iter().map(|s| format!("{s}-blacklist")));
    if emulating {
        v.push("qemu-blacklist".to_string());
    }
    v
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

/// A path from the environment, `var`, ignored when it is set but empty,
/// else `default`.
fn env_path_or(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map_or_else(|| PathBuf::from(default), PathBuf::from)
}

/// `Cli::check_globals` rejects the flag combinations that make no sense
/// (-s or -l with -z/-j/-p, -p with -P), so `req` is consistent here.
pub fn resolve(req: &Request, staging_dir: &Path) -> Result<Settings> {
    // pkg(8)'s own default, which the environment may move.
    let pkg_db = env_path_or("PKG_DBDIR", "/var/db/pkg").join("local.sqlite");
    if let Some(profile) = req.synth {
        let env_profile = std::env::var("SYNTHPROFILE").ok();
        let env_portsdir =
            std::env::var_os("PORTSDIR").filter(|v| !v.is_empty()).map(PathBuf::from);
        let make_portsdir = || {
            let text = make_values(&["PORTSDIR"], None, &[])?;
            let dir = text.trim();
            (!dir.is_empty()).then(|| PathBuf::from(dir))
        };
        let host = SynthHost {
            env_profile: env_profile.as_deref(),
            env_portsdir: env_portsdir.as_deref(),
            make_portsdir: &make_portsdir,
            standard_portsdirs: SYNTH_STANDARD_PORTSDIRS,
        };
        let settings = resolve_synth(
            Path::new(SYNTH_DIR),
            profile,
            &host,
            req.portsdir,
            req.options_dir,
            staging_dir,
        )?;
        return finish(settings, Vec::new(), &pkg_db);
    }
    // -l reads no poudriere configuration, so POUDRIERE_ETC is not asked.
    let etc = if req.local { None } else { poudriere_etc(std::env::var_os("POUDRIERE_ETC"))? };
    let (poudriere_conf, poudriere_d) = match &etc {
        Some(etc) => (etc.join("poudriere.conf"), etc.join("poudriere.d")),
        None => (PathBuf::from(POUDRIERE_CONF), PathBuf::from(POUDRIERE_D)),
    };
    let poudriere_d = poudriere_d.as_path();
    // Only a local build asks make for its defaults: a poudriere build
    // reads neither the environment's nor /etc/make.conf's.
    let local = req.local || !poudriere_d.is_dir();
    let (ports, db) = if local { make_defaults() } else { (None, None) };
    // make's own default, which the environment may move as for any make.
    let make_conf = env_path_or("__MAKE_CONF", "/etc/make.conf");
    let (arch, supported_archs) = if !local && req.jail.is_some() {
        (host_arch(), supported_archs())
    } else {
        Default::default()
    };
    let host = HostPaths {
        poudriere_conf: &poudriere_conf,
        poudriere_d,
        ports: ports.as_deref().unwrap_or(Path::new("/usr/ports")),
        db: db.as_deref().unwrap_or(Path::new("/var/db/ports")),
        make_conf: &make_conf,
        ports_from_make: ports.is_some(),
        db_from_make: db.is_some(),
        pkg_db: &pkg_db,
        env_overlays: &env_overlays(),
        arch: &arch,
        supported_archs: &supported_archs,
    };
    let mut settings = resolve_host(&host, req, staging_dir)?;
    if let Some(etc) = &etc {
        settings.notes.insert(0, poudriere_etc_note(etc, !local));
    }
    Ok(settings)
}

/// poudriere's configuration root as `POUDRIERE_ETC` moves it (`value`,
/// the variable as optique was given it): poudriere.in hands any non-empty
/// value to the sub-command as `-e etcdir` would set it, made absolute.
/// None when unset or empty. poudriere makes no check of it, `-e` alone
/// asking for a directory; optique asks it of both, rather than read a
/// poudriere.d that cannot be there as a missing one.
fn poudriere_etc(value: Option<std::ffi::OsString>) -> Result<Option<PathBuf>> {
    let Some(value) = value.filter(|v| !v.is_empty()) else { return Ok(None) };
    let etc = std::path::absolute(&value)
        .with_context(|| format!("resolving POUDRIERE_ETC {}", Path::new(&value).display()))?;
    if !etc.is_dir() {
        bail!("POUDRIERE_ETC {} is not a directory", etc.display());
    }
    Ok(Some(etc))
}

/// The banner's word on `POUDRIERE_ETC`: where poudriere's configuration
/// was read from, or, when `etc` holds no poudriere.d (`poudriere` false),
/// that the build is a local one for that reason.
fn poudriere_etc_note(etc: &Path, poudriere: bool) -> String {
    if poudriere {
        format!(
            "poudriere.conf and poudriere.d read from {} (POUDRIERE_ETC)",
            etc.display()
        )
    } else {
        format!("POUDRIERE_ETC {} has no poudriere.d: a local build", etc.display())
    }
}

/// Run `cmd`, trimmed stdout on success, empty on any failure.
fn run_capture(cmd: &str, args: &[&str]) -> String {
    std::process::Command::new(cmd)
        .args(args)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// This host's architecture, as poudriere's `get_host_arch` writes it
/// (`uname -m`.`uname -p`, just the one when both agree).
fn host_arch() -> String {
    let (m, p) = (run_capture("uname", &["-m"]), run_capture("uname", &["-p"]));
    if m == p { p } else { format!("{m}.{p}") }
}

/// The archs this host's kernel can run natively, as poudriere's
/// `need_emulation` reads them (common.sh:3552-3573): `kern.supported_archs`,
/// falling back to `uname -m` where the sysctl does not answer, as on
/// DragonFly.
fn supported_archs() -> String {
    let s = run_capture("sysctl", &["-n", "kern.supported_archs"]);
    if s.is_empty() { run_capture("uname", &["-m"]) } else { s }
}

/// `OVERLAYS` from the environment, each made absolute against optique's
/// directory: make, run with -C in a port's directory, would otherwise
/// read a relative one from there.
fn env_overlays() -> Vec<PathBuf> {
    std::env::var("OVERLAYS")
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|p| std::path::absolute(p).ok())
        .collect()
}

/// PORTSDIR and PORT_DBDIR as make(1) sees them on this host, from the
/// environment or /etc/make.conf (sys.mk reads it), None where neither
/// sets one. make is asked rather than the environment read, so that a
/// make.conf setting counts as it does for `make config`; any failure
/// leaves the built-in defaults.
fn make_defaults() -> (Option<PathBuf>, Option<PathBuf>) {
    let Some(text) = make_values(&["PORTSDIR", "PORT_DBDIR"], None, &[]) else {
        return (None, None);
    };
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
    /// `OVERLAYS` from optique's environment, made absolute against its own
    /// directory: what make would read for a local build.
    env_overlays: &'a [PathBuf],
    /// The host's architecture as poudriere's `get_host_arch` writes it
    /// (`uname -m`.`uname -p`, just the one when both agree). Compared
    /// against a jail's own arch to decide the cross-build lines; asked
    /// only when -j names a jail.
    arch: &'a str,
    /// The archs this host's kernel can run natively (`kern.supported_archs`,
    /// poudriere's `need_emulation`): whether -j's jail is run under qemu,
    /// loading the `qemu-blacklist` layer. Asked only when -j names a jail,
    /// like `arch`, but unlike it never bears on the cross-build lines
    /// (`jail_system`'s own doc comment says why).
    supported_archs: &'a str,
}

/// A poudriere build when poudriere.d exists and -l was not given, a local
/// one otherwise.
fn resolve_host(host: &HostPaths, req: &Request, staging_dir: &Path) -> Result<Settings> {
    let local = req.local || !host.poudriere_d.is_dir();
    let given = given_overlays(host.poudriere_d, req.overlays, local)?;
    let settings = if local {
        resolve_local(host, req, &given, staging_dir)?
    } else {
        resolve_poudriere(host, req, &given, staging_dir)?
    };
    finish(settings, given, host.pkg_db)
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
    let reported = make_overlays(settings.make_conf.as_deref());
    settings.overlays = match reported {
        None => given,
        Some(paths) => paths
            .into_iter()
            .map(|path| {
                let path = path.canonicalize().unwrap_or(path);
                let found = given.iter().find(|o| o.path == path);
                if found.is_none() {
                    let what = if path.is_dir() {
                        "set by make.conf or the environment"
                    } else {
                        "does not exist"
                    };
                    settings.notes.push(format!("overlay {} {what}", path.display()));
                }
                Overlay { name: found.and_then(|o| o.name.clone()), path }
            })
            .collect(),
    };
    settings
}

/// `OVERLAYS` as make sees it under `make_conf` (make's own default when
/// None), the environment's left out: a local build's has been written
/// into its make.conf already. None when make cannot be asked.
fn make_overlays(make_conf: Option<&Path>) -> Option<Vec<PathBuf>> {
    let text = make_values(&["OVERLAYS"], make_conf, &["OVERLAYS"])?;
    Some(text.split_whitespace().map(PathBuf::from).collect())
}

/// What `make -V` prints for `vars`, one line each, outside any port: make
/// reads only sys.mk, the make.conf (`make_conf`, else make's own default)
/// and the environment, less `drop_env`. None when make cannot be asked or
/// fails.
fn make_values(vars: &[&str], make_conf: Option<&Path>, drop_env: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new("/usr/bin/make");
    cmd.current_dir("/")
        .args(["-f", "/dev/null"])
        .args(vars.iter().flat_map(|v| ["-V", v]))
        .env_remove("MAKEFLAGS")
        .env_remove("MAKELEVEL")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    for var in drop_env {
        cmd.env_remove(var);
    }
    if let Some(conf) = make_conf {
        cmd.arg(format!("__MAKE_CONF={}", conf.display()));
    }
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
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
    let mtime = crate::cache::mtime(&meta).as_nanos();
    let keyed = format!("{}\n{} {} {mtime}\n", settings.conf_hash, pkg_db.display(), meta.len());
    settings.conf_hash = crate::cache::sha256_hex(keyed.as_bytes());
    settings
}

/// A build on the host runs as the invoking user, whose `USER` its queries
/// see (`Identity::Invoker`), and bsd.ccache.mk answers root and anyone
/// else differently. `user`, optique's `USER` (None when unset), goes into
/// the hash for such a build: the cache lives under `HOME`, which a `sudo`
/// may keep while `USER` changes. A builder's queries run as root whoever
/// runs optique, and are left alone.
fn key_invoker(mut settings: Settings, user: Option<&str>) -> Settings {
    if settings.env.identity() != Identity::Invoker {
        return settings;
    }
    settings.conf_hash = invoker_hash(&settings.conf_hash, user);
    settings
}

/// `conf_hash` with the invoker's `user` folded in.
fn invoker_hash(conf_hash: &str, user: Option<&str>) -> String {
    crate::cache::sha256_hex(format!("{conf_hash}\nUSER {user:?}\n").as_bytes())
}

/// The finishing steps common to every backend once its own `Settings` are
/// built: paths absolute, `overlays` settled against what make reports, and
/// the host's installed packages, the invoking user and the framework
/// values keyed in. Shared by `resolve`'s synth path and `resolve_host`.
fn finish(settings: Settings, given: Vec<Overlay>, pkg_db: &Path) -> Result<Settings> {
    let settings = settings.absolute()?;
    let settings = with_overlays(settings, given);
    let settings = key_installed_packages(settings, pkg_db);
    let settings = key_invoker(settings, std::env::var("USER").ok().as_deref());
    Ok(with_options_place(with_framework(settings)))
}

/// Settle `settings.options_place` on what make.conf says outside any
/// port, where the queries' own command line cannot hide it: a build
/// given no `PORT_DBDIR` there reads whatever make.conf names. A probe
/// that fails is noted for the banner.
fn with_options_place(mut settings: Settings) -> Settings {
    let probed =
        crate::query::makerunner::options_place(settings.env.identity(), settings.make_conf.as_deref());
    let nonempty = |v: String| (!v.is_empty()).then_some(v);
    settings.options_place = probed.map(|(port_dbdir, options_file)| OptionsPlace {
        port_dbdir: nonempty(port_dbdir),
        options_file: nonempty(options_file),
    });
    if settings.options_place.is_none() {
        settings.notes.push(
            "make could not say whether make.conf sets PORT_DBDIR or OPTIONS_FILE".to_string(),
        );
    }
    settings
}

/// Settle `settings.framework` on what the probe port reports, and key it:
/// the host's release, arch and CPU count come in with it, and a host
/// upgraded under an unchanged make.conf is a new cache generation.
fn with_framework(mut settings: Settings) -> Settings {
    let ctx = crate::query::makerunner::QueryCtx {
        portsdir: settings.portsdir.clone(),
        make_conf: settings.make_conf.clone(),
        port_dbdir: PathBuf::from("/nonexistent"),
        overlays: settings.overlay_paths(),
        identity: settings.env.identity(),
        framework: Vec::new(),
    };
    settings.framework = crate::query::makerunner::framework_values(&ctx);
    settings.conf_hash = framework_hash(&settings.conf_hash, &settings.framework);
    settings
}

/// `conf_hash` with the framework values folded in; unchanged when there
/// are none.
fn framework_hash(conf_hash: &str, framework: &[(String, String)]) -> String {
    if framework.is_empty() {
        return conf_hash.to_string();
    }
    let lines: String = framework.iter().map(|(var, value)| format!("{var}={value}\n")).collect();
    crate::cache::sha256_hex(format!("{conf_hash}\nframework\n{lines}").as_bytes())
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
    let (staged, mut conf_hash, make_conf_sources) =
        concat_make_conf(&[host.make_conf.to_path_buf()], QueryEnv::Host, &[], "", false, staging_dir)?;
    // Queries run with a cleared environment, so make would fall back to
    // /etc/make.conf, unhashed, when the one it was given cannot be read:
    // /dev/null stands for "none", as an unreadable __MAKE_CONF is for make.
    let mut make_conf =
        Some(staged.map_or_else(|| PathBuf::from("/dev/null"), |_| host.make_conf.to_path_buf()));
    // --overlay-dir, then the environment's OVERLAYS (absolute), then the
    // make.conf's own: a small make.conf including the real one first,
    // still read in place for its own .includes, then putting them in
    // front of whatever it left, so a make.conf that assigns OVERLAYS=
    // rather than appending cannot drop them. The environment's go through
    // it, not through the queries' own environment, where bmake's += would
    // have put them first.
    let paths: Vec<String> = overlays
        .iter()
        .map(|o| o.path.display().to_string())
        .chain(host.env_overlays.iter().map(|p| p.display().to_string()))
        .collect();
    if !paths.is_empty() {
        let text = format!(
            ".sinclude \"{}\"\nOVERLAYS:={} ${{OVERLAYS}}\n",
            std::path::absolute(host.make_conf)?.display(),
            paths.join(" ")
        );
        let path = staging_dir.join("local-make.conf");
        fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;
        conf_hash = crate::cache::sha256_hex(format!("{conf_hash}\n{text}").as_bytes());
        make_conf = Some(path);
    }
    Ok(Settings {
        portsdir,
        options_dir,
        make_conf,
        make_conf_sources,
        conf_hash,
        env: QueryEnv::Host,
        blacklist: Blacklist::default(),
        notes,
        overlays: Vec::new(),
        flavor_default_all: false,
        framework: Vec::new(),
        options_place: None,
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

    let options_dir = match req.options_dir {
        Some(d) => d.to_path_buf(),
        None => resolve_options_dir(poudriere_d, jail, tree_name, tree.is_some(), set),
    };

    let off = no_package_building(host.poudriere_conf, poudriere_d, jail, tree, set);
    let env = QueryEnv::Jail { package_building: !off };
    let mut notes = Vec::new();
    let jail_lines = jail.map_or_else(String::new, |j| jail_system(poudriere_d, j, host, &mut notes));
    let ccache = ccache_dir(host.poudriere_conf, poudriere_d, jail, tree, set)
        .is_some_and(|dir| Path::new(&dir).is_dir());
    let (make_conf, conf_hash, make_conf_sources) = layer_make_conf(
        poudriere_d,
        jail,
        tree_name,
        set,
        env,
        overlays,
        &jail_lines,
        ccache,
        staging_dir,
    )?;

    let emulating = jail.is_some_and(|j| jail_emulating(poudriere_d, j, host.supported_archs));
    let blacklist = Blacklist::load_emulating(poudriere_d, jail, tree_name, set, emulating);

    let default_all = flavor_default_all(host.poudriere_conf, poudriere_d, jail, tree, set);
    if default_all {
        notes.push("FLAVOR_DEFAULT_ALL=yes: a bare origin listed stands for every flavor".to_string());
    }

    Ok(Settings {
        portsdir,
        options_dir,
        make_conf,
        make_conf_sources,
        conf_hash,
        env,
        blacklist,
        notes,
        overlays: Vec::new(),
        flavor_default_all: default_all,
        framework: Vec::new(),
        options_place: None,
    })
}

/// The make.conf lines that give queries the system of jail `name` rather
/// than the host's, poudriere reading ports inside the jail: `MACHINE`,
/// `MACHINE_ARCH` and `ARCH` whenever the jail's MACHINE_ARCH differs from
/// the host's (`setup_makeconf` in common.sh writes the same lines, but
/// only where its `need_cross_build` says a cross build runs; optique
/// writes them whenever the arch differs, since it is always the host's own
/// `make` that runs the query, never the jail's — see [`machine_arch`]),
/// then `_OSRELEASE`, `OSREL` and `OSVERSION`, which bsd.port.mk takes from
/// `uname -r` and /usr/include/sys/param.h, and poudriere's jail gives as
/// its own (`UNAME_r` from the jail's version, its param.h; ports_env.sh).
/// OSVERSION is derived from the version when the jail's param.h cannot be
/// read. A jail whose arch or version poudriere.d does not hold changes
/// nothing: `notes` says why, for the banner.
fn jail_system(poudriere_d: &Path, name: &str, host: &HostPaths, notes: &mut Vec<String>) -> String {
    let dir = poudriere_d.join("jails").join(name);
    let read = |file: &str| fs::read_to_string(dir.join(file)).ok().map(|s| s.trim().to_string());
    let (Some(arch), Some(version)) = (read("arch"), read("version")) else {
        notes.push(format!(
            "jail {name}: no arch or version in {}; queries see this host's system",
            dir.display()
        ));
        return String::new();
    };
    // `${version% *}`: a version may carry its osversion after a space.
    let release = version.rsplit_once(' ').map_or(version.as_str(), |(r, _)| r);
    // bsd.port.mk's `${_OSRELEASE:C/-.*//}`.
    let osrel = release.split_once('-').map_or(release, |(r, _)| r);
    let derived = || {
        let (major, minor) = osrel.split_once('.')?;
        Some(major.parse::<u32>().ok()? * 100_000 + minor.parse::<u32>().ok()? * 1000)
    };
    let param_h = read("mnt").map(|mnt| Path::new(&mnt).join("usr/include/sys/param.h"));
    let osversion = match param_h.as_deref().and_then(freebsd_version) {
        Some(v) => v,
        None => {
            let Some(v) = derived() else {
                notes.push(format!(
                    "jail {name}: version '{version}' is no release; queries see this host's system"
                ));
                return String::new();
            };
            let from = param_h.map_or_else(|| "no mnt".to_string(), |p| format!("no {}", p.display()));
            notes.push(format!("jail {name}: OSVERSION {v} derived from its version {version} ({from})"));
            v.to_string()
        }
    };
    let mut out = String::new();
    // `${arch%.*}`, `${arch#*.}`: amd64 is amd64 twice, arm64.aarch64 two.
    let machine = arch.rsplit_once('.').map_or(arch.as_str(), |(m, _)| m);
    let jail_machine_arch = machine_arch(&arch);
    if jail_machine_arch != machine_arch(host.arch) {
        out.push_str(&format!(
            "MACHINE={machine}\nMACHINE_ARCH={jail_machine_arch}\nARCH=${{MACHINE_ARCH}}\n"
        ));
    }
    out.push_str(&format!("_OSRELEASE={release}\nOSREL={osrel}\nOSVERSION={osversion}\n"));
    out
}

/// `__FreeBSD_version` as a param.h defines it, None when unreadable.
fn freebsd_version(param_h: &Path) -> Option<String> {
    let text = fs::read(param_h).ok()?;
    String::from_utf8_lossy(&text).lines().find_map(|l| {
        let mut words = l.split_whitespace();
        (words.next()? == "#define" && words.next()? == "__FreeBSD_version")
            .then(|| words.next().map(str::to_string))?
    })
}

/// The MACHINE_ARCH half of a poudriere `arch` string (`<machine>.<machine_arch>`,
/// or the whole string when MACHINE and MACHINE_ARCH are the same, as for
/// amd64). Unlike poudriere's `need_cross_build`/`need_emulation`, nothing
/// here asks what the host's kernel can run natively (`kern.supported_archs`):
/// poudriere skips the cross-build lines for e.g. an armv7 jail on an arm64
/// host because it then runs the jail's own 32-bit `make`, but optique
/// always runs the host's `make`, so a query needs MACHINE_ARCH set to the
/// jail's whenever it differs from the host's, cross-buildable or not.
fn machine_arch(arch: &str) -> &str {
    arch.split_once('.').map_or(arch, |(_, a)| a)
}

/// Does poudriere run a jail whose poudriere `arch` string is `arch` under
/// qemu on a host whose kernel natively runs `supported_archs`
/// (`need_emulation` in common.sh:3552-3573)? Only the MACHINE_ARCH half is
/// asked, as there (`${wanted_arch#*.}`); an armv6 jail counts as native
/// when the kernel lists armv7 too (it runs armv6 code), matching
/// `need_emulation`'s own `grep -qw "armv[67]"`.
fn need_emulation(arch: &str, supported_archs: &str) -> bool {
    let target = machine_arch(arch);
    !supported_archs
        .split_whitespace()
        .any(|a| a == target || (target == "armv6" && a == "armv7"))
}

/// Whether poudriere would run jail `name` under qemu
/// (`check_emulation`/`need_emulation`, common.sh:3531-3573, called from
/// `jail_start` at common.sh:3992, well before `load_blacklist` reads
/// `QEMU_EMULATING` at common.sh:4264): read the same `arch` file
/// `jail_system` does; a jail poudriere.d holds no arch for is never
/// emulated here either.
fn jail_emulating(poudriere_d: &Path, name: &str, supported_archs: &str) -> bool {
    let arch = fs::read_to_string(poudriere_d.join("jails").join(name).join("arch"))
        .ok()
        .map(|s| s.trim().to_string());
    arch.is_some_and(|arch| need_emulation(&arch, supported_archs))
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
/// poudriere there is nothing to probe for. `host` holds what synth reads
/// from the host besides synth.ini.
fn resolve_synth(
    synth_dir: &Path,
    profile: &str,
    host: &SynthHost,
    portsdir_flag: Option<&Path>,
    options_dir_flag: Option<&Path>,
    staging_dir: &Path,
) -> Result<Settings> {
    let ini = read_synth_ini(&synth_dir.join("synth.ini"))?;

    let mut notes = Vec::new();
    // A bare -s (empty profile) follows synth's own selection.
    let selected;
    let profile = if profile.is_empty() {
        let (name, source) = synth_selected_profile(&ini, host.env_profile);
        notes.push(format!("synth profile {name} ({source})"));
        selected = name;
        selected.as_str()
    } else {
        profile
    };
    let setting = |key: &str| synth_ini_lookup(&ini, profile, key);
    notes.extend(synth_ini_misreads(&ini));
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
            let Some((dir, source)) = synth_default_portsdir(host) else {
                bail!(
                    "synth profile '{profile}' sets no Directory_portsdir, and none of \
                     PORTSDIR, make's PORTSDIR, {} exists",
                    host.standard_portsdirs.join(", ")
                );
            };
            if !is_ports_tree(&dir) {
                bail!(
                    "synth profile '{profile}' sets no Directory_portsdir, and {} \
                     ({source}) is not a ports tree",
                    dir.display()
                );
            }
            notes.push(format!("no Directory_portsdir: ports tree {} ({source})", dir.display()));
            dir
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

    // synth reads ports in a chroot with nothing installed, under its own
    // header and the profile's make.conf alone: never the host's
    // /etc/make.conf, whose absence the always non-empty staged file keeps.
    let ccache = synth_ccache(setting("Directory_ccache").as_deref());
    let (make_conf, conf_hash, make_conf_sources) = stage_make_conf(
        synth_prelude(profile, ccache),
        &[synth_dir.join(format!("{profile}-make.conf"))],
        SYNTH_SCAN_TRAILER,
        staging_dir,
    )?;

    Ok(Settings {
        portsdir,
        options_dir,
        make_conf,
        make_conf_sources,
        conf_hash,
        env: QueryEnv::Synth,
        blacklist: Blacklist::default(),
        notes,
        overlays: Vec::new(),
        flavor_default_all: false,
        framework: Vec::new(),
        options_place: None,
    })
}

/// What synth reads from the host besides synth.ini.
struct SynthHost<'a> {
    /// The environment's `SYNTHPROFILE`, which a bare -s heeds.
    env_profile: Option<&'a str>,
    /// The environment's `PORTSDIR`, when non-empty.
    env_portsdir: Option<&'a Path>,
    /// `PORTSDIR` as the host's make answers it outside any tree (the
    /// environment's, else /etc/make.conf's), when it gives one; asked
    /// only when the profile names no tree.
    make_portsdir: &'a dyn Fn() -> Option<PathBuf>,
    /// The trees synth tries last, in order: /usr/dports, /usr/ports.
    standard_portsdirs: &'a [&'a str],
}

/// synth.ini as text: missing is empty, as for synth, which then writes a
/// fresh one; any other read error stops the run, as synth stops on it
/// ("Configuration failed to load"). Bytes that are not UTF-8 are decoded
/// lossily, as a make.conf is read, rather than losing the whole file.
fn read_synth_ini(path: &Path) -> Result<String> {
    match fs::read(path) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e)
            .with_context(|| format!("reading {} (synth will not run without it)", path.display())),
    }
}

/// The ports tree synth takes for a profile naming none, and what gave it
/// (parameters.adb `determine_portsdirs`): the first that exists of the
/// environment's `PORTSDIR`, make's `PORTSDIR` (synth asks `make -f
/// /usr/share/mk/bsd.port.mk -V PORTSDIR`, which reads the environment
/// and /etc/make.conf), /usr/dports, /usr/ports. synth's query includes the
/// tree's own bsd.port.mk, and fails, stopping synth with a "blank
/// PORTSDIR", whenever the tree it names is missing; optique asks make
/// outside any tree and goes on to the next candidate instead.
fn synth_default_portsdir(host: &SynthHost) -> Option<(PathBuf, &'static str)> {
    if let Some(dir) = host.env_portsdir.filter(|d| d.exists()) {
        return Some((dir.to_path_buf(), "from PORTSDIR"));
    }
    if let Some(dir) = (host.make_portsdir)().filter(|d| d.exists()) {
        return Some((dir, "from make's PORTSDIR"));
    }
    host.standard_portsdirs
        .iter()
        .map(Path::new)
        .find(|d| d.exists())
        .map(|d| (d.to_path_buf(), "synth's default"))
}

/// The profile synth itself runs with, and what chose it
/// (parameters.adb:64-71, `load_configuration`): `env_profile`, the
/// environment's `SYNTHPROFILE`, when synth.ini has a section for it with
/// `Directory_packages` set, else `profile_selected`, else `LiveSystem`.
fn synth_selected_profile(ini: &str, env_profile: Option<&str>) -> (String, &'static str) {
    if let Some(env) = env_profile.filter(|p| !p.is_empty()) {
        if synth_ini_lookup(ini, env, "Directory_packages").is_some() {
            return (env.to_string(), "SYNTHPROFILE");
        }
    }
    match synth_ini_lookup(ini, SYNTH_GLOBAL_SECTION, "profile_selected") {
        Some(p) => (p, "synth.ini profile_selected"),
        None => ("LiveSystem".to_string(), "synth's default"),
    }
}

/// The part of a synth.ini data line before the first `;` or `#`: both start
/// a comment that runs to the end of the line (config.adb's `Get_Value`,
/// its `Comment_Ind := Index (..., Set => To_Set ("#;"))`, read only for a
/// line that is not itself a section header).
fn ini_strip_comment(line: &str) -> &str {
    match line.find([';', '#']) {
        Some(i) => &line[..i],
        None => line,
    }
}

/// synth.ini's lines as synth reads them (config.adb's `Get_Value`): split
/// at LF alone, a CR left in place, and a line of fewer than two characters
/// skipped.
fn ini_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split('\n').enumerate().map(|(n, l)| (n + 1, l)).filter(|(_, l)| l.len() > 1)
}

/// A synth.ini section header as synth reads it: a `[` in column 1, the
/// name running untrimmed to the first `]`, anything after it ignored. An
/// indented `[name]` is no header, and `[ C ]` names the section ` C `.
/// These follow synth's INI reader rather than what the line seems to say
/// (an upstream defect that decides which settings synth uses, so optique
/// reads the file as synth does, and `synth_ini_misreads` names each such
/// line). A header with no `]` is no header: it makes synth fail to load
/// its configuration, which is the user's file to fix, not optique's to
/// report.
fn ini_section_header(line: &str) -> Option<&str> {
    let rest = line.strip_prefix('[')?;
    rest.find(']').map(|end| &rest[..end])
}

/// Trim a key or value as config.adb does: spaces alone (`Index_Non_Blank`),
/// a tab or a CRLF file's CR staying part of it, as for synth.
fn ini_trim(s: &str) -> &str {
    s.trim_matches(' ')
}

/// The synth.ini lines synth reads otherwise than they seem to say, each as
/// a note: a tab around a key or value is part of it, the spaces of
/// `[ name ]` are part of the name, an indented `[name]` is no header but a
/// data line of the section above, and a CRLF file's CR stays in each key
/// or value. optique reads them as synth does (`ini_section_header`,
/// `ini_trim`), so a profile, key or path may not be the one meant; the note
/// says why, for the user to fix the file.
///
/// A line with no data once its comment is cut (blank, tab-only, a
/// tab-indented comment) holds nothing synth could misread; a header
/// ignores its CR, which comes after the `]`.
fn synth_ini_misreads(text: &str) -> Vec<String> {
    const WHY: &str = "read as synth reads it (a synth defect)";
    let mut notes = Vec::new();
    let mut crlf = Vec::new();
    for (n, raw) in ini_lines(text) {
        if let Some(name) = ini_section_header(raw) {
            if name.trim() != name {
                notes.push(format!(
                    "synth.ini line {n}: synth keeps the spaces of the section name '{name}'; {WHY}"
                ));
            }
            continue;
        }
        if raw.trim_start().starts_with('[') && raw.contains(']') {
            notes.push(format!(
                "synth.ini line {n}: synth takes no indented [name] for a section header, \
                 the lines after it staying in the section above; {WHY}"
            ));
            continue;
        }
        if raw.starts_with([';', '#']) {
            continue;
        }
        let data = ini_strip_comment(raw);
        if data.trim().is_empty() {
            continue;
        }
        // A comment cut drops the CR with it.
        if data.ends_with('\r') {
            crlf.push(n);
        }
        let (k, v) = data.split_once('=').unwrap_or((data, ""));
        let tabbed = |s: &str| s.trim_matches([' ', '\r']) != s.trim();
        if tabbed(k) || tabbed(v) {
            notes.push(format!("synth.ini line {n}: synth keeps a tab around the key or value; {WHY}"));
        }
    }
    if !crlf.is_empty() {
        let (s, list) = (if crlf.len() == 1 { "" } else { "s" }, line_ranges(&crlf));
        notes.push(format!(
            "synth.ini line{s} {list}: synth keeps the CR of a CRLF line end in the key or value; {WHY}"
        ));
    }
    notes
}

/// Ascending line numbers as runs: `1-3, 5`.
fn line_ranges(lines: &[usize]) -> String {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for &n in lines {
        match runs.last_mut() {
            Some((_, end)) if *end + 1 == n => *end = n,
            _ => runs.push((n, n)),
        }
    }
    runs.iter()
        .map(|&(a, b)| if a == b { a.to_string() } else { format!("{a}-{b}") })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Does synth.ini contain a `[name]` section header?
fn synth_ini_has_section(text: &str, name: &str) -> bool {
    ini_lines(text).any(|(_, line)| ini_section_header(line) == Some(name))
}

/// Minimal INI reader for synth.ini: the value of `key` in section
/// `[profile]` alone, as synth reads a profile's settings
/// (parameters.adb:200-271, `load_specific_profile`, over config.adb's
/// `Get_Value`): no other section, `[Global Configuration]` included,
/// stands in for it. Keys are case-sensitive, `;` and `#` start a comment,
/// and spaces alone around the key and the value are trimmed (`ini_trim`).
/// A key's first match within the section wins — `Get_Value` exits its scan
/// of the whole file the moment one is found, matched or not, so even a
/// bare key with no `=`, or one given an empty value, ends the search
/// there, giving the default rather than reading on to a later, real
/// assignment of the same key.
fn synth_ini_lookup(text: &str, profile: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for (_, line) in ini_lines(text) {
        if line.starts_with('[') {
            // A header with no `]` is skipped, the section going on.
            if let Some(name) = ini_section_header(line) {
                in_section = name == profile;
            }
            continue;
        }
        if !in_section || line.starts_with([';', '#']) {
            continue;
        }
        let data = ini_strip_comment(line);
        let Some((k, v)) = data.split_once('=') else {
            // No `=` at all: config.adb still compares the whole
            // (trimmed) line against `key`, as a bare mark with no value.
            return if ini_trim(data) == key { None } else { continue };
        };
        if ini_trim(k) != key {
            continue;
        }
        let v = ini_trim(v);
        return if v.is_empty() { None } else { Some(v.to_string()) };
    }
    None
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
) -> PathBuf {
    for name in options_dir_candidates(jail, tree, set) {
        let dir = poudriere_d.join(&name);
        if dir.is_dir() {
            return dir;
        }
    }
    let parts = [jail, tree_explicit.then_some(tree), set, Some("options")];
    poudriere_d.join(parts.into_iter().flatten().collect::<Vec<_>>().join("-"))
}

/// make.conf layer names in poudriere(8) inclusion order (least to most
/// specific); every existing file is concatenated:
///
///   make.conf, <set>-make.conf, <tree>-make.conf, <jail>-make.conf,
///   <tree>-<set>-make.conf, <jail>-<tree>-make.conf, <jail>-<set>-make.conf,
///   <jail>-<tree>-<set>-make.conf
fn make_conf_layers(jail: Option<&str>, tree: &str, set: Option<&str>) -> Vec<String> {
    let mut v = vec!["make.conf".to_string()];
    v.extend(layer_stems(jail, Some(tree), set).into_iter().map(|s| format!("{s}-make.conf")));
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

/// Does poudriere.conf set `FLAVOR_DEFAULT_ALL=yes`, which makes poudriere
/// build every flavor of a bare origin listed (common.sh ~7037, ~8573,
/// ~11233)? Read from the same files, in the same order and the same way,
/// as [`no_package_building`]; only the exact value `yes` counts.
fn flavor_default_all(
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
    let mut value = String::new();
    for file in files {
        let Ok(text) = fs::read_to_string(&file) else { continue };
        for line in text.lines() {
            if let Some(v) = shell_assignment(line, "FLAVOR_DEFAULT_ALL") {
                value = v.to_string();
            }
        }
    }
    value == "yes"
}

/// poudriere.conf's `CCACHE_DIR`, read the same files in the same order as
/// [`no_package_building`] (plain assignments only; the last one read wins,
/// an empty value unsets): what `setup_ccache` (common.sh:3591-3600) checks
/// with `[ -d "${CCACHE_DIR:-/nonexistent}" ]` before appending its two
/// lines to the jail's make.conf.
fn ccache_dir(
    poudriere_conf: &Path,
    poudriere_d: &Path,
    jail: Option<&str>,
    tree: Option<&str>,
    set: Option<&str>,
) -> Option<String> {
    let mut files = vec![poudriere_conf.to_path_buf(), poudriere_d.join("poudriere.conf")];
    files.extend(
        poudriere_conf_layers(jail, tree, set)
            .into_iter()
            .map(|name| poudriere_d.join(format!("{name}-poudriere.conf"))),
    );
    let mut value: Option<String> = None;
    for file in files {
        let Ok(text) = fs::read_to_string(&file) else { continue };
        for line in text.lines() {
            if let Some(v) = shell_assignment(line, "CCACHE_DIR") {
                value = (!v.is_empty()).then(|| v.to_string());
            }
        }
    }
    value
}

/// The per-name poudriere.conf layers, in poudriere(8)'s reading order:
/// `<set>`, `<tree>`, `<jail>`, `<tree>-<set>`, `<jail>-<tree>`,
/// `<jail>-<set>`, `<jail>-<tree>-<set>`, each only when its names are given.
fn poudriere_conf_layers(jail: Option<&str>, tree: Option<&str>, set: Option<&str>) -> Vec<String> {
    layer_stems(jail, tree, set)
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

#[allow(clippy::too_many_arguments)]
fn layer_make_conf(
    poudriere_d: &Path,
    jail: Option<&str>,
    tree: &str,
    set: Option<&str>,
    env: QueryEnv,
    overlays: &[Overlay],
    jail_lines: &str,
    ccache: bool,
    staging_dir: &Path,
) -> Result<(Option<PathBuf>, String, Vec<PathBuf>)> {
    let mut paths: Vec<PathBuf> =
        make_conf_layers(jail, tree, set).iter().map(|n| poudriere_d.join(n)).collect();
    paths.extend(plugin_make_confs(poudriere_d));
    concat_make_conf(&paths, env, overlays, jail_lines, ccache, staging_dir)
}

/// The hook plugins' make.conf files poudriere appends after the layers
/// (`setup_makeconf` in common.sh): `hooks/plugins/*/make.conf`, in the
/// shell glob's order (names sorted, dot entries left out); only a regular
/// file counts, as for its `[ -f ]`.
fn plugin_make_confs(poudriere_d: &Path) -> Vec<PathBuf> {
    let Ok(dir) = fs::read_dir(poudriere_d.join("hooks/plugins")) else { return Vec::new() };
    let mut names: Vec<std::ffi::OsString> = dir
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .filter(|n| !n.as_encoded_bytes().starts_with(b"."))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|n| poudriere_d.join("hooks/plugins").join(n).join("make.conf"))
        .filter(|p| p.is_file())
        .collect()
}

/// Stage a make.conf holding the jail environment alone, no layers.
#[cfg(test)]
pub(crate) fn stage_jail_make_conf(staging_dir: &Path) -> Result<PathBuf> {
    let (path, _, _) =
        concat_make_conf(&[], QueryEnv::Jail { package_building: true }, &[], "", false, staging_dir)?;
    Ok(path.expect("the jail environment is never empty"))
}

/// Stage synth's scan make.conf for `profile` over `profile_make_conf`.
#[cfg(test)]
pub(crate) fn stage_synth_make_conf(
    staging_dir: &Path,
    profile: &str,
    profile_make_conf: &Path,
) -> Result<PathBuf> {
    let (path, _, _) = stage_make_conf(
        synth_prelude(profile, false),
        &[profile_make_conf.to_path_buf()],
        SYNTH_SCAN_TRAILER,
        staging_dir,
    )?;
    Ok(path.expect("the synth environment is never empty"))
}

/// Concatenate every existing candidate, in order, into one staged
/// `make.conf`, between the jail prelude and trailer in a jail's `env`, the
/// overlays' `OVERLAYS+=` lines where poudriere writes them in the jail's
/// make.conf: after the prelude, before the layers (host paths, as the
/// jail's `/overlays/<name>` does not exist here), and the jail's system
/// (`jail_system`) last, before the layers, which may still override it as
/// they can in the jail. `ccache` adds `POUDRIERE_JAIL_CCACHE` after the
/// layers too, as poudriere's own `setup_ccache` runs after `setup_makeconf`
/// (common.sh:4121, 4195) — ahead of the hidden `LOCALBASE`, which stays
/// last so no layer, and now no ccache line either, can undo it. The
/// hash covers the combined text (so an empty layering hashes too, and the
/// jail environment keys a cache generation of its own); nothing is written
/// when there is nothing to write.
fn concat_make_conf(
    candidates: &[PathBuf],
    env: QueryEnv,
    overlays: &[Overlay],
    jail_lines: &str,
    ccache: bool,
    staging_dir: &Path,
) -> Result<(Option<PathBuf>, String, Vec<PathBuf>)> {
    let mut prelude = String::new();
    let mut trailer = String::new();
    if let QueryEnv::Jail { package_building } = env {
        prelude.push_str(POUDRIERE_JAIL_PRELUDE);
        prelude.push_str(&overlay_lines(overlays));
        if package_building {
            prelude.push_str(PACKAGE_BUILDING_LINES);
        }
        prelude.push_str(jail_lines);
        if ccache {
            trailer.push_str(POUDRIERE_JAIL_CCACHE);
        }
        trailer.push_str(POUDRIERE_JAIL_TRAILER);
    }
    stage_make_conf(prelude, candidates, &trailer, staging_dir)
}

/// Stage `prelude`, every existing candidate in order, then `trailer`, as one
/// make.conf, returning it (None when there is nothing to write), the hash of
/// the combined text, and the candidates read. A candidate whose realpath was
/// already appended is skipped — poudriere(8)'s `append_make` does the same
/// (common.sh:10605-10611), so a jail's own `<jail>-make.conf` symlinked to a
/// layer already read (or vice versa) contributes once, not twice.
fn stage_make_conf(
    prelude: String,
    candidates: &[PathBuf],
    trailer: &str,
    staging_dir: &Path,
) -> Result<(Option<PathBuf>, String, Vec<PathBuf>)> {
    // Bytes, not text: make reads a make.conf whatever its encoding (a
    // Latin-1 comment is common), so only a missing file is none, and one
    // that cannot be read stops the run as it would stop make.
    let mut combined = prelude.into_bytes();
    let mut sources = Vec::new();
    let mut seen_real: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for path in candidates {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("reading make.conf {}", path.display())),
        };
        let real = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !seen_real.insert(real) {
            continue;
        }
        combined.extend_from_slice(format!("# --- {} ---\n", path.display()).as_bytes());
        combined.extend_from_slice(&bytes);
        if !bytes.ends_with(b"\n") {
            combined.push(b'\n');
        }
        sources.push(path.clone());
    }
    combined.extend_from_slice(trailer.as_bytes());

    let conf_hash = crate::cache::sha256_hex(&combined);
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
        let dir = resolve_options_dir(pd, None, "default", false, Some("s"));
        assert_eq!(dir, pd.join("s-options"));
        assert!(dir.is_dir());
        // jail-specific dirs missing: falls through to the set dir
        let dir = resolve_options_dir(pd, Some("j"), "default", false, Some("s"));
        assert_eq!(dir, pd.join("s-options"));
        // no flags: plain options
        let dir = resolve_options_dir(pd, None, "default", false, None);
        assert_eq!(dir, pd.join("options"));
    }

    #[test]
    fn options_dir_created_name_mirrors_poudriere_options() {
        let tmp = tempfile::tempdir().unwrap();
        let pd = tmp.path();
        let dir = resolve_options_dir(pd, Some("j"), "t", true, Some("s"));
        assert_eq!(dir, pd.join("j-t-s-options"));
        assert!(!dir.is_dir());
        // tree not explicit -> left out of the created name
        let dir = resolve_options_dir(pd, None, "default", false, Some("s"));
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
            env_overlays: &[],
            arch: "amd64",
            supported_archs: "amd64 i386",
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

        let (_, bare, _) = concat_make_conf(&[], QueryEnv::Host, &[], "", false, &stage).unwrap();
        let bare = invoker_hash(&bare, std::env::var("USER").ok().as_deref());
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

    /// synth sees no host package, so the pkg database keys nothing for it;
    /// its profile names a cache generation of its own.
    #[test]
    fn synth_generation_ignores_the_pkg_db() {
        let tmp = tempfile::tempdir().unwrap();
        let synth = tmp.path().join("synth");
        let tree = tmp.path().join("ports");
        fake_tree(&tree);
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(synth.join("synth.ini"), format!("[A]\nDirectory_portsdir={}\n", tree.display()))
            .unwrap();
        let resolve = |profile| resolve_synth(&synth, profile, &synth_host(None), None, None, &stage).unwrap();

        let pkg_db = tmp.path().join("local.sqlite");
        fs::write(&pkg_db, "one package").unwrap();
        let s = resolve("A");
        let before = s.conf_hash.clone();
        assert_eq!(key_installed_packages(s, &pkg_db).conf_hash, before);
        assert_ne!(resolve("B").conf_hash, before, "SYNTHPROFILE differs");
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
        assert!(!s.options_dir.is_dir(), "{} does not exist yet", db.display());
        assert!(s.env == QueryEnv::Host && s.make_conf.as_deref() == Some(Path::new("/dev/null")));
        assert!(s.notes.is_empty(), "built-in defaults earn no note: {:?}", s.notes);
        fs::create_dir_all(&db).unwrap();
        assert!(resolve_host(&host, &none, &stage).unwrap().options_dir.is_dir());
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
        assert!(!s.options_dir.is_dir(), "created on apply");
        let s = resolve_host(&host, &Request { options_dir: Some(&db), ..none }, &stage).unwrap();
        assert!(s.options_dir.is_dir());
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
        assert!(!s.options_dir.is_dir());
        assert_eq!(s.env, QueryEnv::Jail { package_building: true });
        let conf = fs::read_to_string(s.make_conf.unwrap()).unwrap();
        assert!(conf.contains("PACKAGE_BUILDING=yes"), "{conf}");

        // With poudriere.d present too, -o is still taken verbatim, never
        // one of the options-dir candidates.
        let s = resolve_host(&host, &Request { options_dir: Some(&mine), set: Some("s"), ..none }, &stage)
            .unwrap();
        assert_eq!(s.options_dir, mine);
        assert_ne!(s.options_dir, pd.join("s-options"));
    }

    /// A poudriere.d whose jail `j` has `arch` and `version`, its mnt holding
    /// a param.h with `osversion` when given.
    fn fake_jail(tmp: &Path, arch: &str, version: &str, osversion: Option<&str>) -> PathBuf {
        let pd = tmp.join("poudriere.d");
        let jd = pd.join("jails/j");
        let _ = fs::remove_dir_all(&pd);
        fs::create_dir_all(&jd).unwrap();
        fs::write(jd.join("arch"), format!("{arch}\n")).unwrap();
        fs::write(jd.join("version"), format!("{version}\n")).unwrap();
        let mnt = tmp.join("mnt");
        let _ = fs::remove_dir_all(&mnt);
        fs::write(jd.join("mnt"), format!("{}\n", mnt.display())).unwrap();
        if let Some(v) = osversion {
            fs::create_dir_all(mnt.join("usr/include/sys")).unwrap();
            let h = format!("#define __FreeBSD_version {v}\t/* Master, propagated to newvers */\n");
            fs::write(mnt.join("usr/include/sys/param.h"), h).unwrap();
        }
        pd
    }

    /// poudriere reads ports inside the jail: -j gives queries its release,
    /// its param.h's OSVERSION (derived from the release, and said so, when
    /// unreadable), and poudriere's cross-build lines when its arch is not
    /// the host's; a jail poudriere.d does not describe changes nothing.
    #[test]
    fn a_jail_gives_queries_its_arch_and_release() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let req = Request { jail: Some("j"), ..Request::default() };
        let staged = |pd: &Path| {
            let s = resolve_host(&host(tmp.path(), pd, &ports, tmp.path()), &req, &stage).unwrap();
            (fs::read_to_string(s.make_conf.unwrap()).unwrap(), s.notes, s.conf_hash)
        };

        let pd = fake_jail(tmp.path(), "amd64", "11.2-RELEASE", Some("1102000"));
        let (conf, notes, hash) = staged(&pd);
        let os = "_OSRELEASE=11.2-RELEASE\nOSREL=11.2\nOSVERSION=1102000\n";
        assert!(conf.contains(os) && !conf.contains("MACHINE"), "{conf}");
        assert!(conf.find(os).unwrap() < conf.find("LOCALBASE").unwrap(), "{conf}");
        assert!(notes.is_empty(), "{notes:?}");

        let pd = fake_jail(tmp.path(), "amd64", "14.3-RELEASE-p2 1403000", None);
        let (conf, notes, other) = staged(&pd);
        assert!(conf.contains("_OSRELEASE=14.3-RELEASE-p2\nOSREL=14.3\nOSVERSION=1403000\n"), "{conf}");
        assert!(notes.iter().any(|n| n.contains("OSVERSION 1403000 derived")), "{notes:?}");
        assert_ne!(hash, other, "the jail's system keys its own generation");

        for (arch, lines) in [
            ("i386", "MACHINE=i386\nMACHINE_ARCH=i386\nARCH=${MACHINE_ARCH}\n"),
            ("arm64.aarch64", "MACHINE=arm64\nMACHINE_ARCH=aarch64\nARCH=${MACHINE_ARCH}\n"),
        ] {
            let pd = fake_jail(tmp.path(), arch, "14.3-RELEASE", Some("1403000"));
            let (conf, _, _) = staged(&pd);
            assert!(conf.contains(&format!("{lines}_OSRELEASE=14.3-RELEASE\n")), "{arch}: {conf}");
        }

        let pd = fake_jail(tmp.path(), "amd64", "14.3-RELEASE", None);
        fs::remove_file(pd.join("jails/j/version")).unwrap();
        let (conf, notes, _) = staged(&pd);
        assert!(!conf.contains("OSREL"), "{conf}");
        assert!(notes.iter().any(|n| n.contains("this host's system")), "{notes:?}");
        fs::remove_dir_all(pd.join("jails")).unwrap();
        let (conf, notes, _) = staged(&pd);
        assert!(!conf.contains("OSREL") && notes.len() == 1, "{conf}{notes:?}");
    }

    /// An armv7 jail on an arm64 host still gets its cross-build lines: its
    /// MACHINE_ARCH (armv7) differs from the host's (aarch64), even though
    /// the host's kernel can run armv7 natively (poudriere's
    /// `need_cross_build`/`need_emulation` would skip them there, since it
    /// then runs the jail's own 32-bit `make`, never done here). An amd64
    /// jail on an amd64 host, same MACHINE_ARCH, still gets none.
    #[test]
    fn an_arm_jail_on_an_arm64_host_still_gets_cross_build_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let req = Request { jail: Some("j"), ..Request::default() };
        let host = HostPaths { arch: "arm64.aarch64", ..host(tmp.path(), tmp.path(), &ports, tmp.path()) };
        let staged = |host: &HostPaths, pd: &Path| {
            let s = resolve_host(&HostPaths { poudriere_d: pd, ..*host }, &req, &stage).unwrap();
            fs::read_to_string(s.make_conf.unwrap()).unwrap()
        };

        let pd = fake_jail(tmp.path(), "arm.armv7", "14.3-RELEASE", Some("1403000"));
        let conf = staged(&host, &pd);
        assert!(conf.contains("MACHINE=arm\nMACHINE_ARCH=armv7\nARCH=${MACHINE_ARCH}\n"), "{conf}");

        let pd = fake_jail(tmp.path(), "amd64", "14.3-RELEASE", Some("1403000"));
        let amd64_host = HostPaths { arch: "amd64", ..host };
        let conf = staged(&amd64_host, &pd);
        assert!(!conf.contains("MACHINE"), "{conf}");
    }

    /// poudriere appends its hook plugins' make.conf files after the
    /// layers, in glob order: so does the staged one, before the trailer,
    /// and the banner lists them with the layers.
    #[test]
    fn hook_plugin_make_confs_follow_the_layers() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let pd = tmp.path().join("poudriere.d");
        let plugins = pd.join("hooks/plugins");
        for (dir, text) in [("b", "B=1\n"), ("a", "A=1\n"), (".hidden", "H=1\n")] {
            fs::create_dir_all(plugins.join(dir)).unwrap();
            fs::write(plugins.join(dir).join("make.conf"), text).unwrap();
        }
        fs::create_dir_all(plugins.join("none")).unwrap(); // a plugin with no make.conf
        fs::write(plugins.join("loose"), "not a plugin\n").unwrap();
        fs::write(pd.join("make.conf"), "LAYER=1\n").unwrap();
        let s = resolve_host(&host(tmp.path(), &pd, &ports, tmp.path()), &Request::default(), &stage).unwrap();
        let conf = fs::read_to_string(s.make_conf.unwrap()).unwrap();
        let at = |s: &str| conf.find(s).unwrap_or_else(|| panic!("{s} missing: {conf}"));
        assert!(at("LAYER=1") < at("A=1") && at("A=1") < at("B=1") && at("B=1") < at("LOCALBASE"), "{conf}");
        assert!(!conf.contains("H=1"), "{conf}");
        let want = [pd.join("make.conf"), plugins.join("a/make.conf"), plugins.join("b/make.conf")];
        assert_eq!(s.make_conf_sources, want);
    }

    /// Under the staged make.conf of an 11.2 jail, security/ca_root_nss
    /// offers ETCSYMLINK, which it excludes on the 15 this host may run.
    /// Run with: cargo test -- --ignored
    #[test]
    #[ignore = "needs /usr/ports on a FreeBSD host"]
    fn live_a_jail_release_reaches_make() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let pd = fake_jail(tmp.path(), "amd64", "11.2-RELEASE", None);
        let req = Request { jail: Some("j"), ..Request::default() };
        let s = resolve_host(&host(tmp.path(), &pd, &ports, tmp.path()), &req, &stage).unwrap();
        let out = std::process::Command::new("/usr/bin/make")
            .args(["-C", "/usr/ports/security/ca_root_nss", "-V", "COMPLETE_OPTIONS_LIST", "-V", "OSVERSION"])
            .arg(format!("__MAKE_CONF={}", s.make_conf.unwrap().display()))
            .env_remove("MAKEFLAGS")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(text, "ETCSYMLINK\n1102000\n", "{}", String::from_utf8_lossy(&out.stderr));
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

    /// The overlay a port comes from: the first holding it, named when -O
    /// named it; none for a port only the main tree holds.
    #[test]
    fn overlay_of_names_the_first_holder() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        for (dir, port) in [(&a, "cat/x"), (&b, "cat/x"), (&b, "cat/y")] {
            fs::create_dir_all(dir.join(port)).unwrap();
            fs::write(dir.join(port).join("Makefile"), "").unwrap();
        }
        let overlays =
            [Overlay { name: Some("first".into()), path: a.clone() }, Overlay { name: None, path: b.clone() }];
        assert_eq!(overlay_of(&overlays, "cat/x").map(Overlay::label).as_deref(), Some("first"));
        assert_eq!(overlay_of(&overlays, "cat/y").map(Overlay::label), Some(b.display().to_string()));
        assert!(overlay_of(&overlays, "cat/z").is_none());
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
        assert!(s.notes.iter().any(|n| n.contains("set by make.conf")), "{:?}", s.notes);
        let plain = resolve_host(&host, &Request::default(), &stage).unwrap();
        assert_eq!(plain.make_conf.as_deref(), Some(conf.as_path()), "read in place without one");
        assert_ne!(s.conf_hash, plain.conf_hash);

        // A make.conf that assigns rather than appends keeps --overlay-dir.
        fs::write(etc.join("overlays.conf"), format!("OVERLAYS={}\n", theirs.display())).unwrap();
        let s = resolve_host(&host, &Request { overlays: &specs, ..Request::default() }, &stage).unwrap();
        let paths: Vec<_> = s.overlays.iter().map(|o| o.path.clone()).collect();
        assert_eq!(paths, [mine.canonicalize().unwrap(), theirs.canonicalize().unwrap()]);
    }

    /// A local build's overlays, in make's order: --overlay-dir, then the
    /// environment's (absolute), then make.conf's own; the environment's
    /// reach make as make.conf lines, not through the queries' environment.
    #[test]
    fn local_overlay_dirs_come_before_the_environments() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let (mine, env) = (tmp.path().join("mine"), tmp.path().join("env"));
        fs::create_dir_all(&mine).unwrap();
        fs::create_dir_all(&env).unwrap();
        let absent = tmp.path().join("no-poudriere.d");
        let envs = [env.clone()];
        let host = HostPaths { env_overlays: &envs, ..host(tmp.path(), &absent, &ports, tmp.path()) };
        let specs = [OverlaySpec::Dir(mine.clone())];
        let s = resolve_host(&host, &Request { overlays: &specs, ..Request::default() }, &stage).unwrap();
        let paths: Vec<_> = s.overlays.iter().map(|o| o.path.clone()).collect();
        assert_eq!(paths, [mine.canonicalize().unwrap(), env.canonicalize().unwrap()]);
        // Without --overlay-dir the environment's alone, through make.conf.
        let s = resolve_host(&host, &Request::default(), &stage).unwrap();
        let conf = fs::read_to_string(s.make_conf.unwrap()).unwrap();
        assert!(conf.contains(&env.display().to_string()), "{conf}");
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
        fs::write(pd.join("blacklist"), "www/nginx\n").unwrap();
        let host = host(tmp.path(), &pd, &ports, &db);

        let local = Request { local: true, ..Request::default() };
        let s = resolve_host(&host, &local, &stage).unwrap();
        assert_eq!((s.portsdir.as_path(), s.options_dir.as_path()), (ports.as_path(), db.as_path()));
        assert_eq!(s.env, QueryEnv::Host);
        assert_eq!(s.make_conf.as_deref(), Some(Path::new("/dev/null")), "none to read, not /etc/make.conf");
        assert!(s.make_conf_sources.is_empty());
        assert!(s.blacklist.is_empty());
        // The host's make.conf, not poudriere.d's, staged and keyed.
        let etc = tmp.path().join("etc-make.conf");
        fs::write(&etc, "DEFAULT_VERSIONS+=ssl=base\n").unwrap();
        let s = resolve_host(&HostPaths { make_conf: &etc, ..host }, &local, &stage).unwrap();
        assert_eq!(s.make_conf_sources, vec![etc.clone()]);
        // Read in place, for its relative .includes, and keyed all the same.
        assert_eq!(s.make_conf.as_ref(), Some(&etc));
        let (_, empty_hash, _) = concat_make_conf(&[], QueryEnv::Host, &[], "", false, &stage).unwrap();
        assert_ne!(s.conf_hash, empty_hash);
        // A make.conf make reads whatever its bytes, a Latin-1 comment
        // included, is read and keyed, not taken for none; one that exists
        // but cannot be read stops the run, as it would stop make.
        fs::write(&etc, b"# r\xe9glages\nDEFAULT_VERSIONS+=ssl=base\n").unwrap();
        let s = resolve_host(&HostPaths { make_conf: &etc, ..host }, &local, &stage).unwrap();
        assert_eq!((s.make_conf.as_ref(), s.make_conf_sources.as_slice()), (Some(&etc), [etc.clone()].as_slice()));
        if !nix_is_root() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&etc, fs::Permissions::from_mode(0o000)).unwrap();
            let err = resolve_host(&HostPaths { make_conf: &etc, ..host }, &local, &stage).unwrap_err();
            assert!(format!("{err:#}").contains("etc-make.conf"), "{err:#}");
            fs::set_permissions(&etc, fs::Permissions::from_mode(0o644)).unwrap();
        }
        // The same host without -l is a poudriere build.
        let s = resolve_host(&host, &Request::default(), &stage).unwrap();
        assert_eq!(s.options_dir, pd.join("options"));
        assert!(!s.blacklist.is_empty());
    }

    /// Root reads a mode-000 file: the unreadable case only holds for others.
    fn nix_is_root() -> bool {
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .is_ok_and(|o| o.stdout.starts_with(b"0\n"))
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

    /// poudriere's old `<mnt>/ports` tree layout (the real tree one level
    /// under `mnt`, in a `ports/` subdirectory) is not modelled: `mnt`
    /// names the tree directly these days, so a tree whose `mnt` still
    /// points at the old layout's parent directory is simply not a ports
    /// tree, and resolving it fails loudly with the usual message
    /// (docs/conformance.md).
    #[test]
    fn poudriere_tree_old_mnt_ports_layout_fails_loudly() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let db = tmp.path().join("db");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(pd.join("ports/main")).unwrap();
        let old_mnt = tmp.path().join("old-mnt");
        fake_tree(&old_mnt.join("ports"));
        fs::write(pd.join("ports/main/mnt"), format!("{}\n", old_mnt.display())).unwrap();
        let host = host(tmp.path(), &pd, &ports, &db);

        let err = resolve_host(&host, &Request { tree: Some("main"), ..Request::default() }, &stage)
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            format!("poudriere tree 'main' points to {} which is not a ports tree", old_mnt.display())
        );
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

    /// FLAVOR_DEFAULT_ALL is read from the files NO_PACKAGE_BUILDING is,
    /// last assignment winning, and only `yes` turns it on.
    #[test]
    fn flavor_default_all_reads_every_layer_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let conf = tmp.path().join("poudriere.conf");
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        let fda = |jail, tree, set| flavor_default_all(&conf, &pd, jail, tree, set);
        assert!(!fda(None, None, None), "no file at all");
        fs::write(&conf, "#FLAVOR_DEFAULT_ALL=yes\n").unwrap();
        assert!(!fda(None, None, None));
        fs::write(&conf, "FLAVOR_DEFAULT_ALL=yes # every flavor\n").unwrap();
        assert!(fda(None, None, None));
        fs::write(&conf, "FLAVOR_DEFAULT_ALL=\"yes\"\n").unwrap();
        assert!(fda(None, None, None));
        fs::write(pd.join("poudriere.conf"), "FLAVOR_DEFAULT_ALL=no\n").unwrap();
        assert!(!fda(None, None, None), "poudriere.d's turns it off");
        fs::write(pd.join("j-poudriere.conf"), "export FLAVOR_DEFAULT_ALL=yes\n").unwrap();
        assert!(fda(Some("j"), None, None));
        assert!(!fda(Some("other"), None, None));
        fs::write(pd.join("j-poudriere.conf"), "FLAVOR_DEFAULT_ALL=YES\n").unwrap();
        assert!(!fda(Some("j"), None, None), "poudriere matches yes exactly");
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
            layer_make_conf(&pd, Some("j1"), "t1", Some("s1"), QueryEnv::Host, &[], "", false, &stage).unwrap();
        assert_eq!(sources.len(), 8);
        let text = fs::read_to_string(path.unwrap()).unwrap();
        let idx = |needle: &str| text.find(needle).unwrap();
        let order: Vec<usize> = layers.iter().map(|(_, c)| idx(c)).collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "layers out of order: {text}");
        assert!(!text.contains("PACKAGE_BUILDING") && !text.contains("LOCALBASE"), "{text}");
    }

    /// A jail's own `<jail>-make.conf` symlinked to a layer already read
    /// (a common poudriere.d setup when a jail just wants the tree's own
    /// file) is read once, not twice — poudriere's `append_make` dedups by
    /// realpath, and so must optique.
    #[test]
    fn a_layer_symlinked_to_another_is_read_once() {
        let tmp = tempfile::tempdir().unwrap();
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        fs::write(pd.join("make.conf"), "OVERLAYS+=/x\n").unwrap();
        std::os::unix::fs::symlink(pd.join("make.conf"), pd.join("j1-make.conf")).unwrap();
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let (path, _, sources) =
            layer_make_conf(&pd, Some("j1"), "t1", None, QueryEnv::Host, &[], "", false, &stage).unwrap();
        assert_eq!(sources, vec![pd.join("make.conf")], "{sources:?}");
        let text = fs::read_to_string(path.unwrap()).unwrap();
        assert_eq!(text.matches("OVERLAYS+=/x").count(), 1, "{text}");
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
        let (path, _, _) = layer_make_conf(&pd, None, "t1", Some("s1"), QueryEnv::Host, &[], "", false, &stage).unwrap();
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
            layer_make_conf(&pd, None, "t1", None, QueryEnv::Jail { package_building: true }, &[], "", false, &stage).unwrap();
        let text = fs::read_to_string(path.unwrap()).unwrap();
        // poudriere's own lines come before the layers, as in a jail...
        assert!(text.find("PACKAGE_BUILDING=yes").unwrap() < text.find("/opt/local").unwrap());
        // ...and the hidden LOCALBASE after them, so no layer can undo it.
        assert!(text.trim_end().ends_with("LOCALBASE=/nonexistent"), "{text}");

        // The environment keys its own cache generation.
        let (_, host_hash, _) = layer_make_conf(&pd, None, "t1", None, QueryEnv::Host, &[], "", false, &stage).unwrap();
        assert_ne!(jail_hash, host_hash);

        // An empty layering still stages a make.conf in a jail's environment.
        let empty = tmp.path().join("empty.d");
        fs::create_dir_all(&empty).unwrap();
        let (path, _, sources) = layer_make_conf(&empty, None, "t1", None, QueryEnv::Jail { package_building: true }, &[], "", false, &stage).unwrap();
        assert!(sources.is_empty());
        assert!(path.is_some());
        let (path, _, _) = layer_make_conf(&empty, None, "t1", None, QueryEnv::Host, &[], "", false, &stage).unwrap();
        assert!(path.is_none());
    }

    #[test]
    fn synth_ini_lookup_reads_the_profile_section_alone() {
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
        // Key absent from the profile: unset, as synth reads it — the
        // global section never stands in for a profile.
        assert_eq!(synth_ini_lookup(ini, "LiveSystem", "Directory_options"), None);
        // Sections are not confused with each other.
        assert_eq!(
            synth_ini_lookup(ini, "Other", "Directory_portsdir").as_deref(),
            Some("/other/ports")
        );
        // Nor for an unknown profile; the global section is read by name.
        assert_eq!(synth_ini_lookup(ini, "Nope", "Directory_portsdir"), None);
        assert_eq!(
            synth_ini_lookup(ini, SYNTH_GLOBAL_SECTION, "Directory_portsdir").as_deref(),
            Some("/global/ports")
        );
        // An unknown key is None, and keys are case-sensitive.
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

    /// config.adb's own syntax (`Get_Value`), each claim checked on its own,
    /// optique reading it as synth does and naming each line that does not
    /// say what it seems to: a section header's `[` must open the line
    /// (column 1), its name runs untrimmed to the first `]`, anything after
    /// it ignored; a key and its value are trimmed of spaces only, a tab left
    /// in place; and a key's first match within its section ends the whole
    /// search there — `exit Read_File` — even a bare key with no `=` or one
    /// given an empty value, so a later, real assignment is never reached.
    #[test]
    fn synth_ini_reads_as_synth_and_names_what_it_misreads() {
        assert_eq!(ini_section_header("[B] extra"), Some("B"), "anything after ] is ignored");
        assert_eq!(ini_section_header("[ C ]"), Some(" C "), "the name is not trimmed");
        assert_eq!(ini_section_header("   [P]"), None, "an indented header is none");

        let ini = "[A]\nk\t= d\n[ C ]\nDirectory_portsdir=/c\n[P]\n  [Q]\nkey=2\n";
        assert_eq!(synth_ini_lookup(ini, "A", "k"), None, "the key is `k\\t`");
        assert_eq!(synth_ini_lookup(ini, "A", "k\t").as_deref(), Some("d"));
        assert_eq!(synth_ini_lookup(ini, "C", "Directory_portsdir"), None);
        assert_eq!(synth_ini_lookup(ini, " C ", "Directory_portsdir").as_deref(), Some("/c"));
        assert_eq!(synth_ini_lookup(ini, "Q", "key"), None, "no section Q");
        assert_eq!(synth_ini_lookup(ini, "P", "key").as_deref(), Some("2"), "still in [P]");
        assert!(!synth_ini_has_section(ini, "C") && synth_ini_has_section(ini, " C "));
        // Each line synth reads otherwise than it seems is named.
        let why = "read as synth reads it (a synth defect)";
        assert_eq!(
            synth_ini_misreads(ini),
            [
                format!("synth.ini line 2: synth keeps a tab around the key or value; {why}"),
                format!("synth.ini line 3: synth keeps the spaces of the section name ' C '; {why}"),
                format!(
                    "synth.ini line 6: synth takes no indented [name] for a section header, \
                     the lines after it staying in the section above; {why}"
                ),
            ]
        );
        assert!(synth_ini_misreads("[A]\nk = v\n").is_empty(), "spaces are fine");

        // The format's own rule: the first match ends the search, an empty
        // value giving the default rather than a later assignment.
        let ini = "[A]\nDirectory_portsdir=\nDirectory_portsdir=/x\n";
        assert_eq!(synth_ini_lookup(ini, "A", "Directory_portsdir"), None);
    }

    /// The host as a test sees it: no PORTSDIR, make answering none, and
    /// synth's own standard trees, of which /usr/ports may exist here.
    fn synth_host(env_profile: Option<&str>) -> SynthHost<'_> {
        fn no_make() -> Option<PathBuf> {
            None
        }
        SynthHost {
            env_profile,
            env_portsdir: None,
            make_portsdir: &no_make,
            standard_portsdirs: SYNTH_STANDARD_PORTSDIRS,
        }
    }

    /// A line with nothing left once its comment is cut is no misread, tab
    /// or not; a data line's CR, which synth keeps and optique with it, is,
    /// all such lines in one note; a comment cut drops the CR, a header
    /// ignores it, and a CR alone is a line synth skips.
    #[test]
    fn synth_ini_misreads_skip_empty_lines_and_name_crlf() {
        let ini = "[A]\n\t\n\t# comment\n \t; comment\nk = v ; tab\there\n";
        assert!(synth_ini_misreads(ini).is_empty(), "{:?}", synth_ini_misreads(ini));

        let ini = "[A]\r\nk=v\r\nj=w\r\n\r\n; c\r\nm=x ; c\r\nn=y\r\n";
        let why = "read as synth reads it (a synth defect)";
        assert_eq!(
            synth_ini_misreads(ini),
            [format!("synth.ini lines 2-3, 7: synth keeps the CR of a CRLF line end in the key or value; {why}")]
        );
        assert_eq!(
            synth_ini_misreads("[A]\nk=v\r\n"),
            [format!("synth.ini line 2: synth keeps the CR of a CRLF line end in the key or value; {why}")]
        );
        // As synth reads it: the value keeps the CR, a comment cut drops it.
        assert_eq!(synth_ini_lookup(ini, "A", "k").as_deref(), Some("v\r"));
        assert_eq!(synth_ini_lookup(ini, "A", "m").as_deref(), Some("x"));
        assert_eq!(line_ranges(&[1, 2, 3, 5, 7, 8]), "1-3, 5, 7-8");
    }

    /// A header with no `]` is no header: skipped, the section above going
    /// on, and nothing said of it. synth fails to load such a file, but the
    /// file is the user's, not something optique wrote.
    #[test]
    fn synth_ini_header_without_bracket_is_skipped() {
        let ini = "[A]\nk=v\n[B\nj=w\n";
        assert_eq!(synth_ini_lookup(ini, "A", "k").as_deref(), Some("v"));
        assert_eq!(synth_ini_lookup(ini, "A", "j").as_deref(), Some("w"));
        assert!(!synth_ini_has_section(ini, "B"));
        assert!(synth_ini_misreads(ini).is_empty(), "{:?}", synth_ini_misreads(ini));
    }

    /// A missing synth.ini is none; one that cannot be read stops the run,
    /// as it stops synth; bytes that are not UTF-8 cost only themselves.
    #[test]
    fn synth_ini_read_errors_stop_and_bad_bytes_are_kept_lossily() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(read_synth_ini(&tmp.path().join("absent")).unwrap(), "");
        let err = read_synth_ini(tmp.path()).unwrap_err();
        assert!(format!("{err:#}").contains("synth will not run"), "{err:#}");

        let ini = tmp.path().join("synth.ini");
        fs::write(&ini, b"; caf\xe9\n[A]\nDirectory_options=/opts\n").unwrap();
        let text = read_synth_ini(&ini).unwrap();
        assert_eq!(synth_ini_lookup(&text, "A", "Directory_options").as_deref(), Some("/opts"));

        // Through the resolution: an unreadable synth.ini is an error.
        let synth = tmp.path().join("synth");
        fs::create_dir_all(synth.join("synth.ini")).unwrap();
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let tree = tmp.path().join("ports");
        fake_tree(&tree);
        assert!(resolve_synth(&synth, "A", &synth_host(None), Some(&tree), Some(&tree), &stage).is_err());
    }

    /// With no Directory_portsdir, synth takes the first that exists of
    /// PORTSDIR, make's PORTSDIR, /usr/dports, /usr/ports; -P still comes
    /// first, and the banner says which gave the tree.
    #[test]
    fn synth_portsdir_falls_back_as_synth_determines_it() {
        let tmp = tempfile::tempdir().unwrap();
        let [env, make, dports, ports] =
            ["env", "make", "dports", "ports"].map(|d| tmp.path().join(d));
        let absent = tmp.path().join("absent");
        let standard = [dports.to_str().unwrap(), ports.to_str().unwrap()];
        let answer = std::cell::RefCell::new(Some(make.clone()));
        let make_portsdir = || answer.borrow().clone();
        let host = |env_portsdir| SynthHost {
            env_profile: None,
            env_portsdir,
            make_portsdir: &make_portsdir,
            standard_portsdirs: &standard,
        };
        let pick = |env_portsdir| synth_default_portsdir(&host(env_portsdir));

        assert_eq!(pick(Some(&env)), None, "nothing exists yet");
        fake_tree(&ports);
        assert_eq!(pick(Some(&env)), Some((ports.clone(), "synth's default")));
        fake_tree(&dports);
        assert_eq!(pick(Some(&env)), Some((dports.clone(), "synth's default")));
        fake_tree(&make);
        assert_eq!(pick(Some(&env)), Some((make.clone(), "from make's PORTSDIR")));
        fake_tree(&env);
        assert_eq!(pick(Some(&env)), Some((env.clone(), "from PORTSDIR")));
        assert_eq!(pick(Some(&absent)), Some((make.clone(), "from make's PORTSDIR")));
        *answer.borrow_mut() = None;
        assert_eq!(pick(None), Some((dports.clone(), "synth's default")));

        // Through the resolution: noted, -P first, and a candidate that is
        // no tree is an error naming its source.
        let synth = tmp.path().join("synth");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(synth.join("synth.ini"), "[A]\n").unwrap();
        let s = resolve_synth(&synth, "A", &host(Some(&env)), None, Some(&stage), &stage).unwrap();
        assert_eq!(s.portsdir, env);
        let note = format!("no Directory_portsdir: ports tree {} (from PORTSDIR)", env.display());
        assert!(s.notes.contains(&note), "{:?}", s.notes);
        let s = resolve_synth(&synth, "A", &host(Some(&env)), Some(&ports), Some(&stage), &stage).unwrap();
        assert_eq!(s.portsdir, ports);
        assert!(!s.notes.iter().any(|n| n.contains("Directory_portsdir")), "{:?}", s.notes);
        let err = resolve_synth(&synth, "A", &host(Some(&stage)), None, Some(&stage), &stage)
            .unwrap_err()
            .to_string();
        assert!(err.contains("(from PORTSDIR) is not a ports tree"), "{err}");
    }

    /// POUDRIERE_ETC, when set and non-empty, must be a directory, made
    /// absolute; the banner says where poudriere's configuration was read.
    #[test]
    fn poudriere_etc_must_be_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(poudriere_etc(None).unwrap(), None);
        assert_eq!(poudriere_etc(Some("".into())).unwrap(), None);
        assert_eq!(poudriere_etc(Some(tmp.path().into())).unwrap().as_deref(), Some(tmp.path()));
        let file = tmp.path().join("file");
        fs::write(&file, "").unwrap();
        for bad in [file, tmp.path().join("absent")] {
            let err = poudriere_etc(Some(bad.clone().into())).unwrap_err().to_string();
            assert_eq!(err, format!("POUDRIERE_ETC {} is not a directory", bad.display()));
        }
        assert!(poudriere_etc(Some(".".into())).unwrap().unwrap().is_absolute());
        let note = poudriere_etc_note(Path::new("/e"), true);
        assert_eq!(note, "poudriere.conf and poudriere.d read from /e (POUDRIERE_ETC)");
        let note = poudriere_etc_note(Path::new("/e"), false);
        assert_eq!(note, "POUDRIERE_ETC /e has no poudriere.d: a local build");
    }

    /// The framework values key the generation, a changed one (a host
    /// upgraded) a new one; none leave the hash as it was.
    #[test]
    fn framework_values_key_the_generation() {
        let v = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
        };
        assert_eq!(framework_hash("h", &[]), "h");
        let old = framework_hash("h", &v(&[("OSVERSION", "1500000")]));
        assert_ne!(old, "h");
        assert_ne!(framework_hash("h", &v(&[("OSVERSION", "1501000")])), old);
        assert_eq!(framework_hash("h", &v(&[("OSVERSION", "1500000")])), old);
    }

    /// Queries run as root in a builder, as the invoker on the host; the
    /// host's cache generation keys the invoker's USER, a builder's none.
    #[test]
    fn identity_follows_the_build_and_keys_a_host_build() {
        assert_eq!(QueryEnv::Host.identity(), Identity::Invoker);
        assert_eq!(QueryEnv::Jail { package_building: true }.identity(), Identity::Root);
        assert_eq!(QueryEnv::Synth.identity(), Identity::Root);

        let tmp = tempfile::tempdir().unwrap();
        let synth = tmp.path().join("synth");
        let tree = tmp.path().join("ports");
        fake_tree(&tree);
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        let resolve = || resolve_synth(&synth, "A", &synth_host(None), Some(&tree), Some(&tree), &stage).unwrap();
        let hash = |s: Settings| s.conf_hash;
        let builder = hash(resolve());
        assert_eq!(hash(key_invoker(resolve(), Some("alice"))), builder, "a builder runs as root");
        let host = |user| {
            let mut s = resolve();
            s.env = QueryEnv::Host;
            hash(key_invoker(s, user))
        };
        let (alice, root, unset) = (host(Some("alice")), host(Some("root")), host(None));
        assert!(alice != root && root != unset && alice != unset && alice != builder);
        assert_eq!(host(Some("alice")), alice);
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
        let s = resolve_synth(&synth, "typo", &synth_host(None), None, None, &stage).unwrap();
        assert!(s.notes.iter().any(|n| n.contains("'typo' has no section")), "{:?}", s.notes);
        assert!(s.notes.iter().any(|n| n.contains("live /var/db/ports")), "{:?}", s.notes);

        // Bare -s honours profile_selected; known section → no missing note.
        let s = resolve_synth(&synth, "", &synth_host(None), None, None, &stage).unwrap();
        assert!(s.notes.iter().any(|n| n.contains("profile Live (synth.ini")), "{:?}", s.notes);
        assert!(!s.notes.iter().any(|n| n.contains("no section")), "{:?}", s.notes);

        // -o override: no live-dir note.
        let o = tmp.path().join("other");
        let s = resolve_synth(&synth, "Live", &synth_host(None), None, Some(&o), &stage).unwrap();
        assert!(!s.notes.iter().any(|n| n.contains("live /var/db/ports")), "{:?}", s.notes);
    }

    /// synth has no jail: nothing stages the jail system lines poudriere's
    /// own `jail_system` writes for -j, so the staged synth make.conf never
    /// sets OSVERSION, ARCH or _OSRELEASE.
    #[test]
    fn synth_make_conf_never_sets_the_jail_system_vars() {
        let tmp = tempfile::tempdir().unwrap();
        let synth = tmp.path().join("synth");
        let tree = tmp.path().join("ports");
        fake_tree(&tree);
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(synth.join("synth.ini"), format!("[A]\nDirectory_portsdir={}\n", tree.display()))
            .unwrap();
        fs::write(synth.join("A-make.conf"), "X=1\n").unwrap();

        let s = resolve_synth(&synth, "A", &synth_host(None), None, None, &stage).unwrap();
        let conf = fs::read_to_string(s.make_conf.unwrap()).unwrap();
        for var in ["OSVERSION", "ARCH", "_OSRELEASE"] {
            assert!(!conf.contains(var), "{var} in synth make.conf: {conf}");
        }
    }

    /// A bare -s runs with the profile synth itself would: SYNTHPROFILE's
    /// when its section sets Directory_packages, else profile_selected,
    /// else LiveSystem; the banner says which chose it. A named -s wins.
    #[test]
    fn synth_profile_follows_synthprofile_then_the_ini() {
        let tmp = tempfile::tempdir().unwrap();
        let synth = tmp.path().join("synth");
        let stage = tmp.path().join("stage");
        let o = tmp.path().join("other");
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(
            synth.join("synth.ini"),
            "[Global Configuration]\nprofile_selected= Live\n[Live]\n\
             [Env]\nDirectory_packages=/pkgs\n[Bare]\nDirectory_options=/x\n",
        )
        .unwrap();
        let picked = |profile, env| {
            let s = resolve_synth(&synth, profile, &synth_host(env), None, Some(&o), &stage).unwrap();
            let staged = fs::read_to_string(s.make_conf.unwrap()).unwrap();
            let name = staged.lines().find_map(|l| l.strip_prefix("SYNTHPROFILE=")).unwrap().to_string();
            (name, s.notes)
        };

        let (name, notes) = picked("", Some("Env"));
        assert_eq!(name, "Env");
        assert!(notes.iter().any(|n| n == "synth profile Env (SYNTHPROFILE)"), "{notes:?}");
        // A section without Directory_packages, an unknown one, an empty
        // value: synth ignores the variable.
        for env in [Some("Bare"), Some("Nope"), Some(""), None] {
            let (name, notes) = picked("", env);
            assert_eq!(name, "Live", "{env:?}");
            assert!(notes.iter().any(|n| n == "synth profile Live (synth.ini profile_selected)"), "{notes:?}");
        }
        assert_eq!(picked("Bare", Some("Env")).0, "Bare");

        fs::write(synth.join("synth.ini"), "[LiveSystem]\n").unwrap();
        let (name, notes) = picked("", None);
        assert_eq!(name, "LiveSystem");
        assert!(notes.iter().any(|n| n == "synth profile LiveSystem (synth's default)"), "{notes:?}");
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

        let s = resolve_synth(&synth, "LiveSystem", &synth_host(None), None, None, &stage).unwrap();
        assert_eq!(s.portsdir, tree);
        assert_eq!(s.options_dir, opts);
        assert!(s.options_dir.is_dir());
        assert_eq!(s.make_conf_sources, vec![synth.join("LiveSystem-make.conf")]);
        assert_eq!(s.env, QueryEnv::Synth);
        let staged = fs::read_to_string(s.make_conf.as_ref().unwrap()).unwrap();
        assert!(staged.contains("ssl=openssl"), "{staged}");
        assert!(!staged.contains("A=1"), "other profile leaked in: {staged}");
        // synth's scan slave: its header before the profile's make.conf,
        // PACKAGE_BUILDING without _FLAVORS, and an empty LOCALBASE after it
        // so the profile cannot undo it.
        let at = |needle: &str| staged.find(needle).unwrap_or_else(|| panic!("{needle}: {staged}"));
        assert!(at("SYNTHPROFILE=LiveSystem\n") < at("ssl=openssl"), "{staged}");
        assert!(at("PACKAGE_BUILDING=yes\n") < at("ssl=openssl"), "{staged}");
        assert!(!staged.contains("PACKAGE_BUILDING_FLAVORS"), "{staged}");
        assert!(at("ssl=openssl") < at("LOCALBASE=/nonexistent\n"), "{staged}");
        assert!(staged.ends_with("LOCALBASE=/nonexistent\n"), "{staged}");
        // synth has no blacklist, and nothing to note.
        assert!(s.blacklist.is_empty());
        assert!(s.notes.is_empty());
        assert!(!s.conf_hash.is_empty());

        // -o wins over Directory_options, -P over Directory_portsdir.
        let elsewhere = tmp.path().join("elsewhere");
        fake_tree(&elsewhere);
        let s = resolve_synth(&synth, "LiveSystem", &synth_host(None), Some(&elsewhere), Some(&elsewhere), &stage)
            .unwrap();
        assert_eq!((s.portsdir.as_path(), s.options_dir.as_path()), (elsewhere.as_path(), elsewhere.as_path()));
        assert!(s.notes.is_empty(), "{:?}", s.notes);
    }

    /// synth's header gains its ccache lines, after the rest of it and
    /// before the profile's make.conf, only when Directory_ccache exists.
    #[test]
    fn synth_ccache_follows_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let synth = tmp.path().join("synth");
        let tree = tmp.path().join("ports");
        let ccache = tmp.path().join("ccache");
        let stage = tmp.path().join("stage");
        fake_tree(&tree);
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(
            synth.join("synth.ini"),
            format!(
                "[A]\nDirectory_portsdir={}\nDirectory_options={}\nDirectory_ccache={}\n",
                tree.display(),
                tmp.path().display(),
                ccache.display()
            ),
        )
        .unwrap();
        fs::write(synth.join("A-make.conf"), "X=1\n").unwrap();
        let staged = || {
            let s = resolve_synth(&synth, "A", &synth_host(None), None, None, &stage).unwrap();
            fs::read_to_string(s.make_conf.unwrap()).unwrap()
        };

        let absent = staged();
        assert!(!absent.contains("CCACHE"), "{absent}");

        fs::create_dir_all(&ccache).unwrap();
        let present = staged();
        let at = |needle: &str| present.find(needle).unwrap_or_else(|| panic!("{needle}: {present}"));
        assert!(at("BATCH=yes\n") < at("WITH_CCACHE_BUILD=yes\nCCACHE_DIR=/ccache\n"), "{present}");
        assert!(at("CCACHE_DIR=/ccache\n") < at("X=1\n"), "{present}");

        // Unset is synth's "disabled", never a path looked up.
        assert!(!synth_ccache(None));
        assert!(!synth_ccache(Some("disabled")));
    }

    /// poudriere.conf's `CCACHE_DIR`, when it is a directory, adds
    /// `WITH_CCACHE_BUILD=yes` and `CCACHE_DIR=/root/.ccache` after the
    /// make.conf layers, as poudriere's `setup_ccache` does after
    /// `setup_makeconf` (common.sh:3591-3600, 4121, 4195); unset, or naming
    /// something that is not a directory, adds neither.
    #[test]
    fn poudriere_ccache_follows_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let db = tmp.path().join("db");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        let conf = tmp.path().join("poudriere.conf");
        let ccache = tmp.path().join("ccache");

        let host = host(&conf, &pd, &ports, &db);
        let req = Request::default();
        let staged = || {
            let s = resolve_host(&host, &req, &stage).unwrap();
            fs::read_to_string(s.make_conf.unwrap()).unwrap()
        };

        // No CCACHE_DIR at all: neither line.
        let absent = staged();
        assert!(!absent.contains("CCACHE"), "{absent}");

        // CCACHE_DIR set but not (yet) a directory: still neither line.
        fs::write(&conf, format!("CCACHE_DIR={}\n", ccache.display())).unwrap();
        let missing = staged();
        assert!(!missing.contains("CCACHE"), "{missing}");

        // CCACHE_DIR a real directory: both lines, after the layers.
        fs::create_dir_all(&ccache).unwrap();
        let present = staged();
        let at = |needle: &str| present.find(needle).unwrap_or_else(|| panic!("{needle}: {present}"));
        assert!(
            at("PACKAGE_BUILDING=yes") < at("WITH_CCACHE_BUILD=yes\nCCACHE_DIR=/root/.ccache\n"),
            "{present}"
        );
        assert!(at("CCACHE_DIR=/root/.ccache\n") < at("LOCALBASE=/nonexistent"), "{present}");
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
        // No <profile>-make.conf at all.
        fs::write(
            synth.join("synth.ini"),
            format!(
                "[LiveSystem]\nDirectory_portsdir={}\nDirectory_options={}\n",
                tree.display(),
                opts.display()
            ),
        )
        .unwrap();

        let s = resolve_synth(&synth, "LiveSystem", &synth_host(None), None, None, &stage).unwrap();
        assert_eq!(s.portsdir, tree);
        // Nonexistent Directory_options is still the target, not created yet.
        assert_eq!(s.options_dir, opts);
        assert!(!s.options_dir.is_dir());
        // No profile make.conf still stages synth's header: left unset, make
        // would read the host's (/etc/make.conf or the environment's
        // __MAKE_CONF), which a synth builder never does.
        assert_eq!(s.make_conf.as_deref(), Some(stage.join("make.conf").as_path()));
        assert!(s.make_conf_sources.is_empty());
        let staged = fs::read_to_string(stage.join("make.conf")).unwrap();
        assert_eq!(staged, format!("{}{SYNTH_SCAN_TRAILER}", synth_prelude("LiveSystem", false)));

        // Directory_portsdir naming a non-tree is an error, not a fallback.
        fs::write(
            synth.join("synth.ini"),
            format!("[LiveSystem]\nDirectory_portsdir={}/nope\n", tmp.path().display()),
        )
        .unwrap();
        let err = resolve_synth(&synth, "LiveSystem", &synth_host(None), None, Some(&opts), &stage)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a ports tree"), "{err}");

        // A setting in [Global Configuration] alone is no profile's: synth
        // reads Directory_options from the profile's section only.
        fs::write(
            synth.join("synth.ini"),
            format!(
                "[Global Configuration]\nDirectory_options={}\n\
                 [LiveSystem]\nDirectory_portsdir={}\n",
                opts.display(),
                tree.display()
            ),
        )
        .unwrap();
        let s = resolve_synth(&synth, "LiveSystem", &synth_host(None), None, None, &stage).unwrap();
        assert_eq!(s.options_dir, Path::new("/var/db/ports"));

        // No synth.ini: options dir falls back to /var/db/ports (the portsdir
        // fallback is /usr/ports, whose presence is host-dependent, so it is
        // only exercised through -o here).
        fs::remove_file(synth.join("synth.ini")).unwrap();
        if is_ports_tree(Path::new("/usr/ports")) {
            let s = resolve_synth(&synth, "LiveSystem", &synth_host(None), None, None, &stage).unwrap();
            assert_eq!(s.options_dir, Path::new("/var/db/ports"));
        }
    }

    /// A non-default-flavor port, matched as `Blacklist::matches` wants it.
    fn flavor(origin: &str, fl: &str) -> PortKey {
        PortKey::new(origin, Some(fl.to_string()))
    }

    /// Under `-j`, poudriere always loads `qemu-blacklist` too when it
    /// would run the jail under qemu (`load_blacklist`, common.sh
    /// ~4248-4285: `if [ ${QEMU_EMULATING} -eq 1 ]; then bl="${bl} qemu"`) —
    /// a check this host's `kern.supported_archs` answers
    /// (`check_emulation`/`need_emulation`, common.sh:3531-3573), never
    /// poudriere.d's own setting.
    #[test]
    fn qemu_blacklist_loads_only_when_the_jail_is_emulated() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fake_tree(&ports);
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let req = Request { jail: Some("j"), ..Request::default() };
        let host = HostPaths {
            arch: "amd64",
            supported_archs: "amd64 i386",
            ..host(tmp.path(), tmp.path(), &ports, tmp.path())
        };
        let pd = fake_jail(tmp.path(), "arm64.aarch64", "14.3-RELEASE", Some("1403000"));
        fs::write(pd.join("qemu-blacklist"), "lang/ghc\n").unwrap();
        let key = PortKey::new("lang/ghc", None);

        // This host's kernel runs only amd64/i386 natively: aarch64 is
        // emulated, so qemu-blacklist applies.
        let s = resolve_host(&HostPaths { poudriere_d: &pd, ..host }, &req, &stage).unwrap();
        assert!(s.blacklist.matches(&key, false), "{:?}", s.blacklist);

        // A host whose kernel also runs aarch64 natively: no emulation, so
        // qemu-blacklist is never even looked at.
        let native = HostPaths { supported_archs: "amd64 i386 aarch64", ..host };
        let s = resolve_host(&HostPaths { poudriere_d: &pd, ..native }, &req, &stage).unwrap();
        assert!(!s.blacklist.matches(&key, false), "{:?}", s.blacklist);
    }

    #[test]
    fn blacklist_layers_and_matching() {
        let tmp = tempfile::tempdir().unwrap();
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        // A line may hold several origins, split on whitespace, and '#'
        // comments the rest of the line away — no globs anywhere.
        fs::write(pd.join("blacklist"), "# comment\n\nwww/nginx mail/dovecot\n").unwrap();
        fs::write(pd.join("s1-blacklist"), "devel/py-foo@py312   # one flavor only\n").unwrap();

        let bl = Blacklist::load(&pd, None, "default", Some("s1"));
        assert_eq!(bl.sources.len(), 2);
        assert!(!bl.is_empty());
        // A flavorless port matches only its bare origin.
        assert!(bl.matches(&PortKey::new("www/nginx", None), true));
        assert!(bl.matches(&PortKey::new("mail/dovecot", None), true));
        assert!(!bl.matches(&PortKey::new("mail/dovecot2", None), true));
        // A flavored entry names one flavor: only that flavor matches,
        // default or not.
        assert!(bl.matches(&flavor("devel/py-foo", "py312"), true));
        assert!(bl.matches(&flavor("devel/py-foo", "py312"), false));
        assert!(!bl.matches(&flavor("devel/py-foo", "py311"), false));
        // An unglobbed entry does not become a substring match.
        assert!(!bl.matches(&PortKey::new("www/nginx-devel", None), true));

        // Without -z the set layer is not even looked at.
        let bl = Blacklist::load(&pd, None, "default", None);
        assert_eq!(bl.sources, vec![pd.join("blacklist")]);
        assert!(bl.matches(&PortKey::new("www/nginx", None), true));
        assert!(!bl.matches(&flavor("devel/py-foo", "py312"), true));

        // No poudriere.d at all: empty, matches nothing.
        let bl = Blacklist::load(&tmp.path().join("nope"), Some("j1"), "t1", Some("s1"));
        assert!(bl.is_empty());
        assert!(!bl.matches(&PortKey::new("www/nginx", None), true));
    }

    /// A port that has flavors matches `origin`, `origin@all` and
    /// `origin@<flavor>` whatever flavor is asked for, plus `origin@-`
    /// (poudriere's `FLAVOR_DEFAULT`) only for the default flavor — see
    /// `check_bl` in common.sh.
    #[test]
    fn blacklist_matching_follows_poudriere_flavor_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        fs::write(pd.join("blacklist"), "devel/py-foo@- net/bar@all devel/py-baz@py312\n")
            .unwrap();
        let bl = Blacklist::load(&pd, None, "default", None);

        // `origin@-` matches only the default flavor, never another one.
        assert!(bl.matches(&flavor("devel/py-foo", "py311"), true));
        assert!(!bl.matches(&flavor("devel/py-foo", "py311"), false));
        assert!(!bl.matches(&flavor("devel/py-foo", "py312"), false));

        // `origin@all` matches every flavor, default or not.
        assert!(bl.matches(&flavor("net/bar", "py311"), true));
        assert!(bl.matches(&flavor("net/bar", "py312"), false));

        // A named flavor matches only itself, default or not.
        assert!(bl.matches(&flavor("devel/py-baz", "py312"), true));
        assert!(bl.matches(&flavor("devel/py-baz", "py312"), false));
        assert!(!bl.matches(&flavor("devel/py-baz", "py311"), true));

        // A bare origin blacklists every flavor too, default or not.
        fs::write(pd.join("blacklist"), "devel/py-quux\n").unwrap();
        let bl = Blacklist::load(&pd, None, "default", None);
        assert!(bl.matches(&flavor("devel/py-quux", "py312"), true));
        assert!(bl.matches(&flavor("devel/py-quux", "py311"), false));
    }

    #[test]
    fn blacklist_layer_order_matches_manpage() {
        assert_eq!(
            blacklist_layers(Some("j"), "t", Some("s"), false),
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
        assert_eq!(blacklist_layers(None, "t", None, false), vec!["blacklist", "t-blacklist"]);
        // Emulating always loads qemu-blacklist too, and last.
        assert_eq!(
            blacklist_layers(None, "t", None, true),
            vec!["blacklist", "t-blacklist", "qemu-blacklist"]
        );
    }

    /// A make.conf `PORT_DBDIR` is an error only where the build would
    /// read another directory than the options dir: a local build compares
    /// it with the options dir (canonicalised where both exist), which,
    /// without -o, is that very PORT_DBDIR; a builder with the path it
    /// mounts the options dir at.
    #[test]
    fn port_dbdir_error_only_where_the_build_reads_elsewhere() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db");
        fs::create_dir_all(&db).unwrap();
        std::os::unix::fs::symlink(&db, tmp.path().join("link")).unwrap();
        let other = tmp.path().join("other");
        let jail = QueryEnv::Jail { package_building: true };
        let s = |p: &Path| p.display().to_string();

        // Local, no -o: the options dir is make's own PORT_DBDIR.
        assert_eq!(port_dbdir_error(QueryEnv::Host, &db, &s(&db)), None);
        assert_eq!(port_dbdir_error(QueryEnv::Host, &db, &format!("{}/", s(&db))), None);
        assert_eq!(port_dbdir_error(QueryEnv::Host, &db, &s(&tmp.path().join("link"))), None);
        // Local with -o elsewhere.
        assert_eq!(
            port_dbdir_error(QueryEnv::Host, &other, &s(&db)),
            Some(format!(
                "make.conf sets PORT_DBDIR={}: the build reads that directory, not {}",
                s(&db),
                s(&other)
            ))
        );
        // poudriere mounts the options dir at /var/db/ports, synth at /options.
        assert_eq!(port_dbdir_error(jail, &db, "/var/db/ports"), None);
        assert!(port_dbdir_error(jail, &db, "/var/db/myports").is_some());
        assert!(port_dbdir_error(jail, &db, &s(&db)).is_some(), "the host path is not the jail's");
        assert_eq!(port_dbdir_error(QueryEnv::Synth, &db, "/options"), None);
        assert!(port_dbdir_error(QueryEnv::Synth, &db, "/var/db/ports").is_some());

        // Unset, or make could not say: nothing.
        let settings = |place: Option<OptionsPlace>| Settings {
            portsdir: PathBuf::new(),
            options_dir: db.clone(),
            make_conf: None,
            make_conf_sources: Vec::new(),
            conf_hash: String::new(),
            env: QueryEnv::Host,
            blacklist: Blacklist::default(),
            notes: Vec::new(),
            overlays: Vec::new(),
            flavor_default_all: false,
            framework: Vec::new(),
            options_place: place,
        };
        assert_eq!(settings(None).port_dbdir_error(), None);
        assert_eq!(settings(Some(OptionsPlace::default())).port_dbdir_error(), None);
        let set = OptionsPlace { port_dbdir: Some(s(&other)), options_file: None };
        assert!(settings(Some(set)).port_dbdir_error().is_some());
    }

    /// clean holds its vanished and unused removals when make.conf moves
    /// OPTIONS_FILE, or when make could not say; spelling out the
    /// framework's own default moves nothing.
    #[test]
    fn relocated_options_follow_the_probe() {
        let file = |f: &str| OptionsPlace { port_dbdir: None, options_file: Some(f.to_string()) };
        assert_eq!(relocated_options(Some(&OptionsPlace::default())), None);
        assert_eq!(relocated_options(Some(&file(DEFAULT_OPTIONS_FILE))), None);
        let moved = relocated_options(Some(&file("${PORT_DBDIR}/${OPTIONS_NAME}${FLAVOR:D-${FLAVOR}}/options")));
        assert_eq!(
            moved.as_deref(),
            Some(
                "make.conf sets OPTIONS_FILE (${PORT_DBDIR}/${OPTIONS_NAME}${FLAVOR:D-${FLAVOR}}/options): \
                 optique cannot tell which options-dir entries make reads, so none is removed as vanished or unused"
            )
        );
        assert!(relocated_options(None).unwrap().starts_with("make could not say whether make.conf sets OPTIONS_FILE: "));
    }

    /// The probe reads make.conf outside any port: PORT_DBDIR expanded,
    /// OPTIONS_FILE as written, empty where unset.
    #[test]
    fn options_place_reads_make_conf_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let conf = tmp.path().join("make.conf");
        let probe = |text: &str| {
            fs::write(&conf, text).unwrap();
            crate::query::makerunner::options_place(Identity::Invoker, Some(&conf))
        };
        assert_eq!(probe(""), Some((String::new(), String::new())));
        let moved = "D=/var/db\nPORT_DBDIR=${D}/my\nOPTIONS_FILE=${PORT_DBDIR}/${OPTIONS_NAME}/x\n";
        assert_eq!(
            probe(moved),
            Some(("/var/db/my".to_string(), "${PORT_DBDIR}/${OPTIONS_NAME}/x".to_string()))
        );
        assert_eq!(probe(".error broken\n"), None);
    }
}
