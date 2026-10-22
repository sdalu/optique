use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Outcome of resolving an origin through the MOVED file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MovedResult {
    Unchanged,
    /// Followed one or more MOVED entries to a new origin.
    MovedTo { origin: String, reason: String },
    /// Entry with an empty target: the port was removed.
    Removed { reason: String },
    /// The chain comes back to an origin it passed, none of them in the
    /// tree: it leads to no port, but nothing says one was removed.
    Cycle,
}

impl MovedResult {
    /// Why the origin leads to no port, when it does not: removed, or lost
    /// in a cycle. None for an origin that still has (or moved to) a port.
    pub fn gone_reason(&self) -> Option<String> {
        match self {
            MovedResult::Removed { reason } if !reason.is_empty() => {
                Some(format!("port removed from tree ({reason})"))
            }
            MovedResult::Removed { .. } => Some("port removed from tree".to_string()),
            MovedResult::Cycle => Some(
                "port no longer exists in the tree (its MOVED entries form a cycle)".to_string(),
            ),
            MovedResult::Unchanged | MovedResult::MovedTo { .. } => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Moved {
    /// old origin -> (new origin or None if removed, reason)
    map: HashMap<String, (Option<String>, String)>,
}

impl Moved {
    /// The MOVED files of the main tree then of each overlay, in order, read
    /// as poudriere reads them (one concatenation, `parse_MOVED.awk`): a
    /// chain may run across files, and an origin listed again keeps its
    /// first entry, the repeat warned about.
    pub fn load_all(trees: &[&Path]) -> (Self, Vec<String>) {
        let mut moved = Moved::default();
        let mut warnings = Vec::new();
        for tree in trees {
            let file = tree.join("MOVED");
            if let Ok(text) = std::fs::read_to_string(&file) {
                moved.add(&text, &file.display().to_string(), &mut warnings);
            }
        }
        (moved, warnings)
    }

    #[cfg(test)]
    pub fn parse(text: &str) -> Self {
        let mut moved = Moved::default();
        moved.add(text, "MOVED", &mut Vec::new());
        moved
    }

    fn add(&mut self, text: &str, file: &str, warnings: &mut Vec<String>) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split('|');
            let (Some(old), Some(new)) = (fields.next(), fields.next()) else {
                continue;
            };
            let reason = fields.nth(1).unwrap_or("").to_string();
            let new = new.trim();
            let entry = (if new.is_empty() { None } else { Some(new.to_string()) }, reason);
            match self.map.entry(old.trim().to_string()) {
                std::collections::hash_map::Entry::Occupied(o) => {
                    warnings.push(format!("MOVED: {} has duplicate entries ({file})", o.key()));
                }
                std::collections::hash_map::Entry::Vacant(v) => {
                    v.insert(entry);
                }
            }
        }
    }

    /// Chase MOVED chains for a bare origin (no flavor). MOVED is history and
    /// an origin moved away can be reused later, so the chase stops at the
    /// first origin `exists` says is in the tree, the one asked for included.
    /// A chain that comes back to an origin it passed leads to no port: it
    /// is reported as a cycle.
    pub fn resolve(&self, origin: &str, exists: impl Fn(&str) -> bool) -> MovedResult {
        let mut current = origin.to_string();
        let mut reason = String::new();
        let mut passed = HashSet::new();
        while let Some((next, why)) = self.map.get(&current).filter(|_| !exists(&current)) {
            if !passed.insert(current.clone()) {
                return MovedResult::Cycle;
            }
            reason = why.clone();
            match next {
                Some(n) => current = n.clone(),
                None => return MovedResult::Removed { reason },
            }
        }
        if current == origin {
            MovedResult::Unchanged
        } else {
            MovedResult::MovedTo { origin: current, reason }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chains_and_removals() {
        let m = Moved::parse(
            "# comment\n\
             a/one|b/two|2024-01-01|renamed\n\
             b/two|c/three|2024-02-01|renamed again\n\
             d/gone||2024-03-01|abandoned\n",
        );
        let none = |_: &str| false;
        assert_eq!(m.resolve("x/untouched", none), MovedResult::Unchanged);
        assert_eq!(
            m.resolve("a/one", none),
            MovedResult::MovedTo { origin: "c/three".into(), reason: "renamed again".into() }
        );
        assert_eq!(m.resolve("d/gone", none), MovedResult::Removed { reason: "abandoned".into() });
    }

    /// An origin in the tree is that port: the chase never leaves it, and a
    /// chain stops at the first one that exists (math/mumps4 moved to
    /// math/mumps, a name math/mumps had left for math/mumps-edf).
    #[test]
    fn chain_stops_at_an_existing_origin() {
        let m = Moved::parse(
            "math/mumps|math/mumps-edf|2026-10-07|Repomove to math/mumps-edf\n\
             math/mumps4|math/mumps|2026-10-07|Repomove to math/mumps\n",
        );
        let exists = |o: &str| o == "math/mumps" || o == "math/mumps-edf";
        assert_eq!(m.resolve("math/mumps", exists), MovedResult::Unchanged);
        assert_eq!(
            m.resolve("math/mumps4", exists),
            MovedResult::MovedTo { origin: "math/mumps".into(), reason: "Repomove to math/mumps".into() }
        );
    }

    /// A cycle among origins absent from the tree leads nowhere: reported,
    /// never redirected to whichever origin the chase stopped at. One of
    /// its origins in the tree ends the chase there as any chain does.
    /// The main tree's MOVED, then each overlay's: a chain runs across
    /// them, and a repeated origin keeps its first entry, with a warning.
    #[test]
    fn overlays_moved_files_follow_the_main_one() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, over) = (tmp.path().join("main"), tmp.path().join("over"));
        std::fs::create_dir_all(&main).unwrap();
        std::fs::create_dir_all(&over).unwrap();
        std::fs::write(main.join("MOVED"), "a/a|b/b|2026-01-01|main\nx/x|y/y|2026-01-01|main\n").unwrap();
        std::fs::write(over.join("MOVED"), "b/b|c/c|2026-02-01|over\nx/x|z/z|2026-02-01|over\n").unwrap();
        let (m, warnings) = Moved::load_all(&[&main, &over]);
        let none = |_: &str| false;
        assert_eq!(m.resolve("a/a", none), MovedResult::MovedTo { origin: "c/c".into(), reason: "over".into() });
        assert_eq!(m.resolve("x/x", none), MovedResult::MovedTo { origin: "y/y".into(), reason: "main".into() });
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].starts_with("MOVED: x/x has duplicate entries"), "{warnings:?}");
    }

    #[test]
    fn cycle_guard() {
        let m = Moved::parse("a/a|b/b|d|r\nb/b|a/a|d|r\nc/c|a/a|d|into the cycle\n");
        let cycle = MovedResult::Cycle;
        assert_eq!(m.resolve("a/a", |_| false), cycle);
        assert_eq!(m.resolve("b/b", |_| false), cycle);
        assert_eq!(m.resolve("c/c", |_| false), cycle);
        assert_eq!(
            m.resolve("c/c", |o| o == "b/b"),
            MovedResult::MovedTo { origin: "b/b".into(), reason: "r".into() }
        );
    }
}
