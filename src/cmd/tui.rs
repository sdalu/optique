//! The TUI over a fresh scan, or driven headless.

use anyhow::Result;

use crate::cli::Cli;
use crate::color::{ansi, stderr_color, tint};
use crate::{query, session, staging, tui};

use super::{RootSet, query_ctx, run_scan};

pub(crate) fn cmd_tui(cli: &Cli, rs: &RootSet, drive: bool) -> Result<()> {
    if cli.dry_run {
        eprintln!(
            "{} --dry-run has no effect in the TUI; the apply dialog previews changes",
            tint(stderr_color(cli), ansi::YELLOW, "note:")
        );
    }
    // Fail before the (possibly minute-long) scan, not after. The headless
    // driver renders into memory, so it has no use for a terminal.
    if !drive {
        tui::ensure_terminal()?;
    }
    let scanned = run_scan(cli, rs)?;
    let mut session = session::Session::new(
        scanned.result.ports,
        scanned.result.aliases,
        &scanned.roots,
        &scanned.files,
        cli.minimal,
    );
    session.blacklist = scanned.settings.blacklist.clone();
    session.listed_moves = scanned.moves;

    // Background refreshes query against a staging copy of the options files
    // so staged edits take effect before anything is applied for real.
    let db = staging::StagingDb::create(
        scanned.staging.path(),
        &scanned.files,
        session.states.keys(),
    )?;
    let ctx = query_ctx(&scanned.settings, db.path().to_path_buf());
    let refresher = query::refresher::spawn(
        ctx,
        scanned.jobs,
        scanned.cache,
        scanned.moved,
        scanned.settings.blacklist.clone(),
    );
    let blacklist = scanned.settings.blacklist;
    let overlays = scanned.settings.overlays;
    let files = scanned.files;
    if drive {
        tui::run_driver(
            session,
            files,
            db,
            refresher,
            blacklist,
            overlays,
            cli.minimal,
        )
    } else {
        tui::run(
            session,
            files,
            db,
            refresher,
            blacklist,
            overlays,
            cli.minimal,
        )
    }
    // scanned.staging (make.conf + staging db) lives until here
}
