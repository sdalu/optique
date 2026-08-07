use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
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
    /// Staged options contradict the global make.conf OPTIONS_SET/UNSET
    /// policy (only produced when the mc-warn view is active).
    McDeviation,
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
    /// OPTIONS_NAME -> the port whose view owns the shared file (the
    /// default flavor). Staleness and obsolete options are judged against
    /// the owner, since apply writes the file from its point of view.
    pub owners: HashMap<String, PortKey>,
    /// --minimal: bare defaults count as decisions, so nothing is
    /// "undecided" — only deviations and conflicts demand attention.
    pub minimal: bool,
}

/// Is this port the default flavor of its origin (or unflavored)?
fn is_preferred(info: &PortInfo) -> bool {
    match &info.canonical.flavor {
        None => true,
        Some(f) => info.flavors.first() == Some(f),
    }
}

impl Session {
    pub fn new(
        ports: BTreeMap<PortKey, PortInfo>,
        aliases: HashMap<PortKey, PortKey>,
        requested_roots: &[PortKey],
        options_dir: &Path,
        minimal: bool,
    ) -> Self {
        let mut session = Session {
            ports,
            states: HashMap::new(),
            aliases,
            roots: Vec::new(),
            owners: HashMap::new(),
            minimal,
        };
        session.roots = requested_roots
            .iter()
            .filter_map(|r| session.resolve(r))
            .collect();
        if session.roots.len() != requested_roots.len() {
            // Some root didn't resolve (e.g. MOVED rename): disable GC rather
            // than risk collecting live ports.
            session.roots = session.ports.keys().cloned().collect();
        }
        session.rebuild_owners();
        let keys: Vec<PortKey> = session.ports.keys().cloned().collect();
        for key in keys {
            session.ensure_state(&key, options_dir);
        }
        session
    }

    /// Recompute the owner (default flavor) of every shared options file.
    fn rebuild_owners(&mut self) {
        let mut owners: HashMap<String, PortKey> = HashMap::new();
        for (key, info) in &self.ports {
            if !info.options.has_options() {
                continue;
            }
            match owners.get(&info.options_name) {
                Some(existing) => {
                    let existing_preferred =
                        self.ports.get(existing).map(is_preferred).unwrap_or(false);
                    if !existing_preferred && is_preferred(info) {
                        owners.insert(info.options_name.clone(), key.clone());
                    }
                }
                None => {
                    owners.insert(info.options_name.clone(), key.clone());
                }
            }
        }
        self.owners = owners;
    }

    /// The port whose view owns this port's options file (itself when it is
    /// the owner or no other flavor is known).
    pub fn owner_info<'a>(&'a self, info: &'a PortInfo) -> &'a PortInfo {
        self.owners
            .get(&info.options_name)
            .and_then(|k| self.ports.get(k))
            .unwrap_or(info)
    }

    /// All closure ports that are flavors of the same origin, sorted;
    /// includes `key` itself.
    pub fn siblings(&self, key: &PortKey) -> Vec<PortKey> {
        self.ports.keys().filter(|k| k.origin == key.origin).cloned().collect()
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
            // The baseline follows the file OWNER's view (default flavor):
            // that's what apply will write.
            let owner = self.owner_info(info);
            let saved =
                SavedOptionsFile::load(&options_dir.join(&owner.options_name).join("options"));
            let baseline = close_implies(owner, apply::sync_enabled_set(owner, saved.as_ref()));
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
        }
        self.rebuild_owners();
        for key in &added_keys {
            self.ensure_state(key, options_dir);
        }
        // Existing ports may have gained state-worthy options in a refresh.
        let keys: Vec<PortKey> = self.ports.keys().cloned().collect();
        for key in keys {
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
        let mut updates: Vec<(String, Option<SavedOptionsFile>, BTreeSet<String>)> = Vec::new();
        for info in self.ports.values() {
            if !self.states.contains_key(&info.options_name)
                || updates.iter().any(|(n, _, _)| *n == info.options_name)
            {
                continue;
            }
            let owner = self.owner_info(info);
            let saved =
                SavedOptionsFile::load(&options_dir.join(&owner.options_name).join("options"));
            let baseline = close_implies(owner, apply::sync_enabled_set(owner, saved.as_ref()));
            updates.push((info.options_name.clone(), saved, baseline));
        }
        for (name, saved, baseline) in updates {
            if let Some(state) = self.states.get_mut(&name) {
                state.saved = saved;
                state.baseline = baseline;
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

    /// Set `opt` to `on` on every given port that carries it, using the same
    /// rules as toggle (group semantics, FORCED and implied locks).
    /// Returns (changed_ports, skipped) where skipped pairs a port with the
    /// refusal reason. Ports not carrying the option are ignored silently.
    pub fn bulk_set(
        &mut self,
        keys: &[PortKey],
        opt: &str,
        on: bool,
    ) -> (Vec<PortKey>, Vec<(PortKey, String)>) {
        let mut changed = Vec::new();
        let mut skipped = Vec::new();
        // States are shared per OPTIONS_NAME: decide each of them once, so the
        // other flavors of an origin never report a second change.
        let mut visited: BTreeSet<String> = BTreeSet::new();
        for key in keys {
            let Some(info) = self.ports.get(key) else { continue };
            if !info.options.complete.iter().any(|o| o == opt) {
                continue;
            }
            let options_name = info.options_name.clone();
            if !visited.insert(options_name.clone()) {
                continue;
            }
            let Some(state) = self.states.get(&options_name) else { continue };
            if state.staged.contains(opt) == on {
                continue; // already the wanted value; not an error
            }
            match self.toggle(key, opt) {
                Ok(()) => changed.push(key.clone()),
                Err(reason) => skipped.push((key.clone(), reason)),
            }
        }
        (changed, skipped)
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
                // Judge staleness against the file OWNER's option list: a
                // non-default flavor excludes options on purpose and must not
                // read the owner-written file as stale.
                let owner = self.owner_info(info);
                let cur: BTreeSet<&str> =
                    owner.options.complete.iter().map(String::as_str).collect();
                let was: BTreeSet<&str> = saved.complete.iter().map(String::as_str).collect();
                if cur == was { UiStatus::Ok } else { UiStatus::Stale }
            }
        }
    }

    /// True when the port's Stale/Unconfigured status introduces no real
    /// decision because make.conf (global OPTIONS_SET/UNSET, per-port
    /// _SET/_UNSET or *_FORCE) already dictates the relevant options:
    /// for a stale port, every option added since the file was written
    /// (removed options never need a decision); for an unconfigured port,
    /// every option it has.
    pub fn covered_by_makeconf(&self, info: &PortInfo) -> bool {
        let Some(state) = self.state(info) else { return false };
        undecided_options(info, state.saved.as_ref(), self.minimal).is_empty()
    }

    /// Does this option's staged value contradict make.conf policy?
    /// Outcome-based: make.conf must take a stance on the option (any
    /// OPTIONS_SET/UNSET or per-port _SET/_UNSET mention) and the staged
    /// value must differ from what defaults + make.conf alone would yield.
    /// FORCE-decided options are excluded: the file cannot override them.
    pub fn mc_deviates(&self, info: &PortInfo, opt: &str) -> bool {
        let Some(state) = self.state(info) else { return false };
        let o = &info.options;
        if o.is_forced(opt) {
            return false;
        }
        let mentioned = o.mc_set.contains(opt)
            || o.mc_unset.contains(opt)
            || o.port_set.contains(opt)
            || o.port_unset.contains(opt);
        if !mentioned {
            return false;
        }
        let nofile_on = nofile_effective(info).contains(opt);
        state.staged.contains(opt) != nofile_on
    }

    /// All options of the port that deviate from the global make.conf policy.
    pub fn mc_deviations(&self, info: &PortInfo) -> Vec<String> {
        info.options
            .complete
            .iter()
            .filter(|opt| self.mc_deviates(info, opt))
            .cloned()
            .collect()
    }

    /// Which enabled option (if any) implies `opt`, keeping it locked on.
    pub fn implied_by(&self, info: &PortInfo, opt: &str) -> Option<String> {
        let state = self.state(info)?;
        implier_of(info, &state.staged, opt)
    }

    /// Shortest dependency chain from one of the roots to `target`
    /// (inclusive on both ends): a single-element chain when the target is
    /// itself a root, None when it is unknown or unreachable from the roots
    /// (a port kept only by a scan fallback). Breadth-first from all roots at
    /// once, so the first chain found is a shortest one.
    pub fn why_chain(&self, target: &PortKey) -> Option<Vec<PortKey>> {
        let target = self.resolve(target)?;
        let mut pred: HashMap<PortKey, Option<PortKey>> = HashMap::new();
        let mut queue: VecDeque<PortKey> = VecDeque::new();
        for root in &self.roots {
            let Some(root) = self.resolve(root) else { continue };
            if pred.contains_key(&root) {
                continue;
            }
            if root == target {
                return Some(vec![root]);
            }
            pred.insert(root.clone(), None);
            queue.push_back(root);
        }
        while let Some(key) = queue.pop_front() {
            let Some(info) = self.ports.get(&key) else { continue };
            for dep in &info.deps {
                let Some(dep) = self.resolve(&dep.target) else { continue };
                // Never rewrite a settled predecessor: a dependency cycle
                // would otherwise make the chain walk loop forever.
                if pred.contains_key(&dep) {
                    continue;
                }
                pred.insert(dep.clone(), Some(key.clone()));
                if dep == target {
                    return Some(walk_back(&pred, dep));
                }
                queue.push_back(dep);
            }
        }
        None
    }

    /// Direct dependents of `target` within the closure: ports with a dep edge
    /// resolving to it, in key order.
    pub fn dependents(&self, target: &PortKey) -> Vec<PortKey> {
        let Some(target) = self.resolve(target) else { return Vec::new() };
        self.ports
            .iter()
            .filter(|(_, info)| {
                info.deps
                    .iter()
                    .any(|d| self.resolve(&d.target).as_ref() == Some(&target))
            })
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Apply a draft's staged sets onto matching states. Entries for unknown
    /// OPTIONS_NAMEs or with options no longer in the owner's option list are
    /// dropped (per-option filtering against the owner's complete list).
    /// Returns how many states changed.
    pub fn restore_draft(&mut self, draft: &crate::draft::Draft) -> usize {
        let mut changed = 0;
        for (options_name, enabled) in &draft.staged {
            // Judge against the file OWNER's option list, like the baseline:
            // that is the view apply writes the file from.
            let filtered = {
                let Some(owner) =
                    self.owners.get(options_name).and_then(|key| self.ports.get(key))
                else {
                    continue; // the draft names a port no longer in the closure
                };
                enabled
                    .iter()
                    .filter(|opt| owner.options.complete.contains(*opt))
                    .cloned()
                    .collect::<BTreeSet<String>>()
            };
            let Some(state) = self.states.get_mut(options_name) else { continue };
            if state.staged != filtered {
                state.staged = filtered;
                changed += 1;
            }
        }
        changed
    }

    /// Any staged edit or pending baseline write (unconfigured/stale)?
    pub fn dirty(&self) -> bool {
        self.states.values().any(|s| s.staged != s.baseline)
    }
}

/// Format the option changes between two staged snapshots as `+ADDED` /
/// `-REMOVED` tokens (additions first, each group in option order); the empty
/// string when the two sets are identical.
pub fn staged_diff(before: &BTreeSet<String>, after: &BTreeSet<String>) -> String {
    let added = after.difference(before).map(|opt| format!("+{opt}"));
    let removed = before.difference(after).map(|opt| format!("-{opt}"));
    added.chain(removed).collect::<Vec<String>>().join(" ")
}

/// Does the port refuse to build as queried (port-level BROKEN or IGNORE)?
/// The two are one condition for attribution: either one blocks the build.
pub fn is_blocked(info: &PortInfo) -> bool {
    info.broken.is_some() || info.ignore.is_some()
}

/// Options that genuinely need a human decision: not recorded in the saved
/// file (all of them, for an unconfigured port) and not dictated by any
/// make.conf layer (global OPTIONS_SET/UNSET, per-port _SET/_UNSET, *_FORCE).
///
/// Group semantics count as decisions too: when make.conf positively selects
/// a member of a SINGLE (exactly one) or RADIO (at most one) group, the
/// other members of that group are implicitly decided as off.
pub fn undecided_options(
    info: &PortInfo,
    saved: Option<&SavedOptionsFile>,
    minimal: bool,
) -> Vec<String> {
    // Minimal philosophy: the default value IS the decision. Every option has
    // one, so nothing is ever undecided — deviations and conflicts are the
    // only things left demanding attention.
    if minimal {
        return Vec::new();
    }
    let known: BTreeSet<&str> = match saved {
        Some(saved) => saved
            .complete
            .iter()
            .chain(saved.set.iter())
            .chain(saved.unset.iter())
            .map(String::as_str)
            .collect(),
        None => BTreeSet::new(),
    };
    let o = &info.options;
    let mc_selected = |opt: &str| {
        o.mc_set.contains(opt) || o.port_set.contains(opt) || o.forced_value(opt) == Some(true)
    };
    let mc_decided = |opt: &str| {
        mc_selected(opt)
            || o.mc_unset.contains(opt)
            || o.port_unset.contains(opt)
            || o.forced_value(opt) == Some(false)
    };
    let group_decided: BTreeSet<&str> = o
        .groups
        .iter()
        .filter(|g| matches!(g.kind, GroupKind::Single | GroupKind::Radio))
        .filter(|g| g.members.iter().any(|m| mc_selected(m)))
        .flat_map(|g| g.members.iter().map(String::as_str))
        .collect();
    o.complete
        .iter()
        .filter(|opt| !known.contains(opt.as_str()))
        .filter(|opt| !mc_decided(opt) && !group_decided.contains(opt.as_str()))
        .cloned()
        .collect()
}

/// Effective options as they would be WITHOUT any options file: the
/// bsd.options.mk application order (defaults, make.conf layers, FORCE,
/// IMPLIES closure) minus the options-file layer.
pub(crate) fn nofile_effective(info: &PortInfo) -> BTreeSet<String> {
    let o = &info.options;
    let complete: BTreeSet<&str> = o.complete.iter().map(String::as_str).collect();
    let mut set: BTreeSet<String> = o
        .defaults
        .iter()
        .filter(|d| complete.contains(d.as_str()))
        .cloned()
        .collect();
    // One pass per layer, in the framework's own order (bsd.options.mk:312,
    // 320, 326, 334 then 370, 378, 384, 392): each scope's UNSET runs before
    // the next scope's SET, which is what lets the per-port knob override the
    // global one. Applying all the SETs and then all the UNSETs instead would
    // let a global UNSET erase a per-port SET.
    for (adds, list) in [
        (true, &o.mc_set),
        (false, &o.mc_unset),
        (true, &o.port_set),
        (false, &o.port_unset),
        (true, &o.force_set),
        (false, &o.force_unset),
        (true, &o.port_force_set),
        (false, &o.port_force_unset),
    ] {
        for opt in list.iter() {
            if adds {
                if complete.contains(opt.as_str()) {
                    set.insert(opt.clone());
                }
            } else {
                set.remove(opt);
            }
        }
    }
    close_implies(info, set)
}

/// Transitive IMPLIES closure of an enabled set, restricted to the port's
/// known options (mirrors bsd.options.mk, which force-adds implied options).
pub(crate) fn close_implies(info: &PortInfo, mut set: BTreeSet<String>) -> BTreeSet<String> {
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

/// Rebuild a BFS path by following predecessors back to the root, then
/// reversing it so the chain reads root-first.
fn walk_back(pred: &HashMap<PortKey, Option<PortKey>>, end: PortKey) -> Vec<PortKey> {
    let mut chain = vec![end];
    while let Some(Some(prev)) = pred.get(chain.last().expect("chain is never empty")) {
        chain.push(prev.clone());
    }
    chain.reverse();
    chain
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
            pkg_help: None,
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
        Session::new(ports, HashMap::new(), &roots, tmp.path(), false)
    }

    fn set(opts: &[&str]) -> BTreeSet<String> {
        opts.iter().map(|o| o.to_string()).collect()
    }

    #[test]
    fn staged_diff_formats_changes() {
        assert_eq!(staged_diff(&set(&["A"]), &set(&["A", "B"])), "+B");
        // Additions first, each side in option order.
        assert_eq!(staged_diff(&set(&["A", "B"]), &set(&["C"])), "+C -A -B");
        assert_eq!(staged_diff(&set(&["A", "B"]), &set(&["B", "A"])), "");
        assert_eq!(staged_diff(&BTreeSet::new(), &BTreeSet::new()), "");
        assert_eq!(staged_diff(&set(&["A"]), &BTreeSet::new()), "-A");
    }

    #[test]
    fn is_blocked_covers_broken_and_ignore() {
        let (ports, key) = mk_port(&["A"], &[], vec![], vec![]);
        let mut info = ports[&key].clone();
        assert!(!is_blocked(&info));
        info.ignore = Some("needs a GSSAPI implementation".into());
        assert!(is_blocked(&info));
        info.ignore = None;
        info.broken = Some("does not build".into());
        assert!(is_blocked(&info));
        // DEPRECATED alone is not a build blocker.
        info.broken = None;
        info.deprecated = Some("use something else".into());
        assert!(!is_blocked(&info));
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

    /// bsd.options.mk applies each scope's SET and UNSET in turn (312, 320,
    /// 326, 334, then the FORCE quartet at 370-392), so the per-port knob
    /// overrides the global one. Applying every SET and only then every UNSET
    /// used to let a global UNSET erase a per-port SET: the exact shape of
    /// `OPTIONS_UNSET= DOCS` plus `<port>_SET= DOCS`, which then showed up as
    /// a spurious ≠mc deviation on a port that agreed with make.conf.
    #[test]
    fn per_port_makeconf_knob_beats_the_global_one() {
        let (mut ports, key) = mk_port(&["DOCS", "NLS", "X11"], &[], vec![], vec![]);
        {
            let o = &mut ports.get_mut(&key).unwrap().options;
            o.mc_unset.insert("DOCS".into());
            o.port_set.insert("DOCS".into());
            // The opposite order too: global on, per-port off.
            o.mc_set.insert("NLS".into());
            o.port_unset.insert("NLS".into());
            // FORCE has the same two scopes, applied after both.
            o.force_unset.insert("X11".into());
            o.port_force_set.insert("X11".into());
            // What make itself computes for those layers (PORT_OPTIONS), i.e.
            // the staged set of an unconfigured port.
            o.effective = ["DOCS", "X11"].iter().map(|s| s.to_string()).collect();
        }
        let info = &ports[&key];
        let eff = nofile_effective(info);
        assert!(eff.contains("DOCS"), "per-port _SET must survive the global UNSET");
        assert!(!eff.contains("NLS"), "per-port _UNSET must survive the global SET");
        assert!(eff.contains("X11"), "per-port _SET_FORCE must beat the global UNSET_FORCE");
        assert_eq!(info.options.forced_value("X11"), Some(true));

        // ...and the staged state agreeing with make.conf is not a deviation.
        let tmp = tempfile::tempdir().unwrap();
        let roots: Vec<PortKey> = ports.keys().cloned().collect();
        let s = Session::new(ports, HashMap::new(), &roots, tmp.path(), false);
        assert!(s.mc_deviations(&s.ports[&key]).is_empty());
    }

    #[test]
    fn minimal_treats_bare_defaults_as_decided() {
        // Options with no make.conf stance at all.
        let (ports, key) = mk_port(&["DOCS", "X11"], &["DOCS"], vec![], vec![]);
        let tmp = tempfile::tempdir().unwrap();
        let roots: Vec<PortKey> = ports.keys().cloned().collect();
        let s = Session::new(ports.clone(), HashMap::new(), &roots, tmp.path(), true);
        assert!(s.covered_by_makeconf(&s.ports[&key]), "minimal: defaults decide");
        assert!(undecided_options(&s.ports[&key], None, true).is_empty());
        // Same port without minimal: X11 and DOCS are undecided.
        let s2 = Session::new(ports, HashMap::new(), &roots, tmp.path(), false);
        assert!(!s2.covered_by_makeconf(&s2.ports[&key]));
    }

    #[test]
    fn mc_coverage_of_stale_ports() {
        // Saved file knows A; tree update added DOCS and NLS.
        let (ports, key) = mk_port(&["A", "DOCS", "NLS"], &["A"], vec![], vec![]);
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
        std::fs::write(
            tmp.path().join("cat_port/options"),
            "_OPTIONS_READ=port-0.9\n_FILE_COMPLETE_OPTIONS_LIST=A\nOPTIONS_FILE_SET+=A\n",
        )
        .unwrap();
        let roots: Vec<PortKey> = ports.keys().cloned().collect();
        let mut s = Session::new(ports, HashMap::new(), &roots, tmp.path(), false);
        let info_key = key.clone();

        // Not covered: DOCS/NLS undecided by make.conf.
        assert!(!s.covered_by_makeconf(&s.ports[&info_key]));
        assert_eq!(s.status(&s.ports[&info_key]), UiStatus::Stale);

        // Covered once make.conf unsets both globally.
        {
            let o = &mut s.ports.get_mut(&info_key).unwrap().options;
            o.mc_unset.insert("DOCS".into());
            o.mc_unset.insert("NLS".into());
        }
        assert!(s.covered_by_makeconf(&s.ports[&info_key]));

        // No stance -> no deviation, even when differing from the port default
        // (that's the plain yellow case, not ≠mc).
        assert!(!s.mc_deviates(&s.ports[&info_key], "A"));
        // Per-port knobs count as make.conf stance too.
        {
            let o = &mut s.ports.get_mut(&info_key).unwrap().options;
            o.port_unset.insert("NLS".into());
        }
        // NLS staged? baseline had no file... staged came from effective which
        // is defaults ({A}) here, so NLS is off == nofile outcome -> no deviation.
        assert!(!s.mc_deviates(&s.ports[&info_key], "NLS"));
        // Deviation: staged keeps A on although make.conf globally unsets it.
        {
            let o = &mut s.ports.get_mut(&info_key).unwrap().options;
            o.mc_unset.insert("A".into());
        }
        assert_eq!(s.mc_deviations(&s.ports[&info_key]), vec!["A".to_string()]);
        // Forced options never count as deviations.
        {
            let o = &mut s.ports.get_mut(&info_key).unwrap().options;
            o.force_set.insert("A".into());
        }
        assert!(s.mc_deviations(&s.ports[&info_key]).is_empty());

        // A removed-options-only staleness is always covered.
        let (ports2, key2) = mk_port(&["A"], &["A"], vec![], vec![]);
        let tmp2 = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp2.path().join("cat_port")).unwrap();
        std::fs::write(
            tmp2.path().join("cat_port/options"),
            "_OPTIONS_READ=port-0.9\n_FILE_COMPLETE_OPTIONS_LIST=A GONE\n\
             OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=GONE\n",
        )
        .unwrap();
        let roots2: Vec<PortKey> = ports2.keys().cloned().collect();
        let s2 = Session::new(ports2, HashMap::new(), &roots2, tmp2.path(), false);
        assert_eq!(s2.status(&s2.ports[&key2]), UiStatus::Stale);
        assert!(s2.covered_by_makeconf(&s2.ports[&key2]));

        // Unconfigured port (no saved file): covered only when make.conf
        // decides every single option.
        let (ports3, key3) = mk_port(&["DOCS", "X11"], &["DOCS"], vec![], vec![]);
        let tmp3 = tempfile::tempdir().unwrap();
        let roots3: Vec<PortKey> = ports3.keys().cloned().collect();
        let mut s3 = Session::new(ports3, HashMap::new(), &roots3, tmp3.path(), false);
        assert_eq!(s3.status(&s3.ports[&key3]), UiStatus::Unconfigured);
        assert!(!s3.covered_by_makeconf(&s3.ports[&key3]));
        {
            let o = &mut s3.ports.get_mut(&key3).unwrap().options;
            o.mc_unset.insert("DOCS".into());
            o.port_set.insert("X11".into());
        }
        assert!(s3.covered_by_makeconf(&s3.ports[&key3]));

        // A SINGLE group is decided as a whole when make.conf selects one
        // member (e.g. OPTIONS_SET += GSSAPI_NONE).
        let g = OptionGroup {
            kind: GroupKind::Single,
            name: "GSSAPI".into(),
            desc: String::new(),
            members: vec![
                "GSSAPI_BASE".into(),
                "GSSAPI_MIT".into(),
                "GSSAPI_NONE".into(),
            ],
        };
        let (ports4, key4) =
            mk_port(&["GSSAPI_BASE", "GSSAPI_MIT", "GSSAPI_NONE"], &["GSSAPI_BASE"], vec![g], vec![]);
        let tmp4 = tempfile::tempdir().unwrap();
        let roots4: Vec<PortKey> = ports4.keys().cloned().collect();
        let mut s4 = Session::new(ports4, HashMap::new(), &roots4, tmp4.path(), false);
        assert!(!s4.covered_by_makeconf(&s4.ports[&key4]));
        {
            let o = &mut s4.ports.get_mut(&key4).unwrap().options;
            o.mc_set.insert("GSSAPI_NONE".into());
        }
        assert!(s4.covered_by_makeconf(&s4.ports[&key4]));
    }

    fn linked_info(origin: &str, deps: &[&str], complete: &[&str]) -> PortInfo {
        let key = PortKey::parse(origin).unwrap();
        PortInfo {
            key: key.clone(),
            canonical: key,
            pkgname: format!("{}-1.0", origin.split('/').next_back().unwrap()),
            flavors: vec![],
            options_name: origin.replace('/', "_"),
            options: PortOptions {
                complete: complete.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            deps: deps
                .iter()
                .map(|d| crate::model::port::DepEdge {
                    target: PortKey::parse(d).unwrap(),
                    spec: format!("dep:{d}"),
                })
                .collect(),
            broken: None,
            ignore: None,
            deprecated: None,
            pkg_help: None,
            default_versions: vec![],
            warnings: vec![],
        }
    }

    #[test]
    fn shared_file_judged_against_owner_flavor() {
        // @full (default) owns editors_emacs with options A + X11;
        // @nox excludes X11. The file records the owner's view.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("editors_emacs")).unwrap();
        std::fs::write(
            tmp.path().join("editors_emacs/options"),
            "_OPTIONS_READ=emacs-30\n_FILE_COMPLETE_OPTIONS_LIST=A X11\n\
             OPTIONS_FILE_SET+=A\nOPTIONS_FILE_SET+=X11\n",
        )
        .unwrap();

        let mk = |flavor: &str, complete: &[&str]| {
            let key = PortKey::parse(&format!("editors/emacs@{flavor}")).unwrap();
            PortInfo {
                key: key.clone(),
                canonical: key,
                pkgname: format!("emacs-{flavor}-30"),
                flavors: vec!["full".into(), "nox".into()],
                options_name: "editors_emacs".into(),
                options: PortOptions {
                    complete: complete.iter().map(|s| s.to_string()).collect(),
                    effective: complete.iter().map(|s| s.to_string()).collect(),
                    ..Default::default()
                },
                deps: vec![],
                broken: None,
                ignore: None,
                deprecated: None,
                pkg_help: None,
                default_versions: vec![],
                warnings: vec![],
            }
        };
        let full = mk("full", &["A", "X11"]);
        let nox = mk("nox", &["A"]);
        let full_key = full.key.clone();
        let nox_key = nox.key.clone();
        let mut ports = BTreeMap::new();
        // Insert the non-default flavor first: owner selection must still
        // pick @full.
        ports.insert(nox_key.clone(), nox);
        ports.insert(full_key.clone(), full);
        let roots = vec![full_key.clone(), nox_key.clone()];
        let s = Session::new(ports, HashMap::new(), &roots, tmp.path(), false);

        assert_eq!(s.owners["editors_emacs"], full_key);
        assert_eq!(s.status(&s.ports[&full_key]), UiStatus::Ok);
        assert_eq!(
            s.status(&s.ports[&nox_key]),
            UiStatus::Ok,
            "nox flavor must not read the owner-written file as stale"
        );
    }

    #[test]
    fn siblings_lists_closure_flavors() {
        // devel/git declares three flavors but only two are in the closure.
        let mk = |origin: &str, flavors: &[&str]| {
            let key = PortKey::parse(origin).unwrap();
            PortInfo {
                pkgname: format!("{}-1.0", key.origin.split('/').next_back().unwrap()),
                options_name: key.origin.replace('/', "_"),
                key: key.clone(),
                canonical: key,
                flavors: flavors.iter().map(|s| s.to_string()).collect(),
                options: PortOptions {
                    complete: vec!["A".into()],
                    ..Default::default()
                },
                deps: vec![],
                broken: None,
                ignore: None,
                deprecated: None,
                pkg_help: None,
                default_versions: vec![],
                warnings: vec![],
            }
        };
        let flavors = ["default", "lite", "tiny"];
        let mut ports = BTreeMap::new();
        for info in [
            mk("devel/git@default", &flavors),
            mk("devel/git@lite", &flavors),
            mk("www/nginx", &[]),
        ] {
            ports.insert(info.canonical.clone(), info);
        }
        let s = session_for(ports);

        let default_key = PortKey::parse("devel/git@default").unwrap();
        let lite_key = PortKey::parse("devel/git@lite").unwrap();
        let nginx_key = PortKey::parse("www/nginx").unwrap();
        assert_eq!(s.siblings(&default_key), vec![default_key.clone(), lite_key.clone()]);
        assert_eq!(s.siblings(&lite_key), vec![default_key, lite_key]);
        assert_eq!(s.siblings(&nginx_key), vec![nginx_key]);
    }

    #[test]
    fn merge_gc_drops_unreachable_and_keeps_edits() {
        let tmp = tempfile::tempdir().unwrap();
        let root = linked_info("cat/root", &["cat/dep"], &["ROOTOPT"]);
        let dep = linked_info("cat/dep", &[], &["DEPOPT"]);
        let root_key = root.key.clone();
        let dep_key = dep.key.clone();
        let mut ports = BTreeMap::new();
        ports.insert(root_key.clone(), root);
        ports.insert(dep_key.clone(), dep.clone());
        let mut s = Session::new(ports, HashMap::new(), &[root_key.clone()], tmp.path(), false);

        // User edits the dependency's options.
        s.toggle(&dep_key, "DEPOPT").unwrap();
        assert_eq!(s.status(&s.ports[&dep_key]), UiStatus::Edited);

        // Refresh: the root no longer depends on cat/dep -> GC'd.
        let updated_root = linked_info("cat/root", &[], &["ROOTOPT"]);
        let mut result = crate::query::scanner::ScanResult::default();
        result.ports.insert(root_key.clone(), updated_root);
        let (added, removed) = s.merge(result, tmp.path());
        assert_eq!((added, removed), (0, 1));
        assert!(!s.ports.contains_key(&dep_key));

        // Flip back: dep reappears and the staged edit survived.
        let mut result = crate::query::scanner::ScanResult::default();
        result.ports.insert(root_key.clone(), linked_info("cat/root", &["cat/dep"], &["ROOTOPT"]));
        result.ports.insert(dep_key.clone(), dep);
        let (added, removed) = s.merge(result, tmp.path());
        assert_eq!((added, removed), (1, 0));
        let st = s.state(&s.ports[&dep_key]).unwrap();
        assert!(st.staged.contains("DEPOPT"), "edit preserved across GC round-trip");
        assert_eq!(s.status(&s.ports[&dep_key]), UiStatus::Edited);
    }

    #[test]
    fn why_chain_and_dependents() {
        let tmp = tempfile::tempdir().unwrap();
        // root -> mid -> leaf and other -> leaf; orphan hangs off nothing.
        let infos = [
            linked_info("cat/root", &["cat/mid"], &["ROOTOPT"]),
            linked_info("cat/mid", &["cat/leaf"], &["MIDOPT"]),
            linked_info("cat/other", &["cat/leaf"], &["OTHEROPT"]),
            linked_info("cat/leaf", &[], &["LEAFOPT"]),
            linked_info("cat/orphan", &[], &["ORPHANOPT"]),
        ];
        let mut ports = BTreeMap::new();
        for info in infos {
            ports.insert(info.key.clone(), info);
        }
        let root = PortKey::parse("cat/root").unwrap();
        let other = PortKey::parse("cat/other").unwrap();
        let mid = PortKey::parse("cat/mid").unwrap();
        let leaf = PortKey::parse("cat/leaf").unwrap();
        let orphan = PortKey::parse("cat/orphan").unwrap();
        let s = Session::new(ports, HashMap::new(), &[root.clone(), other.clone()], tmp.path(), false);

        let chain = s.why_chain(&leaf).expect("leaf is reachable");
        assert_eq!(chain.len(), 2, "shortest chain goes through cat/other");
        assert!(s.roots.contains(&chain[0]), "chain starts at a root");
        assert_eq!(chain.last(), Some(&leaf), "chain ends at the target");
        assert_eq!(chain, vec![other.clone(), leaf.clone()]);

        assert_eq!(s.why_chain(&root), Some(vec![root]));
        assert_eq!(s.why_chain(&orphan), None, "not reachable from the roots");
        assert_eq!(s.dependents(&leaf), vec![mid, other]);
        assert!(s.dependents(&orphan).is_empty());
    }

    /// A cycle in the dep graph must neither hang the chain walk nor lengthen
    /// the chain: the predecessor of an already-visited port stays put.
    #[test]
    fn why_chain_survives_dependency_cycle() {
        let tmp = tempfile::tempdir().unwrap();
        let infos = [
            linked_info("cat/root", &["cat/mid"], &["ROOTOPT"]),
            linked_info("cat/mid", &["cat/leaf"], &["MIDOPT"]),
            linked_info("cat/leaf", &["cat/mid", "cat/deep"], &["LEAFOPT"]),
            linked_info("cat/other", &["cat/leaf"], &["OTHEROPT"]),
            linked_info("cat/deep", &[], &["DEEPOPT"]),
        ];
        let mut ports = BTreeMap::new();
        for info in infos {
            ports.insert(info.key.clone(), info);
        }
        let roots = [
            PortKey::parse("cat/root").unwrap(),
            PortKey::parse("cat/other").unwrap(),
        ];
        let s = Session::new(ports, HashMap::new(), &roots, tmp.path(), false);
        let deep = PortKey::parse("cat/deep").unwrap();
        assert_eq!(
            s.why_chain(&deep),
            Some(vec![
                PortKey::parse("cat/other").unwrap(),
                PortKey::parse("cat/leaf").unwrap(),
                deep,
            ])
        );
    }

    #[test]
    fn status_precedence_conflict_beats_edited() {
        let defs = vec![(
            "X",
            OptionDef { prevents: vec!["Y".into()], ..Default::default() },
        )];
        let (ports, key) = mk_port(&["X", "Y"], &["Y"], vec![], defs);
        let mut s = session_for(ports);
        s.toggle(&key, "X").unwrap(); // staged != baseline AND a conflict
        let info = s.ports.get(&key).unwrap();
        assert_eq!(s.status(info), UiStatus::Conflict, "conflict outranks edited");
    }

    #[test]
    fn revert_and_reset_lifecycle() {
        let (ports, key) = mk_port(&["A", "B"], &["A"], vec![], vec![]);
        let mut s = session_for(ports);
        assert!(!s.dirty());
        s.toggle(&key, "B").unwrap();
        assert!(s.dirty());
        s.revert(&key);
        assert!(!s.dirty(), "revert restores baseline");
        s.toggle(&key, "A").unwrap(); // A off (deviates from defaults)
        s.reset_to_defaults(&key);
        let st = s.state(&s.ports[&key]).unwrap();
        assert!(st.staged.contains("A") && !st.staged.contains("B"));
    }

    #[test]
    fn reload_saved_refreshes_baseline() {
        let (ports, key) = mk_port(&["A"], &[], vec![], vec![]);
        let tmp = tempfile::tempdir().unwrap();
        let roots: Vec<PortKey> = ports.keys().cloned().collect();
        let mut s = Session::new(ports, HashMap::new(), &roots, tmp.path(), false);
        assert_eq!(s.status(&s.ports[&key]), UiStatus::Unconfigured);

        // Simulate an apply: a file appears on disk recording A=on.
        std::fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
        std::fs::write(
            tmp.path().join("cat_port/options"),
            "_OPTIONS_READ=port-1.0\n_FILE_COMPLETE_OPTIONS_LIST=A\nOPTIONS_FILE_SET+=A\n",
        )
        .unwrap();
        s.reload_saved(tmp.path());
        // Baseline now follows the file; staged still empty -> Edited.
        let st = s.state(&s.ports[&key]).unwrap();
        assert!(st.baseline.contains("A"));
        assert_eq!(s.status(&s.ports[&key]), UiStatus::Edited);
    }

    #[test]
    fn bulk_set_applies_rules_and_dedups_shared_state() {
        let mk = |origin: &str, options_name: &str, complete: &[&str]| {
            let key = PortKey::parse(origin).unwrap();
            PortInfo {
                key: key.clone(),
                canonical: key,
                pkgname: format!("{}-1.0", origin.replace('/', "-")),
                flavors: vec!["f1".into(), "f2".into()],
                options_name: options_name.into(),
                options: PortOptions {
                    complete: complete.iter().map(|s| s.to_string()).collect(),
                    ..Default::default()
                },
                deps: vec![],
                broken: None,
                ignore: None,
                deprecated: None,
                pkg_help: None,
                default_versions: vec![],
                warnings: vec![],
            }
        };
        let mut ports = BTreeMap::new();
        for info in [
            mk("cat/a", "cat_a", &["DOCS", "X11"]),
            mk("cat/b", "cat_b", &["DOCS"]),
            mk("cat/c", "cat_c", &["ZZZ"]),
            // Two flavors of one origin share cat_d's options file/state.
            mk("cat/d@f1", "cat_d", &["DOCS"]),
            mk("cat/d@f2", "cat_d", &["DOCS"]),
        ] {
            ports.insert(info.key.clone(), info);
        }
        let a = PortKey::parse("cat/a").unwrap();
        let b = PortKey::parse("cat/b").unwrap();
        let c = PortKey::parse("cat/c").unwrap();
        let d1 = PortKey::parse("cat/d@f1").unwrap();
        let d2 = PortKey::parse("cat/d@f2").unwrap();
        // cat/b's DOCS is nailed down by a make.conf *_FORCE knob.
        ports.get_mut(&b).unwrap().options.force_set.insert("DOCS".into());
        let keys: Vec<PortKey> = ports.keys().cloned().collect();
        let mut s = session_for(ports);

        let (changed, skipped) = s.bulk_set(&keys, "DOCS", true);
        assert!(changed.contains(&a), "cat/a takes the decision");
        assert_eq!(
            changed.iter().filter(|k| **k == d1 || **k == d2).count(),
            1,
            "one shared state change counts as one changed port"
        );
        assert_eq!(changed.len(), 2, "only cat/a and one cat/d flavor changed");
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].0, b);
        assert!(skipped[0].1.contains("forced"), "reason: {}", skipped[0].1);
        assert!(!changed.contains(&c) && !skipped.iter().any(|(k, _)| *k == c));

        for key in [&a, &d1, &d2] {
            let st = s.state(&s.ports[key]).unwrap();
            assert!(st.staged.contains("DOCS"), "{key} staged DOCS");
        }
        assert!(!s.state(&s.ports[&c]).unwrap().staged.contains("DOCS"));

        // Idempotent: already-equal ports are skipped silently.
        let (changed, skipped) = s.bulk_set(&keys, "DOCS", true);
        assert!(changed.is_empty(), "nothing left to change");
        assert!(!skipped.iter().any(|(k, _)| *k == a), "cat/a not an error");
    }

    #[test]
    fn bulk_set_respects_groups() {
        let g = OptionGroup {
            kind: GroupKind::Single,
            name: "IMPL".into(),
            desc: String::new(),
            members: vec!["A".into(), "B".into()],
        };
        let (ports, key) = mk_port(&["A", "B"], &["A"], vec![g], vec![]);
        let mut s = session_for(ports);
        let (changed, skipped) = s.bulk_set(&[key.clone()], "A", false);
        assert!(changed.is_empty());
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].0, key);
        assert!(skipped[0].1.contains("single-choice group"), "reason: {}", skipped[0].1);
        assert!(s.state(&s.ports[&key]).unwrap().staged.contains("A"), "A stays on");
    }

    #[test]
    fn forced_options_refuse_toggle() {
        let (ports, key) = mk_port(&["A"], &[], vec![], vec![]);
        let mut s = session_for(ports);
        s.ports.get_mut(&key).unwrap().options.force_set.insert("A".into());
        let err = s.toggle(&key, "A").unwrap_err();
        assert!(err.contains("forced"));
    }

    fn draft_with(entries: &[(&str, &[&str])]) -> crate::draft::Draft {
        crate::draft::Draft {
            options_dir: std::path::PathBuf::from("/nonexistent"),
            saved_at_secs: 0,
            staged: entries
                .iter()
                .map(|(name, opts)| {
                    (name.to_string(), opts.iter().map(|o| o.to_string()).collect())
                })
                .collect(),
        }
    }

    #[test]
    fn restore_draft_filters_options_gone_from_the_port() {
        let (ports, key) = mk_port(&["A", "B"], &["B"], vec![], vec![]);
        let mut s = session_for(ports);
        let draft = draft_with(&[("cat_port", &["A", "GONE"])]);
        assert_eq!(s.restore_draft(&draft), 1);
        let st = s.state(&s.ports[&key]).unwrap();
        assert_eq!(st.staged, ["A".to_string()].into_iter().collect::<BTreeSet<_>>());
        // Baseline is untouched, so the restored state still reads as edited.
        assert_eq!(s.status(&s.ports[&key]), UiStatus::Edited);
        // Re-applying the same draft changes nothing.
        assert_eq!(s.restore_draft(&draft), 0);
    }

    #[test]
    fn restore_draft_drops_unknown_options_names() {
        let (ports, key) = mk_port(&["A"], &["A"], vec![], vec![]);
        let mut s = session_for(ports);
        let before = s.state(&s.ports[&key]).unwrap().staged.clone();
        assert_eq!(s.restore_draft(&draft_with(&[("other_port", &["A"])])), 0);
        assert_eq!(s.state(&s.ports[&key]).unwrap().staged, before);
    }
}
