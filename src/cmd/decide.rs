//! `optique decide`: a JSON plan applied, and written.

use anyhow::{Context as _, Result};

use crate::cli::Cli;
use crate::color::{ansi, stderr_color, tint};
use crate::{apply, cli, decide, report, roots, session};

use super::write::{WritePass, commit_writes, print_write_summary};
use super::{RootSet, listed_roots, run_scan};

/// Apply a JSON plan of option values read on stdin. Answers with one JSON
/// object on stdout and exits 1 when the plan was not honoured in full, so a
/// script can branch on the status and read the reasons from the report.
pub(crate) fn cmd_decide(cli: &Cli, args: &cli::DecideArgs) -> Result<usize> {
    use std::io::Read as _;

    // The plan is read before anything else: without an explicit port list it
    // is also what says which ports to scan.
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("reading the plan from stdin")?;
    let plan = decide::Plan::parse(&text)?;

    let rs = if args.roots.origins.is_empty() && cli.files.is_empty() {
        RootSet {
            roots: plan.roots().into_iter().map(roots::Listed::port).collect(),
            notes: vec!["no port list given; the plan's own ports are the closure roots".into()],
            installed: false,
        }
    } else {
        listed_roots(&args.roots.origins, &cli.files)?
    };

    let scanned = run_scan(cli, &rs)?;
    let settings = scanned.settings;
    let paint = stderr_color(cli);
    let mut sess = session::Session::new(
        scanned.result.ports,
        scanned.result.aliases,
        &scanned.roots,
        &scanned.files,
        cli.minimal,
    );

    sess.listed_moves = scanned.moves.clone();
    let outcome = decide::apply_plan(&mut sess, &plan);

    // Only the files of the ports the plan named are rewritten, each unless
    // it already says the same (a plan confirming an unconfigured port's
    // defaults writes them); `sync` refreshes the rest. Every closure port
    // sharing such a file is handed to the planner, so the default flavor
    // still owns the write.
    let touched_names: std::collections::BTreeSet<String> = outcome
        .named
        .iter()
        .filter_map(|k| sess.ports.get(k))
        // A flavor make.conf gives a file of its own writes nothing: its
        // OPTIONS_NAME names the shared file it does not read.
        .filter(|i| session::moved(i).is_none())
        .map(|i| i.options_name.clone())
        .collect();
    let staged = sess.ports.iter().filter_map(|(key, info)| {
        if !touched_names.contains(&info.options_name) {
            return None;
        }
        let state = sess.state(info)?;
        Some((key, info, state.staged.clone()))
    });
    let planned = apply::plan_writes(staged, &scanned.files, cli.minimal);

    let mut pass = WritePass::new(planned.writes, planned.removals, planned.warnings);
    pass.rejected = outcome
        .rejected
        .iter()
        .map(|(key, opt, reason)| report::RejectedEntry {
            port: key.to_string(),
            option: opt.clone(),
            reason: reason.clone(),
        })
        .collect();
    pass.unknown = outcome.unknown.iter().map(ToString::to_string).collect();
    // A file that would record a state the port's own constraints forbid is
    // not written, however the plan got there.
    for name in &touched_names {
        for (key, info) in &sess.ports {
            // A flavor reading a file of its own takes no part in the
            // write: its conflict is its own file's.
            if info.options_name != *name || session::moved(info).is_some() {
                continue;
            }
            let violations = sess.violations(info);
            if !violations.is_empty() {
                pass.conflicts.push(report::ConflictEntry {
                    port: key.to_string(),
                    violations,
                });
            }
        }
    }

    // A listed origin MOVED moves is reported, not part of the gate: decide
    // answers for the plan, and a plan naming such an origin not followed
    // finds no port for it (`unknown`), which is.
    let mut report = commit_writes(&pass, &sess.ports, &settings.options_dir, cli.dry_run);
    report.moved = report::moved_entries(&scanned.moves);
    report::print(&report)?;
    for w in &report.warnings {
        eprintln!("{} {w}", tint(paint, ansi::YELLOW, "warning:"));
    }
    for e in &report.errors {
        eprintln!(
            "{} {}: {}",
            tint(paint, ansi::RED, "error:"),
            e.subject,
            e.message
        );
    }
    if pass.blocked() {
        let what = if outcome.complete() {
            "the configuration it produces violates the port's own constraints"
        } else {
            "some values could not be set"
        };
        eprintln!(
            "{} plan not honoured ({what}): {} value(s) refused, {} conflict(s), \
             {} unknown port(s) \u{2014} nothing written",
            tint(paint, ansi::RED, "error:"),
            pass.rejected.len(),
            pass.conflicts.len(),
            pass.unknown.len()
        );
        return Ok(1);
    }
    print_write_summary(&pass, &report, &settings.options_dir, cli.dry_run);
    Ok(if report.summary.failed > 0 { 1 } else { 0 })
}
