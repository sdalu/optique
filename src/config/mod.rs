//! Which build is configured and how queries see it: what every build
//! shares lives here, each build's own reading in `local`, `poudriere` and
//! `synth`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::query::makerunner::Identity;

mod local;
mod poudriere;
mod synth;

pub use poudriere::Blacklist;
#[cfg(test)]
pub(crate) use poudriere::stage_jail_make_conf;
#[cfg(test)]
pub(crate) use synth::stage_synth_make_conf;

const POUDRIERE_D: &str = "/usr/local/etc/poudriere.d";
const POUDRIERE_CONF: &str = "/usr/local/etc/poudriere.conf";

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

    /// How the startup banner names the environment; None on the host,
    /// which needs no saying.
    pub fn description(self) -> Option<String> {
        match self {
            QueryEnv::Host => None,
            QueryEnv::Jail { package_building } => Some(format!(
                "poudriere jail (host packages under /usr/local not seen{})",
                if package_building {
                    ""
                } else {
                    "; NO_PACKAGE_BUILDING set"
                }
            )),
            QueryEnv::Synth => {
                Some("synth scan slave (host packages under /usr/local not seen)".to_string())
            }
        }
    }
}

/// A staged make.conf: the file make reads (None when there is nothing to
/// write), the hash of its text, and the files read into it.
type StagedConf = (Option<PathBuf>, String, Vec<PathBuf>);

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
        None => Some(format!(
            "make could not say whether make.conf sets OPTIONS_FILE: {tail}"
        )),
        Some(OptionsPlace {
            options_file: Some(file),
            ..
        }) if file != DEFAULT_OPTIONS_FILE => {
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
    // A local build's make also takes PORT_DBDIR from the environment,
    // which the probe passes on (`with_options_place`).
    let source = if env == QueryEnv::Host {
        "make.conf or the environment"
    } else {
        "make.conf"
    };
    (!same).then(|| {
        format!(
            "{source} sets PORT_DBDIR={set}: the build reads that directory, not {}",
            options_dir.display()
        )
    })
}

impl Settings {
    /// A build's resolution as its resolver leaves it: no blacklist and no
    /// `FLAVOR_DEFAULT_ALL` (a poudriere build sets both), the overlays,
    /// the framework values and the options place for [`finish`] to settle.
    fn new(
        portsdir: PathBuf,
        options_dir: PathBuf,
        (make_conf, conf_hash, make_conf_sources): StagedConf,
        env: QueryEnv,
        notes: Vec<String>,
    ) -> Self {
        Settings {
            portsdir,
            options_dir,
            make_conf,
            make_conf_sources,
            conf_hash,
            env,
            blacklist: Blacklist::default(),
            notes,
            overlays: Vec::new(),
            flavor_default_all: false,
            framework: Vec::new(),
            options_place: None,
        }
    }

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
        self.name
            .clone()
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

/// `Settings::overlay_of` for a bare list of overlays (the TUI keeps one).
pub fn overlay_of<'a>(overlays: &'a [Overlay], origin: &str) -> Option<&'a Overlay> {
    overlays
        .iter()
        .find(|o| o.path.join(origin).join("Makefile").is_file())
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
        let abs =
            |p: &Path| std::path::absolute(p).with_context(|| format!("resolving {}", p.display()));
        self.portsdir = abs(&self.portsdir)?;
        self.options_dir = abs(&self.options_dir)?;
        self.make_conf = self.make_conf.as_deref().map(abs).transpose()?;
        Ok(self)
    }
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
        return finish(
            synth::resolve(profile, req, staging_dir)?,
            Vec::new(),
            &pkg_db,
        );
    }
    // -l reads no poudriere configuration, so POUDRIERE_ETC is not asked.
    let etc = if req.local {
        None
    } else {
        poudriere::poudriere_etc(std::env::var_os("POUDRIERE_ETC"))?
    };
    let (poudriere_conf, poudriere_d) = match &etc {
        Some(etc) => (etc.join("poudriere.conf"), etc.join("poudriere.d")),
        None => (PathBuf::from(POUDRIERE_CONF), PathBuf::from(POUDRIERE_D)),
    };
    let poudriere_d = poudriere_d.as_path();
    // Only a local build asks make for its defaults: a poudriere build
    // reads neither the environment's nor /etc/make.conf's.
    let local = req.local || !poudriere_d.is_dir();
    let (ports, db) = if local {
        local::make_defaults()
    } else {
        (None, None)
    };
    // make's own default, which the environment may move as for any make.
    let make_conf = env_path_or("__MAKE_CONF", "/etc/make.conf");
    let (arch, supported_archs) = if !local && req.jail.is_some() {
        (poudriere::host_arch(), poudriere::supported_archs())
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
        settings
            .notes
            .insert(0, poudriere::poudriere_etc_note(etc, !local));
    }
    Ok(settings)
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
        local::resolve(host, req, &given, staging_dir)?
    } else {
        poudriere::resolve(host, req, &given, staging_dir)?
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
                    bail!(
                        "no poudriere overlay '{name}' ({} does not exist)",
                        dir.display()
                    );
                }
                let Ok(mnt) = fs::read_to_string(dir.join("mnt")) else {
                    bail!(
                        "poudriere overlay '{name}' has no mnt file in {}",
                        dir.display()
                    );
                };
                (Some(name.clone()), PathBuf::from(mnt.trim()))
            }
            OverlaySpec::Dir(dir) => (None, dir.clone()),
        };
        let what = name.as_deref().map_or_else(
            || format!("--overlay-dir {}", path.display()),
            |n| {
                format!(
                    "poudriere overlay '{n}' points to {}, which",
                    path.display()
                )
            },
        );
        if !path.is_dir() {
            bail!("{what} is not a directory");
        }
        let path = path
            .canonicalize()
            .with_context(|| format!("resolving {}", path.display()))?;
        out.push(Overlay { name, path });
    }
    Ok(out)
}

/// The `OVERLAYS+=` lines the given overlays add to a make.conf, in order.
fn overlay_lines(overlays: &[Overlay]) -> String {
    overlays
        .iter()
        .map(|o| format!("OVERLAYS+={}\n", o.path.display()))
        .collect()
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
                    settings
                        .notes
                        .push(format!("overlay {} {what}", path.display()));
                }
                Overlay {
                    name: found.and_then(|o| o.name.clone()),
                    path,
                }
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
    let Ok(meta) = fs::metadata(pkg_db) else {
        return settings;
    };
    let mtime = crate::cache::mtime(&meta).as_nanos();
    let keyed = format!(
        "{}\n{} {} {mtime}\n",
        settings.conf_hash,
        pkg_db.display(),
        meta.len()
    );
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
    Ok(with_options_place(
        with_framework(settings),
        std::env::var_os("PORT_DBDIR"),
    ))
}

/// Settle `settings.options_place` on what make.conf says where the
/// queries' own command line cannot hide it: a build given no `PORT_DBDIR`
/// there reads whatever make.conf names. make is asked from a port's
/// directory ([`probe_dir`]), as a build would, and for a build on the
/// host with `env_port_dbdir`, the environment's `PORT_DBDIR`, which a
/// make.conf `?=` defers to there; a builder's environment is not the
/// user's, and goes unasked. A probe that fails is noted for the banner.
fn with_options_place(
    mut settings: Settings,
    env_port_dbdir: Option<std::ffi::OsString>,
) -> Settings {
    let env_port_dbdir = env_port_dbdir.filter(|_| settings.env == QueryEnv::Host);
    let probed = crate::query::makerunner::options_place(
        settings.env.identity(),
        settings.make_conf.as_deref(),
        &probe_dir(&settings.portsdir),
        env_port_dbdir.as_deref(),
    );
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

/// Where [`with_options_place`] asks make: a real port's directory, so that
/// a make.conf testing `.CURDIR` sees what a build sees. ports-mgmt/pkg,
/// which every port depends on, else the tree itself, else `/`.
fn probe_dir(portsdir: &Path) -> PathBuf {
    let pkg = portsdir.join("ports-mgmt/pkg");
    if pkg.join("Makefile").is_file() {
        pkg
    } else if portsdir.is_dir() {
        portsdir.to_path_buf()
    } else {
        PathBuf::from("/")
    }
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
    let lines: String = framework
        .iter()
        .map(|(var, value)| format!("{var}={value}\n"))
        .collect();
    crate::cache::sha256_hex(format!("{conf_hash}\nframework\n{lines}").as_bytes())
}

/// A directory is a usable ports tree when it carries the ports framework.
fn is_ports_tree(dir: &Path) -> bool {
    dir.join("Mk/bsd.port.mk").is_file()
}

/// -P: taken as given, but it has to be a ports tree.
fn checked_portsdir(dir: &Path) -> Result<PathBuf> {
    if !is_ports_tree(dir) {
        bail!(
            "-P {} is not a ports tree (no Mk/bsd.port.mk)",
            dir.display()
        );
    }
    Ok(dir.to_path_buf())
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
) -> Result<StagedConf> {
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
            Err(e) => {
                return Err(e).with_context(|| format!("reading make.conf {}", path.display()));
            }
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

/// Fixtures shared by the tests of every build.
#[cfg(test)]
mod testutil {
    use super::*;

    /// A fake ports tree at `dir`.
    pub(super) fn fake_tree(dir: &Path) {
        fs::create_dir_all(dir.join("Mk")).unwrap();
        fs::write(dir.join("Mk/bsd.port.mk"), "# fake\n").unwrap();
    }

    pub(super) fn host<'a>(
        tmp: &'a Path,
        poudriere_d: &'a Path,
        ports: &'a Path,
        db: &'a Path,
    ) -> HostPaths<'a> {
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

    /// A poudriere.d whose jail `j` has `arch` and `version`, its mnt holding
    /// a param.h with `osversion` when given.
    pub(super) fn fake_jail(
        tmp: &Path,
        arch: &str,
        version: &str,
        osversion: Option<&str>,
    ) -> PathBuf {
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

    /// Root reads a mode-000 file: the unreadable case only holds for others.
    pub(super) fn nix_is_root() -> bool {
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .is_ok_and(|o| o.stdout.starts_with(b"0\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testutil::*;

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
        let host = HostPaths {
            pkg_db: &pkg_db,
            ..host(tmp.path(), &absent, &ports, &db)
        };
        let none = Request::default();
        let hash = |host: &HostPaths| resolve_host(host, &none, &stage).unwrap().conf_hash;

        let (_, bare, _) = stage_make_conf(String::new(), &[], "", &stage).unwrap();
        let bare = invoker_hash(&bare, std::env::var("USER").ok().as_deref());
        assert_eq!(hash(&host), bare, "no database, nothing to key");
        fs::write(&pkg_db, "one package").unwrap();
        let one = hash(&host);
        assert_ne!(one, bare);
        fs::write(&pkg_db, "one package, then another").unwrap();
        assert_ne!(hash(&host), one, "an install is a new generation");

        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        let jail = HostPaths {
            poudriere_d: &pd,
            ..host
        };
        let before = hash(&jail);
        fs::remove_file(&pkg_db).unwrap();
        assert_eq!(
            hash(&jail),
            before,
            "a jail does not see the host's packages"
        );
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
        assert_eq!(
            (s.portsdir.as_path(), s.options_dir.as_path()),
            (ports.as_path(), db.as_path())
        );
        assert!(
            !s.options_dir.is_dir(),
            "{} does not exist yet",
            db.display()
        );
        assert!(s.env == QueryEnv::Host && s.make_conf.as_deref() == Some(Path::new("/dev/null")));
        assert!(
            s.notes.is_empty(),
            "built-in defaults earn no note: {:?}",
            s.notes
        );
        fs::create_dir_all(&db).unwrap();
        assert!(
            resolve_host(&host, &none, &stage)
                .unwrap()
                .options_dir
                .is_dir()
        );
        // A set, jail or tree means poudriere, which is not there.
        for req in [
            Request {
                jail: Some("j"),
                ..none
            },
            Request {
                set: Some("s"),
                ..none
            },
            Request {
                tree: Some("t"),
                ..none
            },
        ] {
            let err = resolve_host(&host, &req, &stage).unwrap_err().to_string();
            assert!(err.starts_with("-j/-z/-p name poudriere's"), "{err}");
        }
        // -o is taken as given either way.
        let mine = tmp.path().join("mine");
        let s = resolve_host(
            &host,
            &Request {
                options_dir: Some(&mine),
                ..none
            },
            &stage,
        )
        .unwrap();
        assert_eq!(s.options_dir, mine);
        assert!(!s.options_dir.is_dir(), "created on apply");
        let s = resolve_host(
            &host,
            &Request {
                options_dir: Some(&db),
                ..none
            },
            &stage,
        )
        .unwrap();
        assert!(s.options_dir.is_dir());
        // No ports tree anywhere: say where it was looked for.
        let bare = tmp.path().join("bare");
        let nohost = HostPaths {
            ports: &bare,
            ..host
        };
        let err = resolve_host(&nohost, &none, &stage)
            .unwrap_err()
            .to_string();
        assert!(err.contains(&bare.display().to_string()), "{err}");

        // A poudriere.d: its options dir naming, a jail's environment.
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        let host = HostPaths {
            poudriere_d: &pd,
            ..host
        };
        let s = resolve_host(
            &host,
            &Request {
                set: Some("s"),
                ..none
            },
            &stage,
        )
        .unwrap();
        assert_eq!(s.options_dir, pd.join("s-options"));
        assert!(!s.options_dir.is_dir());
        assert_eq!(
            s.env,
            QueryEnv::Jail {
                package_building: true
            }
        );
        let conf = fs::read_to_string(s.make_conf.unwrap()).unwrap();
        assert!(conf.contains("PACKAGE_BUILDING=yes"), "{conf}");

        // With poudriere.d present too, -o is still taken verbatim, never
        // one of the options-dir candidates.
        let s = resolve_host(
            &host,
            &Request {
                options_dir: Some(&mine),
                set: Some("s"),
                ..none
            },
            &stage,
        )
        .unwrap();
        assert_eq!(s.options_dir, mine);
        assert_ne!(s.options_dir, pd.join("s-options"));
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
        for req in [
            Request::default(),
            Request {
                options_dir: Some(rel),
                ..Request::default()
            },
        ] {
            let s = resolve_host(&host, &req, &stage).unwrap();
            assert!(s.options_dir.is_absolute(), "{}", s.options_dir.display());
            assert!(
                s.options_dir.starts_with(&cwd),
                "{}",
                s.options_dir.display()
            );
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
        let specs = [
            OverlaySpec::Dir(dir.clone()),
            OverlaySpec::Name("ov".into()),
        ];
        let req = Request {
            overlays: &specs,
            ..Request::default()
        };

        let s = resolve_host(&host, &req, &stage).unwrap();
        let (dir, named) = (dir.canonicalize().unwrap(), named.canonicalize().unwrap());
        assert_eq!(
            s.overlays,
            [
                Overlay {
                    name: None,
                    path: dir.clone()
                },
                Overlay {
                    name: Some("ov".into()),
                    path: named.clone()
                }
            ]
        );
        let conf = fs::read_to_string(s.make_conf.as_ref().unwrap()).unwrap();
        let lines = format!(
            "OVERLAYS+={}\nOVERLAYS+={}\n",
            dir.display(),
            named.display()
        );
        let at = conf.find(&lines).expect("the OVERLAYS lines are staged");
        assert!(
            at > conf.find("BATCH=yes").unwrap() && at < conf.find("PACKAGE_BUILDING").unwrap(),
            "{conf}"
        );
        let bare = resolve_host(&host, &Request::default(), &stage).unwrap();
        assert_ne!(
            s.conf_hash, bare.conf_hash,
            "the overlays key their own generation"
        );
        assert!(bare.overlays.is_empty());

        // The errors, in poudriere's terms.
        let err = |specs: &[OverlaySpec]| {
            resolve_host(
                &host,
                &Request {
                    overlays: specs,
                    ..Request::default()
                },
                &stage,
            )
            .unwrap_err()
            .to_string()
        };
        assert!(
            err(&[OverlaySpec::Name("nope".into())]).starts_with("no poudriere overlay 'nope'")
        );
        fs::create_dir_all(pd.join("ports/nomnt")).unwrap();
        assert!(err(&[OverlaySpec::Name("nomnt".into())]).contains("has no mnt file"));
        fs::create_dir_all(pd.join("ports/gone")).unwrap();
        fs::write(pd.join("ports/gone/mnt"), "/nonexistent/overlay\n").unwrap();
        assert!(err(&[OverlaySpec::Name("gone".into())]).ends_with("is not a directory"));
        assert!(err(&[OverlaySpec::Dir("/nonexistent/dir".into())]).starts_with("--overlay-dir"));

        // No poudriere.d: a name has nothing to be read from.
        let local = HostPaths {
            poudriere_d: Path::new("/nonexistent/poudriere.d"),
            ..host
        };
        let e = resolve_host(
            &local,
            &Request {
                overlays: &[OverlaySpec::Name("ov".into())],
                ..Request::default()
            },
            &stage,
        )
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
        let overlays = [
            Overlay {
                name: Some("first".into()),
                path: a.clone(),
            },
            Overlay {
                name: None,
                path: b.clone(),
            },
        ];
        assert_eq!(
            overlay_of(&overlays, "cat/x")
                .map(Overlay::label)
                .as_deref(),
            Some("first")
        );
        assert_eq!(
            overlay_of(&overlays, "cat/y").map(Overlay::label),
            Some(b.display().to_string())
        );
        assert!(overlay_of(&overlays, "cat/z").is_none());
    }

    /// The framework values key the generation, a changed one (a host
    /// upgraded) a new one; none leave the hash as it was.
    #[test]
    fn framework_values_key_the_generation() {
        let v = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
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
        assert_eq!(
            QueryEnv::Jail {
                package_building: true
            }
            .identity(),
            Identity::Root
        );
        assert_eq!(QueryEnv::Synth.identity(), Identity::Root);

        let tree = Path::new("/nonexistent/ports");
        let resolve = || probe_settings(QueryEnv::Synth, tree, tree, Path::new("/dev/null"));
        let hash = |s: Settings| s.conf_hash;
        let builder = hash(resolve());
        assert_eq!(
            hash(key_invoker(resolve(), Some("alice"))),
            builder,
            "a builder runs as root"
        );
        let host = |user| {
            let mut s = resolve();
            s.env = QueryEnv::Host;
            hash(key_invoker(s, user))
        };
        let (alice, root, unset) = (host(Some("alice")), host(Some("root")), host(None));
        assert!(alice != root && root != unset && alice != unset && alice != builder);
        assert_eq!(host(Some("alice")), alice);
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
        let jail = QueryEnv::Jail {
            package_building: true,
        };
        let s = |p: &Path| p.display().to_string();

        // Local, no -o: the options dir is make's own PORT_DBDIR.
        assert_eq!(port_dbdir_error(QueryEnv::Host, &db, &s(&db)), None);
        assert_eq!(
            port_dbdir_error(QueryEnv::Host, &db, &format!("{}/", s(&db))),
            None
        );
        assert_eq!(
            port_dbdir_error(QueryEnv::Host, &db, &s(&tmp.path().join("link"))),
            None
        );
        // Local with -o elsewhere.
        assert_eq!(
            port_dbdir_error(QueryEnv::Host, &other, &s(&db)),
            Some(format!(
                "make.conf or the environment sets PORT_DBDIR={}: the build reads that directory, \
                 not {}",
                s(&db),
                s(&other)
            ))
        );
        // poudriere mounts the options dir at /var/db/ports, synth at /options.
        assert_eq!(port_dbdir_error(jail, &db, "/var/db/ports"), None);
        assert!(port_dbdir_error(jail, &db, "/var/db/myports").is_some());
        assert!(
            port_dbdir_error(jail, &db, &s(&db)).is_some(),
            "the host path is not the jail's"
        );
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
        assert_eq!(
            settings(Some(OptionsPlace::default())).port_dbdir_error(),
            None
        );
        let set = OptionsPlace {
            port_dbdir: Some(s(&other)),
            options_file: None,
        };
        assert!(settings(Some(set)).port_dbdir_error().is_some());
    }

    /// clean holds its vanished and unused removals when make.conf moves
    /// OPTIONS_FILE, or when make could not say; spelling out the
    /// framework's own default moves nothing.
    #[test]
    fn relocated_options_follow_the_probe() {
        let file = |f: &str| OptionsPlace {
            port_dbdir: None,
            options_file: Some(f.to_string()),
        };
        assert_eq!(relocated_options(Some(&OptionsPlace::default())), None);
        assert_eq!(relocated_options(Some(&file(DEFAULT_OPTIONS_FILE))), None);
        let moved = relocated_options(Some(&file(
            "${PORT_DBDIR}/${OPTIONS_NAME}${FLAVOR:D-${FLAVOR}}/options",
        )));
        assert_eq!(
            moved.as_deref(),
            Some(
                "make.conf sets OPTIONS_FILE (${PORT_DBDIR}/${OPTIONS_NAME}${FLAVOR:D-${FLAVOR}}/options): \
                 optique cannot tell which options-dir entries make reads, so none is removed as vanished or unused"
            )
        );
        assert!(
            relocated_options(None)
                .unwrap()
                .starts_with("make could not say whether make.conf sets OPTIONS_FILE: ")
        );
    }

    /// The probe reads make.conf outside any port: PORT_DBDIR expanded,
    /// OPTIONS_FILE as written, empty where unset.
    #[test]
    fn options_place_reads_make_conf_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let conf = tmp.path().join("make.conf");
        let probe = |text: &str| {
            fs::write(&conf, text).unwrap();
            crate::query::makerunner::options_place(
                Identity::Invoker,
                Some(&conf),
                Path::new("/"),
                None,
            )
        };
        assert_eq!(probe(""), Some((String::new(), String::new())));
        let moved = "D=/var/db\nPORT_DBDIR=${D}/my\nOPTIONS_FILE=${PORT_DBDIR}/${OPTIONS_NAME}/x\n";
        assert_eq!(
            probe(moved),
            Some((
                "/var/db/my".to_string(),
                "${PORT_DBDIR}/${OPTIONS_NAME}/x".to_string()
            ))
        );
        assert_eq!(probe(".error broken\n"), None);
    }

    /// Settings for `env` over the tree `portsdir`, options dir `db` and
    /// make.conf `conf`, the options place not probed yet.
    fn probe_settings(env: QueryEnv, portsdir: &Path, db: &Path, conf: &Path) -> Settings {
        let staged = (Some(conf.to_path_buf()), String::new(), Vec::new());
        Settings::new(
            portsdir.to_path_buf(),
            db.to_path_buf(),
            staged,
            env,
            Vec::new(),
        )
    }

    /// A make.conf `PORT_DBDIR?=` defers to the environment's PORT_DBDIR in
    /// the user's own make, which named the options dir: on the host the
    /// probe sees the same, and there is nothing to report. A plain `=`
    /// wins over the environment, as in make; a builder's environment is
    /// not the user's, and is not asked.
    #[test]
    fn port_dbdir_conditional_assignment_defers_to_the_environment() {
        let tmp = tempfile::tempdir().unwrap();
        let (conf, db) = (tmp.path().join("make.conf"), tmp.path().join("db"));
        fs::create_dir_all(&db).unwrap();
        let env_value = Some(db.clone().into_os_string());
        let probe = |env: QueryEnv, given: Option<std::ffi::OsString>| {
            with_options_place(probe_settings(env, tmp.path(), &db, &conf), given)
        };
        let dbdir = |s: &Settings| s.options_place.as_ref().unwrap().port_dbdir.clone();

        fs::write(&conf, "PORT_DBDIR?=/from/make.conf\n").unwrap();
        let s = probe(QueryEnv::Host, env_value.clone());
        assert_eq!(dbdir(&s), Some(db.display().to_string()));
        assert_eq!(s.port_dbdir_error(), None);
        // No PORT_DBDIR in the environment: make.conf's own applies.
        assert!(probe(QueryEnv::Host, None).port_dbdir_error().is_some());
        // A builder's queries never see optique's environment.
        assert_eq!(
            dbdir(&probe(QueryEnv::Synth, env_value.clone())).as_deref(),
            Some("/from/make.conf")
        );

        fs::write(&conf, "PORT_DBDIR=/from/make.conf\n").unwrap();
        let s = probe(QueryEnv::Host, env_value);
        assert_eq!(dbdir(&s).as_deref(), Some("/from/make.conf"));
        assert!(s.port_dbdir_error().is_some());
    }

    /// A make.conf setting PORT_DBDIR only inside the ports tree (`.if
    /// ${.CURDIR:M*/ports/*}`) moves every build's options dir: the probe
    /// asks make from a port's directory, so it sees the setting.
    #[test]
    fn options_place_sees_a_curdir_conditional_port_dbdir() {
        let tmp = tempfile::tempdir().unwrap();
        let (conf, db, ports) = (
            tmp.path().join("make.conf"),
            tmp.path().join("db"),
            tmp.path().join("ports"),
        );
        fs::create_dir_all(&db).unwrap();
        fs::create_dir_all(ports.join("ports-mgmt/pkg")).unwrap();
        fs::write(ports.join("ports-mgmt/pkg/Makefile"), "PORTNAME=pkg\n").unwrap();
        fs::write(
            &conf,
            ".if ${.CURDIR:M*/ports/*}\nPORT_DBDIR=/elsewhere\n.endif\n",
        )
        .unwrap();
        assert_eq!(probe_dir(&ports), ports.join("ports-mgmt/pkg"));
        let s = with_options_place(probe_settings(QueryEnv::Host, &ports, &db, &conf), None);
        assert_eq!(
            s.options_place.as_ref().unwrap().port_dbdir.as_deref(),
            Some("/elsewhere")
        );
        assert!(s.port_dbdir_error().is_some());

        // A tree without ports-mgmt/pkg is asked from its top, one missing
        // from `/`.
        assert_eq!(probe_dir(&db), db);
        assert_eq!(probe_dir(&tmp.path().join("none")), Path::new("/"));
    }
}
