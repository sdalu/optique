use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use crate::apply;
use crate::model::options::GroupKind;
use crate::model::origin::PortKey;
use crate::model::port::PortInfo;
use crate::optionsfile::SavedOptionsFile;

/// Editable option state for one options file (shared by all flavors of an
/// origin that use the same OPTIONS_NAME).
#[derive(Debug)]
pub struct OptState {
    pub saved: Option<SavedOptionsFile>,
    /// What `sync` would write with no user edits (saved choices kept,
    /// new options at their effective state).
    pub baseline: BTreeSet<String>,
    /// Current user-staged enabled set.
    pub staged: BTreeSet<String>,
}

/// UI-level status of a port, most severe first.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum UiStatus {
    Conflict,
    Edited,
    Unconfigured,
    Stale,
    Ok,
}

/// The whole editing session: scanned ports plus staged option state.
pub struct Session {
    /// Canonical key -> port info (from the scan).
    pub ports: BTreeMap<PortKey, PortInfo>,
    /// OPTIONS_NAME -> editable state.
    pub states: HashMap<String, OptState>,
    /// Requested key -> canonical key (dep edges may use either form).
    pub aliases: HashMap<PortKey, PortKey>,
    /// Canonical roots the closure is anchored on (for reachability GC).
    pub roots: Vec<PortKey>,
}

impl Session {
    pub fn new(
        ports: BTreeMap<PortKey, PortInfo>,
        aliases: HashMap<PortKey, PortKey>,
        requested_roots: &[PortKey],
        options_dir: &Path,
    ) -> Self {
        let mut session =
            Session { ports, states: HashMap::new(), aliases, roots: Vec::new() };
        session.roots = requested_roots
            .iter()
            .filter_map(|r| session.resolve(r))
            .collect();
        if session.roots.len() != requested_roots.len() {
            // Some root didn't resolve (e.g. MOVED rename): disable GC rather
            // than risk collecting live ports.
            session.roots = session.ports.keys().cloned().collect();
        }
        let keys: Vec<PortKey> = session.ports.keys().cloned().collect();
        for key in keys {
            session.ensure_state(&key, options_dir);
        }
        session
    }

    /// Resolve a (possibly non-canonical) key to the canonical ports-map key.
    pub fn resolve(&self, key: &PortKey) -> Option<PortKey> {
        if self.ports.contains_key(key) {
            return Some(key.clone());
        }
        self.aliases.get(key).cloned()
    }

    fn ensure_state(&mut self, key: &PortKey, options_dir: &Path) {
        let Some(info) = self.ports.get(key) else { return };
        if !info.options.has_options() {
            return;
        }
        if !self.states.contains_key(&info.options_name) {
            let saved =
                SavedOptionsFile::load(&options_dir.join(&info.options_name).join("options"));
            let baseline = close_implies(info, apply::sync_enabled_set(info, saved.as_ref()));
            self.states.insert(
                info.options_name.clone(),
                OptState { saved, staged: baseline.clone(), baseline },
            );
        }
    }

    /// Merge a background re-scan into the closure: replace/add ports, create
    /// state for newcomers, then drop ports no longer reachable from the
    /// roots. Returns (added, removed) counts. Option states are never
    /// discarded, so a port that flip-flops out and back keeps its edits.
    pub fn merge(&mut self, result: crate::query::scanner::ScanResult, options_dir: &Path) -> (usize, usize) {
        self.aliases.extend(result.aliases);
        let mut added_keys = Vec::new();
        for (key, info) in result.ports {
            if self.ports.insert(key.clone(), info).is_none() {
                added_keys.push(key.clone());
            }
            self.ensure_state(&key, options_dir);
        }
        // Reachability GC from the roots.
        let mut reachable: std::collections::HashSet<PortKey> = Default::default();
        let mut stack: Vec<PortKey> = self.roots.iter().filter_map(|r| self.resolve(r)).collect();
        while let Some(key) = stack.pop() {
            if !reachable.insert(key.clone()) {
                continue;
            }
            if let Some(info) = self.ports.get(&key) {
                for dep in &info.deps {
                    if let Some(target) = self.resolve(&dep.target) {
                        if !reachable.contains(&target) {
                            stack.push(target);
                        }
                    }
                }
            }
        }
        let before = self.ports.len();
        self.ports.retain(|k, _| reachable.contains(k));
        let removed = before - self.ports.len();
        let added = added_keys.iter().filter(|k| self.ports.contains_key(*k)).count();
        (added, removed)
    }

    pub fn state(&self, info: &PortInfo) -> Option<&OptState> {
        self.states.get(&info.options_name)
    }

    /// Reload saved files after an apply so statuses reflect reality.
    pub fn reload_saved(&mut self, options_dir: &Path) {
        for info in self.ports.values() {
            if let Some(state) = self.states.get_mut(&info.options_name) {
                state.saved =
                    SavedOptionsFile::load(&options_dir.join(&info.options_name).join("options"));
                state.baseline =
                    close_implies(info, apply::sync_enabled_set(info, state.saved.as_ref()));
            }
        }
    }

    /// Toggle an option; returns a user-facing error when refused.
    pub fn toggle(&mut self, key: &PortKey, opt: &str) -> Result<(), String> {
        let info = self.ports.get(key).ok_or_else(|| "unknown port".to_string())?.clone();
        let state = self
            .states
            .get_mut(&info.options_name)
            .ok_or_else(|| "port has no options".to_string())?;
        let opts = &info.options;

        if opts.is_forced(opt) {
            return Err(format!("{opt} is forced by make.conf (*_FORCE); the options file cannot override it"));
        }

        if state.staged.contains(opt) {
            // Turning off.
            if let Some(by) = implier_of(&info, &state.staged, opt) {
                return Err(format!("{opt} is implied by {by}; disable {by} first"));
            }
            let group = opts.groups.iter().find(|g| g.members.iter().any(|m| m == opt));
            if let Some(g) = group {
                let on = g.members.iter().filter(|m| state.staged.contains(*m)).count();
                match g.kind {
                    GroupKind::Single if on == 1 => {
                        return Err(format!(
                            "{}: single-choice group needs exactly one option; select another member instead",
                            g.name
                        ));
                    }
                    GroupKind::Multi if on == 1 => {
                        return Err(format!("{}: multi group needs at least one option enabled", g.name));
                    }
                    _ => {}
                }
            }
            state.staged.remove(opt);
        } else {
            // Turning on: single/radio groups deselect siblings.
            if let Some(g) = opts
                .groups
                .iter()
                .find(|g| matches!(g.kind, GroupKind::Single | GroupKind::Radio) && g.members.iter().any(|m| m == opt))
            {
                for m in &g.members {
                    if m != opt {
                        state.staged.remove(m);
                    }
                }
            }
            state.staged.insert(opt.to_string());
            state.staged = close_implies(&info, std::mem::take(&mut state.staged));
        }
        Ok(())
    }

    pub fn reset_to_defaults(&mut self, key: &PortKey) {
        if let Some(info) = self.ports.get(key).cloned() {
            if let Some(state) = self.states.get_mut(&info.options_name) {
                let defaults: BTreeSet<String> = info
                    .options
                    .complete
                    .iter()
                    .filter(|o| info.options.defaults.contains(*o))
                    .cloned()
                    .collect();
                state.staged = close_implies(&info, defaults);
            }
        }
    }

    pub fn revert(&mut self, key: &PortKey) {
        if let Some(info) = self.ports.get(key) {
            if let Some(state) = self.states.get_mut(&info.options_name) {
                state.staged = state.baseline.clone();
            }
        }
    }

    /// check-config-style validation of the staged set.
    pub fn violations(&self, info: &PortInfo) -> Vec<String> {
        let Some(state) = self.state(info) else { return Vec::new() };
        let staged = &state.staged;
        let mut out = Vec::new();
        for opt in staged {
            if let Some(def) = info.options.defs.get(opt) {
                for p in &def.prevents {
                    if staged.contains(p) {
                        let msg = def
                            .prevents_msg
                            .clone()
                            .unwrap_or_else(|| "mutually exclusive".to_string());
                        out.push(format!("{opt} conflicts with {p}: {msg}"));
                    }
                }
            }
        }
        for g in &info.options.groups {
            let on = g.members.iter().filter(|m| staged.contains(*m)).count();
            match g.kind {
                GroupKind::Single if on != 1 => {
                    out.push(format!("group {} needs exactly one member enabled ({on} enabled)", g.name));
                }
                GroupKind::Multi if on == 0 => {
                    out.push(format!("group {} needs at least one member enabled", g.name));
                }
                GroupKind::Radio if on > 1 => {
                    out.push(format!("group {} allows at most one member enabled ({on} enabled)", g.name));
                }
                _ => {}
            }
        }
        // Deduplicate the symmetric PREVENTS pairs.
        out.sort();
        out.dedup();
        out
    }

    pub fn status(&self, info: &PortInfo) -> UiStatus {
        if !info.options.has_options() {
            return UiStatus::Ok;
        }
        let Some(state) = self.state(info) else { return UiStatus::Ok };
        if !self.violations(info).is_empty() {
            return UiStatus::Conflict;
        }
        if state.staged != state.baseline {
            return UiStatus::Edited;
        }
        match &state.saved {
            None => UiStatus::Unconfigured,
            Some(saved) => {
                let cur: BTreeSet<&str> = info.options.complete.iter().map(String::as_str).collect();
                let was: BTreeSet<&str> = saved.complete.iter().map(String::as_str).collect();
                if cur == was { UiStatus::Ok } else { UiStatus::Stale }
            }
        }
    }

    /// Which enabled option (if any) implies `opt`, keeping it locked on.
    pub fn implied_by(&self, info: &PortInfo, opt: &str) -> Option<String> {
        let state = self.state(info)?;
        implier_of(info, &state.staged, opt)
    }

    /// Any staged edit or pending baseline write (unconfigured/stale)?
    pub fn dirty(&self) -> bool {
        self.states.values().any(|s| s.staged != s.baseline)
    }
}

/// Transitive IMPLIES closure of an enabled set, restricted to the port's
/// known options (mirrors bsd.options.mk, which force-adds implied options).
fn close_implies(info: &PortInfo, mut set: BTreeSet<String>) -> BTreeSet<String> {
    loop {
        let mut added = false;
        for opt in set.clone() {
            if let Some(def) = info.options.defs.get(&opt) {
                for imp in &def.implies {
                    if info.options.complete.iter().any(|o| o == imp) && set.insert(imp.clone()) {
                        added = true;
                    }
                }
            }
        }
        if !added {
            return set;
        }
    }
}

/// Find an enabled option (other than `opt`) whose IMPLIES closure contains `opt`.
fn implier_of(info: &PortInfo, staged: &BTreeSet<String>, opt: &str) -> Option<String> {
    for candidate in staged {
        if candidate == opt {
            continue;
        }
        let mut seen = BTreeSet::new();
        let mut stack = vec![candidate.clone()];
        while let Some(cur) = stack.pop() {
            if let Some(def) = info.options.defs.get(&cur) {
                for imp in &def.implies {
                    if imp == opt {
                        return Some(candidate.clone());
                    }
                    if seen.insert(imp.clone()) {
                        stack.push(imp.clone());
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::options::{OptionDef, OptionGroup, PortOptions};

    fn mk_port(
        complete: &[&str],
        defaults: &[&str],
        groups: Vec<OptionGroup>,
        defs: Vec<(&str, OptionDef)>,
    ) -> (BTreeMap<PortKey, PortInfo>, PortKey) {
        let key = PortKey::parse("cat/port").unwrap();
        let mut d = BTreeMap::new();
        for (name, def) in defs {
            d.insert(name.to_string(), def);
        }
        let info = PortInfo {
            key: key.clone(),
            canonical: key.clone(),
            pkgname: "port-1.0".into(),
            flavors: vec![],
            options_name: "cat_port".into(),
            options: PortOptions {
                complete: complete.iter().map(|s| s.to_string()).collect(),
                defaults: defaults.iter().map(|s| s.to_string()).collect(),
                effective: defaults.iter().map(|s| s.to_string()).collect(),
                groups,
                defs: d,
                ..Default::default()
            },
            deps: vec![],
            broken: None,
            ignore: None,
            deprecated: None,
            default_versions: vec![],
            warnings: vec![],
        };
        let mut ports = BTreeMap::new();
        ports.insert(key.clone(), info);
        (ports, key)
    }

    fn session_for(ports: BTreeMap<PortKey, PortInfo>) -> Session {
        let tmp = tempfile::tempdir().unwrap();
        let roots: Vec<PortKey> = ports.keys().cloned().collect();
        Session::new(ports, HashMap::new(), &roots, tmp.path())
    }

    #[test]
    fn single_group_switches_selection() {
        let g = OptionGroup {
            kind: GroupKind::Single,
            name: "IMPL".into(),
            desc: String::new(),
            members: vec!["A".into(), "B".into()],
        };
        let (ports, key) = mk_port(&["A", "B"], &["A"], vec![g], vec![]);
        let mut s = session_for(ports);
        // Turning off the only selected member is refused.
        assert!(s.toggle(&key, "A").is_err());
        // Selecting B deselects A.
        s.toggle(&key, "B").unwrap();
        let info = s.ports.get(&key).unwrap();
        let st = s.state(info).unwrap();
        assert!(st.staged.contains("B") && !st.staged.contains("A"));
        assert!(s.violations(info).is_empty());
    }

    #[test]
    fn multi_group_keeps_one() {
        let g = OptionGroup {
            kind: GroupKind::Multi,
            name: "M".into(),
            desc: String::new(),
            members: vec!["A".into(), "B".into()],
        };
        let (ports, key) = mk_port(&["A", "B"], &["A"], vec![g], vec![]);
        let mut s = session_for(ports);
        assert!(s.toggle(&key, "A").is_err()); // last enabled member
        s.toggle(&key, "B").unwrap();
        s.toggle(&key, "A").unwrap(); // now B carries the group
        let info = s.ports.get(&key).unwrap();
        assert!(s.violations(info).is_empty());
    }

    #[test]
    fn implies_are_closed_and_locked() {
        let defs = vec![(
            "KRB",
            OptionDef { implies: vec!["SSL".into()], ..Default::default() },
        )];
        let (ports, key) = mk_port(&["KRB", "SSL"], &[], vec![], defs);
        let mut s = session_for(ports);
        s.toggle(&key, "KRB").unwrap();
        let info = s.ports.get(&key).unwrap().clone();
        let st = s.state(&info).unwrap();
        assert!(st.staged.contains("SSL"), "implied option auto-enabled");
        assert_eq!(s.implied_by(&info, "SSL").as_deref(), Some("KRB"));
        assert!(s.toggle(&key, "SSL").is_err(), "implied option locked on");
        s.toggle(&key, "KRB").unwrap(); // off
        s.toggle(&key, "SSL").unwrap(); // now free
    }

    #[test]
    fn prevents_reported_as_conflict() {
        let defs = vec![(
            "X",
            OptionDef {
                prevents: vec!["Y".into()],
                prevents_msg: Some("pick one".into()),
                ..Default::default()
            },
        )];
        let (ports, key) = mk_port(&["X", "Y"], &["Y"], vec![], defs);
        let mut s = session_for(ports);
        s.toggle(&key, "X").unwrap();
        let info = s.ports.get(&key).unwrap();
        assert_eq!(s.status(info), UiStatus::Conflict);
        assert!(s.violations(info)[0].contains("pick one"));
    }

    #[test]
    fn forced_options_refuse_toggle() {
        let (ports, key) = mk_port(&["A"], &[], vec![], vec![]);
        let mut s = session_for(ports);
        s.ports.get_mut(&key).unwrap().options.force_set.insert("A".into());
        let err = s.toggle(&key, "A").unwrap_err();
        assert!(err.contains("forced"));
    }
}
