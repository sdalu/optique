//! Re-queries after an edit: the debounce, the refresher's events, and
//! which change a new status or loop is blamed on.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use crate::model::origin::PortKey;
use crate::optionsfile;
use crate::query::refresher::RefreshEvent;

use super::App;

/// How long after the last toggle before the port is re-queried.
pub(super) const REFRESH_DEBOUNCE: Duration = Duration::from_millis(300);

/// What a refresh changed in the closure: the ports by name when there are
/// at most three of them, else how many came in and went.
pub(super) fn refreshed_message(added: &[PortKey], removed: &[PortKey]) -> String {
    if added.len() + removed.len() > 3 {
        return format!(
            "dependencies refreshed: +{} −{} port(s)",
            added.len(),
            removed.len()
        );
    }
    let names: Vec<String> = added
        .iter()
        .map(|k| format!("+{k}"))
        .chain(removed.iter().map(|k| format!("−{k}")))
        .collect();
    format!("dependencies refreshed: {}", names.join(" "))
}

/// Squeeze a BROKEN/IGNORE message onto one status-bar line: whitespace
/// collapsed, elided past ~80 characters.
pub(super) fn trim_reason(msg: &str) -> String {
    const MAX: usize = 80;
    let msg: String = msg.split_whitespace().collect::<Vec<&str>>().join(" ");
    match msg.char_indices().nth(MAX) {
        None => msg,
        Some((cut, _)) => format!("{}…", &msg[..cut]),
    }
}

impl App {
    /// Schedule the port for a debounced background re-query.
    pub(super) fn mark_pending(&mut self, key: &PortKey) {
        self.pending.insert(key.clone(), Instant::now());
    }

    /// Push due pending ports to the refresher: write their staged options
    /// file into the staging db, then re-scan from them.
    ///
    /// Nothing is written while a re-scan is outstanding: it may still be
    /// reading the staging copy, and its cache entries pair the bytes it saw
    /// at lookup with what make computed, so a file rewritten under it would
    /// file one option value's result under the other's bytes. Due ports wait
    /// and go out together once it reports done.
    pub(super) fn flush_due_refreshes(&mut self) {
        if self.pending.is_empty() || self.refreshing > 0 {
            return;
        }
        let now = Instant::now();
        let due: Vec<PortKey> = self
            .pending
            .iter()
            .filter(|(_, t)| now.duration_since(**t) >= REFRESH_DEBOUNCE)
            .map(|(k, _)| k.clone())
            .collect();
        if due.is_empty() {
            return;
        }
        let mut batch = Vec::new();
        for key in due {
            self.pending.remove(&key);
            let Some(info) = self.session.ports.get(&key) else {
                continue;
            };
            let Some(state) = self.session.state(info) else {
                continue;
            };
            // A flavor make.conf moves OPTIONS_FILE for reads a file of its
            // own, not the staged one: its state is no content for the
            // shared file the other flavors are queried with.
            if crate::session::moved(info).is_some() {
                batch.push(key);
                continue;
            }
            let options_name = info.options_name.clone();
            let staged = state.staged.clone();
            // A newcomer left as its file has it gets the file itself, byte
            // for byte as the scan's cache knows it; an edited one, the
            // edit.
            let written = if self.to_seed.contains(&options_name) && staged == state.baseline {
                self.staging_db
                    .seed(&self.options_files, &options_name)
                    .map(|_| ())
            } else {
                let local = if self.to_seed.remove(&options_name) {
                    self.staging_db
                        .seed_local(&self.options_files, &options_name)
                } else {
                    Ok(())
                };
                // The file apply would write: the owning flavor's view, and
                // under an owner that is no default flavor every other
                // flavor's own options too, so each flavor reading it is
                // queried with its edits.
                let owner = self.session.owner_info(info);
                let list = self.session.file_option_list(&options_name);
                let content = optionsfile::render(&owner.pkgname, &list, &staged);
                local.and_then(|()| self.staging_db.write(&options_name, content.as_bytes()))
            };
            if let Err(e) = written {
                self.notify(&format!("staging write failed: {e:#}"), true);
                continue;
            }
            self.record_snapshot(options_name, staged);
            batch.push(key);
        }
        // An edit to a shared file changes every flavor reading it: each is
        // queried again, or a dependency the edit gives a sibling would
        // never join the closure (graphics/libwmf@nox11 for
        // ImageMagick7@nox11 when WMF is turned on in @x11).
        let shared: BTreeSet<String> = batch
            .iter()
            .filter_map(|k| self.session.ports.get(k))
            .filter(|i| crate::session::moved(i).is_none())
            .map(|i| i.options_name.clone())
            .collect();
        let siblings: Vec<PortKey> = self
            .session
            .ports
            .iter()
            .filter(|(k, i)| {
                shared.contains(&i.options_name)
                    && crate::session::moved(i).is_none()
                    && !batch.contains(k)
            })
            .map(|(k, _)| k.clone())
            .collect();
        batch.extend(siblings);
        for key in &batch {
            if let Some(info) = self.session.ports.get(key) {
                self.to_seed.remove(&info.options_name);
            }
        }
        if !batch.is_empty() && self.refresher.tx.send(batch).is_ok() {
            self.refreshing += 1;
        }
    }

    /// Remember the staged set a refresh was submitted with, keeping the
    /// previous one for attribution. A resubmission of an unchanged set (the
    /// other flavors of an origin share the file, and a revert can land on the
    /// same set) keeps the recorded history instead of flattening it.
    fn record_snapshot(&mut self, options_name: String, staged: BTreeSet<String>) {
        match self.refresh_snapshots.get_mut(&options_name) {
            Some((current, previous)) => {
                if *current != staged {
                    *previous = Some(std::mem::replace(current, staged));
                }
            }
            None => {
                self.refresh_snapshots.insert(options_name, (staged, None));
            }
        }
    }

    /// The option changes to blame when a port turns IGNORE/BROKEN: the diff
    /// between the last two staged sets submitted for its options file, or
    /// staged-vs-baseline when only one refresh is on record.
    fn blame_diff(&self, info: &crate::model::port::PortInfo) -> String {
        if let Some((current, Some(previous))) = self.refresh_snapshots.get(&info.options_name) {
            return crate::session::staged_diff(previous, current);
        }
        match self.session.state(info) {
            Some(state) => crate::session::staged_diff(&state.baseline, &state.staged),
            None => String::new(),
        }
    }

    /// Attribute freshly discovered IGNORE/BROKEN states to recent option
    /// changes, comparing the incoming scan against the still-unmerged
    /// session. Returns the flash text for the last port that just turned
    /// bad, if any; ports that came back clean drop their attribution.
    fn attribute_blame(
        &mut self,
        fresh: &std::collections::BTreeMap<PortKey, crate::model::port::PortInfo>,
    ) -> Option<String> {
        use crate::session::is_blocked;
        let mut flash = None;
        for (key, info) in fresh {
            let was = self.session.ports.get(key).map(is_blocked).unwrap_or(false);
            match (was, is_blocked(info)) {
                (true, false) => {
                    self.blame.remove(&info.options_name);
                }
                (false, true) => {
                    let diff = self.blame_diff(info);
                    if diff.is_empty() {
                        continue; // nothing recent to point at
                    }
                    // is_blocked: one of the two is set.
                    let cause = info
                        .broken
                        .as_ref()
                        .map(|m| ("BROKEN", m.as_str()))
                        .or(info.ignore.as_ref().map(|m| ("IGNORE", m.as_str())));
                    if let Some((label, reason)) = cause {
                        flash = Some(format!(
                            "{key}: now {label} ({}) — {diff}",
                            trim_reason(reason)
                        ));
                        self.blame.insert(info.options_name.clone(), diff);
                    }
                }
                _ => {}
            }
        }
        flash
    }

    /// Merge finished background scans into the session.
    pub(super) fn drain_refresh_events(&mut self) {
        let mut merged = None;
        // Set once a dep error or a port that just broke is flashed: the
        // refresh count must not take its place on the line.
        let mut alerted = false;
        while let Ok(ev) = self.refresher.rx.try_recv() {
            match ev {
                RefreshEvent::Progress { done, discovered } => {
                    self.refresh_progress = Some((done, discovered));
                }
                RefreshEvent::Done { result, batches } => {
                    self.refreshing = self.refreshing.saturating_sub(batches);
                    let errors = result.errors.len();
                    let first_error = result.errors.first().cloned();
                    // Blame before the merge: it needs the pre-merge flags.
                    let blamed = self.attribute_blame(&result.ports);
                    let (added, removed) = self.session.merge(*result, &self.options_files);
                    let (a, r) = merged.get_or_insert((Vec::new(), Vec::new()));
                    a.extend(added);
                    r.extend(removed);
                    if let Some((key, msg)) = first_error {
                        self.notify(
                            &format!("refresh: {errors} dep error(s), e.g. {key}: {msg}"),
                            true,
                        );
                        alerted = true;
                    }
                    // A port that just broke outranks dep errors on the line.
                    if let Some(msg) = blamed {
                        self.notify(&msg, true);
                        alerted = true;
                    }
                }
            }
        }
        if self.refreshing == 0 {
            self.refresh_progress = None;
        }
        if let Some((added, removed)) = merged {
            // A newcomer was queried without its real options file, which
            // staging never had: it is copied in and the port queried again.
            for key in &added {
                let Some(info) = self.session.ports.get(key) else {
                    continue;
                };
                // One reading a file of its own was queried with it.
                if crate::session::moved(info).is_some() {
                    continue;
                }
                let name = &info.options_name;
                let real = self.options_files.bytes(name).is_some()
                    || self.options_files.local_bytes(name).is_some();
                if !self.staging_db.has(name) && real {
                    self.to_seed.insert(name.clone());
                    self.mark_pending(key);
                }
            }
            // The list's title counts the optionless ports the merge left.
            self.hidden = self
                .session
                .ports
                .values()
                .filter(|p| !p.options.has_options())
                .count();
            if !(added.is_empty() && removed.is_empty()) && !alerted {
                self.notify(&refreshed_message(&added, &removed), false);
            }
            // Dep edges may have changed without any port appearing or
            // vanishing, so the loops are recomputed on every merge — and a
            // loop that just appeared outranks the refresh line. Before the
            // rebuild: the L view lists the loops as they are now, and is
            // left when the last one went.
            self.refresh_loops(alerted);
            self.refresh_view();
        }
    }

    /// Recompute the closure's dependency loops, announcing one that just
    /// appeared (or the moment the last one goes away).
    /// `alerted`: an alert already holds the line; a loop gone is news, not
    /// an alert, and leaves it there (a loop that appeared outranks it).
    pub(super) fn refresh_loops(&mut self, alerted: bool) {
        let before: Vec<Vec<PortKey>> = self.loops.iter().map(|dl| dl.members.clone()).collect();
        self.loops = self.session.dep_loops();
        match self.loops.iter().find(|dl| !before.contains(&dl.members)) {
            Some(fresh) => {
                let msg = format!("dependency loop: {}", fresh.render());
                self.notify(&msg, true);
            }
            None if self.loops.is_empty() && !before.is_empty() => {
                // The L view would now show an empty list, which says nothing
                // about why: leave it rather than strand the user in it.
                let msg = if self.only_loops {
                    self.only_loops = false;
                    "dependency loop gone; showing all ports again"
                } else {
                    "dependency loop gone"
                };
                if !alerted {
                    self.notify(msg, false);
                }
            }
            None => {}
        }
    }
}
