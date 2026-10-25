//! `optique sync`: every pending file written as its defaults and make.conf decide.

use anyhow::Result;

use crate::cli::Cli;
use crate::color::{ansi, stderr_color, tint};
use crate::{apply, cli, report, session};

use super::write::{WritePass, commit_writes, print_write_summary};
use super::{RootSet, run_scan};

pub(crate) fn cmd_sync(cli: &Cli, rs: &RootSet, args: &cli::SyncArgs) -> Result<()> {
    let (dry_run, json) = (cli.dry_run, args.json);
    let scanned = run_scan(cli, rs)?;
    let (settings, result) = (&scanned.settings, &scanned.result);
    let paint = stderr_color(cli);

    let files = &scanned.files;
    let staged = result.ports.iter().map(|(key, info)| {
        let saved = files.saved(&info.options_name);
        (key, info, apply::sync_enabled_set(info, saved.as_ref()))
    });
    let planned = apply::plan_writes(staged, files, cli.minimal);

    // A port that lost ALL its options never reaches plan_writes; its
    // leftover file is dead configuration and must go too (unless another
    // flavor sharing the file still has options). --minimal adds files whose
    // content defaults + make.conf already dictate.
    let mut stale_files = apply::plan_stale_removals(&result.ports, files);
    stale_files.extend(planned.removals);
    stale_files.sort_by(|a, b| a.options_name.cmp(&b.options_name));

    let pass = WritePass::new(planned.writes, stale_files, planned.warnings);
    for w in &pass.warnings {
        errln!("{} {w}", tint(paint, ansi::YELLOW, "warning:"));
    }

    if pass.nothing_to_do() && !json {
        errln!("everything up to date, nothing to write");
        return Ok(());
    }
    // stdout carries either the listing or the JSON object, never both.
    if !json && !cli.quiet {
        for r in &pass.removals {
            outln!("{}  removing options file ({})", r.options_name, r.reason)?;
        }
        for w in &pass.writes {
            outln!("{}  {}", w.key, w.describe())?;
            if cli.verbose {
                outln!(
                    "     final: {}",
                    session::signed_options(&w.complete, &w.enabled)
                )?;
            }
        }
    }

    let mut report = commit_writes(&pass, &result.ports, &settings.options_dir, dry_run);
    report.moved = report::moved_entries(&scanned.moves);
    if json {
        report::print(&report)?;
    }
    for e in &report.errors {
        errln!(
            "{} {}: {}",
            tint(paint, ansi::RED, "error:"),
            e.subject,
            e.message
        );
    }
    print_write_summary(&pass, &report, &settings.options_dir, dry_run);
    Ok(())
}
