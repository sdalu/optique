use std::collections::HashMap;
use std::path::Path;

/// Outcome of resolving an origin through the MOVED file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MovedResult {
    Unchanged,
    /// Followed one or more MOVED entries to a new origin.
    MovedTo { origin: String, reason: String },
    /// Entry with an empty target: the port was removed.
    Removed { reason: String },
}

#[derive(Debug, Default)]
pub struct Moved {
    /// old origin -> (new origin or None if removed, reason)
    map: HashMap<String, (Option<String>, String)>,
}

impl Moved {
    pub fn load(portsdir: &Path) -> Self {
        let text = std::fs::read_to_string(portsdir.join("MOVED")).unwrap_or_default();
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Self {
        let mut map = HashMap::new();
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
            map.insert(
                old.trim().to_string(),
                (if new.is_empty() { None } else { Some(new.to_string()) }, reason),
            );
        }
        Moved { map }
    }

    /// Chase MOVED chains (with a cycle guard) for a bare origin (no flavor).
    /// MOVED is history and an origin moved away can be reused later, so the
    /// chase stops at the first origin `exists` says is in the tree, the one
    /// asked for included.
    pub fn resolve(&self, origin: &str, exists: impl Fn(&str) -> bool) -> MovedResult {
        let mut current = origin.to_string();
        let mut reason = String::new();
        let mut hops = 0;
        while let Some((next, why)) = self.map.get(&current).filter(|_| !exists(&current)) {
            hops += 1;
            if hops > 32 {
                break; // cycle or absurd chain; treat as unresolvable
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

    #[test]
    fn cycle_guard() {
        let m = Moved::parse("a/a|b/b|d|r\nb/b|a/a|d|r\n");
        // must terminate; result is one of the two origins
        let _ = m.resolve("a/a", |_| false);
    }
}
