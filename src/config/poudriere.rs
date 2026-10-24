//! A poudriere(8) build: the jail's make.conf, as poudriere layers it from
//! poudriere.d, its options dir, its blacklist and the poudriere.conf
//! settings that bear on what a query sees.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::model::origin::PortKey;

use super::{
    HostPaths, Overlay, QueryEnv, Request, Settings, StagedConf, checked_portsdir, is_ports_tree,
    overlay_lines, stage_make_conf,
};

/// What poudriere(8) writes at the top of every jail's make.conf, ahead of
/// the poudriere.d layers (`setup_makeconf` in its common.sh), as far as it
/// bears on evaluating a Makefile.
const POUDRIERE_JAIL_PRELUDE: &str = "\
# --- poudriere jail environment (optique) ---
USE_PACKAGE_DEPENDS=yes
BATCH=yes
";

/// What `jail_start` (common.sh ~4100-4115) writes after the overlay lines
/// unless poudriere.conf sets `NO_FORCE_PACKAGE`: bsd.port.mk:2191
/// undefines `NO_PACKAGE` under it, which `PACKAGE_BUILDING` would
/// otherwise turn into an IGNORE (devel/papi, audio/madfufw).
const FORCE_PACKAGE_LINE: &str = "FORCE_PACKAGE=yes\n";

/// The part of that prelude poudriere leaves out when poudriere.conf sets
/// `NO_PACKAGE_BUILDING`: several framework probes are skipped under
/// `PACKAGE_BUILDING`, and some ports change option defaults with it
/// (math/openblas turns `DYNAMIC_ARCH` on).
const PACKAGE_BUILDING_LINES: &str = "\
PACKAGE_BUILDING=yes
PACKAGE_BUILDING_FLAVORS=yes
";

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
    pub(crate) fn load(
        poudriere_d: &Path,
        jail: Option<&str>,
        tree: &str,
        set: Option<&str>,
    ) -> Self {
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
            // Bytes, as poudriere's grep and sed read it under LC_ALL=C: a
            // Latin-1 comment drops nothing ([`readable`] stopped the run
            // on one it cannot read).
            let Some(text) = read_lossy(&path) else {
                continue;
            };
            for line in text.lines() {
                let entry = match line.split_once('#') {
                    Some((before, _)) => before,
                    None => line,
                };
                bl.patterns
                    .extend(entry.split_whitespace().map(str::to_string));
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
    let mut v: Vec<String> = [set, tree, jail]
        .into_iter()
        .flatten()
        .map(str::to_string)
        .collect();
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
fn blacklist_layers(
    jail: Option<&str>,
    tree: &str,
    set: Option<&str>,
    emulating: bool,
) -> Vec<String> {
    let mut v = vec!["blacklist".to_string()];
    v.extend(
        layer_stems(jail, Some(tree), set)
            .into_iter()
            .map(|s| format!("{s}-blacklist")),
    );
    if emulating {
        v.push("qemu-blacklist".to_string());
    }
    v
}

/// poudriere's configuration root as `POUDRIERE_ETC` moves it (`value`,
/// the variable as optique was given it): poudriere.in hands any non-empty
/// value to the sub-command as `-e etcdir` would set it, made absolute.
/// None when unset or empty. poudriere makes no check of it, `-e` alone
/// asking for a directory; optique asks it of both, rather than read a
/// poudriere.d that cannot be there as a missing one.
pub(super) fn poudriere_etc(value: Option<std::ffi::OsString>) -> Result<Option<PathBuf>> {
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
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
pub(super) fn poudriere_etc_note(etc: &Path, poudriere: bool) -> String {
    if poudriere {
        format!(
            "poudriere.conf and poudriere.d read from {} (POUDRIERE_ETC)",
            etc.display()
        )
    } else {
        format!(
            "POUDRIERE_ETC {} has no poudriere.d: a local build",
            etc.display()
        )
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
pub(super) fn host_arch() -> String {
    let (m, p) = (run_capture("uname", &["-m"]), run_capture("uname", &["-p"]));
    if m == p { p } else { format!("{m}.{p}") }
}

/// The archs this host's kernel can run natively, as poudriere's
/// `need_emulation` reads them (common.sh:3552-3573): `kern.supported_archs`,
/// falling back to `uname -m` where the sysctl does not answer, as on
/// DragonFly.
pub(super) fn supported_archs() -> String {
    let s = run_capture("sysctl", &["-n", "kern.supported_archs"]);
    if s.is_empty() {
        run_capture("uname", &["-m"])
    } else {
        s
    }
}

pub(super) fn resolve(
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

    let emulating = jail.is_some_and(|j| jail_emulating(poudriere_d, j, host.supported_archs));
    readable(
        host.poudriere_conf,
        poudriere_d,
        jail,
        tree,
        set,
        tree_name,
        emulating,
    )?;
    let off = no_package_building(host.poudriere_conf, poudriere_d, jail, tree, set);
    let env = QueryEnv::Jail {
        package_building: !off,
    };
    let mut notes = Vec::new();
    let no_force = poudriere_conf_value(
        host.poudriere_conf,
        poudriere_d,
        jail,
        tree,
        set,
        "NO_FORCE_PACKAGE",
    )
    .is_some();
    // FORCE_PACKAGE, then the jail's system: both before the layers, as
    // poudriere writes them.
    let mut jail_lines = if no_force {
        String::new()
    } else {
        FORCE_PACKAGE_LINE.to_string()
    };
    if let Some(j) = jail {
        jail_lines.push_str(&jail_system(poudriere_d, j, host, &mut notes));
    }
    let ccache = ccache_dir(host.poudriere_conf, poudriere_d, jail, tree, set)
        .is_some_and(|dir| Path::new(&dir).is_dir());
    let staged = layer_make_conf(
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

    let blacklist = Blacklist::load_emulating(poudriere_d, jail, tree_name, set, emulating);

    let default_all = flavor_default_all(host.poudriere_conf, poudriere_d, jail, tree, set);
    if default_all {
        notes.push(
            "FLAVOR_DEFAULT_ALL=yes: a bare origin listed stands for every flavor".to_string(),
        );
    }

    Ok(Settings {
        blacklist,
        flavor_default_all: default_all,
        ..Settings::new(portsdir, options_dir, staged, env, notes)
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
fn jail_system(
    poudriere_d: &Path,
    name: &str,
    host: &HostPaths,
    notes: &mut Vec<String>,
) -> String {
    let dir = poudriere_d.join("jails").join(name);
    let read = |file: &str| {
        fs::read_to_string(dir.join(file))
            .ok()
            .map(|s| s.trim().to_string())
    };
    let (Some(arch), Some(version)) = (read("arch"), read("version")) else {
        notes.push(format!(
            "jail {name}: no arch or version in {}; queries see this host's system",
            dir.display()
        ));
        return String::new();
    };
    // `${version% *}`: a version may carry its osversion after a space.
    let release = version
        .rsplit_once(' ')
        .map_or(version.as_str(), |(r, _)| r);
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
            let from =
                param_h.map_or_else(|| "no mnt".to_string(), |p| format!("no {}", p.display()));
            notes.push(format!(
                "jail {name}: OSVERSION {v} derived from its version {version} ({from})"
            ));
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
    out.push_str(&format!(
        "_OSRELEASE={release}\nOSREL={osrel}\nOSVERSION={osversion}\n"
    ));
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
        bail!(
            "poudriere tree '{tree}' points to {} which is not a ports tree",
            p.display()
        );
    }
    if tree_explicit {
        bail!(
            "no poudriere tree '{tree}' ({} does not exist)",
            mnt.display()
        );
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
    v.extend(
        layer_stems(jail, Some(tree), set)
            .into_iter()
            .map(|s| format!("{s}-make.conf")),
    );
    v
}

/// The value poudriere.conf gives `name`, None when unset or empty. The
/// files are read in poudriere(8)'s own order (`include_poudriere_confs` in
/// its common.sh): its poudriere.conf, poudriere.d's, then the set, tree and
/// jail ones of poudriere.d for what the command line names. A tree counts
/// only when `-p` gave it: poudriere takes the names from its arguments, not
/// from defaults.
///
/// The files are shell, sourced in turn, so the last assignment wins and an
/// empty value unsets. Only plain assignment lines are read (`export` and
/// quotes allowed); a value set by shell logic is not seen, but nothing runs.
fn poudriere_conf_value(
    poudriere_conf: &Path,
    poudriere_d: &Path,
    jail: Option<&str>,
    tree: Option<&str>,
    set: Option<&str>,
    name: &str,
) -> Option<String> {
    let mut files = vec![
        poudriere_conf.to_path_buf(),
        poudriere_d.join("poudriere.conf"),
    ];
    files.extend(
        poudriere_conf_layers(jail, tree, set)
            .into_iter()
            .map(|layer| poudriere_d.join(format!("{layer}-poudriere.conf"))),
    );
    let mut value = None;
    for file in files {
        // Bytes, as sh sources it: a Latin-1 comment drops nothing.
        let Some(text) = read_lossy(&file) else {
            continue;
        };
        for line in text.lines() {
            if let Some(v) = shell_assignment(line, name) {
                value = (!v.is_empty()).then(|| v.to_string());
            }
        }
    }
    value
}

/// Does poudriere.conf turn `PACKAGE_BUILDING` off
/// ([`poudriere_conf_value`])?
fn no_package_building(
    poudriere_conf: &Path,
    poudriere_d: &Path,
    jail: Option<&str>,
    tree: Option<&str>,
    set: Option<&str>,
) -> bool {
    poudriere_conf_value(
        poudriere_conf,
        poudriere_d,
        jail,
        tree,
        set,
        "NO_PACKAGE_BUILDING",
    )
    .is_some()
}

/// Does poudriere.conf set `FLAVOR_DEFAULT_ALL=yes`, which makes poudriere
/// build every flavor of a bare origin listed (common.sh ~7037, ~8573,
/// ~11233)? Only the exact value `yes` counts.
fn flavor_default_all(
    poudriere_conf: &Path,
    poudriere_d: &Path,
    jail: Option<&str>,
    tree: Option<&str>,
    set: Option<&str>,
) -> bool {
    poudriere_conf_value(
        poudriere_conf,
        poudriere_d,
        jail,
        tree,
        set,
        "FLAVOR_DEFAULT_ALL",
    )
    .as_deref()
        == Some("yes")
}

/// poudriere.conf's `CCACHE_DIR`: what `setup_ccache` (common.sh:3591-3600)
/// checks with `[ -d "${CCACHE_DIR:-/nonexistent}" ]` before appending its
/// two lines to the jail's make.conf.
fn ccache_dir(
    poudriere_conf: &Path,
    poudriere_d: &Path,
    jail: Option<&str>,
    tree: Option<&str>,
    set: Option<&str>,
) -> Option<String> {
    poudriere_conf_value(poudriere_conf, poudriere_d, jail, tree, set, "CCACHE_DIR")
}

/// `path`'s text, bytes that are no UTF-8 replaced; None when it cannot be
/// read.
fn read_lossy(path: &Path) -> Option<String> {
    fs::read(path)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Every poudriere.conf layer and blacklist file this build reads, checked
/// readable: poudriere, run as root, reads every one that exists (its only
/// test is `[ -r ]`), so one optique cannot read stops the run, as an
/// unreadable make.conf layer does, rather than leave its settings out.
fn readable(
    poudriere_conf: &Path,
    poudriere_d: &Path,
    jail: Option<&str>,
    tree: Option<&str>,
    set: Option<&str>,
    tree_name: &str,
    emulating: bool,
) -> Result<()> {
    let mut files = vec![
        poudriere_conf.to_path_buf(),
        poudriere_d.join("poudriere.conf"),
    ];
    files.extend(
        poudriere_conf_layers(jail, tree, set)
            .into_iter()
            .map(|layer| poudriere_d.join(format!("{layer}-poudriere.conf"))),
    );
    files.extend(
        blacklist_layers(jail, tree_name, set, emulating)
            .into_iter()
            .map(|name| poudriere_d.join(name)),
    );
    // A directory in a file's place is no file poudriere sources either.
    for file in files.into_iter().filter(|f| !f.is_dir()) {
        match fs::read(&file) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
        }
    }
    Ok(())
}

/// The per-name poudriere.conf layers, in poudriere(8)'s reading order:
/// `<set>`, `<tree>`, `<jail>`, `<tree>-<set>`, `<jail>-<tree>`,
/// `<jail>-<set>`, `<jail>-<tree>-<set>`, each only when its names are given.
fn poudriere_conf_layers(jail: Option<&str>, tree: Option<&str>, set: Option<&str>) -> Vec<String> {
    layer_stems(jail, tree, set)
}

/// The value a plain shell assignment line gives `name`, if it is one:
/// `NAME=value`, `export NAME=value`, quoted or not, a trailing comment
/// dropped ([`comment_start`]). A commented-out line is no assignment.
fn shell_assignment<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let line = line.trim();
    let line = line
        .strip_prefix("export ")
        .map(str::trim_start)
        .unwrap_or(line);
    let value = line.strip_prefix(name)?.strip_prefix('=')?;
    let value = match comment_start(value) {
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

/// Where a trailing comment starts in `value`, as sh reads it: a `#` after
/// a space or a tab, outside quotes (a `\` in double quotes, or outside
/// any, escaping the next character).
fn comment_start(value: &str) -> Option<usize> {
    let (mut quote, mut escaped, mut blank) = (None, false, false);
    for (i, c) in value.char_indices() {
        let literal = escaped;
        if escaped {
            escaped = false;
        } else if quote == Some('\'') {
            if c == '\'' {
                quote = None;
            }
        } else if c == '\\' {
            escaped = true;
        } else if quote == Some('"') {
            if c == '"' {
                quote = None;
            }
        } else if c == '"' || c == '\'' {
            quote = Some(c);
        } else if c == '#' && blank {
            return Some(i);
        }
        blank = quote.is_none() && !literal && (c == ' ' || c == '\t');
    }
    None
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
) -> Result<StagedConf> {
    let mut paths: Vec<PathBuf> = make_conf_layers(jail, tree, set)
        .iter()
        .map(|n| poudriere_d.join(n))
        .collect();
    paths.extend(plugin_make_confs(poudriere_d));
    concat_make_conf(&paths, env, overlays, jail_lines, ccache, staging_dir)
}

/// The hook plugins' make.conf files poudriere appends after the layers
/// (`setup_makeconf` in common.sh): `hooks/plugins/*/make.conf`, in the
/// shell glob's order (names sorted, dot entries left out); only a regular
/// file counts, as for its `[ -f ]`.
fn plugin_make_confs(poudriere_d: &Path) -> Vec<PathBuf> {
    let Ok(dir) = fs::read_dir(poudriere_d.join("hooks/plugins")) else {
        return Vec::new();
    };
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
    let (path, _, _) = concat_make_conf(
        &[],
        QueryEnv::Jail {
            package_building: true,
        },
        &[],
        "",
        false,
        staging_dir,
    )?;
    Ok(path.expect("the jail environment is never empty"))
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
) -> Result<StagedConf> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::resolve_host;
    use crate::config::testutil::*;

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
        assert_eq!(
            options_dir_candidates(None, "t", None),
            vec!["t-options", "options"]
        );
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
        let req = Request {
            jail: Some("j"),
            ..Request::default()
        };
        let staged = |pd: &Path| {
            let s = resolve_host(&host(tmp.path(), pd, &ports, tmp.path()), &req, &stage).unwrap();
            (
                fs::read_to_string(s.make_conf.unwrap()).unwrap(),
                s.notes,
                s.conf_hash,
            )
        };

        let pd = fake_jail(tmp.path(), "amd64", "11.2-RELEASE", Some("1102000"));
        let (conf, notes, hash) = staged(&pd);
        let os = "_OSRELEASE=11.2-RELEASE\nOSREL=11.2\nOSVERSION=1102000\n";
        assert!(conf.contains(os) && !conf.contains("MACHINE"), "{conf}");
        assert!(
            conf.find(os).unwrap() < conf.find("LOCALBASE").unwrap(),
            "{conf}"
        );
        assert!(notes.is_empty(), "{notes:?}");

        let pd = fake_jail(tmp.path(), "amd64", "14.3-RELEASE-p2 1403000", None);
        let (conf, notes, other) = staged(&pd);
        assert!(
            conf.contains("_OSRELEASE=14.3-RELEASE-p2\nOSREL=14.3\nOSVERSION=1403000\n"),
            "{conf}"
        );
        assert!(
            notes
                .iter()
                .any(|n| n.contains("OSVERSION 1403000 derived")),
            "{notes:?}"
        );
        assert_ne!(hash, other, "the jail's system keys its own generation");

        for (arch, lines) in [
            (
                "i386",
                "MACHINE=i386\nMACHINE_ARCH=i386\nARCH=${MACHINE_ARCH}\n",
            ),
            (
                "arm64.aarch64",
                "MACHINE=arm64\nMACHINE_ARCH=aarch64\nARCH=${MACHINE_ARCH}\n",
            ),
        ] {
            let pd = fake_jail(tmp.path(), arch, "14.3-RELEASE", Some("1403000"));
            let (conf, _, _) = staged(&pd);
            assert!(
                conf.contains(&format!("{lines}_OSRELEASE=14.3-RELEASE\n")),
                "{arch}: {conf}"
            );
        }

        let pd = fake_jail(tmp.path(), "amd64", "14.3-RELEASE", None);
        fs::remove_file(pd.join("jails/j/version")).unwrap();
        let (conf, notes, _) = staged(&pd);
        assert!(!conf.contains("OSREL"), "{conf}");
        assert!(
            notes.iter().any(|n| n.contains("this host's system")),
            "{notes:?}"
        );
        fs::remove_dir_all(pd.join("jails")).unwrap();
        let (conf, notes, _) = staged(&pd);
        assert!(
            !conf.contains("OSREL") && notes.len() == 1,
            "{conf}{notes:?}"
        );
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
        let req = Request {
            jail: Some("j"),
            ..Request::default()
        };
        let host = HostPaths {
            arch: "arm64.aarch64",
            ..host(tmp.path(), tmp.path(), &ports, tmp.path())
        };
        let staged = |host: &HostPaths, pd: &Path| {
            let s = resolve_host(
                &HostPaths {
                    poudriere_d: pd,
                    ..*host
                },
                &req,
                &stage,
            )
            .unwrap();
            fs::read_to_string(s.make_conf.unwrap()).unwrap()
        };

        let pd = fake_jail(tmp.path(), "arm.armv7", "14.3-RELEASE", Some("1403000"));
        let conf = staged(&host, &pd);
        assert!(
            conf.contains("MACHINE=arm\nMACHINE_ARCH=armv7\nARCH=${MACHINE_ARCH}\n"),
            "{conf}"
        );

        let pd = fake_jail(tmp.path(), "amd64", "14.3-RELEASE", Some("1403000"));
        let amd64_host = HostPaths {
            arch: "amd64",
            ..host
        };
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
        let s = resolve_host(
            &host(tmp.path(), &pd, &ports, tmp.path()),
            &Request::default(),
            &stage,
        )
        .unwrap();
        let conf = fs::read_to_string(s.make_conf.unwrap()).unwrap();
        let at = |s: &str| {
            conf.find(s)
                .unwrap_or_else(|| panic!("{s} missing: {conf}"))
        };
        assert!(
            at("LAYER=1") < at("A=1") && at("A=1") < at("B=1") && at("B=1") < at("LOCALBASE"),
            "{conf}"
        );
        assert!(!conf.contains("H=1"), "{conf}");
        let want = [
            pd.join("make.conf"),
            plugins.join("a/make.conf"),
            plugins.join("b/make.conf"),
        ];
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
        let req = Request {
            jail: Some("j"),
            ..Request::default()
        };
        let s = resolve_host(&host(tmp.path(), &pd, &ports, tmp.path()), &req, &stage).unwrap();
        let out = std::process::Command::new("/usr/bin/make")
            .args([
                "-C",
                "/usr/ports/security/ca_root_nss",
                "-V",
                "COMPLETE_OPTIONS_LIST",
                "-V",
                "OSVERSION",
            ])
            .arg(format!("__MAKE_CONF={}", s.make_conf.unwrap().display()))
            .env_remove("MAKEFLAGS")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            text,
            "ETCSYMLINK\n1102000\n",
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
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

        let s = resolve_host(
            &host,
            &Request {
                tree: Some("main"),
                ..none
            },
            &stage,
        )
        .unwrap();
        assert_eq!(s.portsdir, named);
        let err = resolve_host(
            &host,
            &Request {
                tree: Some("mian"),
                ..none
            },
            &stage,
        )
        .unwrap_err()
        .to_string();
        assert!(err.starts_with("no poudriere tree 'mian'"), "{err}");
        // No -p and no default tree: the fallback.
        assert_eq!(resolve_host(&host, &none, &stage).unwrap().portsdir, ports);

        // -P: taken as given, but checked.
        let s = resolve_host(
            &host,
            &Request {
                portsdir: Some(&named),
                ..none
            },
            &stage,
        )
        .unwrap();
        assert_eq!(s.portsdir, named);
        assert!(
            matches!(s.env, QueryEnv::Jail { .. }),
            "still a poudriere build"
        );
        let err = resolve_host(
            &host,
            &Request {
                portsdir: Some(&db),
                ..none
            },
            &stage,
        )
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
        fs::write(
            pd.join("ports/main/mnt"),
            format!("{}\n", old_mnt.display()),
        )
        .unwrap();
        let host = host(tmp.path(), &pd, &ports, &db);

        let err = resolve_host(
            &host,
            &Request {
                tree: Some("main"),
                ..Request::default()
            },
            &stage,
        )
        .unwrap_err()
        .to_string();
        assert_eq!(
            err,
            format!(
                "poudriere tree 'main' points to {} which is not a ports tree",
                old_mnt.display()
            )
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
        assert_eq!(
            v("# NO_PACKAGE_BUILDING=yes"),
            None,
            "the sample's commented line"
        );
        assert_eq!(v("NO_PACKAGE_BUILDING_X=yes"), None);
        assert_eq!(v("XNO_PACKAGE_BUILDING=yes"), None);
        // A tab starts a comment as a space does, as sh reads it.
        assert_eq!(v("NO_PACKAGE_BUILDING=yes\t# for this box"), Some("yes"));
        assert_eq!(v("NO_PACKAGE_BUILDING=\t# unset"), Some(""));
        // Inside quotes, or escaped, a `#` is the value's.
        assert_eq!(
            v("NO_PACKAGE_BUILDING=\"/tmp/my cache #1\""),
            Some("/tmp/my cache #1")
        );
        assert_eq!(v("NO_PACKAGE_BUILDING='a # b' # c"), Some("a # b"));
        assert_eq!(comment_start("a\\ #b"), None, "an escaped blank");
        assert_eq!(v("NO_PACKAGE_BUILDING=a#b"), Some("a#b"));
    }

    /// poudriere's order (include_poudriere_confs): names alone, then pairs,
    /// then all three; only the names given.
    #[test]
    fn poudriere_conf_layers_follow_poudriere() {
        assert_eq!(
            poudriere_conf_layers(Some("j"), Some("t"), Some("s")),
            ["s", "t", "j", "t-s", "j-t", "j-s", "j-t-s"]
        );
        assert_eq!(
            poudriere_conf_layers(Some("j"), None, Some("s")),
            ["s", "j", "j-s"]
        );
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
        fs::write(
            &conf,
            "# NO_PACKAGE_BUILDING=yes\nBASEFS=/usr/local/poudriere\n",
        )
        .unwrap();
        assert!(!npb(None, None, None));
        fs::write(&conf, "NO_PACKAGE_BUILDING=yes\n").unwrap();
        assert!(npb(None, None, None));
        fs::write(pd.join("poudriere.conf"), "NO_PACKAGE_BUILDING=\n").unwrap();
        assert!(!npb(None, None, None), "poudriere.d's unsets it");
        fs::write(pd.join("s-poudriere.conf"), "NO_PACKAGE_BUILDING=yes\n").unwrap();
        assert!(npb(None, None, Some("s")));
        assert!(!npb(None, None, Some("other")));
        fs::write(pd.join("j-s-poudriere.conf"), "NO_PACKAGE_BUILDING=\n").unwrap();
        assert!(
            !npb(Some("j"), None, Some("s")),
            "the jail-set file comes last"
        );
        fs::write(
            pd.join("default-poudriere.conf"),
            "NO_PACKAGE_BUILDING=yes\n",
        )
        .unwrap();
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
        fs::write(
            pd.join("j-poudriere.conf"),
            "export FLAVOR_DEFAULT_ALL=yes\n",
        )
        .unwrap();
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
        let host = HostPaths {
            poudriere_conf: &conf,
            ..host(tmp.path(), &pd, &ports, &db)
        };

        let on = resolve(&host, &Request::default(), &[], &stage).unwrap();
        assert_eq!(
            on.env,
            QueryEnv::Jail {
                package_building: true
            }
        );
        fs::write(&conf, "NO_PACKAGE_BUILDING=yes\n").unwrap();
        let off = resolve(&host, &Request::default(), &[], &stage).unwrap();
        assert_eq!(
            off.env,
            QueryEnv::Jail {
                package_building: false
            }
        );
        let text = fs::read_to_string(off.make_conf.unwrap()).unwrap();
        assert!(!text.contains("PACKAGE_BUILDING"), "{text}");
        assert!(
            text.contains("BATCH=yes") && text.contains("LOCALBASE=/nonexistent"),
            "{text}"
        );
        assert_ne!(on.conf_hash, off.conf_hash);
    }

    /// poudriere writes FORCE_PACKAGE=yes before the layers unless
    /// poudriere.conf sets NO_FORCE_PACKAGE: without it, PACKAGE_BUILDING
    /// turns a port's NO_PACKAGE into an IGNORE.
    #[test]
    fn force_package_unless_no_force_package() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        fs::create_dir_all(ports.join("Mk")).unwrap();
        fs::write(ports.join("Mk/bsd.port.mk"), "# fake\n").unwrap();
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        fs::write(pd.join("make.conf"), "LAYER=yes\n").unwrap();
        let conf = tmp.path().join("poudriere.conf");
        let db = tmp.path().join("db");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let host = HostPaths {
            poudriere_conf: &conf,
            ..host(tmp.path(), &pd, &ports, &db)
        };
        let text = |s: &Settings| fs::read_to_string(s.make_conf.as_ref().unwrap()).unwrap();
        let on = text(&resolve(&host, &Request::default(), &[], &stage).unwrap());
        let at = on.find("FORCE_PACKAGE=yes\n").expect(&on);
        assert!(at < on.find("LAYER=yes").unwrap(), "{on}");
        // A Latin-1 comment does not hide the setting, as sh sources bytes.
        fs::write(&conf, b"# St\xe9phane's box\nNO_FORCE_PACKAGE=yes\n").unwrap();
        let off = text(&resolve(&host, &Request::default(), &[], &stage).unwrap());
        assert!(!off.contains("FORCE_PACKAGE"), "{off}");
    }

    /// A poudriere.conf layer or blacklist that exists and cannot be read
    /// stops the run: poudriere, as root, would read it.
    #[test]
    fn an_unreadable_poudriere_conf_layer_stops_the_run() {
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
        let host = HostPaths {
            poudriere_conf: &conf,
            ..host(tmp.path(), &pd, &ports, &db)
        };
        use std::os::unix::fs::PermissionsExt as _;
        let blacklist = pd.join("blacklist");
        fs::write(&blacklist, "devel/papi\n").unwrap();
        fs::set_permissions(&blacklist, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&blacklist).is_ok() {
            return; // root reads it anyway, as poudriere does
        }
        let err = resolve(&host, &Request::default(), &[], &stage).unwrap_err();
        assert!(format!("{err:#}").contains("blacklist"), "{err:#}");
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
        assert_eq!(
            resolve_portsdir(&pd, Path::new("/nonexistent"), "mytree", true).unwrap(),
            tree
        );
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
        let (path, _, sources) = layer_make_conf(
            &pd,
            Some("j1"),
            "t1",
            Some("s1"),
            QueryEnv::Host,
            &[],
            "",
            false,
            &stage,
        )
        .unwrap();
        assert_eq!(sources.len(), 8);
        let text = fs::read_to_string(path.unwrap()).unwrap();
        let idx = |needle: &str| text.find(needle).unwrap();
        let order: Vec<usize> = layers.iter().map(|(_, c)| idx(c)).collect();
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "layers out of order: {text}"
        );
        assert!(
            !text.contains("PACKAGE_BUILDING") && !text.contains("LOCALBASE"),
            "{text}"
        );
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
        let (path, _, sources) = layer_make_conf(
            &pd,
            Some("j1"),
            "t1",
            None,
            QueryEnv::Host,
            &[],
            "",
            false,
            &stage,
        )
        .unwrap();
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
        let (path, _, _) = layer_make_conf(
            &pd,
            None,
            "t1",
            Some("s1"),
            QueryEnv::Host,
            &[],
            "",
            false,
            &stage,
        )
        .unwrap();
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
        let (path, jail_hash, _) = layer_make_conf(
            &pd,
            None,
            "t1",
            None,
            QueryEnv::Jail {
                package_building: true,
            },
            &[],
            "",
            false,
            &stage,
        )
        .unwrap();
        let text = fs::read_to_string(path.unwrap()).unwrap();
        // poudriere's own lines come before the layers, as in a jail...
        assert!(text.find("PACKAGE_BUILDING=yes").unwrap() < text.find("/opt/local").unwrap());
        // ...and the hidden LOCALBASE after them, so no layer can undo it.
        assert!(
            text.trim_end().ends_with("LOCALBASE=/nonexistent"),
            "{text}"
        );

        // The environment keys its own cache generation.
        let (_, host_hash, _) = layer_make_conf(
            &pd,
            None,
            "t1",
            None,
            QueryEnv::Host,
            &[],
            "",
            false,
            &stage,
        )
        .unwrap();
        assert_ne!(jail_hash, host_hash);

        // An empty layering still stages a make.conf in a jail's environment.
        let empty = tmp.path().join("empty.d");
        fs::create_dir_all(&empty).unwrap();
        let (path, _, sources) = layer_make_conf(
            &empty,
            None,
            "t1",
            None,
            QueryEnv::Jail {
                package_building: true,
            },
            &[],
            "",
            false,
            &stage,
        )
        .unwrap();
        assert!(sources.is_empty());
        assert!(path.is_some());
        let (path, _, _) = layer_make_conf(
            &empty,
            None,
            "t1",
            None,
            QueryEnv::Host,
            &[],
            "",
            false,
            &stage,
        )
        .unwrap();
        assert!(path.is_none());
    }

    /// POUDRIERE_ETC, when set and non-empty, must be a directory, made
    /// absolute; the banner says where poudriere's configuration was read.
    #[test]
    fn poudriere_etc_must_be_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(poudriere_etc(None).unwrap(), None);
        assert_eq!(poudriere_etc(Some("".into())).unwrap(), None);
        assert_eq!(
            poudriere_etc(Some(tmp.path().into())).unwrap().as_deref(),
            Some(tmp.path())
        );
        let file = tmp.path().join("file");
        fs::write(&file, "").unwrap();
        for bad in [file, tmp.path().join("absent")] {
            let err = poudriere_etc(Some(bad.clone().into()))
                .unwrap_err()
                .to_string();
            assert_eq!(
                err,
                format!("POUDRIERE_ETC {} is not a directory", bad.display())
            );
        }
        assert!(
            poudriere_etc(Some(".".into()))
                .unwrap()
                .unwrap()
                .is_absolute()
        );
        let note = poudriere_etc_note(Path::new("/e"), true);
        assert_eq!(
            note,
            "poudriere.conf and poudriere.d read from /e (POUDRIERE_ETC)"
        );
        let note = poudriere_etc_note(Path::new("/e"), false);
        assert_eq!(note, "POUDRIERE_ETC /e has no poudriere.d: a local build");
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
        let at = |needle: &str| {
            present
                .find(needle)
                .unwrap_or_else(|| panic!("{needle}: {present}"))
        };
        assert!(
            at("PACKAGE_BUILDING=yes") < at("WITH_CCACHE_BUILD=yes\nCCACHE_DIR=/root/.ccache\n"),
            "{present}"
        );
        assert!(
            at("CCACHE_DIR=/root/.ccache\n") < at("LOCALBASE=/nonexistent"),
            "{present}"
        );
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
        let req = Request {
            jail: Some("j"),
            ..Request::default()
        };
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
        let s = resolve_host(
            &HostPaths {
                poudriere_d: &pd,
                ..host
            },
            &req,
            &stage,
        )
        .unwrap();
        assert!(s.blacklist.matches(&key, false), "{:?}", s.blacklist);

        // A host whose kernel also runs aarch64 natively: no emulation, so
        // qemu-blacklist is never even looked at.
        let native = HostPaths {
            supported_archs: "amd64 i386 aarch64",
            ..host
        };
        let s = resolve_host(
            &HostPaths {
                poudriere_d: &pd,
                ..native
            },
            &req,
            &stage,
        )
        .unwrap();
        assert!(!s.blacklist.matches(&key, false), "{:?}", s.blacklist);
    }

    #[test]
    fn blacklist_layers_and_matching() {
        let tmp = tempfile::tempdir().unwrap();
        let pd = tmp.path().join("poudriere.d");
        fs::create_dir_all(&pd).unwrap();
        // A line may hold several origins, split on whitespace, and '#'
        // comments the rest of the line away — no globs anywhere.
        fs::write(
            pd.join("blacklist"),
            "# comment\n\nwww/nginx mail/dovecot\n",
        )
        .unwrap();
        fs::write(
            pd.join("s1-blacklist"),
            "devel/py-foo@py312   # one flavor only\n",
        )
        .unwrap();

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
        fs::write(
            pd.join("blacklist"),
            "devel/py-foo@- net/bar@all devel/py-baz@py312\n",
        )
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
        assert_eq!(
            blacklist_layers(None, "t", None, false),
            vec!["blacklist", "t-blacklist"]
        );
        // Emulating always loads qemu-blacklist too, and last.
        assert_eq!(
            blacklist_layers(None, "t", None, true),
            vec!["blacklist", "t-blacklist", "qemu-blacklist"]
        );
    }
}
