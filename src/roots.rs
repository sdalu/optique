//! Roots as the build tool reads them, before the closure is scanned:
//! `origin@all` stands for every flavor of the port and `origin@-` for its
//! default flavor, as poudriere(8) reads them in a list (common.sh ~7026,
//! ~8531-8537, ~8714-8724: `FLAVOR_ALL`, `FLAVOR_DEFAULT`).

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::cache::Cache;
use crate::model::origin::PortKey;
use crate::optfiles::OptionsFiles;
use crate::query::makerunner::{MakeRunner, QueryCtx, ScanEvent};

/// The flavor that names every flavor of a port (poudriere's `FLAVOR_ALL`).
pub const FLAVOR_ALL: &str = "all";
/// The flavor that names the default one, the bare origin (poudriere's
/// `FLAVOR_DEFAULT`); `cli::collect_roots` reads it.
pub const FLAVOR_DEFAULT: &str = "-";

/// Does `key` stand for every flavor of its port?
fn wants_all(key: &PortKey) -> bool {
    key.flavor.as_deref() == Some(FLAVOR_ALL)
}

/// `roots` with each `origin@all` replaced by one root per flavor in its
/// port's FLAVORS (`flavors`, by origin), in that order, or by the bare
/// origin when the port has none or its FLAVORS are unknown (a failed
/// query, which the scan then reports). Duplicates collapse, first kept.
pub fn expand(roots: &[PortKey], flavors: &HashMap<String, Vec<String>>) -> Vec<PortKey> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for key in roots {
        let keys = if wants_all(key) {
            match flavors.get(&key.origin).filter(|f| !f.is_empty()) {
                Some(fl) => fl.iter().map(|f| PortKey::new(key.origin.clone(), Some(f.clone()))).collect(),
                None => vec![PortKey::new(key.origin.clone(), None)],
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

/// `roots` as the scan should take them: every `origin@all` expanded, its
/// port's FLAVORS queried first.
pub fn resolve(
    roots: &[PortKey],
    ctx: &QueryCtx,
    files: &OptionsFiles,
    jobs: usize,
    cache: &mut Cache,
) -> Vec<PortKey> {
    let origins: BTreeSet<String> =
        roots.iter().filter(|k| wants_all(k)).map(|k| k.origin.clone()).collect();
    if origins.is_empty() {
        return roots.to_vec();
    }
    let flavors = query_flavors(&origins, ctx, files, jobs, cache);
    expand(roots, &flavors)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        );
        assert_eq!(
            got,
            keys(&["misc/flav@full", "misc/flav@nox", "misc/flav@pgtk", "misc/plain", "misc/gone", "misc/x"])
        );
    }
}
