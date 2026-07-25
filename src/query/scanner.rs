use std::collections::{BTreeMap, HashMap, HashSet};

use crate::cache::Cache;
use crate::model::origin::PortKey;
use crate::model::port::PortInfo;
use crate::moved::{Moved, MovedResult};
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
/// ports still contribute their dep edges to the traversal.
pub fn scan(
    roots: &[PortKey],
    ctx: &QueryCtx,
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
            // Resolve renames/removals before anything else.
            let key = match moved.resolve(&key.origin) {
                MovedResult::Unchanged => key,
                MovedResult::MovedTo { origin, reason } => {
                    result
                        .moved_notes
                        .push(format!("{} moved to {origin} ({reason})", key.origin));
                    key.with_origin(&origin)
                }
                MovedResult::Removed { reason } => {
                    result
                        .errors
                        .push((key.clone(), format!("port removed from tree: {reason}")));
                    continue;
                }
            };
            if !seen.insert(key.clone()) {
                continue;
            }
            if let Some(info) = cache.lookup(&key, &ctx.port_dbdir) {
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
                cache.insert(&info, &ctx.port_dbdir);
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
