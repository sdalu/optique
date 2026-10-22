use std::collections::{BTreeMap, HashMap, HashSet};

use crate::cache::Cache;
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
    /// Requested key -> canonical key (for dedup of default-flavor aliases).
    pub aliases: HashMap<PortKey, PortKey>,
    /// Ports whose query failed: (requested key, error).
    pub errors: Vec<(PortKey, String)>,
    /// MOVED redirections applied during the scan (informational).
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
/// Every discovered dep target is resolved through MOVED, deduplicated,
/// looked up in the cache, and queried via the runner on a miss. Cached
/// ports still contribute their dep edges to the traversal. `files` reads
/// the options files of `ctx.port_dbdir` for the cache, each once.
pub fn scan(
    roots: &[PortKey],
    ctx: &QueryCtx,
    files: &OptionsFiles,
    jobs: usize,
    cache: &mut Cache,
    moved: &Moved,
    mut progress: impl FnMut(&ScanProgress),
) -> ScanResult {
    let runner = MakeRunner::new(ctx.clone(), jobs);
    let mut result = ScanResult::default();
    let mut seen: HashSet<PortKey> = HashSet::new();
    let mut in_flight = 0usize;
    let mut done = 0usize;

    let mut pending: Vec<PortKey> = roots.to_vec();
    loop {
        while let Some(key) = pending.pop() {
            // Dedup on the raw key first so renamed/removed ports are noted
            // once, not once per referencing dep edge.
            if !seen.insert(key.clone()) {
                continue;
            }
            // Resolve renames/removals before anything else.
            let key = match moved.resolve(&key.origin) {
                MovedResult::Unchanged => key,
                MovedResult::MovedTo { origin, reason } => {
                    result
                        .moved_notes
                        .push(format!("{} moved to {origin} ({reason})", key.origin));
                    let new_key = key.with_origin(&origin);
                    if !seen.insert(new_key.clone()) {
                        continue;
                    }
                    new_key
                }
                MovedResult::Removed { reason } => {
                    result
                        .errors
                        .push((key.clone(), format!("port removed from tree: {reason}")));
                    continue;
                }
            };
            if let Some(info) = cache.lookup(&key, files) {
                result.from_cache += 1;
                pending.extend(register(info, &mut result));
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
                pending.extend(register(*info, &mut result));
            }
            Ok(ScanEvent::PortError { key, msg }) => {
                in_flight -= 1;
                done += 1;
                result.errors.push((key, msg));
            }
            Err(_) => break,
        }
        progress(&ScanProgress {
            done: done + result.from_cache,
            discovered: seen.len(),
            from_cache: result.from_cache,
        });
    }

    runner.shutdown();
    result
}

/// Store a finished port under its canonical key and return its dep targets
/// for further traversal.
fn register(info: PortInfo, result: &mut ScanResult) -> Vec<PortKey> {
    result.aliases.insert(info.key.clone(), info.canonical.clone());
    let targets: Vec<PortKey> = info.deps.iter().map(|d| d.target.clone()).collect();
    result.ports.entry(info.canonical.clone()).or_insert(info);
    targets
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
        let result = scan(&roots, &ctx, &files, 2, &mut cache, &moved, |_| {});
        assert!(result.ports.is_empty());
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

    /// The counts the summary line reports: queried cold, cached warm.
    #[test]
    fn scan_counts_queried_and_cached_ports() {
        let tmp = fake_tree(&[("cat/a", "PKGNAME=a-1.0\n"), ("cat/b", "PKGNAME=b-1.0\n")]);
        let ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().join("options"),
        };
        let roots = vec![PortKey::parse("cat/a").unwrap(), PortKey::parse("cat/b").unwrap()];
        let mut cache = Cache::open(&tmp.path().join("cache"), "tree", "conf");
        let moved = Moved::parse("");
        let mut last = None;
        let files = OptionsFiles::new(&ctx.port_dbdir);
        let cold = scan(&roots, &ctx, &files, 2, &mut cache, &moved, |p| {
            last = Some((p.done, p.discovered, p.from_cache))
        });
        assert!(cold.errors.is_empty(), "{:?}", cold.errors);
        assert_eq!((cold.queried, cold.from_cache, cold.ports.len()), (2, 0, 2));
        assert_eq!(last, Some((2, 2, 0)), "progress ends at every port done");

        let files = OptionsFiles::new(&ctx.port_dbdir);
        let warm = scan(&roots, &ctx, &files, 2, &mut cache, &moved, |_| {});
        assert_eq!((warm.queried, warm.from_cache, warm.ports.len()), (0, 2, 2));
    }

    /// A Makefile make cannot read is that port's error, with make's own last
    /// words; a port with no Makefile says so.
    #[test]
    fn a_failing_or_missing_makefile_is_the_ports_error() {
        let tmp = fake_tree(&[("cat/bad", ".error the port is broken here\n")]);
        let ctx = QueryCtx {
            portsdir: tmp.path().join("ports"),
            make_conf: None,
            port_dbdir: tmp.path().join("options"),
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
    }

    /// Startup on a real closure reads each options file once, cold or warm:
    /// the scan's cache checks, the session and the staging copy share the
    /// reads. Run with: cargo test -- --ignored
    #[test]
    #[ignore = "needs /usr/ports on a FreeBSD host"]
    fn live_startup_reads_each_options_file_once() {
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
        };
        let roots = vec![PortKey::parse("audio/lame").unwrap()];
        let moved = Moved::parse("");
        let mut cache = Cache::open(&tmp.path().join("cache"), "tree", "conf");

        for run in ["cold", "warm"] {
            let files = OptionsFiles::new(&options_dir);
            let result = scan(&roots, &ctx, &files, 4, &mut cache, &moved, |_| {});
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
