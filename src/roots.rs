//! Roots as the build tool reads them, before the closure is scanned:
//! `origin@all` stands for every flavor of the port and `origin@-` for its
//! default flavor, as poudriere(8) reads them in a list (common.sh ~7026,
//! ~8531-8537, ~8714-8724: `FLAVOR_ALL`, `FLAVOR_DEFAULT`).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::cache::Cache;
use crate::model::origin::PortKey;
use crate::moved::{Moved, MovedResult};
use crate::optfiles::OptionsFiles;
use crate::query::makerunner::{MakeRunner, QueryCtx, ScanEvent};

/// The flavor that names every flavor of a port (poudriere's `FLAVOR_ALL`).
pub const FLAVOR_ALL: &str = "all";
/// The flavor that names the default one, the bare origin (poudriere's
/// `FLAVOR_DEFAULT`).
pub const FLAVOR_DEFAULT: &str = "-";

/// Does `key` stand for every flavor of its port? `origin@all` does, and
/// under `default_all` (poudriere.conf's FLAVOR_DEFAULT_ALL) a bare origin
/// too, but never `origin@-`: poudriere sees a flavor given there.
fn wants_all(key: &PortKey, default_all: bool) -> bool {
    key.flavor.as_deref() == Some(FLAVOR_ALL) || (default_all && key.flavor.is_none())
}

/// `roots` with each `origin@-` replaced by the bare origin and each root
/// that [`wants_all`] by one root per flavor in its port's FLAVORS
/// (`flavors`, by origin), in that order, or by the bare origin when the
/// port has none or its FLAVORS are unknown (a failed query, which the
/// scan then reports). Duplicates collapse, first kept.
pub fn expand(
    roots: &[PortKey],
    flavors: &HashMap<String, Vec<String>>,
    default_all: bool,
) -> Vec<PortKey> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for key in roots {
        let bare = || PortKey::new(key.origin.clone(), None);
        let keys = if key.flavor.as_deref() == Some(FLAVOR_DEFAULT) {
            vec![bare()]
        } else if wants_all(key, default_all) {
            match flavors.get(&key.origin).filter(|f| !f.is_empty()) {
                Some(fl) => fl.iter().map(|f| PortKey::new(key.origin.clone(), Some(f.clone()))).collect(),
                None => vec![bare()],
            }
        } else {
            vec![key.clone()]
        };
        for k in keys {
            if seen.insert(k.clone()) {
                out.push(k);
            }
        }
    }
    out
}

/// The FLAVORS of each of `origins`, from its bare origin's query (the
/// cache first, as the scan reads it, and filled for the scan to reuse).
/// An origin whose query fails is left out.
pub fn query_flavors(
    origins: &BTreeSet<String>,
    ctx: &QueryCtx,
    files: &OptionsFiles,
    jobs: usize,
    cache: &mut Cache,
) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    let mut missing = Vec::new();
    for origin in origins {
        let key = PortKey::new(origin.clone(), None);
        match cache.lookup(&key, files) {
            Some(info) => {
                out.insert(origin.clone(), info.flavors);
            }
            None => missing.push(key),
        }
    }
    if missing.is_empty() {
        return out;
    }
    let runner = MakeRunner::new(ctx.clone(), jobs.min(missing.len()));
    let pending = missing.len();
    for key in missing {
        runner.submit(key);
    }
    for _ in 0..pending {
        match runner.events.recv() {
            Ok(ScanEvent::PortDone(info)) => {
                cache.insert(&info, files);
                out.insert(info.key.origin.clone(), info.flavors.clone());
            }
            Ok(ScanEvent::PortError { .. }) => {}
            Err(_) => break,
        }
    }
    runner.shutdown();
    out
}

/// `roots` with each one that [`wants_all`] mapped through MOVED first, as
/// poudriere applies MOVED to the listed originspec before FLAVOR_ALL and
/// FLAVOR_DEFAULT_ALL expand it (`_check_moved`, common.sh ~9627-9636,
/// ~9936-9966: `old@all` becomes `new@all`, a bare old origin the bare new
/// one). Without this, `@all` and a bare origin under FLAVOR_DEFAULT_ALL
/// would query the FLAVORS of the old origin, which has no directory, and
/// fall back to its default flavor alone. A root the chase cannot place in
/// the tree (removed, or a cycle) is left as given, for the scan to report.
fn apply_moved(
    roots: &[PortKey],
    default_all: bool,
    moved: &Moved,
    exists: impl Fn(&str) -> bool,
) -> Vec<PortKey> {
    roots
        .iter()
        .map(|key| {
            if !wants_all(key, default_all) {
                return key.clone();
            }
            match moved.resolve(&key.origin, &exists) {
                MovedResult::MovedTo { origin, .. } => PortKey::new(origin, key.flavor.clone()),
                _ => key.clone(),
            }
        })
        .collect()
}

/// `roots` as the scan should take them, each [`wants_all`] one [`apply_moved`]ed
/// first, [`expand`]ed, the FLAVORS of the ports that need them queried in
/// between.
pub fn resolve(
    roots: &[PortKey],
    default_all: bool,
    ctx: &QueryCtx,
    files: &OptionsFiles,
    jobs: usize,
    cache: &mut Cache,
    moved: &Moved,
) -> Vec<PortKey> {
    let exists = |o: &str| ctx.locate(o).is_some();
    let roots = apply_moved(roots, default_all, moved, exists);
    let origins: BTreeSet<String> =
        roots.iter().filter(|k| wants_all(k, default_all)).map(|k| k.origin.clone()).collect();
    let flavors = if origins.is_empty() {
        HashMap::new()
    } else {
        query_flavors(&origins, ctx, files, jobs, cache)
    };
    expand(&roots, &flavors, default_all)
}

/// One line's `SUBDIR` entry, the way synth's `grep_Makefile` regex reads
/// it (portscan.adb ~1266:
/// `^SUBDIR[[:space:]]*[:+:]=[[:space:]]*([[:graph:]]*)`, applied to the
/// line already trimmed): `SUBDIR`, optional whitespace, one `:` or `+`,
/// `=`, optional whitespace, then the first run of non-space graphic
/// characters — a second name on the same line is never read.
fn subdir_entry(line: &str) -> Option<&str> {
    let is_blank = |c: char| c == ' ' || c == '\t';
    let rest = line.trim().strip_prefix("SUBDIR")?.trim_start_matches(is_blank);
    let rest = rest.strip_prefix(['+', ':'])?.strip_prefix('=')?.trim_start_matches(is_blank);
    let name = rest.split_whitespace().next().unwrap_or("");
    (!name.is_empty()).then_some(name)
}

/// The categories synth's prescan reads from one ports tree, the way
/// `prescan_ports_tree` lists them (portscan.adb ~1045): every directory
/// entry starting with a lowercase ASCII letter, except the ports
/// framework's own `base`, `distfiles` and `packages`. Sorted, as synth's
/// own walk is.
fn synth_categories(portsdir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(portsdir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.chars().next().is_some_and(|c| c.is_ascii_lowercase()))
        .filter(|name| !matches!(name.as_str(), "base" | "distfiles" | "packages"))
        .collect();
    out.sort();
    out
}

/// synth's flavor index, built the way `prescan_ports_tree` /
/// `grep_Makefile` build it (portscan.adb ~1045, ~1266): `category/name`
/// for every `SUBDIR` entry of every category Makefile under `portsdir` and
/// each of `overlays` (optique's own notion of the tree; synth itself knows
/// none). A category with no Makefile of its own is not read — synth's
/// `walk_all_subdirectories` fallback for that case is outside this scan's
/// scope. An origin absent from this index is "missing from ports" to
/// synth, whatever is on disk at its path.
pub fn synth_index(portsdir: &Path, overlays: &[PathBuf]) -> HashSet<String> {
    let mut out = HashSet::new();
    for tree in overlays.iter().map(PathBuf::as_path).chain(std::iter::once(portsdir)) {
        for category in synth_categories(tree) {
            let Ok(text) = std::fs::read_to_string(tree.join(&category).join("Makefile")) else {
                continue;
            };
            for line in text.lines() {
                if let Some(name) = subdir_entry(line) {
                    out.insert(format!("{category}/{name}"));
                }
            }
        }
    }
    out
}

/// The installed packages synth roots (`installed`: origin and flavor
/// annotation of each), and those it skips with why. synth matches each
/// against its flavor index, where a port without FLAVORS is its bare
/// origin and a flavored one is listed only as `origin@flavor`
/// (portscan-pilot.adb ~1137-1173, portscan.adb ~1726-1762): an origin not
/// in [`synth_index`] is skipped as "missing from ports" — absent from
/// synth's own index, not merely a directory without a Makefile; a port
/// without FLAVORS is rooted as its bare origin, whatever the annotation;
/// a flavored port only when the annotation names one of its FLAVORS.
/// `exists` says whether the index has an origin, `flavors` gives the
/// FLAVORS of those queried; a port whose FLAVORS are unknown (a failed
/// query) is kept as given for the scan to report.
pub fn synth_installed(
    installed: &[PortKey],
    exists: impl Fn(&str) -> bool,
    flavors: &HashMap<String, Vec<String>>,
) -> (Vec<PortKey>, Vec<(PortKey, &'static str)>) {
    let mut kept = Vec::new();
    let mut seen = HashSet::new();
    let mut skipped = Vec::new();
    for key in installed {
        let root = if !exists(&key.origin) {
            Err("missing from ports")
        } else {
            match flavors.get(&key.origin) {
                None => Ok(key.clone()),
                Some(fl) if fl.is_empty() => Ok(PortKey::new(key.origin.clone(), None)),
                Some(fl) => match &key.flavor {
                    Some(f) if fl.contains(f) => Ok(key.clone()),
                    _ => Err("package unmatched"),
                },
            }
        };
        match root {
            Ok(k) => {
                if seen.insert(k.clone()) {
                    kept.push(k);
                }
            }
            Err(why) => skipped.push((key.clone(), why)),
        }
    }
    (kept, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// synth's flavor index comes from each category Makefile's `SUBDIR`
    /// entries (`prescan_ports_tree` / `grep_Makefile`, portscan.adb ~1045,
    /// ~1266), not merely a directory with a Makefile: a port present on
    /// disk but left out of its category's `SUBDIR` list is "missing from
    /// ports" to synth, exactly as one truly absent from the tree would be.
    #[test]
    fn synth_installed_checks_the_subdir_index_not_the_makefile() {
        let tmp = tempfile::tempdir().unwrap();
        let portsdir = tmp.path().join("ports");
        std::fs::create_dir_all(portsdir.join("misc/other")).unwrap();
        std::fs::create_dir_all(portsdir.join("misc/local")).unwrap();
        std::fs::write(portsdir.join("misc/Makefile"), "SUBDIR += other\n").unwrap();
        std::fs::write(portsdir.join("misc/other/Makefile"), "PKGNAME=other-1.0\n").unwrap();
        std::fs::write(portsdir.join("misc/local/Makefile"), "PKGNAME=local-1.0\n").unwrap();

        let index = synth_index(&portsdir, &[]);
        let exists = |o: &str| index.contains(o);
        let (kept, skipped) =
            synth_installed(&keys(&["misc/local", "misc/other"]), exists, &HashMap::new());
        assert_eq!(kept, keys(&["misc/other"]));
        let skipped: Vec<String> = skipped.iter().map(|(k, why)| format!("{k} {why}")).collect();
        assert_eq!(skipped, ["misc/local missing from ports"]);
    }

    /// synth roots an installed package only when its flavor index matches
    /// it: a stale flavor, a flavored port's unannotated package and an
    /// origin gone from the tree are skipped, an unflavored port is rooted
    /// bare whatever its annotation says.
    #[test]
    fn synth_installed_keeps_what_the_flavor_index_matches() {
        let flavors: HashMap<String, Vec<String>> = [
            ("misc/flav".to_string(), vec!["full".into(), "nox".into()]),
            ("misc/plain".to_string(), Vec::new()),
        ]
        .into();
        let exists = |o: &str| o != "misc/gone";
        let (kept, skipped) = synth_installed(
            &keys(&[
                "misc/flav@nox",
                "misc/flav@old",
                "misc/flav",
                "misc/plain@py39",
                "misc/plain",
                "misc/gone",
                "misc/broken@x",
            ]),
            exists,
            &flavors,
        );
        assert_eq!(kept, keys(&["misc/flav@nox", "misc/plain", "misc/broken@x"]));
        let skipped: Vec<String> = skipped.iter().map(|(k, why)| format!("{k} {why}")).collect();
        assert_eq!(
            skipped,
            [
                "misc/flav@old package unmatched",
                "misc/flav package unmatched",
                "misc/gone missing from ports",
            ]
        );
    }

    fn keys(specs: &[&str]) -> Vec<PortKey> {
        specs.iter().map(|s| PortKey::parse(s).unwrap()).collect()
    }

    #[test]
    fn all_expands_to_every_flavor_in_order() {
        let flavors: HashMap<String, Vec<String>> = [
            ("misc/flav".to_string(), vec!["full".into(), "nox".into(), "pgtk".into()]),
            ("misc/plain".to_string(), Vec::new()),
        ]
        .into();
        let got = expand(
            &keys(&["misc/flav@all", "misc/flav@nox", "misc/plain@all", "misc/gone@all", "misc/x"]),
            &flavors,
            false,
        );
        assert_eq!(
            got,
            keys(&["misc/flav@full", "misc/flav@nox", "misc/flav@pgtk", "misc/plain", "misc/gone", "misc/x"])
        );
        assert_eq!(expand(&keys(&["misc/flav@-", "misc/flav"]), &flavors, false), keys(&["misc/flav"]));
    }

    /// Under FLAVOR_DEFAULT_ALL a bare origin listed is every flavor of its
    /// port, as `@all` is; a flavor given, `@-` included, is that flavor.
    #[test]
    fn flavor_default_all_expands_bare_origins_only() {
        let flavors: HashMap<String, Vec<String>> = [
            ("misc/flav".to_string(), vec!["full".into(), "nox".into(), "pgtk".into()]),
            ("misc/plain".to_string(), Vec::new()),
        ]
        .into();
        let every = keys(&["misc/flav@full", "misc/flav@nox", "misc/flav@pgtk"]);
        assert_eq!(expand(&keys(&["misc/flav"]), &flavors, true), every);
        assert_eq!(expand(&keys(&["misc/flav"]), &flavors, false), keys(&["misc/flav"]));
        assert_eq!(expand(&keys(&["misc/flav@-"]), &flavors, true), keys(&["misc/flav"]));
        assert_eq!(expand(&keys(&["misc/flav@nox"]), &flavors, true), keys(&["misc/flav@nox"]));
        assert_eq!(expand(&keys(&["misc/plain"]), &flavors, true), keys(&["misc/plain"]));
        // Only the bare origin has its FLAVORS queried.
        let queried: Vec<String> = keys(&["misc/flav", "misc/flav@-", "misc/x@nox"])
            .iter()
            .filter(|k| wants_all(k, true))
            .map(|k| k.to_string())
            .collect();
        assert_eq!(queried, ["misc/flav"]);
    }

    /// `cat/old@all` and, under FLAVOR_DEFAULT_ALL, bare `cat/old` root
    /// every flavor of `cat/new` (MOVED `cat/old|cat/new|...`), not just its
    /// default one: `resolve` must apply MOVED to the originspec before
    /// querying FLAVORS, as poudriere's `_check_moved` does (common.sh
    /// ~9627-9636, ~9936-9966), since the old origin has no directory for
    /// the query to see.
    #[test]
    fn resolve_follows_moved_before_querying_flavors() {
        let _lock = crate::query::makerunner::make_test_lock();
        let tmp = fake_tree(&[("cat/new", "PKGNAME=new-1.0\nFLAVORS=\ta b\n")]);
        let ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().join("options"),
            overlays: Vec::new(),
        };
        let moved = Moved::parse("cat/old|cat/new|2026-01-01|renamed\n");
        let files = OptionsFiles::new(&ctx.port_dbdir);
        let mut cache = Cache::disabled();
        let roots = keys(&["cat/old@all", "cat/old"]);
        let got = resolve(&roots, true, &ctx, &files, 1, &mut cache, &moved);
        assert_eq!(got, keys(&["cat/new@a", "cat/new@b"]));
    }

    /// A ports tree of one-line Makefiles: make runs for real, but nothing
    /// outside the tempdir is read.
    fn fake_tree(ports: &[(&str, &str)]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for (origin, makefile) in ports {
            let dir = tmp.path().join("ports").join(origin);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("Makefile"), makefile).unwrap();
        }
        std::fs::create_dir_all(tmp.path().join("options")).unwrap();
        tmp
    }
}
