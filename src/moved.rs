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
                "port no longer exists in the tree (its MOVED entries form a cycle, \
                 which poudriere's check_moved follows forever: an upstream defect)"
                    .to_string(),
            ),
            MovedResult::Unchanged | MovedResult::MovedTo { .. } => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Moved {
    /// old origin -> (new origin or None if removed, reason), chains
    /// collapsed as `parse_MOVED.awk` collapses them (see `add`).
    map: HashMap<String, (Option<String>, String)>,
    /// new origin -> the old origins whose entry now leads to it, the
    /// awk's `backrefs`: what a later entry moving that origin repoints.
    backrefs: HashMap<String, Vec<String>>,
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

    /// Read entries in file order, collapsing chains as `update_forwards`
    /// in poudriere's `awk/parse_MOVED.awk` does: an entry `B|C` repoints
    /// every earlier entry that led to `B` so it leads to `C` (or is
    /// removed with `B|C`'s reason, when `C` is empty). An entry `X|B`
    /// read after `B|C` keeps `B`: only earlier entries are repointed.
    /// A line without a `|`, or with an empty old origin, names no move
    /// and is skipped.
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
            let old = old.trim();
            if old.is_empty() {
                continue;
            }
            if self.map.contains_key(old) {
                warnings.push(format!("MOVED: {old} has duplicate entries ({file})"));
                continue;
            }
            let reason = fields.nth(1).unwrap_or("").to_string();
            let new = new.trim();
            let target = (!new.is_empty()).then(|| new.to_string());
            let led_here = self.backrefs.remove(old).unwrap_or_default();
            for earlier in &led_here {
                self.map.insert(earlier.clone(), (target.clone(), reason.clone()));
            }
            self.map.insert(old.to_string(), (target.clone(), reason));
            if let Some(target) = target {
                let refs = self.backrefs.entry(target).or_default();
                refs.push(old.to_string());
                refs.extend(led_here);
            }
        }
    }

    /// Chase MOVED for a bare origin (no flavor), as poudriere's
    /// `check_moved` follows the collapsed entries: the target if it is in
    /// the tree, else that target's own entry, and so on. MOVED is history
    /// and an origin moved away can be reused later, so an origin `exists`
    /// says is in the tree is that port and is not chased. The collapse
    /// can leave an entry leading to itself (`a|b` then `b|a` give `a|a`
    /// and `b|a`); where poudriere then loops forever, the chase reports a
    /// cycle once it comes back to an origin it passed.
    pub fn resolve(&self, origin: &str, exists: impl Fn(&str) -> bool) -> MovedResult {
        if exists(origin) {
            return MovedResult::Unchanged;
        }
        let mut current = origin.to_string();
        let mut reason = String::new();
        let mut passed = HashSet::new();
        while let Some((next, why)) = self.map.get(&current) {
            if !passed.insert(current.clone()) {
                return MovedResult::Cycle;
            }
            reason = why.clone();
            match next {
                Some(n) => current = n.clone(),
                None => return MovedResult::Removed { reason },
            }
            if exists(&current) {
                break;
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

    /// `a|b`, `b|c`, `x|b`: the awk repoints `a` to `c` and leaves `x`
    /// on `b`, read after `b|c`. `a` goes to `c` even with `b` back in the
    /// tree, never tested; `x` stops at `b` when it exists, else goes on.
    #[test]
    fn chains_collapse_in_file_order() {
        let m = Moved::parse("a/a|b/b|2026-01-01|one\nb/b|c/c|2026-02-01|two\nx/x|b/b|2026-03-01|three\n");
        let to = |o: &str, r: &str| MovedResult::MovedTo { origin: o.into(), reason: r.into() };
        let b_back = |o: &str| o == "b/b" || o == "c/c";
        let c_only = |o: &str| o == "c/c";
        assert_eq!(m.resolve("a/a", b_back), to("c/c", "two"));
        assert_eq!(m.resolve("a/a", c_only), to("c/c", "two"));
        assert_eq!(m.resolve("x/x", b_back), to("b/b", "three"));
        assert_eq!(m.resolve("x/x", c_only), to("c/c", "two"));
        assert_eq!(m.resolve("b/b", b_back), MovedResult::Unchanged);
    }

    /// A removal read later repoints earlier entries to it, with its reason.
    #[test]
    fn a_later_removal_ends_the_earlier_chain() {
        let m = Moved::parse("a/a|b/b|2026-01-01|renamed\nb/b||2026-02-01|expired\n");
        let removed = MovedResult::Removed { reason: "expired".into() };
        assert_eq!(m.resolve("a/a", |o| o == "b/b"), removed);
        assert_eq!(m.resolve("b/b", |_| false), removed);
    }

    /// `a|b`, `b|a` collapse to `a|a` and `b|a`. With `a` in the tree both
    /// lead there; without it poudriere's `check_moved` loops forever, and
    /// the chase reports a cycle, `b` there or not, as for an entry
    /// leading into the cycle.
    #[test]
    fn a_cycle_resolves_to_its_collapsed_target_or_is_reported() {
        let m = Moved::parse("a/a|b/b|d|r\nb/b|a/a|d|back\nc/c|a/a|d|into the cycle\n");
        let a_there = |o: &str| o == "a/a";
        assert_eq!(m.resolve("a/a", a_there), MovedResult::Unchanged);
        assert_eq!(
            m.resolve("b/b", a_there),
            MovedResult::MovedTo { origin: "a/a".into(), reason: "back".into() }
        );
        assert_eq!(
            m.resolve("c/c", a_there),
            MovedResult::MovedTo { origin: "a/a".into(), reason: "into the cycle".into() }
        );
        let nothing = |_: &str| false;
        let b_only = |o: &str| o == "b/b";
        for exists in [&nothing as &dyn Fn(&str) -> bool, &b_only] {
            assert_eq!(m.resolve("a/a", exists), MovedResult::Cycle);
            assert_eq!(m.resolve("c/c", exists), MovedResult::Cycle);
        }
        assert_eq!(m.resolve("b/b", nothing), MovedResult::Cycle);
        assert!(MovedResult::Cycle.gone_reason().unwrap().contains("cycle"));
    }
}
