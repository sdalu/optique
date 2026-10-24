use std::collections::{BTreeMap, HashMap, HashSet};

use crate::cache::Cache;
use crate::config::Blacklist;
use crate::model::origin::PortKey;
use crate::model::port::PortInfo;
use crate::moved::{Moved, MovedResult};
use crate::optfiles::OptionsFiles;
use crate::query::makerunner::{MakeRunner, QueryCtx, ScanEvent};

/// Result of a (full or incremental) closure scan.
#[derive(Debug, Default)]
pub struct ScanResult {
    /// Ports keyed by canonical key (origin + resolved flavor).
    pub ports: BTreeMap<PortKey, PortInfo>,
    /// Requested key -> canonical key (for dedup of default-flavor aliases),
    /// and a MOVED root -> the key it moved to; `deps::resolve` follows
    /// the chain.
    pub aliases: HashMap<PortKey, PortKey>,
    /// (key, error): a port whose query failed, a root MOVED says is gone
    /// (both missing from `ports`), or a port in `ports` depending on an
    /// origin that does not exist, once per such dependency.
    pub errors: Vec<(PortKey, String)>,
    /// MOVED redirections applied to the roots (informational).
    pub moved_notes: Vec<String>,
    pub from_cache: usize,
    pub queried: usize,
}

pub struct ScanProgress {
    pub done: usize,
    pub discovered: usize,
    pub from_cache: usize,
}

/// Breadth-first scan of the dependency closure of `roots`.
///
/// The roots, the listed origins, are resolved through MOVED; dependencies
/// are not, as poudriere applies MOVED to listed origins only and refuses
/// a dependency on an origin that does not exist (see [`register`]). Every
/// target is deduplicated, looked up in the cache, and queried via the
/// runner on a miss. Cached ports still contribute their dep edges to the
/// traversal. `files` reads the options files of `ctx.port_dbdir` for the
/// cache, each once. A port `blacklist` matches is kept but not walked
/// through, as poudriere gathers no dependency of a blacklisted port.
#[allow(clippy::too_many_arguments)]
pub fn scan(
    roots: &[PortKey],
    ctx: &QueryCtx,
    files: &OptionsFiles,
    jobs: usize,
    cache: &mut Cache,
    moved: &Moved,
    blacklist: &Blacklist,
    mut progress: impl FnMut(&ScanProgress),
) -> ScanResult {
    let runner = MakeRunner::new(ctx.clone(), jobs);
    let mut result = ScanResult::default();
    // Every key ever queued, deduplicated as it is queued, so a renamed or
    // removed root is noted once and a port queried once.
    let mut seen: HashSet<PortKey> = HashSet::new();
    let mut in_flight = 0usize;
    let mut done = 0usize;
    // Keys in `seen` that are no port to query: a moved root's old name and
    // a removed one. Done as soon as resolved, so progress reaches its total.
    let mut resolved_away = 0usize;
    let exists = |o: &str| ctx.locate(o).is_some();

    let mut pending: Vec<PortKey> = Vec::new();
    for key in roots {
        if !seen.insert(key.clone()) {
            continue;
        }
        match moved.resolve(&key.origin, exists) {
            MovedResult::Unchanged => pending.push(key.clone()),
            MovedResult::MovedTo { origin, reason } => {
                result.moved_notes.push(format!("{} moved to {origin} ({reason})", key.origin));
                resolved_away += 1;
                let new_key = key.with_origin(&origin);
                // The session's roots still name the old origin: resolved
                // through this alias, then the new key's own, to the port.
                result.aliases.insert(key.clone(), new_key.clone());
                if seen.insert(new_key.clone()) {
                    pending.push(new_key);
                }
            }
            gone @ (MovedResult::Removed { .. } | MovedResult::Cycle) => {
                let reason = gone.gone_reason().expect("removed and cycle both have a reason");
                result.errors.push((key.clone(), reason));
                resolved_away += 1;
            }
        }
    }
    loop {
        while let Some(key) = pending.pop() {
            if let Some(info) = cache.lookup(&key, files) {
                result.from_cache += 1;
                let targets = register(info, blacklist, moved, &exists, &mut result);
                pending.extend(targets.into_iter().filter(|t| seen.insert(t.clone())));
            } else {
                runner.submit(key);
                in_flight += 1;
            }
        }
        if in_flight == 0 {
            break;
        }
        match runner.events.recv() {
            Ok(ScanEvent::PortDone(info)) => {
                in_flight -= 1;
                done += 1;
                result.queried += 1;
                cache.insert(&info, files);
                let targets = register(*info, blacklist, moved, &exists, &mut result);
                pending.extend(targets.into_iter().filter(|t| seen.insert(t.clone())));
            }
            Ok(ScanEvent::PortError { key, msg }) => {
                in_flight -= 1;
                done += 1;
                result.errors.push((key, msg));
            }
            Err(_) => break,
        }
        progress(&ScanProgress {
            done: done + result.from_cache + resolved_away,
            discovered: seen.len(),
            from_cache: result.from_cache,
        });
    }

    runner.shutdown();
    result
}

/// Store a finished port under its canonical key and return its dep targets
/// for further traversal: none for a blacklisted port, whose dependencies
/// poudriere does not gather (`gather_port_vars_port` in its common.sh
/// returns on IGNORE, which its blacklist sets). An IGNORE'd port is still
/// walked: its IGNORE may follow the very options being configured.
///
/// A dependency on an origin that is in no tree is never walked, and never
/// redirected through MOVED: the framework cannot build it there, and
/// poudriere's `deps_sanity` (common.sh) aborts the bulk on it, naming the
/// MOVED entry; synth fails the listed port with `nonexistent_port`. It is
/// an error on the depending port, once per such dependency, except where
/// `deps_sanity` never looks: an IGNORE'd or blacklisted port's
/// dependencies, and a `TEST_DEPENDS` edge, which poudriere never reads.
fn register(
    info: PortInfo,
    blacklist: &Blacklist,
    moved: &Moved,
    exists: &dyn Fn(&str) -> bool,
    result: &mut ScanResult,
) -> Vec<PortKey> {
    let canonical = info.canonical.clone();
    result.aliases.insert(info.key.clone(), canonical.clone());
    // A port met under a second key (bare and default flavor) was checked
    // the first time.
    let check = !result.ports.contains_key(&canonical) && info.ignore.is_none();
    let mut targets = Vec::new();
    if !blacklist.matches(&canonical, info.is_default_flavor()) {
        for dep in &info.deps {
            if exists(&dep.target.origin) {
                targets.push(dep.target.clone());
            } else if check && !dep.test_only {
                let error = nonexistent_dependency(&dep.target.origin, moved, exists);
                result.errors.push((canonical.clone(), error));
            }
        }
    }
    result.ports.entry(canonical).or_insert(info);
    targets
}

/// `deps_sanity`'s refusal of a dependency on `origin`, which is in no
/// tree, with what MOVED says of it.
fn nonexistent_dependency(origin: &str, moved: &Moved, exists: &dyn Fn(&str) -> bool) -> String {
    let why = match moved.resolve(origin, exists) {
        MovedResult::MovedTo { origin: new, .. } if exists(&new) => format!(" (moved to {new})"),
        MovedResult::MovedTo { origin: new, .. } => format!(" (moved to nonexistent {new})"),
        MovedResult::Removed { reason } if reason.is_empty() => " (port EXPIRED)".to_string(),
        MovedResult::Removed { reason } => format!(" (port EXPIRED: {reason})"),
        MovedResult::Cycle => " (its MOVED entries form a cycle)".to_string(),
        MovedResult::Unchanged => String::new(),
    };
    format!("depends on nonexistent origin {origin}{why}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optfiles::OptionsFiles;
    use crate::query::makerunner::QueryCtx;

    /// Hermetic: the ports tree is an empty tempdir, so no `make` ever runs
    /// (missing Makefiles bail before spawning) — this exercises the BFS
    /// bookkeeping, MOVED handling and error dedup only.
    #[test]
    fn moved_and_missing_ports_reported_once() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = QueryCtx {
            portsdir: tmp.path().to_path_buf(),
            make_conf: None,
            port_dbdir: tmp.path().to_path_buf(),
            overlays: Vec::new(),
            identity: crate::query::makerunner::Identity::Invoker,
        };
        let moved = Moved::parse("cat/gone||2026-01-01|expired\ncat/old|cat/new|2026-01-01|renamed\n");
        let mut cache = Cache::disabled();
        let roots = vec![
            PortKey::parse("cat/gone").unwrap(),
            PortKey::parse("cat/gone").unwrap(), // duplicate root
            PortKey::parse("cat/old").unwrap(),  // renamed, target also missing
            PortKey::parse("cat/absent").unwrap(),
        ];
        let files = OptionsFiles::new(tmp.path());
        let mut last = None;
        let result =
            scan(&roots, &ctx, &files, 2, &mut cache, &moved, &Blacklist::default(), |p| last = Some((p.done, p.discovered)));
        assert!(result.ports.is_empty());
        // Progress ends complete: the removed port and the old name of the
        // moved one count as done, not as work never finished.
        let (done, discovered) = last.unwrap();
        assert_eq!(done, discovered, "progress ends at {done}/{discovered}");
        // cat/gone reported exactly once despite two references.
        let gone: Vec<_> =
            result.errors.iter().filter(|(k, _)| k.origin == "cat/gone").collect();
        assert_eq!(gone.len(), 1);
        assert!(gone[0].1.contains("expired"));
        // The rename was noted and its (missing) target reported.
        assert_eq!(result.moved_notes.len(), 1);
        assert!(result.moved_notes[0].contains("cat/new"));
        assert!(result.errors.iter().any(|(k, _)| k.origin == "cat/new"));
        assert!(result.errors.iter().any(|(k, _)| k.origin == "cat/absent"));
    }

    /// MOVED is history: an origin moved away can be reused later (math/mumps
    /// moved to math/mumps-edf, then math/mumps4 moved to math/mumps). A port
    /// present in the tree is that port, whatever MOVED once said, and a
    /// chain stops at the first origin that exists.
    #[test]
    fn a_port_present_in_the_tree_ignores_moved() {
        let _lock = crate::query::makerunner::make_test_lock();
        let tmp = fake_tree(&[("cat/old", "PKGNAME=old-2.0\n"), ("cat/new", "PKGNAME=new-1.0\n")]);
        let ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().join("options"),
            overlays: Vec::new(),
            identity: crate::query::makerunner::Identity::Invoker,
        };
        let moved = Moved::parse(
            "cat/old|cat/new|2026-01-01|renamed\n\
             cat/older|cat/old|2026-02-01|reused the old name\n",
        );
        let files = OptionsFiles::new(&ctx.port_dbdir);
        let mut cache = Cache::disabled();
        for (root, want) in [("cat/old", "cat/old"), ("cat/older", "cat/old")] {
            let roots = vec![PortKey::parse(root).unwrap()];
            let result = scan(&roots, &ctx, &files, 1, &mut cache, &moved, &Blacklist::default(), |_| {});
            let got: Vec<String> = result.ports.keys().map(|k| k.to_string()).collect();
            assert_eq!(got, [want], "{root}: {:?}", result.moved_notes);
            assert!(result.errors.is_empty(), "{root}: {:?}", result.errors);
        }
    }

    /// A ports tree of one-line Makefiles: make runs for real, but nothing
    /// outside the tempdir is read. `PKGNAME` is all a dump needs.
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

    /// An overlay's port answers for its origin over the main tree's, the
    /// first overlay over the next; a port only an overlay holds is found,
    /// as a root and as a dependency, and MOVED's existence check sees it.
    #[test]
    fn overlays_answer_first_and_add_ports() {
        let _lock = crate::query::makerunner::make_test_lock();
        let deps = "_UNIFIED_DEPENDS=only:cat/only\nRUN_DEPENDS_ALL=only:cat/only\n";
        let tmp = fake_tree(&[
            ("cat/a", "PKGNAME=a-main\n"),
            ("cat/root", &format!("PKGNAME=root-1.0\n{deps}")),
        ]);
        let make = |tree: &str, port: &str, pkgname: &str| {
            let dir = tmp.path().join(tree).join(port);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("Makefile"), format!("PKGNAME={pkgname}\n")).unwrap();
        };
        make("first", "cat/a", "a-first");
        make("second", "cat/a", "a-second");
        make("second", "cat/only", "only-1.0");
        let mut ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().join("options"),
            overlays: vec![tmp.path().join("first"), tmp.path().join("second")],
            identity: crate::query::makerunner::Identity::Invoker,
        };
        let a = PortKey::parse("cat/a").unwrap();
        let files = OptionsFiles::new(&ctx.port_dbdir);
        let pkg = |ctx: &QueryCtx| crate::query::makerunner::run_query(ctx, &a).unwrap().pkgname;
        assert_eq!(pkg(&ctx), "a-first", "the first overlay shadows the next and the tree");

        ctx.overlays.remove(0);
        assert_eq!(pkg(&ctx), "a-second");

        // The main tree's cat/root pulls in a port only the overlay holds;
        // MOVED leaves an origin an overlay holds where it is.
        let moved = Moved::parse("cat/only|cat/gone|2026-01-01|moved away\n");
        let root = PortKey::parse("cat/root").unwrap();
        let result = scan(&[root], &ctx, &files, 1, &mut Cache::disabled(), &moved, &Blacklist::default(), |_| {});
        let got: Vec<String> = result.ports.values().map(|i| i.pkgname.clone()).collect();
        assert_eq!(got, ["only-1.0", "root-1.0"], "{:?}", result.errors);

        ctx.overlays.clear();
        let err = crate::query::makerunner::run_query(&ctx, &PortKey::parse("cat/only").unwrap())
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("no such port: cat/only"), "{err}");
    }

    /// An IGNORE'd port is not a blacklisted one: `register`'s own doc says
    /// its dependencies are still queried, since its IGNORE may follow the
    /// very options being configured. poudriere's `gather_port_vars_port`
    /// returns on any IGNORE, so this differs on purpose (conformance.md);
    /// a blacklisted port, IGNORE'd by poudriere itself, is not walked.
    #[test]
    fn an_ignored_ports_dependencies_are_still_queried() {
        let _lock = crate::query::makerunner::make_test_lock();
        let dep = "_UNIFIED_DEPENDS=x:cat/dep\nRUN_DEPENDS_ALL=x:cat/dep\n";
        let tmp = fake_tree(&[
            ("cat/root", &format!("PKGNAME=root-1.0\nIGNORE=not for this platform\n{dep}")),
            ("cat/dep", "PKGNAME=dep-1.0\n"),
        ]);
        let ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().join("options"),
            overlays: Vec::new(),
            identity: crate::query::makerunner::Identity::Invoker,
        };
        let files = OptionsFiles::new(&ctx.port_dbdir);
        let roots = vec![PortKey::parse("cat/root").unwrap()];
        let result = scan(
            &roots,
            &ctx,
            &files,
            1,
            &mut Cache::disabled(),
            &Moved::parse(""),
            &Blacklist::default(),
            |_| {},
        );
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let root = &result.ports[&PortKey::parse("cat/root").unwrap()];
        assert_eq!(root.ignore.as_deref(), Some("not for this platform"));
        assert!(
            result.ports.contains_key(&PortKey::parse("cat/dep").unwrap()),
            "cat/dep must still be queried: {:?}",
            result.ports.keys().collect::<Vec<_>>()
        );
    }

    /// poudriere gathers no dependency of a blacklisted port: the port is
    /// listed, its dependencies are not queried, as a root or deeper down.
    #[test]
    fn a_blacklisted_port_is_not_walked_through() {
        let _lock = crate::query::makerunner::make_test_lock();
        let dep = |o: &str| format!("_UNIFIED_DEPENDS=x:{o}\nRUN_DEPENDS_ALL=x:{o}\n");
        let tmp = fake_tree(&[
            ("cat/root", &format!("PKGNAME=root-1.0\n{}", dep("cat/mid"))),
            ("cat/mid", &format!("PKGNAME=mid-1.0\n{}", dep("cat/leaf"))),
            ("cat/leaf", "PKGNAME=leaf-1.0\n"),
        ]);
        let ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().join("options"),
            overlays: Vec::new(),
            identity: crate::query::makerunner::Identity::Invoker,
        };
        let files = OptionsFiles::new(&ctx.port_dbdir);
        let pd = tmp.path().join("poudriere.d");
        std::fs::create_dir_all(&pd).unwrap();
        let walk = |listed: &str| {
            std::fs::write(pd.join("blacklist"), listed).unwrap();
            let bl = Blacklist::load(&pd, None, "default", None);
            let roots = [PortKey::parse("cat/root").unwrap()];
            let r = scan(&roots, &ctx, &files, 1, &mut Cache::disabled(), &Moved::parse(""), &bl, |_| {});
            assert!(r.errors.is_empty(), "{:?}", r.errors);
            (r.queried, r.ports.keys().map(|k| k.to_string()).collect::<Vec<_>>())
        };
        assert_eq!(walk(""), (3, vec!["cat/leaf".into(), "cat/mid".into(), "cat/root".into()]));
        assert_eq!(walk("cat/root\n"), (1, vec!["cat/root".to_string()]));
        assert_eq!(walk("cat/mid\n"), (2, vec!["cat/mid".into(), "cat/root".into()]));
    }

    /// The counts the summary line reports: queried cold, cached warm.
    #[test]
    fn scan_counts_queried_and_cached_ports() {
        let _lock = crate::query::makerunner::make_test_lock();
        let tmp = fake_tree(&[("cat/a", "PKGNAME=a-1.0\n"), ("cat/b", "PKGNAME=b-1.0\n")]);
        let ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().join("options"),
            overlays: Vec::new(),
            identity: crate::query::makerunner::Identity::Invoker,
        };
        let roots = vec![PortKey::parse("cat/a").unwrap(), PortKey::parse("cat/b").unwrap()];
        let mut cache = Cache::open(&tmp.path().join("cache"), "tree", "conf");
        let moved = Moved::parse("");
        let mut last = None;
        let files = OptionsFiles::new(&ctx.port_dbdir);
        let cold = scan(&roots, &ctx, &files, 2, &mut cache, &moved, &Blacklist::default(), |p| {
            last = Some((p.done, p.discovered, p.from_cache))
        });
        assert!(cold.errors.is_empty(), "{:?}", cold.errors);
        assert_eq!((cold.queried, cold.from_cache, cold.ports.len()), (2, 0, 2));
        assert_eq!(last, Some((2, 2, 0)), "progress ends at every port done");

        let files = OptionsFiles::new(&ctx.port_dbdir);
        let warm = scan(&roots, &ctx, &files, 2, &mut cache, &moved, &Blacklist::default(), |_| {});
        assert_eq!((warm.queried, warm.from_cache, warm.ports.len()), (0, 2, 2));
    }

    /// A dependency on an origin in no tree is an error on the depending
    /// port, worded as poudriere's `deps_sanity` words it: never redirected
    /// through MOVED, so the port it moved to is not queried through it,
    /// and no loop runs through it. The depending port stays in the
    /// closure, and a refresh's GC keeps it.
    #[test]
    fn a_dependency_on_a_nonexistent_origin_is_the_dependers_error() {
        let _lock = crate::query::makerunner::make_test_lock();
        let dep = |o: &[&str]| {
            let list: Vec<String> = o.iter().map(|o| format!("x:{o}")).collect();
            format!("_UNIFIED_DEPENDS={0}\nRUN_DEPENDS_ALL={0}\n", list.join(" "))
        };
        let tmp = fake_tree(&[
            ("cat/a", &format!("PKGNAME=a-1.0\n{}", dep(&["cat/old", "cat/expired", "cat/never", "cat/b"]))),
            ("cat/b", "PKGNAME=b-1.0\n"),
            ("cat/new", &format!("PKGNAME=new-1.0\n{}", dep(&["cat/a"]))),
        ]);
        let ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().join("options"),
            overlays: Vec::new(),
            identity: crate::query::makerunner::Identity::Invoker,
        };
        let moved = Moved::parse(
            "cat/old|cat/new|2026-01-01|renamed\n\
             cat/expired||2026-01-01|Has expired: unmaintained\n",
        );
        let files = OptionsFiles::new(&ctx.port_dbdir);
        let roots = vec![PortKey::parse("cat/a").unwrap()];
        let result = scan(&roots, &ctx, &files, 1, &mut Cache::disabled(), &moved, &Blacklist::default(), |_| {});
        let errors: Vec<String> = result.errors.iter().map(|(k, m)| format!("{k}: {m}")).collect();
        assert_eq!(
            errors,
            [
                "cat/a: depends on nonexistent origin cat/old (moved to cat/new)",
                "cat/a: depends on nonexistent origin cat/expired (port EXPIRED: Has expired: unmaintained)",
                "cat/a: depends on nonexistent origin cat/never",
            ]
        );
        let got: Vec<String> = result.ports.keys().map(|k| k.to_string()).collect();
        assert_eq!(got, ["cat/a", "cat/b"], "cat/new is not queried through cat/old");
        assert!(result.moved_notes.is_empty(), "{:?}", result.moved_notes);
        let old = PortKey::parse("cat/old").unwrap();
        assert_eq!(crate::deps::resolve(&result.ports, &result.aliases, &old), None);
        assert!(crate::deps::detect(&result.ports, &result.aliases).is_empty());

        let mut session =
            crate::session::Session::new(result.ports, result.aliases, &roots, &files, false);
        let refresh = scan(&roots, &ctx, &files, 1, &mut Cache::disabled(), &moved, &Blacklist::default(), |_| {});
        assert_eq!(refresh.errors.len(), 3, "{:?}", refresh.errors);
        let (_, removed) = session.merge(refresh, &files);
        assert!(removed.is_empty(), "the GC dropped {removed:?}");
        assert_eq!(session.ports.len(), 2);
    }

    /// `deps_sanity` never looks at an IGNORE'd port's dependencies, nor at
    /// a blacklisted one's, nor at `TEST_DEPENDS`, which poudriere never
    /// reads: a nonexistent origin there is no error, and is not walked.
    /// The IGNORE'd port's other dependencies are still queried.
    #[test]
    fn a_nonexistent_origin_poudriere_never_checks_is_no_error() {
        let _lock = crate::query::makerunner::make_test_lock();
        let tmp = fake_tree(&[
            (
                "cat/ignored",
                "PKGNAME=ignored-1.0\nIGNORE=not here\n\
                 _UNIFIED_DEPENDS=x:cat/gone x:cat/dep\nRUN_DEPENDS_ALL=x:cat/gone x:cat/dep\n",
            ),
            ("cat/listed", "PKGNAME=listed-1.0\n_UNIFIED_DEPENDS=x:cat/gone\nRUN_DEPENDS_ALL=x:cat/gone\n"),
            ("cat/tested", "PKGNAME=tested-1.0\n_UNIFIED_DEPENDS=x:cat/gone\n"),
            ("cat/dep", "PKGNAME=dep-1.0\n"),
        ]);
        let ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().join("options"),
            overlays: Vec::new(),
            identity: crate::query::makerunner::Identity::Invoker,
        };
        let files = OptionsFiles::new(&ctx.port_dbdir);
        let pd = tmp.path().join("poudriere.d");
        std::fs::create_dir_all(&pd).unwrap();
        std::fs::write(pd.join("blacklist"), "cat/listed\n").unwrap();
        let bl = Blacklist::load(&pd, None, "default", None);
        let roots: Vec<PortKey> =
            ["cat/ignored", "cat/listed", "cat/tested"].iter().map(|o| PortKey::parse(o).unwrap()).collect();
        let moved = Moved::parse("cat/gone|cat/dep|2026-01-01|renamed\n");
        let result = scan(&roots, &ctx, &files, 1, &mut Cache::disabled(), &moved, &bl, |_| {});
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        let got: Vec<String> = result.ports.keys().map(|k| k.to_string()).collect();
        assert_eq!(got, ["cat/dep", "cat/ignored", "cat/listed", "cat/tested"]);
        assert!(result.ports[&PortKey::parse("cat/tested").unwrap()].deps[0].test_only);
    }

    /// A root follows MOVED (the existing `moved_and_missing_ports_reported_once`
    /// covers the note); a dependency naming that same old origin is still
    /// refused, the root's resolution notwithstanding.
    #[test]
    fn a_moved_root_does_not_excuse_a_dependency_on_its_old_name() {
        let _lock = crate::query::makerunner::make_test_lock();
        let tmp = fake_tree(&[
            ("cat/a", "PKGNAME=a-1.0\n_UNIFIED_DEPENDS=x:cat/old\nRUN_DEPENDS_ALL=x:cat/old\n"),
            ("cat/new", "PKGNAME=new-1.0\n"),
        ]);
        let ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().join("options"),
            overlays: Vec::new(),
            identity: crate::query::makerunner::Identity::Invoker,
        };
        let moved = Moved::parse("cat/old|cat/new|2026-01-01|renamed\n");
        let files = OptionsFiles::new(&ctx.port_dbdir);
        let roots: Vec<PortKey> = ["cat/old", "cat/a"].iter().map(|o| PortKey::parse(o).unwrap()).collect();
        let result = scan(&roots, &ctx, &files, 1, &mut Cache::disabled(), &moved, &Blacklist::default(), |_| {});
        assert_eq!(result.moved_notes, ["cat/old moved to cat/new (renamed)"]);
        let errors: Vec<String> = result.errors.iter().map(|(k, m)| format!("{k}: {m}")).collect();
        assert_eq!(errors, ["cat/a: depends on nonexistent origin cat/old (moved to cat/new)"]);
        let got: Vec<String> = result.ports.keys().map(|k| k.to_string()).collect();
        assert_eq!(got, ["cat/a", "cat/new"]);
    }

    /// make reads `options.local` after the options file (bsd.options.mk's
    /// `.sinclude`), so adding, changing or removing one answers anew,
    /// never from the entry cached without it.
    #[test]
    fn options_local_is_part_of_the_cache_key() {
        let _lock = crate::query::makerunner::make_test_lock();
        let tmp = fake_tree(&[(
            "cat/a",
            "OPTIONS_NAME=cat_a\nPKGNAME=a-1.0\n.sinclude \"${PORT_DBDIR}/cat_a/options.local\"\n",
        )]);
        let ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().join("options"),
            overlays: Vec::new(),
            identity: crate::query::makerunner::Identity::Invoker,
        };
        let roots = vec![PortKey::parse("cat/a").unwrap()];
        let mut cache = Cache::open(&tmp.path().join("cache"), "tree", "conf");
        let moved = Moved::parse("");
        let local = ctx.port_dbdir.join("cat_a/options.local");
        let pkgname = |cache: &mut Cache| {
            let files = OptionsFiles::new(&ctx.port_dbdir);
            let r = scan(&roots, &ctx, &files, 1, cache, &moved, &Blacklist::default(), |_| {});
            assert!(r.errors.is_empty(), "{:?}", r.errors);
            (r.queried, r.ports.values().next().unwrap().pkgname.clone())
        };
        assert_eq!(pkgname(&mut cache), (1, "a-1.0".to_string()));
        assert_eq!(pkgname(&mut cache), (0, "a-1.0".to_string()), "cached");

        std::fs::create_dir_all(local.parent().unwrap()).unwrap();
        std::fs::write(&local, "PKGNAME=a-2.0\n").unwrap();
        assert_eq!(pkgname(&mut cache), (1, "a-2.0".to_string()), "added");
        std::fs::write(&local, "PKGNAME=a-3.0\n").unwrap();
        assert_eq!(pkgname(&mut cache), (1, "a-3.0".to_string()), "changed");
        std::fs::remove_file(&local).unwrap();
        assert_eq!(pkgname(&mut cache), (1, "a-1.0".to_string()), "removed");
    }

    /// An options file that is there but cannot be read: make fails on it,
    /// and the port is reported with the file and the reason, never taken
    /// from a cache entry made while the file was missing. That entry, read
    /// as "unconfigured", would have let sync or apply overwrite the file.
    #[test]
    fn an_unreadable_options_file_fails_the_port_even_when_cached() {
        use std::os::unix::fs::PermissionsExt;
        let _lock = crate::query::makerunner::make_test_lock();
        // What bsd.options.mk does: include the options file, unconditionally
        // once it exists.
        let tmp = fake_tree(&[(
            "cat/a",
            "PKGNAME=a-1.0\nOPTIONS_NAME=cat_a\n\
             .if exists(${PORT_DBDIR}/cat_a/options)\n.include \"${PORT_DBDIR}/cat_a/options\"\n.endif\n",
        )]);
        let ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().join("options"),
            overlays: Vec::new(),
            identity: crate::query::makerunner::Identity::Invoker,
        };
        let roots = vec![PortKey::parse("cat/a").unwrap()];
        let mut cache = Cache::open(&tmp.path().join("cache"), "tree", "conf");
        let moved = Moved::parse("");
        let files = OptionsFiles::new(&ctx.port_dbdir);
        let first = scan(&roots, &ctx, &files, 1, &mut cache, &moved, &Blacklist::default(), |_| {});
        assert_eq!((first.queried, first.ports.len()), (1, 1), "{:?}", first.errors);

        let file = ctx.port_dbdir.join("cat_a/options");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "OPTIONS_FILE_SET+=DOCS\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&file).is_ok() {
            return; // root reads it anyway: the case only exists for others
        }
        let files = OptionsFiles::new(&ctx.port_dbdir);
        let second = scan(&roots, &ctx, &files, 1, &mut cache, &moved, &Blacklist::default(), |_| {});
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(second.from_cache, 0, "the entry made while it was missing is not reused");
        assert!(second.ports.is_empty());
        let (key, msg) = &second.errors[0];
        assert_eq!(key.to_string(), "cat/a");
        assert_eq!(msg, &format!("cannot read options file {}: Permission denied (os error 13)", file.display()));
    }

    /// A Makefile make cannot read is that port's error, with make's own last
    /// words; a port with no Makefile says so.
    #[test]
    fn a_failing_or_missing_makefile_is_the_ports_error() {
        let _lock = crate::query::makerunner::make_test_lock();
        let tmp = fake_tree(&[("cat/bad", ".error the port is broken here\n")]);
        let ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().join("options"),
            overlays: Vec::new(),
            identity: crate::query::makerunner::Identity::Invoker,
        };
        let bad = crate::query::makerunner::run_query(&ctx, &PortKey::parse("cat/bad").unwrap())
            .unwrap_err()
            .to_string();
        assert!(bad.starts_with("make failed"), "{bad}");
        assert!(bad.contains("the port is broken here"), "{bad}");
        let none = crate::query::makerunner::run_query(&ctx, &PortKey::parse("cat/none").unwrap())
            .unwrap_err()
            .to_string();
        assert!(none.starts_with("no such port: cat/none"), "{none}");

        // A failed .include: make carries on, prints a dump and exits 1.
        // The dump is partial, so the port fails, naming the missing file.
        let tmp = fake_tree(&[(
            "cat/half",
            "PKGNAME=a-1.0\nCOMPLETE_OPTIONS_LIST=X\n.include \"/nonexistent/common.mk\"\n",
        )]);
        let ctx = QueryCtx { portsdir: tmp.path().join("ports"), ..ctx };
        let half = crate::query::makerunner::run_query(&ctx, &PortKey::parse("cat/half").unwrap())
            .unwrap_err()
            .to_string();
        assert!(half.starts_with("make failed"), "{half}");
        assert!(half.contains("Cannot open /nonexistent/common.mk"), "{half}");
    }

    /// Startup on a real closure reads each options file once, cold or warm:
    /// the scan's cache checks, the session and the staging copy share the
    /// reads. Run with: cargo test -- --ignored
    #[test]
    #[ignore = "needs /usr/ports on a FreeBSD host"]
    fn live_startup_reads_each_options_file_once() {
        let _lock = crate::query::makerunner::make_test_lock();
        use std::collections::BTreeSet;

        let tmp = tempfile::tempdir().unwrap();
        let options_dir = tmp.path().join("options");
        // One configured port, the rest unconfigured: both kinds are read.
        std::fs::create_dir_all(options_dir.join("audio_lame")).unwrap();
        std::fs::write(options_dir.join("audio_lame/options"), "OPTIONS_FILE_UNSET+=DOCS\n").unwrap();
        let ctx = QueryCtx {
            portsdir: "/usr/ports".into(),
            make_conf: None,
            port_dbdir: options_dir.clone(),
            overlays: Vec::new(),
            identity: crate::query::makerunner::Identity::Invoker,
        };
        let roots = vec![PortKey::parse("audio/lame").unwrap()];
        let moved = Moved::parse("");
        let mut cache = Cache::open(&tmp.path().join("cache"), "tree", "conf");

        for run in ["cold", "warm"] {
            let files = OptionsFiles::new(&options_dir);
            let result = scan(&roots, &ctx, &files, 4, &mut cache, &moved, &Blacklist::default(), |_| {});
            assert!(result.errors.is_empty(), "{run}: {:?}", result.errors);
            if run == "warm" {
                assert_eq!(result.queried, 0, "warm: everything from the cache");
            }
            let names: BTreeSet<&str> =
                result.ports.values().map(|i| i.options_name.as_str()).collect();
            assert_eq!(files.disk_reads(), names.len(), "{run}: one read per file in the scan");

            let session = crate::session::Session::new(
                result.ports.clone(),
                result.aliases.clone(),
                &roots,
                &files,
                false,
            );
            let stage = tempfile::tempdir().unwrap();
            let db =
                crate::staging::StagingDb::create(stage.path(), &files, session.states.keys()).unwrap();
            assert_eq!(files.disk_reads(), names.len(), "{run}: session and staging read nothing");
            assert_eq!(
                std::fs::read(db.path().join("audio_lame/options")).unwrap(),
                b"OPTIONS_FILE_UNSET+=DOCS\n"
            );
        }
    }
}
