//! The machine-readable JSON contract: one module so every `--json` payload
//! is defined, and documented, in a single place.
//!
//! Three reports, one per shape of answer:
//! [`ScanReport`] says what the closure looks like and what it owes a
//! decision on, [`WriteReport`] what `decide` or `sync` did (or would do) to
//! the options dir, and [`CleanReport`] what `clean` removed from it.
//! Everything an agent needs to choose an option value is in
//! [`OptionReport`], which `scan --json --options` fills in.

use serde::Serialize;

use crate::apply::PendingWrite;
use crate::model::options::GroupKind;
use crate::model::port::PortInfo;
use crate::session::Session;

/// Everything `scan --json` prints.
#[derive(Serialize)]
pub struct ScanReport {
    pub options_dir: String,
    pub ports_tree: String,
    pub ports: Vec<PortReport>,
    pub loops: Vec<LoopReport>,
    pub summary: ScanSummary,
}

#[derive(Serialize)]
pub struct PortReport {
    /// `category/name[@flavor]`, the canonical key the closure stores.
    pub port: String,
    pub pkgname: String,
    /// `ok`, `unconfigured`, `stale` or `conflict`.
    pub status: &'static str,
    /// Options this port still owes a human answer for.
    pub undecided: Vec<String>,
    /// Options the tree gained (`added`) or lost (`removed`) since the file
    /// was written; both empty unless the status is `stale`.
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// make.conf decides every option that was still open.
    pub mc_covered: bool,
    pub blacklisted: bool,
    pub in_loop: bool,
    /// The options file this port reads, relative to `options_dir`.
    pub options_file: String,
    /// Why the status is `conflict`; empty otherwise. Only with `--options`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub violations: Option<Vec<String>>,
    /// Full per-option detail. Only with `--options`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<OptionReport>>,
}

/// One option, with everything needed to decide its value without re-running
/// `make`: the value in force, where it comes from, what constrains it and
/// what enabling it drags in.
#[derive(Clone, Serialize)]
pub struct OptionReport {
    pub name: String,
    /// The value that would be written now (saved choice, or the default for
    /// an option the file does not know).
    pub staged: bool,
    /// The port's own default (`OPTIONS_DEFAULT` plus the options the
    /// framework always turns on: DOCS, NLS, EXAMPLES, IPV6).
    pub default: bool,
    /// The value the options file records, null when it has no entry for the
    /// option — that is exactly the case `undecided` reports.
    pub saved: Option<bool>,
    /// The option was added to the port since the file was written.
    pub new: bool,
    /// What make.conf dictates, null when no layer mentions the option.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub makeconf: Option<MakeConfVerdict>,
    /// The options file cannot change this value.
    pub locked: bool,
    /// Why, when it is locked: a `*_FORCE` knob or the option that implies it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_by: Option<String>,
    /// The staged value contradicts what make.conf asks for.
    pub deviates_makeconf: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupReport>,
    pub implies: Vec<String>,
    pub prevents: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prevents_msg: Option<String>,
    pub desc: String,
    /// Enabling the option marks the port BROKEN / IGNOREd with this message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub broken: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore: Option<String>,
    /// Port origins this option declares as dependencies, any class, sorted.
    pub adds_deps: Vec<String>,
    /// `USES` frameworks it activates, as written in the Makefile.
    pub uses: Vec<String>,
}

/// Which make.conf knob decides an option, and how.
#[derive(Clone, Serialize)]
pub struct MakeConfVerdict {
    pub value: bool,
    /// `global` (OPTIONS_SET/UNSET), `port` (${OPTIONS_NAME}_SET/_UNSET) or
    /// `force` (a `*_FORCE` knob, which the options file cannot override).
    pub scope: &'static str,
}

#[derive(Clone, Serialize)]
pub struct GroupReport {
    pub name: String,
    /// `group`, `multi` (at least one), `single` (exactly one) or `radio`
    /// (at most one).
    pub kind: &'static str,
    pub members: Vec<String>,
}

#[derive(Serialize)]
pub struct LoopReport {
    /// Every port of the tangle.
    pub ports: Vec<String>,
    /// A shortest closed walk through it, its first port listed once.
    pub cycle: Vec<String>,
    /// Every member is blacklisted, so the loop holds nothing up.
    pub blacklisted: bool,
}

#[derive(Serialize)]
pub struct ScanSummary {
    pub total: usize,
    pub unconfigured: usize,
    pub stale: usize,
    pub conflict: usize,
    pub ok: usize,
    pub optionless: usize,
    pub blacklisted: usize,
    /// Ports owing a human decision.
    pub attention: usize,
    pub loops: usize,
    /// Loops with at least one member that is not blacklisted.
    pub loops_blocking: usize,
    /// What the exit status reports: `attention` + `loops_blocking`.
    pub pending: usize,
}

/// What `decide` and `sync` report.
#[derive(Serialize)]
pub struct WriteReport {
    pub options_dir: String,
    /// Nothing was written, whatever the plan said.
    pub dry_run: bool,
    /// The options dir was actually changed.
    pub applied: bool,
    pub writes: Vec<WriteEntry>,
    pub removals: Vec<RemovalEntry>,
    /// Requested option values the port's own rules refused.
    pub rejected: Vec<RejectedEntry>,
    /// Ports whose resulting option set violates PREVENTS or a group rule.
    /// A non-empty list means nothing was written.
    pub conflicts: Vec<ConflictEntry>,
    /// Ports named in the plan that the closure does not hold.
    pub unknown: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<ErrorEntry>,
    pub summary: WriteSummary,
}

#[derive(Serialize)]
pub struct WriteEntry {
    pub port: String,
    pub options_name: String,
    /// Absolute path of the options file.
    pub file: String,
    /// There was no options file for this port before.
    pub new_file: bool,
    /// `+OPT` / `-OPT` for options the file already knew.
    pub diff: Vec<String>,
    /// Options the port gained since the file was written, with the value
    /// being recorded. Empty for a new file, whose whole state is `enabled`.
    pub adopted: Vec<AdoptedOption>,
    /// Options the port no longer has; the file stops mentioning them.
    pub dropped: Vec<String>,
    /// The complete enabled set the file will record.
    pub enabled: Vec<String>,
    /// Origins the newly enabled options declare as dependencies.
    pub new_deps: Vec<String>,
}

#[derive(Serialize)]
pub struct AdoptedOption {
    pub name: String,
    pub on: bool,
}

#[derive(Serialize)]
pub struct RemovalEntry {
    pub options_name: String,
    pub reason: String,
}

#[derive(Clone, Serialize)]
pub struct RejectedEntry {
    pub port: String,
    pub option: String,
    pub reason: String,
}

#[derive(Clone, Serialize)]
pub struct ConflictEntry {
    pub port: String,
    pub violations: Vec<String>,
}

#[derive(Serialize)]
pub struct ErrorEntry {
    /// The port or options-dir entry the failure is about.
    pub subject: String,
    pub message: String,
}

#[derive(Serialize)]
pub struct WriteSummary {
    pub written: usize,
    pub removed: usize,
    pub failed: usize,
    pub rejected: usize,
    pub conflicts: usize,
}

/// What `clean` reports.
#[derive(Serialize)]
pub struct CleanReport {
    pub options_dir: String,
    pub dry_run: bool,
    pub removals: Vec<RemovalEntry>,
    /// Entries kept, with the reason, only under `--verbose`.
    pub kept: Vec<KeptEntry>,
    pub warnings: Vec<String>,
    pub errors: Vec<ErrorEntry>,
    pub summary: CleanSummary,
}

#[derive(Serialize)]
pub struct KeptEntry {
    pub options_name: String,
    pub reason: String,
}

#[derive(Serialize)]
pub struct CleanSummary {
    /// Entries found in the options dir.
    pub entries: usize,
    pub removed: usize,
    pub failed: usize,
}

/// The `--options` detail for one port, in the framework's option order.
///
/// Judged from the file OWNER's point of view, exactly like the status and
/// the write are: flavors of an origin share one options file, and the
/// default flavor's view is what gets written.
pub fn option_reports(sess: &Session, info: &PortInfo) -> Vec<OptionReport> {
    let owner = sess.owner_info(info);
    let opts = &owner.options;
    let state = sess.state(owner);
    let saved = state.and_then(|s| s.saved.as_ref());
    opts.complete
        .iter()
        .map(|name| {
            let staged = state.map(|s| s.staged.contains(name)).unwrap_or(false);
            // What the file says about the option: Some(value) when it has an
            // entry, None when the option is new to it (or there is no file).
            let file_value = saved.and_then(|s| {
                if s.set.contains(name) {
                    Some(true)
                } else if s.unset.contains(name) || s.complete.contains(name) {
                    Some(false)
                } else {
                    None
                }
            });
            // What make.conf asks for, regardless of what the file says —
            // the *_FORCE knobs first, since they win over the file too.
            let makeconf = match opts.forced_value(name) {
                Some(value) => Some(MakeConfVerdict { value, scope: "force" }),
                None if opts.port_set.contains(name) || opts.port_unset.contains(name) => {
                    Some(MakeConfVerdict { value: opts.port_set.contains(name), scope: "port" })
                }
                None if opts.mc_set.contains(name) || opts.mc_unset.contains(name) => {
                    Some(MakeConfVerdict { value: opts.mc_set.contains(name), scope: "global" })
                }
                None => None,
            };
            let implier = sess.implied_by(owner, name).filter(|_| staged);
            let (locked, locked_by) = match (opts.is_forced(name), implier) {
                (true, _) => (true, Some("make.conf *_FORCE".to_string())),
                (false, Some(by)) => (true, Some(format!("implied by {by}"))),
                (false, None) => (false, None),
            };
            let def = opts.defs.get(name);
            let group = opts
                .groups
                .iter()
                .find(|g| g.members.iter().any(|m| m == name))
                .map(|g| GroupReport {
                    name: g.name.clone(),
                    kind: group_kind(g.kind),
                    members: g.members.clone(),
                });
            let mut adds_deps: Vec<String> = def
                .map(|d| d.deps.iter().flat_map(|(_, origins)| origins.clone()).collect())
                .unwrap_or_default();
            adds_deps.sort();
            adds_deps.dedup();
            OptionReport {
                name: name.clone(),
                staged,
                default: opts.defaults.contains(name),
                saved: file_value,
                new: saved.is_some() && file_value.is_none(),
                makeconf,
                locked,
                locked_by,
                deviates_makeconf: sess.mc_deviates(owner, name),
                group,
                implies: def.map(|d| d.implies.clone()).unwrap_or_default(),
                prevents: def.map(|d| d.prevents.clone()).unwrap_or_default(),
                prevents_msg: def.and_then(|d| d.prevents_msg.clone()),
                desc: def.map(|d| d.desc.clone()).unwrap_or_default(),
                broken: def.and_then(|d| d.broken.clone()),
                ignore: def.and_then(|d| d.ignore.clone()),
                adds_deps,
                uses: def.map(|d| d.uses.clone()).unwrap_or_default(),
            }
        })
        .collect()
}

fn group_kind(kind: GroupKind) -> &'static str {
    match kind {
        GroupKind::Group => "group",
        GroupKind::Multi => "multi",
        GroupKind::Single => "single",
        GroupKind::Radio => "radio",
    }
}

/// One pending write as JSON, with the dependencies its newly enabled
/// options declare.
pub fn write_entry(w: &PendingWrite, info: Option<&PortInfo>) -> WriteEntry {
    let c = w.changes();
    let mut diff: Vec<String> = c.turned_on.iter().map(|o| format!("+{o}")).collect();
    diff.extend(c.turned_off.iter().map(|o| format!("-{o}")));
    let newly_on: Vec<&String> = c
        .turned_on
        .iter()
        .chain(c.adopted.iter().filter(|(_, on)| *on).map(|(o, _)| o))
        .collect();
    let mut new_deps: Vec<String> = Vec::new();
    if let Some(info) = info {
        for opt in newly_on {
            if let Some(def) = info.options.defs.get(opt) {
                for (_, origins) in &def.deps {
                    new_deps.extend(origins.clone());
                }
            }
        }
    }
    new_deps.sort();
    new_deps.dedup();
    let new_file = w.old.is_none();
    WriteEntry {
        port: w.key.to_string(),
        new_file,
        options_name: w
            .path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        file: w.path.display().to_string(),
        diff,
        // A new file adopts every option the port has; listing them all
        // would bury the change, and `enabled` already says the whole state.
        adopted: if new_file {
            Vec::new()
        } else {
            c.adopted
                .iter()
                .map(|(name, on)| AdoptedOption { name: name.clone(), on: *on })
                .collect()
        },
        dropped: c.dropped.clone(),
        enabled: w.enabled.iter().cloned().collect(),
        new_deps,
    }
}

/// Serialize a report as pretty JSON on stdout, the single machine payload.
pub fn print(report: &impl Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(report)?);
    Ok(())
}
