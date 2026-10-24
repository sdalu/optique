//! The editing session: the scanned ports and the option state staged for
//! each options file, the rules an edit must keep (group kinds, IMPLIES,
//! PREVENTS, the locks), which flavor owns a shared file, and which files
//! optique leaves alone.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use crate::apply;
use crate::model::options::{GroupKind, McLayer, PortOptions, str_match};
use crate::model::origin::PortKey;
use crate::model::port::PortInfo;
use crate::optfiles::OptionsFiles;
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

/// UI-level status of a port, most severe first. Staged edits are not a
/// status: `Session::edited` says whether a port has any, whatever its state.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum UiStatus {
    Conflict,
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
    /// Canonical key -> the state of a flavor make.conf gives an
    /// OPTIONS_FILE of its own (`moved`): it reads none of the shared
    /// file, so it keeps out of that file's state, and its own is make's
    /// PORT_OPTIONS, which no edit changes.
    pub moved_states: HashMap<PortKey, OptState>,
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
    /// The build's blacklist: the reachability GC does not walk through a
    /// port it matches, as the scan does not (scanner::register).
    pub blacklist: crate::config::Blacklist,
    /// OPTIONS_NAME -> the options file (empty when absent) with its
    /// options.local read after it (`OptionsFiles::combined`): what
    /// options.local decides, which an edit to the file cannot change.
    pub locals: HashMap<String, SavedOptionsFile>,
    /// The listed origins MOVED moves, followed or flagged
    /// (`roots::apply_moved`): the TUI names them on their targets' panes
    /// and in its first-run message.
    pub listed_moves: Vec<crate::moved::ListedMove>,
}

/// Is this port the default flavor of its origin (or unflavored)?
pub(crate) fn is_preferred(info: &PortInfo) -> bool {
    match &info.canonical.flavor {
        None => true,
        Some(f) => info.flavors.first() == Some(f),
    }
}

/// Does `candidate` take the shared options file from `owner`, the flavor
/// holding it so far? The one rule for who owns a shared file, used by the
/// session and by `apply::plan_writes` alike: the default flavor; without
/// it, the widest view (most options), the first met on ties. Flavors
/// exclude options, so the widest view covers the others' (editors/emacs@pgtk's
/// holds @nox's).
pub(crate) fn outranks(candidate: &PortInfo, owner: &PortInfo) -> bool {
    !is_preferred(owner)
        && (is_preferred(candidate)
            || candidate.options.complete.len() > owner.options.complete.len())
}

/// Why optique leaves an options file alone: it neither writes nor removes
/// it, takes no edit, and shows make's own outcome for its port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unmanaged {
    /// What makes it so, as `locked_by` names it: `options file`,
    /// `options.local` or `OPTIONS_FILE`.
    pub by: &'static str,
    /// The reason, in full.
    pub why: String,
}

/// Why the port reads none of the options file optique manages: its
/// OPTIONS_FILE, set by make.conf or its own Makefile, lies outside the
/// options dir (`PortOptions::moved_file`), so make reads another file, and
/// `${OPTIONS_FILE}.local` with it. Such a flavor is no
/// reader of the shared file: its own edits are refused and its state is
/// make's own, while the flavors still reading the file keep it managed.
pub(crate) fn moved(info: &PortInfo) -> Option<Unmanaged> {
    info.options.moved_file.as_ref().map(|path| Unmanaged {
        by: "OPTIONS_FILE",
        why: format!(
            "OPTIONS_FILE is {path}, outside the options dir optique manages; edit it there"
        ),
    })
}

/// Whether optique can manage an options file, `combined` being the file
/// with its options.local read after it (`OptionsFiles::combined`). It
/// cannot when a line of either file is one optique cannot model, or
/// options.local assigns a list afresh or with `?=`
/// (`SavedOptionsFile::unmanaged`). Who reads the file is the caller's to
/// say: a flavor make.conf moves OPTIONS_FILE for reads none of it
/// (`moved`).
pub(crate) fn unmanaged_file(combined: &SavedOptionsFile) -> Option<Unmanaged> {
    combined.unmanaged().map(|(file, why)| Unmanaged {
        by: if file == "options.local" {
            "options.local"
        } else {
            "options file"
        },
        why,
    })
}

impl Session {
    pub fn new(
        ports: BTreeMap<PortKey, PortInfo>,
        aliases: HashMap<PortKey, PortKey>,
        requested_roots: &[PortKey],
        files: &OptionsFiles,
        minimal: bool,
    ) -> Self {
        let mut session = Session {
            ports,
            states: HashMap::new(),
            moved_states: HashMap::new(),
            aliases,
            roots: Vec::new(),
            owners: HashMap::new(),
            minimal,
            blacklist: crate::config::Blacklist::default(),
            locals: HashMap::new(),
            listed_moves: Vec::new(),
        };
        session.roots = requested_roots
            .iter()
            .filter_map(|r| session.resolve(r))
            .collect();
        if session.roots.len() != requested_roots.len() {
            // Some root didn't resolve (its query failed): disable GC rather
            // than risk collecting live ports.
            session.roots = session.ports.keys().cloned().collect();
        }
        session.rebuild_owners();
        let keys: Vec<PortKey> = session.ports.keys().cloned().collect();
        for key in keys {
            session.ensure_state(&key, files);
        }
        session
    }

    /// Recompute the owner of every shared options file (`outranks`: the
    /// default flavor, else the widest view). Without the default flavor in
    /// the closure the widest view stands for it, so an edit to an option
    /// only it has reaches the shared staged set. A flavor make.conf moves
    /// OPTIONS_FILE for reads none of the shared file, and owns none of it.
    fn rebuild_owners(&mut self) {
        let mut owners: HashMap<String, PortKey> = HashMap::new();
        for (key, info) in &self.ports {
            if !info.options.has_options() || moved(info).is_some() {
                continue;
            }
            match owners
                .get(&info.options_name)
                .and_then(|k| self.ports.get(k))
            {
                Some(existing) if !outranks(info, existing) => {}
                _ => {
                    owners.insert(info.options_name.clone(), key.clone());
                }
            }
        }
        self.owners = owners;
    }

    /// The port whose view owns this port's options file (itself when it is
    /// the owner or no other flavor is known, and when make.conf moved its
    /// OPTIONS_FILE: the file it reads is its own).
    pub fn owner_info<'a>(&'a self, info: &'a PortInfo) -> &'a PortInfo {
        if moved(info).is_some() {
            return info;
        }
        self.owners
            .get(&info.options_name)
            .and_then(|k| self.ports.get(k))
            .unwrap_or(info)
    }

    /// All closure ports that are flavors of the same origin, sorted;
    /// includes `key` itself.
    pub fn siblings(&self, key: &PortKey) -> Vec<PortKey> {
        self.ports
            .keys()
            .filter(|k| k.origin == key.origin)
            .cloned()
            .collect()
    }

    /// Resolve a (possibly non-canonical) key to the canonical ports-map key.
    pub fn resolve(&self, key: &PortKey) -> Option<PortKey> {
        crate::deps::resolve(&self.ports, &self.aliases, key)
    }

    /// Every dependency loop in the closure (see `deps::detect`). Cheap
    /// enough to recompute whenever the closure changes: a toggle can create
    /// or break a loop by changing which dependencies a port pulls in.
    pub fn dep_loops(&self) -> Vec<crate::deps::DepLoop> {
        crate::deps::detect(&self.ports, &self.aliases)
    }

    fn ensure_state(&mut self, key: &PortKey, files: &OptionsFiles) {
        let Some(info) = self.ports.get(key) else {
            return;
        };
        if !info.options.has_options() {
            return;
        }
        // A flavor reading a file of its own: make's PORT_OPTIONS, fresh
        // from each query, as no edit changes it (`edit_refusal`).
        if moved(info).is_some() {
            let effective = info.options.effective.clone();
            self.moved_states.insert(
                info.canonical.clone(),
                OptState {
                    saved: None,
                    staged: effective.clone(),
                    baseline: effective,
                },
            );
            return;
        }
        if !self.states.contains_key(&info.options_name) {
            // The baseline follows the file OWNER's view (default flavor):
            // that's what apply will write.
            let owner = self.owner_info(info);
            let combined = files.combined(&info.options_name);
            // A file optique cannot model claims nothing: no saved choices,
            // and make's own PORT_OPTIONS as the state, which no edit
            // changes (`edit_refusal`).
            let others = sharing(&self.ports, &info.options_name, &owner.key);
            let (saved, baseline) = match unmanaged_file(&combined) {
                Some(_) => (None, owner.options.effective.clone()),
                None => saved_baseline(owner, &others, files),
            };
            self.locals.insert(info.options_name.clone(), combined);
            self.states.insert(
                info.options_name.clone(),
                OptState {
                    saved,
                    staged: baseline.clone(),
                    baseline,
                },
            );
        }
    }

    /// Merge a background re-scan into the closure: replace/add ports, create
    /// state for newcomers, then drop ports no longer reachable from the
    /// roots. Returns (added, removed) counts. Option states are never
    /// discarded, so a port that flip-flops out and back keeps its edits.
    /// Merge a refresh's ports into the closure, then drop what the roots no
    /// longer reach. Returns the ports that came in and those dropped.
    pub fn merge(
        &mut self,
        result: crate::query::scanner::ScanResult,
        files: &OptionsFiles,
    ) -> (Vec<PortKey>, Vec<PortKey>) {
        self.aliases.extend(result.aliases);
        let owners_before = self.readers();
        let mut added_keys = Vec::new();
        for (key, info) in result.ports {
            if self.ports.insert(key.clone(), info).is_none() {
                added_keys.push(key.clone());
            }
        }
        self.rebuild_owners();
        // Newcomers, and existing ports that gained options in a refresh.
        let keys: Vec<PortKey> = self.ports.keys().cloned().collect();
        for key in keys {
            self.ensure_state(&key, files);
        }
        // Reachability GC from the roots.
        let mut reachable: std::collections::HashSet<PortKey> = Default::default();
        let mut stack: Vec<PortKey> = self.roots.iter().filter_map(|r| self.resolve(r)).collect();
        while let Some(key) = stack.pop() {
            if !reachable.insert(key.clone()) {
                continue;
            }
            if let Some(info) = self.ports.get(&key)
                && !self
                    .blacklist
                    .matches(&info.canonical, info.is_default_flavor())
            {
                stack.extend(self.resolved_deps(info));
            }
        }
        let removed: Vec<PortKey> = self
            .ports
            .keys()
            .filter(|k| !reachable.contains(*k))
            .cloned()
            .collect();
        self.ports.retain(|k, _| reachable.contains(k));
        self.moved_states.retain(|k, _| reachable.contains(k));
        // The GC may have dropped an owner whose sibling flavor stays.
        self.rebuild_owners();
        self.rebase_new_owners(&owners_before, files);
        self.refresh_unmanaged();
        added_keys.retain(|k| self.ports.contains_key(k));
        (added_keys, removed)
    }

    /// Give every file optique cannot manage its owner's fresh PORT_OPTIONS
    /// as the state. A newcomer is first queried against the staging
    /// options dir, which does not hold its file yet; the state it got then
    /// is make's outcome without the file, and the seeded re-query brings
    /// the real one. Such a state takes no edit, so nothing staged is lost.
    fn refresh_unmanaged(&mut self) {
        for (name, state) in self.states.iter_mut() {
            if !self
                .locals
                .get(name)
                .is_some_and(|l| unmanaged_file(l).is_some())
            {
                continue;
            }
            let Some(owner) = self.owners.get(name).and_then(|k| self.ports.get(k)) else {
                continue;
            };
            state.baseline = owner.options.effective.clone();
            state.staged = state.baseline.clone();
        }
    }

    /// The state the port shows: its options file's, shared by the flavors
    /// reading it, or, for a flavor make.conf moves OPTIONS_FILE for, its
    /// own (`moved_states`).
    pub fn state(&self, info: &PortInfo) -> Option<&OptState> {
        if moved(info).is_some() {
            return self.moved_states.get(&info.canonical);
        }
        self.states.get(&info.options_name)
    }

    /// Is the option staged on (false for a port without state)?
    pub fn is_staged(&self, info: &PortInfo, opt: &str) -> bool {
        if self.unstored(info, opt).is_some() {
            return info.options.effective.contains(opt);
        }
        self.state(info).is_some_and(|s| s.staged.contains(opt))
    }

    /// The flavor owning the options file `info` shares, when `opt` is an
    /// option of `info` that owner has not: the file is written from the
    /// owner's view, so it never records `opt`, which `info` builds at its
    /// own outcome (multimedia/audacious-plugins@gtk3's AOSD, which @qt6
    /// lacks). None for an option the file can hold.
    pub fn unstored<'a>(&'a self, info: &'a PortInfo, opt: &str) -> Option<&'a PortInfo> {
        let owner = self.owner_info(info);
        let theirs = |i: &PortInfo| i.options.complete.iter().any(|o| o == opt);
        // Only the default flavor writes its own view alone; another owner
        // writes every flavor's options (`apply::planned_file`).
        (owner.key != info.key && is_preferred(owner) && theirs(info) && !theirs(owner))
            .then_some(owner)
    }

    /// The port's dependencies, resolved to closure keys; unknown ones are
    /// skipped.
    fn resolved_deps<'a>(&'a self, info: &'a PortInfo) -> impl Iterator<Item = PortKey> + 'a {
        info.deps.iter().filter_map(|d| self.resolve(&d.target))
    }

    /// Reload the files an apply wrote or removed (`touched`, by
    /// OPTIONS_NAME) so statuses reflect reality. Only those change: every
    /// other state keeps the baseline the scan gave it, since recomputing it
    /// would read the port's `effective`, which a TUI refresh may have
    /// queried with an edit staged since undone.
    pub fn reload_saved<'a>(
        &mut self,
        files: &OptionsFiles,
        touched: impl IntoIterator<Item = &'a str>,
    ) {
        for name in touched {
            let owner = self.owners.get(name).and_then(|k| self.ports.get(k));
            let (Some(state), Some(owner)) = (self.states.get_mut(name), owner) else {
                continue;
            };
            let saved = files.saved(name);
            let combined = files.combined(name);
            if unmanaged_file(&combined).is_some() {
                // Never written or removed by optique; kept as `ensure_state` has it.
                state.saved = None;
                state.baseline = owner.options.effective.clone();
                self.locals.insert(name.to_string(), combined);
                continue;
            }
            state.baseline = if files.own(name).is_some() {
                // A file apply wrote has a line for every option.
                let others = sharing(&self.ports, name, &owner.key);
                shared_enabled(owner, &others, saved.as_ref())
            } else {
                // Removed under -m: what make builds with no file but its
                // options.local, which is what the removal was judged
                // against, a conditional group's members included, built as
                // every other baseline is: each flavor's own options (no
                // OPTIONS_SLAVE option or unlisted implied one, which the
                // staged set never has), every flavor's under an owner that
                // is no default flavor.
                let others = sharing(&self.ports, name, &owner.key);
                shared_enabled_by(
                    apply::file_enabled_set,
                    owner,
                    &others,
                    Some(&files.combined(name)),
                )
            };
            state.saved = saved;
            // A `?=` in options.local takes effect or not by the file.
            self.locals.insert(name.to_string(), combined);
        }
    }

    /// Each shared file's owner and the flavors reading it.
    fn readers(&self) -> HashMap<String, (PortKey, BTreeSet<PortKey>)> {
        self.owners
            .iter()
            .map(|(name, owner)| {
                let keys = sharing(&self.ports, name, owner)
                    .into_iter()
                    .map(|i| i.key.clone())
                    .collect();
                (name.clone(), (owner.clone(), keys))
            })
            .collect()
    }

    /// A file whose owner or readers changed in a merge (the default flavor
    /// came in or went, a flavor with options of its own joined an owner
    /// that writes them all) had its state built from another view: its
    /// baseline is built again, and the edits staged on the old one carried
    /// over as edits, each option turned on or off since the old baseline
    /// turned so on the new.
    fn rebase_new_owners(
        &mut self,
        before: &HashMap<String, (PortKey, BTreeSet<PortKey>)>,
        files: &OptionsFiles,
    ) {
        let now = self.readers();
        let changed: Vec<(String, PortKey)> = now
            .into_iter()
            .filter(|(name, readers)| before.get(name).is_some_and(|old| old != readers))
            .map(|(name, (owner, _))| (name, owner))
            .collect();
        for (name, key) in changed {
            let Some(owner) = self.ports.get(&key) else {
                continue;
            };
            if unmanaged_file(&files.combined(&name)).is_some() {
                continue;
            }
            let others = sharing(&self.ports, &name, &key);
            // Never from the ports' queried PORT_OPTIONS: in the TUI the
            // refresh that brought them queried the staged edits, which
            // would read as the baseline.
            let saved = files.saved(&name);
            let baseline = if owner.options.r#override.is_some() {
                nofile_effective(owner)
            } else {
                shared_enabled_by(apply::file_enabled_set, owner, &others, saved.as_ref())
            };
            let holds = self.file_options(&name);
            let Some(state) = self.states.get_mut(&name) else {
                continue;
            };
            // The edits carried over, within what the file now holds.
            let on: Vec<String> = state
                .staged
                .difference(&state.baseline)
                .filter(|o| holds.contains(*o))
                .cloned()
                .collect();
            let off: Vec<String> = state.baseline.difference(&state.staged).cloned().collect();
            let mut staged = baseline.clone();
            staged.extend(on);
            for o in &off {
                staged.remove(o);
            }
            state.baseline = baseline;
            state.staged = close_implies(owner, staged);
        }
    }

    /// Whether the port's options.local has an UNSET entry matching `opt`
    /// (`SavedOptionsFile::local_decides`): every SET entry goes in before
    /// any UNSET one, so an edit to the file cannot turn it back on.
    pub fn local_decides(&self, info: &PortInfo, opt: &str) -> bool {
        self.locals
            .get(&info.options_name)
            .is_some_and(|l| l.local_decides(opt))
    }

    /// Why the port's options file is one optique cannot manage: make.conf
    /// moved OPTIONS_FILE for this port (`moved`), or the shared file holds
    /// what optique cannot model (`unmanaged_file`). Nothing is written or
    /// removed for it, no edit is taken, and its state is make's own
    /// PORT_OPTIONS. A flavor moved elsewhere leaves the shared file to the
    /// flavors still reading it.
    pub fn unmanaged(&self, info: &PortInfo) -> Option<Unmanaged> {
        moved(info).or_else(|| self.locals.get(&info.options_name).and_then(unmanaged_file))
    }

    /// The listed origins MOVED moves to this port: its key, or, for one
    /// listed `@all`, any flavor of its origin, and for one listed `@-`,
    /// its default flavor.
    pub fn moved_to(&self, info: &PortInfo) -> Vec<&crate::moved::ListedMove> {
        use crate::roots::{FLAVOR_ALL, FLAVOR_DEFAULT};
        self.listed_moves
            .iter()
            .filter(|m| match m.to.flavor.as_deref() {
                Some(FLAVOR_ALL) => m.to.origin == info.canonical.origin,
                Some(FLAVOR_DEFAULT) => {
                    self.resolve(&PortKey::new(m.to.origin.clone(), None))
                        .as_ref()
                        == Some(&info.canonical)
                }
                _ => self.resolve(&m.to).as_ref() == Some(&info.canonical),
            })
            .collect()
    }

    /// The options the port still owes a human answer for
    /// (`undecided_options`); none for a file optique cannot manage, which
    /// it cannot read.
    pub fn undecided(&self, info: &PortInfo) -> Vec<String> {
        if self.unmanaged(info).is_some() {
            return Vec::new();
        }
        undecided_options(
            info,
            self.state(info).and_then(|s| s.saved.as_ref()),
            self.minimal,
        )
    }

    /// Why no edit of the port's options file is taken, whatever the
    /// option: an OPTIONS_OVERRIDE, or a file optique cannot manage.
    pub fn edit_refusal(&self, key: &PortKey) -> Option<String> {
        let info = self.ports.get(key)?;
        // bsd.options.mk:300-304: OPTIONS_OVERRIDE decides the port by
        // itself; the options file is never read, so editing it is pointless.
        if info.options.r#override.is_some() {
            return Some(
                "OPTIONS_OVERRIDE in make.conf decides this port; its options file is not read"
                    .to_string(),
            );
        }
        // A file optique cannot model is not its to edit.
        self.unmanaged(info).map(|u| u.why)
    }

    /// Toggle an option; returns a user-facing error when refused.
    pub fn toggle(&mut self, key: &PortKey, opt: &str) -> Result<(), String> {
        if let Some(why) = self.edit_refusal(key) {
            return Err(why);
        }
        let info = self
            .ports
            .get(key)
            .ok_or_else(|| "unknown port".to_string())?;
        if let Some(owner) = self.unstored(info, opt) {
            return Err(format!(
                "{opt} is {}'s alone: the options file it shares is written from {}'s view, which has no {opt}",
                info.key, owner.key
            ));
        }
        // Checked before `states` is borrowed mutably.
        let by_local = self.local_decides(info, opt);
        let state = self
            .states
            .get_mut(&info.options_name)
            .ok_or_else(|| "port has no options".to_string())?;
        let opts = &info.options;

        if opts.is_forced(opt) {
            return Err(format!(
                "{opt} is forced by make.conf (*_FORCE or WITH/WITHOUT); the options file cannot override it"
            ));
        }

        // bsd.options.mk:352-363: options.local's UNSET entries go in after
        // every SET entry, the file's included; an edit to the file would
        // be written and change nothing make sees.
        if by_local {
            return Err(format!("options.local decides {opt}; edit it there"));
        }

        if state.staged.contains(opt) {
            // Turning off.
            if let Some(by) = implier_of(info, &state.staged, opt) {
                return Err(format!("{opt} is implied by {by}; disable {by} first"));
            }
            // Judged on what make ends up with once it is off, FORCE and
            // OPTIONS_SLAVE included: a forced-on member keeps the group
            // satisfied, and a conditional group whose option is off is not
            // checked.
            let mut after = state.staged.clone();
            after.remove(opt);
            let outcome = staged_effective(info, &after, self.locals.get(&info.options_name));
            let (unchecked, _) = unchecked_groups(opts, outcome.clone());
            if let Some(g) = opts
                .group_of(opt)
                .filter(|g| !unchecked.contains(g.name.as_str()))
            {
                let on = g.count_on(&outcome);
                match g.kind {
                    GroupKind::Single if on == 0 => {
                        return Err(format!(
                            "{}: single-choice group needs exactly one option; select another member instead",
                            g.name
                        ));
                    }
                    GroupKind::Multi if on == 0 => {
                        return Err(format!(
                            "{}: multi group needs at least one option enabled",
                            g.name
                        ));
                    }
                    _ => {}
                }
            }
            state.staged.remove(opt);
        } else {
            // Turning on: single/radio groups deselect siblings.
            if let Some(g) = opts.exclusive_group_of(opt) {
                // A forced-on sibling stays on whatever the file says.
                if let Some(m) = g
                    .members
                    .iter()
                    .find(|m| *m != opt && opts.forced_value(m) == Some(true))
                {
                    return Err(format!(
                        "{}: {m} is forced on by make.conf (*_FORCE) and the group allows one option; fix it there",
                        g.name
                    ));
                }
                for m in &g.members {
                    if m != opt {
                        state.staged.remove(m);
                    }
                }
            }
            state.staged.insert(opt.to_string());
            state.staged = close_implies(info, std::mem::take(&mut state.staged));
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
            let Some(info) = self.ports.get(key) else {
                continue;
            };
            if !info.options.complete.iter().any(|o| o == opt) {
                continue;
            }
            // A flavor reading a file of its own shares no state: it is
            // refused by itself, and leaves the shared file to the others.
            if moved(info).is_none() && !visited.insert(info.options_name.clone()) {
                continue;
            }
            let Some(state) = self.state(info) else {
                continue;
            };
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

    /// Stage the port's defaults; nothing for a port whose file takes no
    /// edit (`edit_refusal`, which the TUI shows instead).
    pub fn reset_to_defaults(&mut self, key: &PortKey) {
        if self.edit_refusal(key).is_some() {
            return;
        }
        let Some(info) = self.ports.get(key) else {
            return;
        };
        // The shared state follows the file OWNER's view, like the
        // baseline: a flavor lacking some of its options must not drop them.
        // (owner_info would borrow all of self, states included.)
        let owner = self
            .owners
            .get(&info.options_name)
            .and_then(|k| self.ports.get(k))
            .unwrap_or(info);
        // What the options file cannot decide keeps the value it has: an
        // option options.local's UNSET entries or a make.conf knob decide,
        // which `Space` refuses too (`toggle`).
        let o = &owner.options;
        let locked: Vec<String> = o
            .complete
            .iter()
            .filter(|opt| o.is_forced(opt) || self.local_decides(owner, opt))
            .cloned()
            .collect();
        let mut defaults = port_defaults(o);
        // An owner that is no default flavor writes every flavor's options:
        // each other one's own options take that flavor's defaults.
        if !is_preferred(owner) {
            for other in sharing(&self.ports, &owner.options_name, &owner.key) {
                defaults.extend(
                    port_defaults(&other.options)
                        .into_iter()
                        .filter(|d| !o.complete.contains(d)),
                );
            }
        }
        if let Some(state) = self.states.get_mut(&owner.options_name) {
            let mut staged = defaults;
            for opt in &locked {
                if state.staged.contains(opt) {
                    staged.insert(opt.clone());
                } else {
                    staged.remove(opt);
                }
            }
            state.staged = close_implies(owner, staged);
        }
    }

    pub fn revert(&mut self, key: &PortKey) {
        // A flavor reading a file of its own has nothing to revert, and the
        // shared state is not its own.
        if let Some(info) = self.ports.get(key).filter(|i| moved(i).is_none())
            && let Some(state) = self.states.get_mut(&info.options_name)
        {
            state.staged = state.baseline.clone();
        }
    }

    /// check-config-style validation of the staged set.
    pub fn violations(&self, info: &PortInfo) -> Vec<String> {
        self.conflicts(info).into_iter().map(|c| c.text).collect()
    }

    /// The rules the staged set breaks, in `violations` order, each with
    /// the options it is about and what make.conf did to it.
    pub fn conflicts(&self, info: &PortInfo) -> Vec<Conflict> {
        let Some(state) = self.state(info) else {
            return Vec::new();
        };
        let staged = &state.staged;
        let saved = state.saved.as_ref();
        // Where optique cannot read the files, it cannot say what make.conf
        // had to do with a rule broken.
        let unmanaged = self.unmanaged(info).is_some();
        let on_cause = |staged: &BTreeSet<String>, members: &[String]| {
            if unmanaged {
                None
            } else {
                on_cause(info, saved, staged, members)
            }
        };
        let pick_cause = |staged: &BTreeSet<String>, members: &[String]| {
            if unmanaged {
                None
            } else {
                pick_cause(info, saved, staged, members)
            }
        };
        let o = &info.options;
        // Judged on what make computes from the staged set as the options
        // file: *_FORCE overrides it, IMPLIES closes it.
        let local = self.locals.get(&info.options_name);
        let (unchecked, checked) = unchecked_groups(o, staged_effective(info, staged, local));
        let staged = &checked;
        let mut out = Vec::new();
        // bsd.port.mk's pre-check-config: each PREVENTS word of an option
        // that is on is a pattern (`${PORT_OPTIONS:M${conflict}}`), matched
        // against every option on, the option itself included; make names
        // the word, as written, in its message.
        for opt in staged {
            if let Some(def) = o.defs.get(opt) {
                for p in &def.prevents {
                    let hit: Vec<String> =
                        staged.iter().filter(|s| str_match(s, p)).cloned().collect();
                    if !hit.is_empty() {
                        let msg = def
                            .prevents_msg
                            .clone()
                            .unwrap_or_else(|| "mutually exclusive".to_string());
                        let mut members = vec![opt.clone()];
                        members.extend(hit.into_iter().filter(|h| h != opt));
                        out.push(Conflict {
                            text: format!("{opt} conflicts with {p}: {msg}"),
                            cause: on_cause(staged, &members),
                            members,
                        });
                    }
                }
            }
        }
        for g in o
            .groups
            .iter()
            .filter(|g| !unchecked.contains(g.name.as_str()))
        {
            let on = g.count_on(staged);
            let members: Vec<String> = g
                .members
                .iter()
                .filter(|m| o.complete.contains(*m))
                .cloned()
                .collect();
            let (text, cause) = match g.kind {
                GroupKind::Single if on == 0 => (
                    format!(
                        "group {} needs exactly one member enabled ({on} enabled)",
                        g.name
                    ),
                    pick_cause(staged, &members),
                ),
                GroupKind::Single if on > 1 => (
                    format!(
                        "group {} needs exactly one member enabled ({on} enabled)",
                        g.name
                    ),
                    on_cause(staged, &members),
                ),
                GroupKind::Multi if on == 0 => (
                    format!("group {} needs at least one member enabled", g.name),
                    pick_cause(staged, &members),
                ),
                GroupKind::Radio if on > 1 => (
                    format!(
                        "group {} allows at most one member enabled ({on} enabled)",
                        g.name
                    ),
                    on_cause(staged, &members),
                ),
                _ => continue,
            };
            out.push(Conflict {
                text,
                members,
                cause,
            });
        }
        // Deduplicate the symmetric PREVENTS pairs.
        out.sort_by(|a, b| a.text.cmp(&b.text));
        out.dedup_by(|a, b| a.text == b.text);
        out
    }

    /// Does the port have staged edits (staged differs from the baseline)?
    pub fn edited(&self, info: &PortInfo) -> bool {
        self.state(info).is_some_and(|s| s.staged != s.baseline)
    }

    pub fn status(&self, info: &PortInfo) -> UiStatus {
        if !info.options.has_options() || self.state(info).is_none() {
            return UiStatus::Ok;
        }
        if !self.violations(info).is_empty() {
            return UiStatus::Conflict;
        }
        // bsd.options.mk:300-304: OPTIONS_OVERRIDE decides the port by
        // itself, the options file unread; it can be neither unconfigured
        // nor stale, whatever is or is not on disk.
        if info.options.r#override.is_some() {
            return UiStatus::Ok;
        }
        // Nor can a file optique cannot model (`unmanaged`): it is not
        // written, and what make makes of it is make's alone.
        if self.unmanaged(info).is_some() {
            return UiStatus::Ok;
        }
        match self.stale_delta(info) {
            None => UiStatus::Unconfigured,
            Some((added, removed)) if added.is_empty() && removed.is_empty() => UiStatus::Ok,
            Some(_) => UiStatus::Stale,
        }
    }

    /// The options the port gained and lost since its saved file was
    /// written; None when there is no file. Judged against the file OWNER's
    /// option list: a non-default flavor excludes options on purpose and
    /// must not read the owner-written file as stale.
    pub fn stale_delta(&self, info: &PortInfo) -> Option<(Vec<String>, Vec<String>)> {
        let saved = self.state(info)?.saved.as_ref()?;
        let owner = self.owner_info(info);
        let cur = &owner.options.complete;
        // New: what the file gives no SET or UNSET line, as make's
        // NEW_OPTIONS counts it (SavedOptionsFile::decides).
        // Obsolete: whatever it mentions, list or line, that the tree lost;
        // a non-default flavor cannot tell that from what it merely lacks,
        // and a write keeps those lines (apply::planned_file), so none.
        let mentioned: BTreeSet<&str> = if is_preferred(owner) {
            saved
                .complete
                .iter()
                .chain(saved.recorded())
                .map(String::as_str)
                .collect()
        } else {
            BTreeSet::new()
        };
        Some((
            cur.iter().filter(|o| !saved.decides(o)).cloned().collect(),
            // An entry that is a pattern is obsolete once it matches none.
            mentioned
                .into_iter()
                .filter(|o| !cur.iter().any(|c| str_match(c, o)))
                .map(String::from)
                .collect(),
        ))
    }

    /// True when the port's Stale/Unconfigured status introduces no real
    /// decision because make.conf (global OPTIONS_SET/UNSET, per-port
    /// _SET/_UNSET or *_FORCE) already dictates the relevant options:
    /// for a stale port, every option added since the file was written
    /// (removed options never need a decision); for an unconfigured port,
    /// every option it has.
    pub fn covered_by_makeconf(&self, info: &PortInfo) -> bool {
        self.state(info).is_some() && self.undecided(info).is_empty()
    }

    /// Does this option's staged value contradict make.conf policy?
    /// Outcome-based: make.conf must take a stance on the option (any
    /// OPTIONS_SET/UNSET or per-port _SET/_UNSET mention) and the staged
    /// value must differ from what defaults + make.conf alone would yield.
    /// FORCE-decided options are excluded: the file cannot override them.
    /// So is every option of a file optique cannot manage: what the
    /// make.conf layers hold may be the file's own assignments.
    pub fn mc_deviates(&self, info: &PortInfo, opt: &str) -> bool {
        let Some(state) = self.state(info) else {
            return false;
        };
        let o = &info.options;
        if o.is_forced(opt) || self.unmanaged(info).is_some() {
            return false;
        }
        if !o.mc_mentions(opt) {
            return false;
        }
        // Both sides as make ends up: a member of a conditional group whose
        // option is off is dropped from either.
        let nofile_on = nofile_effective(info).contains(opt);
        staged_effective(info, &state.staged, self.locals.get(&info.options_name)).contains(opt)
            != nofile_on
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
            let Some(root) = self.resolve(root) else {
                continue;
            };
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
            let Some(info) = self.ports.get(&key) else {
                continue;
            };
            for dep in self.resolved_deps(info) {
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
        let Some(target) = self.resolve(target) else {
            return Vec::new();
        };
        self.ports
            .iter()
            .filter(|(_, info)| self.resolved_deps(info).any(|d| d == target))
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Apply a draft's staged sets onto matching states. Entries for unknown
    /// OPTIONS_NAMEs or with options no longer in the owner's option list are
    /// dropped (per-option filtering against the owner's complete list).
    /// Returns how many states changed.
    pub fn restore_draft(&mut self, draft: &crate::draft::Draft) -> usize {
        let mut changed = 0;
        let empty = BTreeSet::new();
        for (options_name, enabled) in &draft.staged {
            let known = draft.known.get(options_name).unwrap_or(&empty);
            // Judge against the options the file holds now, as the state
            // does: one the draft never knew (the port gained it since), or
            // one a make.conf knob or options.local decides, keeps the value
            // it has; the rest take the draft's, IMPLIES closed after.
            let restored = {
                let Some(owner) = self
                    .owners
                    .get(options_name)
                    .and_then(|key| self.ports.get(key))
                else {
                    continue; // the draft names a port no longer in the closure
                };
                // A file that became one optique cannot manage takes no edit.
                if self.unmanaged(owner).is_some() {
                    continue;
                }
                let Some(state) = self.states.get(options_name) else {
                    continue;
                };
                let mut set = BTreeSet::new();
                for opt in self.file_options(options_name) {
                    let keep = !known.contains(&opt)
                        || owner.options.is_forced(&opt)
                        || self.local_decides(owner, &opt);
                    let on = if keep {
                        state.staged.contains(&opt)
                    } else {
                        enabled.contains(&opt)
                    };
                    if on {
                        set.insert(opt);
                    }
                }
                close_implies(owner, set)
            };
            let Some(state) = self.states.get_mut(options_name) else {
                continue;
            };
            if state.staged != restored {
                state.staged = restored;
                changed += 1;
            }
        }
        changed
    }

    /// The options a shared file's state holds: its owner's, and every
    /// other flavor's under an owner that is no default flavor
    /// ([`shared_enabled`]).
    pub fn file_options(&self, name: &str) -> BTreeSet<String> {
        self.file_option_list(name).into_iter().collect()
    }

    /// [`file_options`](Self::file_options) in the order a write lists
    /// them: the owner's, then each other flavor's own, as
    /// `apply::planned_file` carries them.
    pub fn file_option_list(&self, name: &str) -> Vec<String> {
        let Some(owner) = self.owners.get(name).and_then(|k| self.ports.get(k)) else {
            return Vec::new();
        };
        let mut opts = owner.options.complete.clone();
        if !is_preferred(owner) {
            for other in sharing(&self.ports, name, &owner.key) {
                for o in &other.options.complete {
                    if !opts.contains(o) {
                        opts.push(o.clone());
                    }
                }
            }
        }
        opts
    }

    /// Does a port in the closure read the file `name`? A state outlives its
    /// ports (a flip-flop keeps its edits), but one no port reads is none of
    /// this session's business.
    pub fn reads(&self, name: &str) -> bool {
        self.owners.contains_key(name)
    }

    /// Any staged edit or pending baseline write (unconfigured/stale) on a
    /// file a port in the closure reads?
    pub fn dirty(&self) -> bool {
        self.states
            .iter()
            .any(|(name, s)| s.staged != s.baseline && self.reads(name))
    }
}

/// A rule the staged options break.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// What `violations` says of it.
    pub text: String,
    /// The options it is about: the two of a PREVENTS pair, or the group's
    /// members the port has.
    pub members: Vec<String>,
    /// What make.conf did to it, when it had a hand in it.
    pub cause: Option<String>,
}

/// What make computes with the staged set written as the options file,
/// `local` (the entry's options.local, as `Session::locals` holds it) read
/// after it: every SET entry of either before any UNSET one.
fn staged_effective(
    info: &PortInfo,
    staged: &BTreeSet<String>,
    local: Option<&SavedOptionsFile>,
) -> BTreeSet<String> {
    let complete = &info.options.complete;
    let mut file = SavedOptionsFile {
        complete: complete.clone(),
        set: staged.clone(),
        unset: complete
            .iter()
            .filter(|o| !staged.contains(*o))
            .cloned()
            .collect(),
        ..Default::default()
    };
    if let Some(local) = local {
        file.set.extend(local.local_set.iter().cloned());
        file.unset.extend(local.local_unset.iter().cloned());
    }
    effective_with(info, Some(&file))
}

/// The SINGLE and MULTI groups make's pre-check-config (bsd.port.mk) leaves
/// unchecked for `set`, and `set` as it leaves it. A group whose name
/// ALL_OPTIONS matches (`ALL_OPTIONS:M${group}`: OPTIONS_DEFINE, not group
/// members) is conditional: checked while `set` has an option the name
/// matches (`PORT_OPTIONS:M${group}`); otherwise every option a member
/// matches is dropped from PORT_OPTIONS (`PORT_OPTIONS:N${opt}`) and the
/// group is not checked. The SINGLE groups go first, then the MULTI ones,
/// each judged on the set the ones before left. RADIO and GROUP are never
/// conditional, and PREVENTS is judged after all of them.
fn unchecked_groups(
    o: &PortOptions,
    mut set: BTreeSet<String>,
) -> (BTreeSet<&str>, BTreeSet<String>) {
    let mut unchecked = BTreeSet::new();
    for kind in [GroupKind::Single, GroupKind::Multi] {
        for g in o.groups.iter().filter(|g| g.kind == kind) {
            let conditional = o.all.iter().any(|a| str_match(a, &g.name));
            if conditional && !set.iter().any(|p| str_match(p, &g.name)) {
                for m in &g.members {
                    set.retain(|p| !str_match(p, m));
                }
                unchecked.insert(g.name.as_str());
            }
        }
    }
    (unchecked, set)
}

/// The make.conf variable a non-FORCE layer sets an option with.
fn set_knob(info: &PortInfo, layer: McLayer) -> String {
    match layer {
        McLayer::Port => format!("{}_SET", info.options_name),
        _ => "OPTIONS_SET".to_string(),
    }
}

/// Whether the options file leaves the option to make.conf: there is no
/// file, or it gives the option no SET or UNSET line.
fn file_silent(saved: Option<&SavedOptionsFile>, opt: &str) -> bool {
    saved.is_none_or(|s| s.value(opt).is_none())
}

/// For a rule broken by options that are on — a PREVENTS pair, a group
/// with too many members on: those of `members` on because make.conf turns
/// them on, by the layer that does (`A, B on by make.conf OPTIONS_SET`),
/// and those a *_FORCE knob pins on (`A forced on by make.conf; fix it
/// there`). A non-FORCE layer counts only where the options file says
/// nothing of the option: a file line comes after it.
fn on_cause(
    info: &PortInfo,
    saved: Option<&SavedOptionsFile>,
    staged: &BTreeSet<String>,
    members: &[String],
) -> Option<String> {
    let o = &info.options;
    let mut by_knob: Vec<(String, Vec<&str>)> = Vec::new();
    let mut forced: Vec<&str> = Vec::new();
    for m in members.iter().filter(|m| staged.contains(*m)) {
        match o.mc_value(m) {
            Some((true, McLayer::Force | McLayer::With)) => forced.push(m),
            Some((true, layer)) if file_silent(saved, m) => {
                let knob = set_knob(info, layer);
                match by_knob.iter_mut().find(|(k, _)| *k == knob) {
                    Some((_, opts)) => opts.push(m),
                    None => by_knob.push((knob, vec![m])),
                }
            }
            _ => {}
        }
    }
    // The global knob before the per-port one, as the framework reads them.
    by_knob.sort_by_key(|(k, _)| k != "OPTIONS_SET");
    let mut parts: Vec<String> = by_knob
        .into_iter()
        .map(|(knob, opts)| format!("{} on by make.conf {knob}", opts.join(", ")))
        .collect();
    if !forced.is_empty() {
        parts.push(format!(
            "{} forced on by make.conf; fix it there",
            forced.join(", ")
        ));
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// For a group with no member on: the members to pick from (`pick one of A
/// B C`), and those make.conf turns off (`(A off by make.conf)`) — a FORCE
/// knob always, another layer where the options file says nothing.
fn pick_cause(
    info: &PortInfo,
    saved: Option<&SavedOptionsFile>,
    staged: &BTreeSet<String>,
    members: &[String],
) -> Option<String> {
    let o = &info.options;
    let off: Vec<&str> = members
        .iter()
        .filter(|m| !staged.contains(*m))
        .filter(|m| match o.mc_value(m) {
            Some((false, McLayer::Force | McLayer::With)) => true,
            Some((false, _)) => file_silent(saved, m),
            _ => false,
        })
        .map(String::as_str)
        .collect();
    let mut cause = format!("pick one of {}", members.join(" "));
    if !off.is_empty() {
        cause.push_str(&format!(" ({} off by make.conf)", off.join(", ")));
    }
    Some(cause)
}

/// Format the option changes between two staged snapshots as `+ADDED` /
/// `-REMOVED` tokens (additions first, each group in option order); the empty
/// string when the two sets are identical.
pub fn staged_diff(before: &BTreeSet<String>, after: &BTreeSet<String>) -> String {
    let added = after.difference(before).map(|opt| format!("+{opt}"));
    let removed = before.difference(after).map(|opt| format!("-{opt}"));
    added.chain(removed).collect::<Vec<String>>().join(" ")
}

/// `+OPT` or `-OPT` for every option of `complete`, in its order, by whether
/// `on` holds it.
pub fn signed_options(complete: &[String], on: &BTreeSet<String>) -> String {
    complete
        .iter()
        .map(|o| {
            if on.contains(o) {
                format!("+{o}")
            } else {
                format!("-{o}")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The saved file of the port owning an options file, and the baseline it
/// gives: the file read as make reads it, IMPLIES-closed. `others` are the
/// other flavors in the closure sharing the file ([`sharing`]).
fn saved_baseline(
    owner: &PortInfo,
    others: &[&PortInfo],
    files: &OptionsFiles,
) -> (Option<SavedOptionsFile>, BTreeSet<String>) {
    let saved = files.saved(&owner.options_name);
    // bsd.options.mk:300-304: with an OPTIONS_OVERRIDE, the file (even one
    // left on disk saying otherwise) is never read; the baseline is make's
    // no-file outcome, which `effective_with` computes from the override
    // alone, same as it would with `saved` passed in.
    let baseline = if owner.options.r#override.is_some() {
        nofile_effective(owner)
    } else {
        shared_enabled(owner, others, saved.as_ref())
    };
    (saved, baseline)
}

/// The enabled set a shared file's state starts from: the owner's view,
/// and, when the owner is no default flavor, every option another flavor
/// sharing the file has besides, at that flavor's value: such an owner
/// writes them all (`apply::planned_file`), so the state must hold them.
fn shared_enabled(
    owner: &PortInfo,
    others: &[&PortInfo],
    saved: Option<&SavedOptionsFile>,
) -> BTreeSet<String> {
    shared_enabled_by(apply::sync_enabled_set, owner, others, saved)
}

/// [`shared_enabled`], each flavor's set taken by `enabled`.
fn shared_enabled_by(
    enabled: fn(&PortInfo, Option<&SavedOptionsFile>) -> BTreeSet<String>,
    owner: &PortInfo,
    others: &[&PortInfo],
    saved: Option<&SavedOptionsFile>,
) -> BTreeSet<String> {
    let mut set = enabled(owner, saved);
    if !is_preferred(owner) {
        for other in others {
            let theirs = enabled(other, saved);
            set.extend(
                theirs
                    .into_iter()
                    .filter(|o| !owner.options.complete.contains(o)),
            );
        }
    }
    close_implies(owner, set)
}

/// The closure's flavors reading the file `name`, but `except`: ports with
/// options whose OPTIONS_FILE make.conf did not move.
fn sharing<'a>(
    ports: &'a BTreeMap<PortKey, PortInfo>,
    name: &str,
    except: &PortKey,
) -> Vec<&'a PortInfo> {
    ports
        .values()
        .filter(|i| {
            i.options_name == name
                && i.key != *except
                && i.options.has_options()
                && moved(i).is_none()
        })
        .collect()
}

/// OPTIONS_DEFAULT restricted to the options the port has.
fn port_defaults(o: &PortOptions) -> BTreeSet<String> {
    o.defaults
        .iter()
        .filter(|d| o.complete.contains(d))
        .cloned()
        .collect()
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
    // bsd.options.mk:300-304: OPTIONS_OVERRIDE decides every option of the
    // port by itself; none of them is left for a human.
    if info.options.r#override.is_some() {
        return Vec::new();
    }
    // Recorded means a SET or UNSET line names it (SavedOptionsFile::decides,
    // patterns included): an option only in `_FILE_COMPLETE_OPTIONS_LIST`
    // is left by make at its default, which nobody decided.
    let known = |opt: &str| saved.is_some_and(|s| s.decides(opt));
    let o = &info.options;
    // IMPLIES runs after FORCE (bsd.options.mk:411): what a forced-on option
    // implies is on whatever the file says.
    let forced_on: BTreeSet<String> = o
        .complete
        .iter()
        .filter(|opt| o.forced_value(opt) == Some(true))
        .cloned()
        .collect();
    let forced_implied = close_implies(info, forced_on);
    let mc_decided = |opt: &str| o.mc_decides(opt) || forced_implied.contains(opt);
    // Selected means make.conf ENDS UP enabling it, not merely that some layer
    // names it: a later layer can undo an earlier one, and then the member
    // actually in force was picked by the port default — nobody's decision.
    // Hence the outcome (which resolves the layer order, and the IMPLIES
    // closure with it) rather than membership of mc_set/port_set.
    let nofile = nofile_effective(info);
    let mc_selected = |opt: &str| mc_decided(opt) && nofile.contains(opt);
    let group_decided: BTreeSet<&str> = o
        .groups
        .iter()
        .filter(|g| matches!(g.kind, GroupKind::Single | GroupKind::Radio))
        .filter(|g| g.members.iter().any(|m| mc_selected(m)))
        .flat_map(|g| g.members.iter().map(String::as_str))
        .collect();
    o.complete
        .iter()
        .filter(|opt| !known(opt))
        .filter(|opt| !mc_decided(opt) && !group_decided.contains(opt.as_str()))
        .cloned()
        .collect()
}

/// Effective options as they would be WITHOUT any options file: the
/// bsd.options.mk application order (defaults, make.conf layers, FORCE,
/// IMPLIES closure, then bsd.port.mk's drop of an unchecked conditional
/// group's members) minus the options-file layer.
pub(crate) fn nofile_effective(info: &PortInfo) -> BTreeSet<String> {
    effective_with(info, None)
}

/// Effective options as make computes them with `file` as the options file
/// (None: no file), from what the scan knows — never from the port's
/// queried `effective`, which in the TUI already holds staged edits once a
/// refresh has run. `file` is the options file with its `options.local`
/// read after it (`OptionsFiles::saved`), when it comes from disk; an
/// `options.local` without an options file is read after an empty one.
/// `file` is ignored outright when make.conf has an OPTIONS_OVERRIDE for
/// the port — see below.
pub(crate) fn effective_with(info: &PortInfo, file: Option<&SavedOptionsFile>) -> BTreeSet<String> {
    unchecked_groups(&info.options, built_with(info, file)).1
}

/// What the build reads from `file`: [`effective_with`] before bsd.port.mk
/// drops the members of a conditional group whose option is off. Their
/// helpers (`_DEPENDS`, `_CONFIGURE_ON`, ...) ran on this set first and stay
/// applied, so two files only build the same when this set is the same:
/// deciding a file is identical or redundant compares this, not PORT_OPTIONS.
pub(crate) fn built_with(info: &PortInfo, file: Option<&SavedOptionsFile>) -> BTreeSet<String> {
    let o = &info.options;
    let complete = &o.complete;
    // bsd.options.mk:300-304: OPTIONS_OVERRIDE, defined even empty, forgets
    // everything the `.else` branch would have done — OPTIONS_DEFAULT, the
    // two SET/UNSET pairs, the options file and its options.local — and
    // starts PORT_OPTIONS as its own value, taken as is: not filtered by
    // COMPLETE_OPTIONS_LIST, unlike every SET entry below. What follows
    // `.endif` (the FORCE knobs, WITH/WITHOUT, IMPLIES, OPTIONS_SLAVE) still
    // runs on top of it.
    let mut set = match &o.r#override {
        Some(over) => over.clone(),
        None => {
            let mut set = port_defaults(o);
            let empty = BTreeSet::new();
            let (file_set, file_unset) = file.map_or((&empty, &empty), |f| (&f.set, &f.unset));
            // One pass per layer, in the framework's own order
            // (bsd.options.mk:312, 320, 326, 334, the file at 354 and 361):
            // each scope's UNSET runs before the next scope's SET, which is
            // what lets the per-port knob override the global one. Applying
            // all the SETs and then all the UNSETs instead would let a
            // global UNSET erase a per-port SET.
            for (adds, list) in [
                (true, &o.mc_set),
                (false, &o.mc_unset),
                (true, &o.port_set),
                (false, &o.port_unset),
                (true, file_set),
                (false, file_unset),
            ] {
                apply_layer(complete, &mut set, adds, list);
            }
            set
        }
    };
    // The *_FORCE knobs and make.conf's WITH/WITHOUT (bsd.options.mk:370,
    // 378, 384, 392, 399, 406) apply whether or not OPTIONS_OVERRIDE
    // replaced everything above `.endif`.
    for (adds, list) in [
        (true, &o.force_set),
        (false, &o.force_unset),
        (true, &o.port_force_set),
        (false, &o.port_force_unset),
        (true, &o.with),
        (false, &o.without),
    ] {
        apply_layer(complete, &mut set, adds, list);
    }
    // IMPLIES (bsd.options.mk:411) and OPTIONS_SLAVE (:447) come last, not in
    // the list but on whatever the layers above said. bsd.port.mk then drops
    // the members of a conditional group whose option is off from
    // PORT_OPTIONS (`effective_with`).
    let mut set = make_implies(info, set);
    set.extend(o.slave.iter().cloned());
    set
}

/// The IMPLIES closure as bsd.options.mk:410-440 computes PORT_OPTIONS:
/// only an option of the port's list implies (`_DEPCHAIN` pairs start in
/// COMPLETE_OPTIONS_LIST), and what it implies goes in unchecked, listed
/// or not (an option excluded on this platform, say), unless an option
/// already on matches it as a pattern (`empty(_PORT_OPTIONS:M$d)`). Such an
/// option implies nothing in turn, but a PREVENTS can name it. The staged
/// sets, which hold only listed options, use `close_implies`.
fn make_implies(info: &PortInfo, mut set: BTreeSet<String>) -> BTreeSet<String> {
    let complete: BTreeSet<&str> = info.options.complete.iter().map(String::as_str).collect();
    let mut todo: Vec<String> = set.iter().cloned().collect();
    while let Some(opt) = todo.pop() {
        if !complete.contains(opt.as_str()) {
            continue;
        }
        let Some(def) = info.options.defs.get(&opt) else {
            continue;
        };
        for imp in &def.implies {
            if !set.iter().any(|s| str_match(s, imp)) {
                set.insert(imp.clone());
                todo.push(imp.clone());
            }
        }
    }
    set
}

/// One SET/UNSET layer applied to `set`: a SET entry goes in word for word
/// when it matches an option of the port (`COMPLETE_OPTIONS_LIST:M${opt}`);
/// an UNSET entry takes out every option it matches (`PORT_OPTIONS:N${opt}`).
fn apply_layer(
    complete: &[String],
    set: &mut BTreeSet<String>,
    adds: bool,
    list: &BTreeSet<String>,
) {
    for entry in list.iter() {
        if adds {
            if complete.iter().any(|c| str_match(c, entry)) {
                set.insert(entry.clone());
            }
        } else {
            set.retain(|o| !str_match(o, entry));
        }
    }
}

/// Transitive IMPLIES closure of an enabled set, restricted to the port's
/// known options (mirrors bsd.options.mk, which force-adds implied options).
/// make runs IMPLIES after the `*_FORCE` knobs and WITH/WITHOUT
/// (bsd.options.mk:411), so the options that imply are those `set` has and
/// no knob forces off, and those a knob forces on ([`impliers`]); what they
/// imply goes in, and implies in turn, forced off or not.
pub(crate) fn close_implies(info: &PortInfo, mut set: BTreeSet<String>) -> BTreeSet<String> {
    let complete: BTreeSet<&str> = info.options.complete.iter().map(String::as_str).collect();
    let mut active = impliers(info, &set);
    // Each option is expanded once: when it turns active.
    let mut todo: Vec<String> = active.iter().cloned().collect();
    while let Some(opt) = todo.pop() {
        let Some(def) = info.options.defs.get(&opt) else {
            continue;
        };
        for imp in &def.implies {
            if complete.contains(imp.as_str()) {
                set.insert(imp.clone());
                if active.insert(imp.clone()) {
                    todo.push(imp.clone());
                }
            }
        }
    }
    set
}

/// The options of `set` on once make's knobs have run, which is what IMPLIES
/// starts from: those no `*_FORCE` knob or WITHOUT turns off, and those one
/// turns on, whatever `set` says.
fn impliers(info: &PortInfo, set: &BTreeSet<String>) -> BTreeSet<String> {
    let o = &info.options;
    set.iter()
        .filter(|opt| o.forced_value(opt) != Some(false))
        .chain(
            o.complete
                .iter()
                .filter(|opt| o.forced_value(opt) == Some(true)),
        )
        .cloned()
        .collect()
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
    for candidate in &impliers(info, staged) {
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
mod tests;
