use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::model::origin::PortKey;

/// Outcome of resolving an originspec through the MOVED file, as
/// poudriere's `check_moved` follows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MovedResult {
    /// No entry leads anywhere: none for the originspec, or a chain ending
    /// at an origin in no tree with no entry of its own, which
    /// `check_moved` reports as no move at all.
    Unchanged,
    /// Followed one or more MOVED entries to an origin in the tree.
    MovedTo { key: PortKey, reason: String },
    /// Entry with an empty target: the port was removed.
    Removed { reason: String },
    /// The chain comes back to an originspec it passed, none of them in
    /// the tree: it leads to no port, but nothing says one was removed.
    Cycle,
    /// A flavored originspec found only its bare origin's entry, and that
    /// entry names another flavor (`to`): `_check_moved` stops poudriere
    /// on it.
    FlavorClash { to: String },
}

impl MovedResult {
    /// Why the originspec leads to no port, when it does not: removed,
    /// lost in a cycle, or sent to another flavor. None for one that still
    /// has (or moved to) a port.
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
            MovedResult::FlavorClash { to } => Some(format!(
                "MOVED moves its bare origin to {to}, another flavor, which poudriere's \
                 check_moved stops on: a wrong entry, or the list should name the default flavor"
            )),
            MovedResult::Unchanged | MovedResult::MovedTo { .. } => None,
        }
    }
}

/// A listed origin MOVED moves, and what became of it: followed to `to`
/// (`--follow-moved`), or left out of the scan for the list to be fixed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedMove {
    pub listed: PortKey,
    pub to: PortKey,
    pub reason: String,
    pub followed: bool,
    /// `--follow-moved` would follow it: false under synth, which reads no
    /// MOVED and never builds the port it leads to.
    pub can_follow: bool,
    /// `listed` is buildable all the same (its origin in a tree, bare,
    /// `@all` or `@-`): poudriere moves it because its `_listed_ports` asks
    /// `check_moved` before testing the origin, an upstream defect optique
    /// conforms to and names. A named flavor is not known to be in the
    /// port's FLAVORS when MOVED is read, and a flavor-level entry is how
    /// a flavor is retired (audio/kid3@qt5): no defect is named for one.
    pub in_tree: bool,
}

impl ListedMove {
    /// What is said of it on standard error.
    pub fn message(&self) -> String {
        let reason = if self.reason.is_empty() {
            String::new()
        } else {
            format!(" ({})", self.reason)
        };
        let (listed, to) = (&self.listed, &self.to);
        let defect = if self.in_tree {
            format!(
                "; {} is in the tree too, but poudriere's check_moved moves it all the same \
                 (an upstream defect)",
                listed.origin
            )
        } else {
            String::new()
        };
        if self.followed {
            format!(
                "listed {listed} moved to {to}{reason}{defect}: followed, but the list still names {listed}"
            )
        } else {
            format!(
                "listed {listed} moved to {to}{reason}{defect}: fix the list{}",
                self.or_follow(" to follow it")
            )
        }
    }

    /// `, or --follow-moved` and `tail` when that would follow it, for a
    /// move not followed; nothing otherwise.
    pub fn or_follow(&self, tail: &str) -> String {
        if self.followed || !self.can_follow {
            String::new()
        } else {
            format!(", or --follow-moved{tail}")
        }
    }
}

/// An originspec as MOVED writes it, `origin[@flavor]`, taken as written:
/// MOVED is not checked, a malformed target only leads nowhere.
fn spec(text: &str) -> PortKey {
    match text.split_once('@') {
        Some((origin, flavor)) if !flavor.is_empty() => {
            PortKey::new(origin, Some(flavor.to_string()))
        }
        Some((origin, _)) => PortKey::new(origin, None),
        None => PortKey::new(text, None),
    }
}

#[derive(Debug, Default)]
pub struct Moved {
    /// old originspec -> (new originspec or None if removed, reason),
    /// as written, chains collapsed as `parse_MOVED.awk` collapses them
    /// (see `add`).
    map: HashMap<String, (Option<String>, String)>,
    /// new originspec -> the old ones whose entry now leads to it, the
    /// awk's `backrefs`: what a later entry moving that one repoints.
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
                self.map
                    .insert(earlier.clone(), (target.clone(), reason.clone()));
            }
            self.map.insert(old.to_string(), (target.clone(), reason));
            if let Some(target) = target {
                let refs = self.backrefs.entry(target).or_default();
                refs.push(old.to_string());
                refs.extend(led_here);
            }
        }
    }

    /// One step of poudriere's `_check_moved`: the entry of `key` as
    /// written; else, for a flavored one, its bare origin's
    /// ([`bare_step`](Self::bare_step)). None when neither has an entry;
    /// `Err` the clashing target.
    fn step(&self, key: &PortKey) -> Option<Result<(Option<PortKey>, &str), String>> {
        if let Some((next, why)) = self.map.get(&key.to_string()) {
            return Some(Ok((next.as_deref().map(spec), why)));
        }
        self.bare_step(key)
    }

    /// `_check_moved`'s second lookup: for a flavored `key`, its bare
    /// origin's entry, the target taking `key`'s flavor when it names none
    /// and refused when it names another. None for an unflavored `key`, or
    /// when its bare origin has no entry; `Err` the clashing target.
    fn bare_step(&self, key: &PortKey) -> Option<Result<(Option<PortKey>, &str), String>> {
        let flavor = key.flavor.as_ref()?;
        let (next, why) = self.map.get(&key.origin)?;
        let Some(next) = next else {
            return Some(Ok((None, why)));
        };
        let mut target = spec(next);
        match &target.flavor {
            None => target.flavor = Some(flavor.clone()),
            Some(f) if f == flavor => {}
            Some(_) => return Some(Err(next.clone())),
        }
        Some(Ok((Some(target), why)))
    }

    /// Chase MOVED for an originspec as poudriere's `check_moved` does:
    /// each step is [`step`](Self::step), and the chase stops at the first
    /// target whose origin `exists` says is in a tree. The originspec
    /// itself is never tested: poudriere moves a listed origin still in the
    /// tree when it has an entry (math/mumps, back after moving to
    /// math/mumps-edf), and the caller tests it first where the build does
    /// (`make config`, or a dependency). A chain ending at an origin in no
    /// tree, with no entry of its own, is no move, as `check_moved` returns
    /// it. The collapse can leave an entry leading to itself (`a|b` then
    /// `b|a` give `a|a` and `b|a`); where poudriere then loops forever, the
    /// chase reports a cycle once it comes back to an originspec it passed.
    pub fn resolve(&self, key: &PortKey, exists: impl Fn(&str) -> bool) -> MovedResult {
        self.resolve_spec(key, false, exists)
    }

    /// [`resolve`](Self::resolve) for an originspec that may name a
    /// subpackage: `subpackage` says a listed root named one
    /// (`origin[@flavor]~sub`, dropped from the key by
    /// `PortKey::parse_listed`), and a flavor still holding one
    /// (`@flavor~sub`, as `PortKey::parse_dependency` keeps it) is split as
    /// `originspec_decode` splits it. `_check_moved` looks the whole
    /// originspec up first, `~sub` included, and MOVED names no subpackage:
    /// without a flavor there is no move; with one, only the bare origin's
    /// entry applies, never `origin@flavor`'s. Its target names no
    /// subpackage, and the chase goes on from it as from any other.
    pub fn resolve_spec(
        &self,
        key: &PortKey,
        subpackage: bool,
        exists: impl Fn(&str) -> bool,
    ) -> MovedResult {
        let (start, mut sub) = match key.flavor.as_deref().and_then(|f| f.split_once('~')) {
            Some((flavor, _)) => (
                PortKey::new(
                    key.origin.clone(),
                    (!flavor.is_empty()).then(|| flavor.to_string()),
                ),
                true,
            ),
            None => (key.clone(), subpackage),
        };
        let mut current = start.clone();
        let mut reason = String::new();
        let mut passed = HashSet::new();
        let mut found = false;
        loop {
            let step = if sub {
                self.bare_step(&current)
            } else {
                self.step(&current)
            };
            let Some(step) = step else { break };
            let (next, why) = match step {
                Ok(s) => s,
                Err(to) => return MovedResult::FlavorClash { to },
            };
            // The `~sub` originspec is not one the chase can come back to.
            if !std::mem::take(&mut sub) && !passed.insert(current.clone()) {
                return MovedResult::Cycle;
            }
            reason = why.to_string();
            match next {
                Some(n) => current = n,
                None => return MovedResult::Removed { reason },
            }
            if exists(&current.origin) {
                found = true;
                break;
            }
        }
        if !found || current == start {
            MovedResult::Unchanged
        } else {
            MovedResult::MovedTo {
                key: current,
                reason,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> PortKey {
        PortKey::parse(s).unwrap()
    }

    fn to(k: &str, reason: &str) -> MovedResult {
        MovedResult::MovedTo {
            key: key(k),
            reason: reason.into(),
        }
    }

    #[test]
    fn chains_and_removals() {
        let m = Moved::parse(
            "# comment\n\
             a/one|b/two|2024-01-01|renamed\n\
             b/two|c/three|2024-02-01|renamed again\n\
             d/gone||2024-03-01|abandoned\n",
        );
        let c_only = |o: &str| o == "c/three";
        assert_eq!(
            m.resolve(&key("x/untouched"), c_only),
            MovedResult::Unchanged
        );
        assert_eq!(
            m.resolve(&key("a/one"), c_only),
            to("c/three", "renamed again")
        );
        assert_eq!(
            m.resolve(&key("d/gone"), c_only),
            MovedResult::Removed {
                reason: "abandoned".into()
            }
        );
    }

    /// `check_moved` never tests the originspec it starts from: math/mumps,
    /// in the tree again, still moves to math/mumps-edf (the caller tests
    /// it first where the build does). A chain stops at the first origin in
    /// the tree: math/mumps4 moved to math/mumps, which exists.
    #[test]
    fn the_start_is_never_tested_and_a_chain_stops_at_an_existing_origin() {
        let m = Moved::parse(
            "math/mumps|math/mumps-edf|2026-10-07|Repomove to math/mumps-edf\n\
             math/mumps4|math/mumps|2026-10-07|Repomove to math/mumps\n",
        );
        let exists = |o: &str| o == "math/mumps" || o == "math/mumps-edf";
        assert_eq!(
            m.resolve(&key("math/mumps"), exists),
            to("math/mumps-edf", "Repomove to math/mumps-edf")
        );
        assert_eq!(
            m.resolve(&key("math/mumps4"), exists),
            to("math/mumps", "Repomove to math/mumps")
        );
    }

    /// A chain ending at an origin in no tree, with no entry of its own, is
    /// no move: `check_moved` returns 1, and poudriere goes on with the
    /// originspec as listed.
    #[test]
    fn a_chain_leading_nowhere_is_no_move() {
        let m = Moved::parse("a/a|b/b|2026-01-01|renamed\n");
        assert_eq!(m.resolve(&key("a/a"), |_| false), MovedResult::Unchanged);
        assert_eq!(m.resolve(&key("a/a"), |o| o == "b/b"), to("b/b", "renamed"));
    }

    /// The four shapes of the ports tree's MOVED: a flavor moved to another
    /// (kid3), a flavor dropped (ghostwriter), a port become a flavor
    /// (mg-static), a port moved while its name is in the tree again
    /// (mumps). A flavored originspec without an entry of its own takes
    /// its bare origin's, the target keeping the listed flavor, or naming
    /// it already; a target naming another flavor stops poudriere.
    #[test]
    fn originspecs_follow_check_moved() {
        let m = Moved::parse(
            "editors/mg-static|editors/mg@static|2021-02-28|Flavored\n\
             audio/kid3@qt5|audio/kid3@qt6|2024-02-25|The legacy qt5 flavor has been replaced by the qt6 flavor\n\
             editors/ghostwriter@qt5|editors/ghostwriter|2024-12-29|Flavors removed, now supports only qt6\n\
             math/mumps|math/mumps-edf|2026-10-07|Repomove to math/mumps-edf\n\
             cat/old|cat/new|2026-01-01|renamed\n\
             cat/gone||2026-01-01|Has expired\n",
        );
        let tree = [
            "editors/mg",
            "audio/kid3",
            "editors/ghostwriter",
            "math/mumps",
            "math/mumps-edf",
            "cat/new",
        ];
        let exists = |o: &str| tree.contains(&o);
        let r = |s: &str| m.resolve(&key(s), exists);
        assert_eq!(
            r("audio/kid3@qt5"),
            to(
                "audio/kid3@qt6",
                "The legacy qt5 flavor has been replaced by the qt6 flavor"
            )
        );
        assert_eq!(
            r("audio/kid3"),
            MovedResult::Unchanged,
            "only @qt5 has an entry"
        );
        assert_eq!(r("audio/kid3@kf6"), MovedResult::Unchanged);
        assert_eq!(
            r("editors/ghostwriter@qt5"),
            to(
                "editors/ghostwriter",
                "Flavors removed, now supports only qt6"
            )
        );
        assert_eq!(r("editors/mg-static"), to("editors/mg@static", "Flavored"));
        assert_eq!(
            r("editors/mg-static@static"),
            to("editors/mg@static", "Flavored")
        );
        assert_eq!(
            r("editors/mg-static@dynamic"),
            MovedResult::FlavorClash {
                to: "editors/mg@static".into()
            }
        );
        assert!(
            r("editors/mg-static@all")
                .gone_reason()
                .unwrap()
                .contains("editors/mg@static, another flavor")
        );
        assert_eq!(
            r("math/mumps"),
            to("math/mumps-edf", "Repomove to math/mumps-edf")
        );
        assert_eq!(
            r("cat/old@py311"),
            to("cat/new@py311", "renamed"),
            "the flavor re-attached"
        );
        assert_eq!(r("cat/old@all"), to("cat/new@all", "renamed"));
        assert_eq!(
            r("cat/gone@x"),
            MovedResult::Removed {
                reason: "Has expired".into()
            }
        );
    }

    /// An originspec naming a subpackage goes to `_check_moved` whole, and
    /// MOVED names none: `cat/old~sub` is not moved, `cat/old@fl~sub` only
    /// by the bare origin's entry, its flavor re-attached and its
    /// subpackage gone, never by `cat/old@fl`'s. A dependency's flavor
    /// still holding the `~sub` is split the same way.
    #[test]
    fn a_subpackage_spec_is_looked_up_whole() {
        let m = Moved::parse(
            "cat/old|cat/new|2026-01-01|renamed\n\
             audio/kid3@qt5|audio/kid3@qt6|2024-02-25|qt6 now\n\
             cat/new|cat/newer|2026-02-01|again\n",
        );
        let exists = |o: &str| o == "audio/kid3" || o == "cat/newer";
        let sub = |s: &str| m.resolve_spec(&key(s), true, exists);
        assert_eq!(sub("cat/old"), MovedResult::Unchanged);
        assert_eq!(
            sub("cat/old@fl"),
            to("cat/newer@fl", "again"),
            "the chase goes on from the target"
        );
        assert_eq!(
            sub("audio/kid3@qt5"),
            MovedResult::Unchanged,
            "no flavor-level entry for a ~sub"
        );
        assert_eq!(m.resolve(&key("cat/old"), exists), to("cat/newer", "again"));
        assert_eq!(
            m.resolve(&key("audio/kid3@qt5"), exists),
            to("audio/kid3@qt6", "qt6 now")
        );
        // A dependency's `@fl~sub` keeps the subpackage in its flavor.
        let dep = |s: &str| m.resolve(&PortKey::parse_dependency(s).unwrap(), exists);
        assert_eq!(dep("cat/old@fl~lib"), to("cat/newer@fl", "again"));
        assert_eq!(dep("audio/kid3@qt5~lib"), MovedResult::Unchanged);
        assert_eq!(dep("cat/old@~lib"), MovedResult::Unchanged);
    }

    /// The main tree's MOVED, then each overlay's: a chain runs across
    /// them, and a repeated origin keeps its first entry, with a warning.
    #[test]
    fn overlays_moved_files_follow_the_main_one() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, over) = (tmp.path().join("main"), tmp.path().join("over"));
        std::fs::create_dir_all(&main).unwrap();
        std::fs::create_dir_all(&over).unwrap();
        std::fs::write(
            main.join("MOVED"),
            "a/a|b/b|2026-01-01|main\nx/x|y/y|2026-01-01|main\n",
        )
        .unwrap();
        std::fs::write(
            over.join("MOVED"),
            "b/b|c/c|2026-02-01|over\nx/x|z/z|2026-02-01|over\n",
        )
        .unwrap();
        let (m, warnings) = Moved::load_all(&[&main, &over]);
        let exists = |o: &str| o == "c/c" || o == "y/y" || o == "z/z";
        assert_eq!(m.resolve(&key("a/a"), exists), to("c/c", "over"));
        assert_eq!(m.resolve(&key("x/x"), exists), to("y/y", "main"));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].starts_with("MOVED: x/x has duplicate entries"),
            "{warnings:?}"
        );
    }

    /// `a|b`, `b|c`, `x|b`: the awk repoints `a` to `c` and leaves `x`
    /// on `b`, read after `b|c`. `a` goes to `c` even with `b` back in the
    /// tree, never tested; `x` stops at `b` when it exists, else goes on;
    /// `b` itself, in the tree or not, has its entry and moves to `c`.
    #[test]
    fn chains_collapse_in_file_order() {
        let m = Moved::parse(
            "a/a|b/b|2026-01-01|one\nb/b|c/c|2026-02-01|two\nx/x|b/b|2026-03-01|three\n",
        );
        let b_back = |o: &str| o == "b/b" || o == "c/c";
        let c_only = |o: &str| o == "c/c";
        assert_eq!(m.resolve(&key("a/a"), b_back), to("c/c", "two"));
        assert_eq!(m.resolve(&key("a/a"), c_only), to("c/c", "two"));
        assert_eq!(m.resolve(&key("x/x"), b_back), to("b/b", "three"));
        assert_eq!(m.resolve(&key("x/x"), c_only), to("c/c", "two"));
        assert_eq!(m.resolve(&key("b/b"), b_back), to("c/c", "two"));
    }

    /// A removal read later repoints earlier entries to it, with its reason.
    #[test]
    fn a_later_removal_ends_the_earlier_chain() {
        let m = Moved::parse("a/a|b/b|2026-01-01|renamed\nb/b||2026-02-01|expired\n");
        let removed = MovedResult::Removed {
            reason: "expired".into(),
        };
        assert_eq!(m.resolve(&key("a/a"), |o| o == "b/b"), removed);
        assert_eq!(m.resolve(&key("b/b"), |_| false), removed);
    }

    /// `a|b`, `b|a` collapse to `a|a` and `b|a`. With `a` in the tree both
    /// lead there (`a` to itself, no move); without it poudriere's
    /// `check_moved` loops forever, and the chase reports a cycle, `b`
    /// there or not, as for an entry leading into the cycle.
    #[test]
    fn a_cycle_resolves_to_its_collapsed_target_or_is_reported() {
        let m = Moved::parse("a/a|b/b|d|r\nb/b|a/a|d|back\nc/c|a/a|d|into the cycle\n");
        let a_there = |o: &str| o == "a/a";
        assert_eq!(m.resolve(&key("a/a"), a_there), MovedResult::Unchanged);
        assert_eq!(m.resolve(&key("b/b"), a_there), to("a/a", "back"));
        assert_eq!(m.resolve(&key("c/c"), a_there), to("a/a", "into the cycle"));
        let nothing = |_: &str| false;
        let b_only = |o: &str| o == "b/b";
        for exists in [&nothing as &dyn Fn(&str) -> bool, &b_only] {
            assert_eq!(m.resolve(&key("a/a"), exists), MovedResult::Cycle);
            assert_eq!(m.resolve(&key("c/c"), exists), MovedResult::Cycle);
        }
        assert_eq!(m.resolve(&key("b/b"), nothing), MovedResult::Cycle);
        assert!(MovedResult::Cycle.gone_reason().unwrap().contains("cycle"));
    }

    /// The same shapes against the host's own MOVED and tree.
    /// Run with: cargo test -- --ignored
    #[test]
    #[ignore = "needs /usr/ports on a FreeBSD host"]
    fn live_moved_shapes() {
        let tree = Path::new("/usr/ports");
        let (m, _) = Moved::load_all(&[tree]);
        let exists = |o: &str| tree.join(o).join("Makefile").is_file();
        let target = |s: &str| match m.resolve(&key(s), exists) {
            MovedResult::MovedTo { key, .. } => key.to_string(),
            other => format!("{other:?}"),
        };
        assert_eq!(target("audio/kid3@qt5"), "audio/kid3@qt6");
        assert_eq!(target("editors/ghostwriter@qt5"), "editors/ghostwriter");
        assert_eq!(target("editors/mg-static"), "editors/mg@static");
        assert_eq!(target("math/mumps"), "math/mumps-edf");
        assert!(matches!(
            m.resolve(&key("editors/mg-static@dynamic"), exists),
            MovedResult::FlavorClash { .. }
        ));
    }
}
