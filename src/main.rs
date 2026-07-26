mod apply;
mod cache;
mod clean;
mod cli;
mod config;
mod model;
mod moved;
mod optionsfile;
mod query;
mod session;
mod staging;
mod tui;

use std::io::Write as _;
use std::time::Instant;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};
use crate::model::port::PortStatus;
use crate::moved::Moved;
use crate::optionsfile::SavedOptionsFile;
use crate::query::makerunner::QueryCtx;
use crate::query::scanner::{self, ScanResult};

fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Some(Command::Tui(args)) => {
            let roots = cli::collect_roots(&args.origins, &cli.files)?;
            cmd_tui(&cli, &roots)
        }
        Some(Command::Scan(args)) => {
            let roots = cli::collect_roots(&args.origins, &cli.files)?;
            cmd_scan(&cli, &roots)
        }
        Some(Command::Sync(args)) => {
            let roots = cli::collect_roots(&args.origins, &cli.files)?;
            cmd_sync(&cli, &roots, cli.dry_run)
        }
        Some(Command::Clean(args)) => cmd_clean(&cli, args),
        Some(Command::Origins(raw)) => {
            // Bare origins (default TUI). Tolerate -f/--file mixed in after
            // the first origin, where clap no longer parses flags.
            let (origins, files) = split_raw_origins(raw)?;
            let mut all_files = cli.files.clone();
            all_files.extend(files);
            let roots = cli::collect_roots(&origins, &all_files)?;
            cmd_tui(&cli, &roots)
        }
        None => {
            if cli.files.is_empty() {
                anyhow::bail!(
                    "no ports given; try `optique -z <set> category/port…` or `optique -f pkglist`"
                );
            }
            let roots = cli::collect_roots(&[], &cli.files)?;
            cmd_tui(&cli, &roots)
        }
    }
}

/// Remove obsolete (and optionally redundant) options files from the
/// resolved options dir. Unlike scan/sync this walks the directory itself,
/// not a package list.
fn cmd_clean(cli: &Cli, args: &cli::CleanArgs) -> Result<()> {
    use crate::query::makerunner::{MakeRunner, QueryCtx, ScanEvent};

    let staging = tempfile::tempdir()?;
    let settings = config::resolve(
        cli.tree.as_deref(),
        cli.jail.as_deref(),
        cli.set.as_deref(),
        cli.options_dir.as_deref(),
        staging.path(),
    )?;
    let moved = Moved::load(&settings.portsdir);
    eprintln!("optique clean: options dir {}", settings.options_dir.display());

    let (mut removals, live, warnings) =
        clean::classify_entries(&settings.options_dir, &settings.portsdir, &moved);
    let total_entries = removals.len() + live.len();
    for w in &warnings {
        eprintln!("warning: {w}");
    }

    // Optionally find files that only repeat defaults + make.conf.
    if args.redundant {
        let jobs = cli.jobs.unwrap_or_else(|| {
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16)
        });
        let mut cache = if cli.no_cache {
            cache::Cache::disabled()
        } else {
            let tree_key = cache::tree_key(&settings.portsdir);
            cache::Cache::open(&cache::default_cache_dir(), &tree_key, &settings.conf_hash)
        };
        let ctx = QueryCtx {
            portsdir: settings.portsdir.clone(),
            make_conf: settings.make_conf.clone(),
            port_dbdir: settings.options_dir.clone(),
        };
        let runner = MakeRunner::new(ctx.clone(), jobs);
        let mut by_key: std::collections::HashMap<_, _> =
            live.iter().map(|e| (e.key.clone(), e)).collect();
        let mut in_flight = 0usize;
        let mut done = 0usize;
        let verbose = cli.verbose;
        let mut kept: Vec<(String, String)> = Vec::new();
        let mut handle = |info: model::port::PortInfo,
                          removals: &mut Vec<clean::Removal>,
                          kept: &mut Vec<(String, String)>,
                          by_key: &std::collections::HashMap<_, &clean::LiveEntry>| {
            if let Some(entry) = by_key.get(&info.key) {
                // The verdict below is only valid for the file this port
                // actually reads; a custom/legacy OPTIONS_NAME means the
                // entry belongs to some other port — leave it alone.
                if info.options_name != entry.options_name {
                    eprintln!(
                        "warning: {}: {} uses options name {} — not this entry, left alone",
                        entry.options_name, info.key, info.options_name
                    );
                    return;
                }
                let diff = clean::redundancy_diff(&info);
                if diff.is_empty() {
                    removals.push(clean::Removal {
                        options_name: entry.options_name.clone(),
                        dir: entry.dir.clone(),
                        reason: "redundant: repeats defaults + make.conf".to_string(),
                    });
                } else if verbose {
                    kept.push((
                        entry.options_name.clone(),
                        format!("deviates from defaults + make.conf: {}", diff.join(" ")),
                    ));
                }
            }
        };
        for entry in &live {
            if let Some(info) = cache.lookup(&entry.key, &settings.options_dir) {
                done += 1;
                handle(info, &mut removals, &mut kept, &by_key);
            } else {
                runner.submit(entry.key.clone());
                in_flight += 1;
            }
        }
        while in_flight > 0 {
            match runner.events.recv() {
                Ok(ScanEvent::PortDone(info)) => {
                    in_flight -= 1;
                    done += 1;
                    cache.insert(&info, &settings.options_dir);
                    handle(*info, &mut removals, &mut kept, &by_key);
                }
                Ok(ScanEvent::PortError { key, msg }) => {
                    in_flight -= 1;
                    eprintln!("warning: {key}: query failed, left alone ({msg})");
                    by_key.remove(&key);
                }
                Err(_) => break,
            }
            eprint!("\rchecking… {done}/{} ports", live.len());
            let _ = std::io::stderr().flush();
        }
        if done > 0 {
            eprintln!();
        }
        runner.shutdown();
        removals.sort_by(|a, b| a.options_name.cmp(&b.options_name));
        if cli.verbose {
            kept.sort();
            for (name, why) in &kept {
                println!("keep  {name:<38} {why}");
            }
        }
    }

    if removals.is_empty() {
        eprintln!("nothing to clean ({total_entries} entries kept)");
        return Ok(());
    }
    for r in &removals {
        println!("{:<44} {}", r.options_name, r.reason);
    }
    if cli.dry_run {
        eprintln!(
            "dry run: {} of {total_entries} entries would be removed from {}",
            removals.len(),
            settings.options_dir.display()
        );
        return Ok(());
    }
    let mut removed = 0;
    for r in &removals {
        match clean::remove_entry(r) {
            Ok(note) => {
                removed += 1;
                if let Some(note) = note {
                    eprintln!("note: {note}");
                }
            }
            Err(e) => eprintln!("error: {}: {e}", r.options_name),
        }
    }
    eprintln!("{removed} entry(ies) removed from {}", settings.options_dir.display());
    Ok(())
}

/// Split an external-subcommand argument vector into origins and -f/--file
/// values (clap stops parsing flags once the first bare origin appears).
fn split_raw_origins(raw: &[String]) -> Result<(Vec<String>, Vec<std::path::PathBuf>)> {
    let mut origins = Vec::new();
    let mut files = Vec::new();
    let mut it = raw.iter();
    while let Some(arg) = it.next() {
        if arg == "-f" || arg == "--file" {
            let path = it
                .next()
                .ok_or_else(|| anyhow::anyhow!("{arg} needs a pkglist file argument"))?;
            files.push(path.into());
        } else if let Some(path) = arg.strip_prefix("--file=") {
            files.push(path.into());
        } else if arg.starts_with('-') {
            anyhow::bail!("unexpected flag {arg} after port origins; put flags before the first origin");
        } else {
            origins.push(arg.clone());
        }
    }
    Ok((origins, files))
}

fn cmd_tui(cli: &Cli, roots: &[model::origin::PortKey]) -> Result<()> {
    // Fail before the (possibly minute-long) scan, not after.
    tui::ensure_terminal()?;
    let scanned = run_scan(cli, roots)?;
    let options_dir = scanned.settings.options_dir.clone();
    let session = session::Session::new(
        scanned.result.ports,
        scanned.result.aliases,
        roots,
        &options_dir,
    );

    // Background refreshes query against a staging copy of the options files
    // so staged edits take effect before anything is applied for real.
    let db = staging::StagingDb::create(
        scanned.staging.path(),
        &options_dir,
        session.states.keys(),
    )?;
    let ctx = QueryCtx {
        portsdir: scanned.settings.portsdir.clone(),
        make_conf: scanned.settings.make_conf.clone(),
        port_dbdir: db.path().to_path_buf(),
    };
    let refresher = query::refresher::spawn(ctx, scanned.jobs, scanned.cache, scanned.moved);
    tui::run(session, options_dir, db, refresher)
    // scanned.staging (make.conf + staging db) lives until here
}

/// Shared setup + closure scan used by every subcommand.
struct Scanned {
    settings: config::Settings,
    result: ScanResult,
    elapsed: f32,
    /// Holds the layered make.conf (and the TUI's staging db) alive.
    staging: tempfile::TempDir,
    cache: cache::Cache,
    moved: Moved,
    jobs: usize,
}

fn run_scan(cli: &Cli, roots: &[model::origin::PortKey]) -> Result<Scanned> {
    let staging = tempfile::tempdir()?;
    let settings = config::resolve(
        cli.tree.as_deref(),
        cli.jail.as_deref(),
        cli.set.as_deref(),
        cli.options_dir.as_deref(),
        staging.path(),
    )?;

    let jobs = cli.jobs.unwrap_or_else(|| {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16)
    });

    let mut cache = if cli.no_cache {
        cache::Cache::disabled()
    } else {
        let tree_key = cache::tree_key(&settings.portsdir);
        cache::Cache::open(&cache::default_cache_dir(), &tree_key, &settings.conf_hash)
    };
    let moved = Moved::load(&settings.portsdir);

    let ctx = QueryCtx {
        portsdir: settings.portsdir.clone(),
        make_conf: settings.make_conf.clone(),
        port_dbdir: settings.options_dir.clone(),
    };

    eprintln!("optique: ports tree {} · {} jobs", settings.portsdir.display(), jobs);
    eprintln!(
        "  options dir: {}{}",
        settings.options_dir.display(),
        if settings.options_dir_is_new { " (new, created on apply)" } else { "" }
    );
    if settings.make_conf_sources.is_empty() {
        eprintln!("  make.conf:   (none)");
    } else {
        for (i, src) in settings.make_conf_sources.iter().enumerate() {
            eprintln!(
                "  {}   {}",
                if i == 0 { "make.conf:" } else { "          " },
                src.display()
            );
        }
    }

    let t0 = Instant::now();
    let result = scanner::scan(roots, &ctx, jobs, &mut cache, &moved, |p| {
        eprint!("\rscanning… {}/{} ports ({} cached)", p.done, p.discovered, p.from_cache);
        let _ = std::io::stderr().flush();
    });
    eprintln!();

    for note in &result.moved_notes {
        eprintln!("moved: {note}");
    }
    for (key, msg) in &result.errors {
        eprintln!("error: {key}: {msg}");
    }

    Ok(Scanned {
        settings,
        result,
        elapsed: t0.elapsed().as_secs_f32(),
        staging,
        cache,
        moved,
        jobs,
    })
}

fn cmd_scan(cli: &Cli, roots: &[model::origin::PortKey]) -> Result<()> {
    let scanned = run_scan(cli, roots)?;
    let (settings, result) = (&scanned.settings, &scanned.result);

    struct Row {
        key: String,
        pkgname: String,
        status: PortStatus,
        undecided: Vec<String>,
        state: String,
        warnings: Vec<String>,
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut hidden = 0usize;
    for (key, info) in &result.ports {
        if !info.options.has_options() {
            hidden += 1;
            continue;
        }
        let saved =
            SavedOptionsFile::load(&settings.options_dir.join(&info.options_name).join("options"));
        let undecided = session::undecided_options(info, saved.as_ref());
        let state = if cli.verbose {
            info.options
                .complete
                .iter()
                .map(|o| {
                    if info.options.effective.contains(o) { format!("+{o}") } else { format!("-{o}") }
                })
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            String::new()
        };
        rows.push(Row {
            key: key.to_string(),
            pkgname: info.pkgname.clone(),
            status: info.status(saved.as_ref()),
            undecided,
            state,
            warnings: if cli.verbose { info.warnings.clone() } else { Vec::new() },
        });
    }
    rows.sort_by_key(|r| (matches!(r.status, PortStatus::Ok), r.key.clone()));

    let mut unconfigured = 0;
    let mut stale = 0;
    for row in &rows {
        let (key, pkgname) = (&row.key, &row.pkgname);
        let decision = if row.undecided.is_empty() {
            " [mc-covered ≈]".to_string()
        } else {
            format!(" undecided: {}", row.undecided.join(" "))
        };
        match &row.status {
            PortStatus::Unconfigured => {
                unconfigured += 1;
                println!("?  {key:<40} {pkgname:<32} UNCONFIGURED{decision}");
            }
            PortStatus::Stale { added, removed } => {
                stale += 1;
                let mut detail = String::new();
                if !added.is_empty() {
                    detail.push_str(&format!(" +{}", added.join(" +")));
                }
                if !removed.is_empty() {
                    detail.push_str(&format!(" -{}", removed.join(" -")));
                }
                println!("!  {key:<40} {pkgname:<32} STALE{detail}{decision}");
            }
            PortStatus::Ok => println!("   {key:<40} {pkgname:<32} ok"),
        }
        if cli.verbose {
            if !row.state.is_empty() {
                println!("     options: {}", row.state);
            }
            for w in &row.warnings {
                println!("     warning: {w}");
            }
        }
    }

    eprintln!(
        "{} ports with options ({} unconfigured, {} stale) · {} without options · \
         {} queried, {} cached · {:.1}s",
        rows.len(),
        unconfigured,
        stale,
        hidden,
        result.queried,
        result.from_cache,
        scanned.elapsed
    );
    Ok(())
}

fn cmd_sync(cli: &Cli, roots: &[model::origin::PortKey], dry_run: bool) -> Result<()> {
    let scanned = run_scan(cli, roots)?;
    let (settings, result) = (&scanned.settings, &scanned.result);

    let staged = result.ports.iter().map(|(key, info)| {
        let saved =
            SavedOptionsFile::load(&settings.options_dir.join(&info.options_name).join("options"));
        (key, info, apply::sync_enabled_set(info, saved.as_ref()))
    });
    let planned = apply::plan_writes(staged, &settings.options_dir);
    for w in &planned.warnings {
        eprintln!("warning: {w}");
    }
    let writes = planned.writes;

    if writes.is_empty() {
        eprintln!("everything up to date, nothing to write");
        return Ok(());
    }
    for w in &writes {
        println!("{}  {}", w.key, w.describe());
        if cli.verbose {
            let state = w
                .complete
                .iter()
                .map(|o| if w.enabled.contains(o) { format!("+{o}") } else { format!("-{o}") })
                .collect::<Vec<_>>()
                .join(" ");
            println!("     final: {state}");
        }
    }
    if dry_run {
        eprintln!("dry run: {} file(s) would be written to {}", writes.len(), settings.options_dir.display());
        return Ok(());
    }
    let summary = apply::apply(&writes);
    for (key, msg) in &summary.failed {
        eprintln!("error: {key}: {msg}");
    }
    eprintln!(
        "{} file(s) written to {}{}",
        summary.written,
        settings.options_dir.display(),
        if summary.failed.is_empty() { String::new() } else { format!(", {} failed", summary.failed.len()) }
    );
    Ok(())
}
