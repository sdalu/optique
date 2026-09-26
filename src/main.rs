mod apply;
mod cache;
mod clean;
mod cli;
mod config;
mod decide;
mod deps;
mod draft;
mod model;
mod moved;
mod optionsfile;
mod query;
mod report;
mod session;
mod staging;
mod tui;

use std::io::Write as _;
use std::time::Instant;

use anyhow::{Context as _, Result};
use clap::Parser;

use crate::cli::{Cli, Command};
use crate::moved::Moved;
use crate::optionsfile::SavedOptionsFile;
use crate::query::makerunner::QueryCtx;
use crate::query::scanner::{self, ScanResult};

fn main() -> Result<()> {
    let cli = Cli::parse_from(cli::disambiguate_synth(std::env::args_os()));
    if cli.clear_cache {
        let dir = cache::default_cache_dir();
        let (files, bytes) = cache::clear(&dir);
        eprintln!("cache cleared: {files} file(s), {} KiB in {}", bytes / 1024, dir.display());
        // Given alone, clearing IS the action.
        if cli.command.is_none() && cli.files.is_empty() {
            return Ok(());
        }
    }
    match &cli.command {
        Some(Command::Tui(args)) => {
            let rs = roots_or_installed(&cli, &args.roots.origins)?;
            cmd_tui(&cli, &rs, args.drive)
        }
        Some(Command::Scan(args)) => {
            let rs = roots_or_installed(&cli, &args.roots.origins)?;
            // Exit code is the cron/CI gate: 1 = decisions pending, 0 = clean.
            // Real errors keep travelling up the anyhow path (nonzero, 1 from
            // clap's runner) — only the *clean* run may return 0.
            let attention = cmd_scan(&cli, &rs, args.json, args.options)?;
            if attention > 0 {
                let _ = std::io::stdout().flush();
                let _ = std::io::stderr().flush();
                std::process::exit(1);
            }
            Ok(())
        }
        Some(Command::Sync(args)) => {
            let rs = roots_or_installed(&cli, &args.roots.origins)?;
            cmd_sync(&cli, &rs, cli.dry_run, args.json)
        }
        Some(Command::Decide(args)) => {
            // Same gate contract as scan: 1 means "not done", not "broke".
            let code = cmd_decide(&cli, args)?;
            if code != 0 {
                let _ = std::io::stdout().flush();
                let _ = std::io::stderr().flush();
                std::process::exit(1);
            }
            Ok(())
        }
        Some(Command::Clean(args)) => cmd_clean(&cli, args),
        Some(Command::Origins(raw)) => {
            // Bare origins (default TUI). Tolerate -f/--file mixed in after
            // the first origin, where clap no longer parses flags.
            let (origins, files) = split_raw_origins(raw)?;
            let mut all_files = cli.files.clone();
            all_files.extend(files);
            let rs = RootSet { roots: cli::collect_roots(&origins, &all_files)?, notes: Vec::new() };
            cmd_tui(&cli, &rs, false)
        }
        None => {
            if cli.files.is_empty() && cli.synth.is_none() {
                anyhow::bail!(
                    "no ports given; try `optique -z <set> category/port…` or `optique -f pkglist`"
                );
            }
            let rs = roots_or_installed(&cli, &[])?;
            cmd_tui(&cli, &rs, false)
        }
    }
}

/// Everything the cleaning pass needs, whichever path assembled it: plain
/// `clean` resolves the settings on its own, `clean --unused` gets them from
/// the closure scan it has to run first.
struct CleanCtx {
    settings: config::Settings,
    moved: Moved,
    jobs: usize,
    /// An already-open query cache (the --unused path reuses the scan's).
    cache: Option<cache::Cache>,
    /// OPTIONS_NAMEs reached by the given list's closure; None without --unused.
    used: Option<std::collections::HashSet<String>>,
    /// Holds the layered make.conf alive until the pass is done.
    _staging: tempfile::TempDir,
}

/// Remove obsolete (and optionally redundant or unused) options files from
/// the resolved options dir. Unlike scan/sync this walks the directory
/// itself; a package list is only consulted for --unused.
fn cmd_clean(cli: &Cli, args: &cli::CleanArgs) -> Result<()> {
    if cli.minimal && !args.redundant {
        eprintln!("{} --minimal does not affect clean; its counterpart here is --redundant", tint(stderr_color(cli), ansi::YELLOW, "note:"));
    }
    let has_list = !args.origins.is_empty() || !cli.files.is_empty();
    // In synth mode the installed packages are the implicit list.
    if args.unused && !has_list && cli.synth.is_none() {
        anyhow::bail!(
            "clean --unused needs a package list to compare against: \
             give port origins or -f pkglist"
        );
    }
    if !args.unused && has_list {
        anyhow::bail!(
            "clean walks the whole options dir; port origins and -f pkglist \
             only mean something together with --unused"
        );
    }

    if !args.unused {
        let staging = tempfile::tempdir()?;
        let settings = config::resolve(
            cli.tree.as_deref(),
            cli.jail.as_deref(),
            cli.set.as_deref(),
            cli.synth.as_deref(),
            cli.options_dir.as_deref(),
            staging.path(),
        )?;
        let moved = Moved::load(&settings.portsdir);
        let ctx = CleanCtx {
            settings,
            moved,
            jobs: default_jobs(cli),
            cache: None,
            used: None,
            _staging: staging,
        };
        return clean_options_dir(cli, args, ctx);
    }

    // --unused: only the closure of the given list justifies keeping an entry,
    // so the closure has to be resolved first — settings, cache, MOVED and the
    // job count are then reused for the cleaning pass itself.
    let rs = roots_or_installed(cli, &args.origins)?;
    let scanned = run_scan(cli, &rs)?;
    if !scanned.result.errors.is_empty() {
        // A port that failed to query is absent from the closure and would be
        // pruned as unused — refuse rather than delete on partial knowledge.
        anyhow::bail!(
            "{} port(s) failed to query: the dependency closure is incomplete, \
             refusing to prune entries",
            scanned.result.errors.len()
        );
    }
    let used = scanned.result.ports.values().map(|i| i.options_name.clone()).collect();
    let ctx = CleanCtx {
        settings: scanned.settings,
        moved: scanned.moved,
        jobs: scanned.jobs,
        cache: Some(scanned.cache),
        used: Some(used),
        _staging: scanned.staging,
    };
    clean_options_dir(cli, args, ctx)
}

fn clean_options_dir(cli: &Cli, args: &cli::CleanArgs, ctx: CleanCtx) -> Result<()> {
    use crate::query::makerunner::{MakeRunner, QueryCtx, ScanEvent};

    let CleanCtx { settings, moved, jobs, cache: open_cache, used, _staging } = ctx;
    let paint = stderr_color(cli);
    eprintln!("{} options dir {}", tint(paint, ansi::BOLD, "optique clean:"), settings.options_dir.display());
    for note in &settings.notes {
        eprintln!("  {}        {note}", tint(paint, ansi::YELLOW, "note:"));
    }

    let (mut removals, live, mut warnings) =
        clean::classify_entries(&settings.options_dir, &settings.portsdir, &moved);
    let total_entries = removals.len() + live.len();
    for w in &warnings {
        eprintln!("{} {w}", tint(paint, ansi::YELLOW, "warning:"));
    }
    // Only --verbose fills this in; JSON reports it as `kept`.
    let mut kept_entries: Vec<(String, String)> = Vec::new();

    // Entries nobody in the closure reads go; the redundancy pass below then
    // only has to look at what --unused still keeps.
    let live = match &used {
        Some(used) => {
            let (kept, unused) = clean::split_unused(live, used);
            removals.extend(unused);
            kept
        }
        None => live,
    };

    // Optionally find files that only repeat defaults + make.conf.
    if args.redundant {
        let mut cache = open_cache.unwrap_or_else(|| new_cache(cli, &settings));
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
        let handle = |info: model::port::PortInfo,
                          removals: &mut Vec<clean::Removal>,
                          kept: &mut Vec<(String, String)>,
                          notes: &mut Vec<String>,
                          by_key: &std::collections::HashMap<_, &clean::LiveEntry>| {
            if let Some(entry) = by_key.get(&info.key) {
                // The verdict below is only valid for the file this port
                // actually reads; a custom/legacy OPTIONS_NAME means the
                // entry belongs to some other port — leave it alone.
                if info.options_name != entry.options_name {
                    let note = format!(
                        "{}: {} uses options name {} — not this entry, left alone",
                        entry.options_name, info.key, info.options_name
                    );
                    eprintln!("warning: {note}");
                    notes.push(note);
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
                handle(info, &mut removals, &mut kept, &mut warnings, &by_key);
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
                    handle(*info, &mut removals, &mut kept, &mut warnings, &by_key);
                }
                Ok(ScanEvent::PortError { key, msg }) => {
                    in_flight -= 1;
                    eprintln!("{} {key}: query failed, left alone ({msg})", tint(paint, ansi::YELLOW, "warning:"));
                    warnings.push(format!("{key}: query failed, left alone ({msg})"));
                    by_key.remove(&key);
                }
                Err(_) => break,
            }
            eprint!("\r{}", tint(paint, ansi::GRAY, &format!("checking… {done}/{} ports", live.len())));
            let _ = std::io::stderr().flush();
        }
        if done > 0 {
            eprintln!();
        }
        runner.shutdown();
        if cli.verbose && !cli.quiet && !args.json {
            kept.sort();
            for (name, why) in &kept {
                println!("keep  {name:<38} {why}");
            }
        }
        kept.sort();
        kept_entries = kept;
    }

    // Obsolete, unused and redundant removals were collected separately.
    removals.sort_by(|a, b| a.options_name.cmp(&b.options_name));

    // stdout carries either the listing or the JSON object, never both.
    if !args.json && !cli.quiet {
        for r in &removals {
            println!("{:<44} {}", r.options_name, r.reason);
        }
    }

    let mut errors: Vec<report::ErrorEntry> = Vec::new();
    let mut removed = 0usize;
    if !cli.dry_run {
        for r in &removals {
            match clean::remove_entry(r) {
                Ok(note) => {
                    removed += 1;
                    if let Some(note) = note {
                        eprintln!("{} {note}", tint(paint, ansi::YELLOW, "note:"));
                        warnings.push(note);
                    }
                }
                Err(e) => {
                    eprintln!("{} {}: {e}", tint(paint, ansi::RED, "error:"), r.options_name);
                    errors.push(report::ErrorEntry {
                        subject: r.options_name.clone(),
                        message: e.to_string(),
                    });
                }
            }
        }
    }

    if args.json {
        report::print(&report::CleanReport {
            options_dir: settings.options_dir.display().to_string(),
            dry_run: cli.dry_run,
            removals: removals
                .iter()
                .map(|r| report::RemovalEntry {
                    options_name: r.options_name.clone(),
                    reason: r.reason.clone(),
                })
                .collect(),
            kept: kept_entries
                .iter()
                .map(|(options_name, reason)| report::KeptEntry {
                    options_name: options_name.clone(),
                    reason: reason.clone(),
                })
                .collect(),
            warnings,
            summary: report::CleanSummary {
                entries: total_entries,
                removed,
                failed: errors.len(),
            },
            errors,
        })?;
    }

    if removals.is_empty() {
        eprintln!("nothing to clean ({total_entries} entries kept)");
    } else if cli.dry_run {
        eprintln!(
            "dry run: {} of {total_entries} entries would be removed from {}",
            removals.len(),
            settings.options_dir.display()
        );
    } else {
        eprintln!("{removed} entry(ies) removed from {}", settings.options_dir.display());
    }
    Ok(())
}

/// Origins of everything installed, from pkg(8) — synth's natural root set
/// when no list is given (synth builds what is installed). Flavors come from
/// the pkg "flavor" annotation; `repo` restricts to packages installed from
/// that pkg repository (%R).
fn installed_roots(repos: &[String]) -> Result<Vec<model::origin::PortKey>> {
    let origins = std::process::Command::new("pkg")
        .args(["query", "-a", "%o\t%R"])
        .output()
        .map_err(|e| anyhow::anyhow!("cannot run pkg query: {e}"))?;
    if !origins.status.success() {
        anyhow::bail!("pkg query failed: {}", String::from_utf8_lossy(&origins.stderr).trim());
    }
    // One line per annotation; only the "flavor" ones matter.
    let annots = std::process::Command::new("pkg")
        .args(["query", "-a", "%o\t%At\t%Av"])
        .output()
        .map_err(|e| anyhow::anyhow!("cannot run pkg query: {e}"))?;
    let roots = parse_installed(
        &String::from_utf8_lossy(&origins.stdout),
        &String::from_utf8_lossy(&annots.stdout),
        repos,
    );
    if roots.is_empty() {
        if repos.is_empty() {
            anyhow::bail!("no installed packages with port origins found (pkg query -a %o)");
        }
        anyhow::bail!(
            "no installed packages from {} '{}' \
             (check the names with: pkg query -a %R | sort -u)",
            if repos.len() == 1 { "repository" } else { "repositories" },
            repos.join("', '")
        );
    }
    Ok(roots)
}

/// Pure part of installed_roots: `origins` lines are `origin\trepository`,
/// `annots` lines are `origin\tannotation\tvalue`. Unparsable origins are
/// skipped, duplicates collapse, the result is sorted.
fn parse_installed(
    origins: &str,
    annots: &str,
    repos: &[String],
) -> Vec<model::origin::PortKey> {
    let mut flavor: std::collections::HashMap<&str, &str> = Default::default();
    for line in annots.lines() {
        let mut f = line.split('\t');
        if let (Some(origin), Some("flavor"), Some(value)) = (f.next(), f.next(), f.next()) {
            flavor.insert(origin, value);
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut roots = Vec::new();
    for line in origins.lines() {
        let mut f = line.split('\t');
        let (Some(origin), pkg_repo) = (f.next(), f.next()) else { continue };
        let origin = origin.trim();
        if origin.is_empty() || !seen.insert(origin.to_string()) {
            continue;
        }
        if !repos.is_empty() {
            let from = pkg_repo.map(str::trim).unwrap_or("");
            if !repos.iter().any(|r| r == from) {
                continue;
            }
        }
        let spec = match flavor.get(origin) {
            Some(fl) => format!("{origin}@{fl}"),
            None => origin.to_string(),
        };
        // Packages not built from ports may carry unparsable origins: skip.
        if let Some(key) = model::origin::PortKey::parse(&spec) {
            roots.push(key);
        }
    }
    roots.sort();
    roots
}

/// The resolved root list plus notes destined for the startup banner's
/// aligned `note:` block (nothing is printed here).
struct RootSet {
    roots: Vec<model::origin::PortKey>,
    notes: Vec<String>,
}

/// Roots for a subcommand: the given list, or — in synth mode only — the
/// installed packages when nothing was given.
fn roots_or_installed(cli: &Cli, origins: &[String]) -> Result<RootSet> {
    if origins.is_empty() && cli.files.is_empty() && cli.synth.is_some() {
        let roots = installed_roots(&cli.repo)?;
        let note = if cli.repo.is_empty() {
            format!("no ports given; using {} installed package(s) as the list", roots.len())
        } else {
            format!(
                "no ports given; using {} installed package(s) from {} '{}'",
                roots.len(),
                if cli.repo.len() == 1 { "repository" } else { "repositories" },
                cli.repo.join("', '")
            )
        };
        return Ok(RootSet { roots, notes: vec![note] });
    }
    if !cli.repo.is_empty() {
        anyhow::bail!(
            "--repo filters the installed-package list, which is only used when \
             no ports are given"
        );
    }
    Ok(RootSet { roots: cli::collect_roots(origins, &cli.files)?, notes: Vec::new() })
}

/// Split an external-subcommand argument vector into origins and -f/--file
/// values (clap stops parsing flags once the first bare origin appears).
pub(crate) fn split_raw_origins(
    raw: &[String],
) -> Result<(Vec<String>, Vec<std::path::PathBuf>)> {
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

fn cmd_tui(cli: &Cli, rs: &RootSet, drive: bool) -> Result<()> {
    if cli.dry_run {
        eprintln!("{} --dry-run has no effect in the TUI; the apply dialog previews changes", tint(stderr_color(cli), ansi::YELLOW, "note:"));
    }
    // Fail before the (possibly minute-long) scan, not after. The headless
    // driver renders into memory, so it has no use for a terminal.
    if !drive {
        tui::ensure_terminal()?;
    }
    let scanned = run_scan(cli, rs)?;
    let options_dir = scanned.settings.options_dir.clone();
    let session = session::Session::new(
        scanned.result.ports,
        scanned.result.aliases,
        &rs.roots,
        &options_dir,
        cli.minimal,
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
    let blacklist = scanned.settings.blacklist;
    if drive {
        tui::run_driver(session, options_dir, db, refresher, blacklist, cli.minimal)
    } else {
        tui::run(session, options_dir, db, refresher, blacklist, cli.minimal)
    }
    // scanned.staging (make.conf + staging db) lives until here
}

/// Shared setup + closure scan used by every subcommand.
struct Scanned {
    settings: config::Settings,
    result: ScanResult,
    /// Dependency loops found in the scanned closure (already reported on
    /// stderr by `run_scan`; kept for the table, the JSON and the gate).
    dep_loops: Vec<deps::DepLoop>,
    elapsed: f32,
    /// Holds the layered make.conf (and the TUI's staging db) alive.
    staging: tempfile::TempDir,
    cache: cache::Cache,
    moved: Moved,
    jobs: usize,
}

/// Parallel `make` jobs: -J, else min(16, ncpu).
fn default_jobs(cli: &Cli) -> usize {
    cli.jobs.unwrap_or_else(|| {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16)
    })
}

/// Open the persistent query cache for this tree + make.conf pair (or a
/// no-op cache under --no-cache).
fn new_cache(cli: &Cli, settings: &config::Settings) -> cache::Cache {
    if cli.no_cache {
        cache::Cache::disabled()
    } else {
        let tree_key = cache::tree_key(&settings.portsdir);
        cache::Cache::open(&cache::default_cache_dir(), &tree_key, &settings.conf_hash)
    }
}

fn run_scan(cli: &Cli, rs: &RootSet) -> Result<Scanned> {
    let staging = tempfile::tempdir()?;
    let settings = config::resolve(
        cli.tree.as_deref(),
        cli.jail.as_deref(),
        cli.set.as_deref(),
        cli.synth.as_deref(),
        cli.options_dir.as_deref(),
        staging.path(),
    )?;

    let jobs = default_jobs(cli);
    let mut cache = new_cache(cli, &settings);
    let moved = Moved::load(&settings.portsdir);

    let ctx = QueryCtx {
        portsdir: settings.portsdir.clone(),
        make_conf: settings.make_conf.clone(),
        port_dbdir: settings.options_dir.clone(),
    };

    let paint = stderr_color(cli);
    if !cli.quiet {
        eprintln!(
            "{} ports tree {} · {} jobs",
            tint(paint, ansi::BOLD, "optique:"),
            settings.portsdir.display(),
            jobs
        );
        for note in rs.notes.iter().chain(settings.notes.iter()) {
            eprintln!("  {}        {note}", tint(paint, ansi::YELLOW, "note:"));
        }
        eprintln!(
            "  {} {}{}",
            tint(paint, ansi::CYAN, "options dir:"),
            settings.options_dir.display(),
            tint(
                paint,
                ansi::YELLOW,
                if settings.options_dir_is_new { " (new, created on apply)" } else { "" }
            )
        );
        if settings.make_conf_sources.is_empty() {
            eprintln!("  {}   (none)", tint(paint, ansi::CYAN, "make.conf:"));
        } else {
            for (i, src) in settings.make_conf_sources.iter().enumerate() {
                let label = if i == 0 { "make.conf:" } else { "          " };
                eprintln!("  {}   {}", tint(paint, ansi::CYAN, label), src.display());
            }
        }
        if !settings.blacklist.is_empty() {
            for (i, src) in settings.blacklist.sources.iter().enumerate() {
                let label = if i == 0 { "blacklist:" } else { "          " };
                eprintln!("  {}   {}", tint(paint, ansi::CYAN, label), src.display());
            }
        }
    }

    let t0 = Instant::now();
    let result = scanner::scan(&rs.roots, &ctx, jobs, &mut cache, &moved, move |p| {
        let line = format!("scanning… {}/{} ports ({} cached)", p.done, p.discovered, p.from_cache);
        eprint!("\r{}", tint(paint, ansi::GRAY, &line));
        let _ = std::io::stderr().flush();
    });
    eprintln!();

    for note in &result.moved_notes {
        eprintln!("{} {note}", tint(paint, ansi::MAGENTA, "moved:"));
    }
    for (key, msg) in &result.errors {
        eprintln!("{} {key}: {msg}", tint(paint, ansi::RED, "error:"));
    }

    // A dependency loop is a property of the closure, not of one command:
    // whatever the caller went on to do, poudriere will not build it.
    let dep_loops = deps::detect(&result.ports, &result.aliases);
    for dl in &dep_loops {
        eprintln!("{} {}", tint(paint, ansi::RED, "loop:"), dl.render());
        if let Some(extra) = dl.extra_line() {
            eprintln!("       also tangled: {extra}");
        }
    }

    Ok(Scanned {
        settings,
        result,
        dep_loops,
        elapsed: t0.elapsed().as_secs_f32(),
        staging,
        cache,
        moved,
        jobs,
    })
}

/// SGR escapes for the scan marker column, matching the TUI's marker colors.
/// A handful of constants beats a dependency for four markers.
mod ansi {
    pub const RESET: &str = "\x1b[0m";
    pub const RED: &str = "\x1b[31m";
    pub const LIGHT_RED: &str = "\x1b[91m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const GRAY: &str = "\x1b[90m";
    pub const CYAN: &str = "\x1b[36m";
    pub const MAGENTA: &str = "\x1b[35m";
    pub const BOLD: &str = "\x1b[1m";
}

/// Should the informational output on stderr get colors? Same policy as
/// stdout (--color / NO_COLOR), judged against stderr's tty-ness.
fn stderr_color(cli: &Cli) -> bool {
    use std::io::IsTerminal as _;
    cli::use_color(
        cli.color,
        std::io::stderr().is_terminal(),
        std::env::var("NO_COLOR").ok().as_deref(),
    )
}

/// Wrap `text` in a color when painting is enabled.
fn tint(on: bool, color: &str, text: &str) -> String {
    if on && !text.is_empty() {
        format!("{color}{text}{}", ansi::RESET)
    } else {
        text.to_string()
    }
}

/// Color for a scan status marker; None for the unmarked ok rows.
fn marker_color(marker: &str) -> Option<&'static str> {
    match marker {
        "?" => Some(ansi::YELLOW),      // unconfigured
        "!" => Some(ansi::LIGHT_RED),   // stale
        "✗" => Some(ansi::RED),         // conflict
        "⊘" => Some(ansi::GRAY),       // blacklisted
        _ => None,
    }
}

/// Is stdout allowed ANSI color? Padding is applied inside the escapes by the
/// caller, so column alignment is unaffected either way.
fn stdout_color(cli: &Cli) -> bool {
    use std::io::IsTerminal as _;
    let no_color = std::env::var("NO_COLOR").ok();
    cli::use_color(cli.color, std::io::stdout().is_terminal(), no_color.as_deref())
}

/// Scan and report. Returns how many *human* decisions are pending — ports
/// needing one (see `Row::needs_attention`) plus dependency loops that are
/// not entirely blacklisted — which main turns into exit code 1.
fn cmd_scan(cli: &Cli, rs: &RootSet, json: bool, with_options: bool) -> Result<usize> {
    use crate::session::UiStatus;

    let scanned = run_scan(cli, rs)?;
    let (queried, from_cache, elapsed) =
        (scanned.result.queried, scanned.result.from_cache, scanned.elapsed);
    let settings = scanned.settings;
    let dep_loops = scanned.dep_loops;
    // Session gives owner-aware statuses (flavors sharing an options file
    // are judged against the default flavor's view).
    let sess = session::Session::new(
        scanned.result.ports,
        scanned.result.aliases,
        &rs.roots,
        &settings.options_dir,
        cli.minimal,
    );

    struct Row {
        key: String,
        pkgname: String,
        status: UiStatus,
        /// Options the tree gained since the file was written (stale only).
        added: Vec<String>,
        /// Options the tree lost since then (stale only).
        removed: Vec<String>,
        undecided: Vec<String>,
        state: String,
        warnings: Vec<String>,
        /// Blacklisted for this jail/tree/set: poudriere would never build it.
        blacklisted: bool,
        /// Tangled in a dependency loop (the loop itself is reported whole,
        /// on stderr and in the JSON; this only marks the row).
        in_loop: bool,
        /// OPTIONS_NAME, i.e. the options-dir entry this port reads.
        options_name: String,
        /// Only under `--options`: what a caller needs to decide a value.
        detail: Option<PortDetail>,
    }
    /// The `--options` payload of one port.
    struct PortDetail {
        violations: Vec<String>,
        options: Vec<report::OptionReport>,
    }
    impl Row {
        /// make.conf already dictates every option this port still owes an
        /// answer for, so no human has to decide anything.
        fn mc_covered(&self) -> bool {
            self.undecided.is_empty()
        }
        /// Does this row make `scan` exit 1? A conflict always does (saved
        /// options violate the port's own constraints); missing or outdated
        /// configuration only does when make.conf leaves something open.
        /// Blacklisted ports never do — they are never built here.
        fn needs_attention(&self) -> bool {
            if self.blacklisted {
                return false;
            }
            match self.status {
                UiStatus::Conflict => true,
                UiStatus::Unconfigured | UiStatus::Stale => !self.mc_covered(),
                _ => false,
            }
        }
        fn status_str(&self) -> &'static str {
            match self.status {
                UiStatus::Conflict => "conflict",
                UiStatus::Unconfigured => "unconfigured",
                UiStatus::Stale => "stale",
                // Edited/McDeviation need staged edits, which a scan never has.
                _ => "ok",
            }
        }
        /// The " +NEW -GONE" tail printed after STALE.
        fn stale_detail(&self) -> String {
            let mut d = String::new();
            for o in &self.added {
                d.push_str(&format!(" +{o}"));
            }
            for o in &self.removed {
                d.push_str(&format!(" -{o}"));
            }
            d
        }
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut hidden = 0usize;
    for (key, info) in &sess.ports {
        if !info.options.has_options() {
            hidden += 1;
            continue;
        }
        let saved = sess.state(info).and_then(|s| s.saved.as_ref());
        let status = sess.status(info);
        let (added, removed) = if status == UiStatus::Stale {
            let owner = sess.owner_info(info);
            let cur: std::collections::BTreeSet<&str> =
                owner.options.complete.iter().map(String::as_str).collect();
            let was: std::collections::BTreeSet<&str> = saved
                .map(|s| s.complete.iter().map(String::as_str).collect())
                .unwrap_or_default();
            (
                cur.difference(&was).map(|o| o.to_string()).collect(),
                was.difference(&cur).map(|o| o.to_string()).collect(),
            )
        } else {
            (Vec::new(), Vec::new())
        };
        let undecided = session::undecided_options(info, saved, cli.minimal);
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
            status,
            added,
            removed,
            undecided,
            state,
            warnings: if cli.verbose { info.warnings.clone() } else { Vec::new() },
            blacklisted: settings.blacklist.matches(&key.origin),
            in_loop: dep_loops.iter().any(|dl| dl.contains(key)),
            options_name: info.options_name.clone(),
            detail: with_options.then(|| PortDetail {
                violations: sess.violations(info),
                options: report::option_reports(&sess, info),
            }),
        });
    }
    rows.sort_by_key(|r| (r.status == UiStatus::Ok, r.key.clone()));

    let unconfigured = rows.iter().filter(|r| r.status == UiStatus::Unconfigured).count();
    let stale = rows.iter().filter(|r| r.status == UiStatus::Stale).count();
    let conflict = rows.iter().filter(|r| r.status == UiStatus::Conflict).count();
    let ok = rows.iter().filter(|r| r.status_str() == "ok").count();
    let blacklisted = rows.iter().filter(|r| r.blacklisted).count();
    // A loop every member of which is blacklisted blocks nothing here:
    // poudriere never builds any of them.
    let loop_blacklisted = |dl: &deps::DepLoop| {
        dl.members.iter().all(|k| settings.blacklist.matches(&k.origin))
    };
    let blocking_loops = dep_loops.iter().filter(|dl| !loop_blacklisted(dl)).count();
    let attention = rows.iter().filter(|r| r.needs_attention()).count();
    // The gate: ports owing a decision, plus loops a human has to untangle.
    let pending = attention + blocking_loops;

    if json {
        // stdout must stay pure JSON: one object, no table, quiet ignored.
        let report = report::ScanReport {
            options_dir: settings.options_dir.display().to_string(),
            ports_tree: settings.portsdir.display().to_string(),
            ports: rows
                .iter()
                .map(|r| report::PortReport {
                    port: r.key.clone(),
                    pkgname: r.pkgname.clone(),
                    status: r.status_str(),
                    undecided: r.undecided.clone(),
                    added: r.added.clone(),
                    removed: r.removed.clone(),
                    mc_covered: r.mc_covered(),
                    blacklisted: r.blacklisted,
                    in_loop: r.in_loop,
                    options_file: format!("{}/options", r.options_name),
                    violations: r.detail.as_ref().map(|d| d.violations.clone()),
                    options: r.detail.as_ref().map(|d| d.options.clone()),
                })
                .collect(),
            loops: dep_loops
                .iter()
                .map(|dl| report::LoopReport {
                    ports: dl.members.iter().map(|k| k.to_string()).collect(),
                    cycle: dl.cycle.iter().map(|k| k.to_string()).collect(),
                    blacklisted: loop_blacklisted(dl),
                })
                .collect(),
            summary: report::ScanSummary {
                total: rows.len(),
                unconfigured,
                stale,
                conflict,
                ok,
                optionless: hidden,
                blacklisted,
                attention,
                loops: dep_loops.len(),
                loops_blocking: blocking_loops,
                pending,
            },
        };
        report::print(&report)?;
    } else if !cli.quiet {
        let color = stdout_color(cli);
        for row in &rows {
            let (key, pkgname) = (&row.key, &row.pkgname);
            let decision = if row.mc_covered() {
                if cli.minimal { " [defaults/mc ≈]" } else { " [mc-covered ≈]" }.to_string()
            } else {
                format!(" undecided: {}", row.undecided.join(" "))
            };
            let (marker, text) = match &row.status {
                UiStatus::Unconfigured => ("?", format!("UNCONFIGURED{decision}")),
                UiStatus::Stale => {
                    ("!", format!("STALE{}{decision}", row.stale_detail()))
                }
                UiStatus::Conflict => {
                    ("✗", "CONFLICT (saved options violate constraints)".to_string())
                }
                _ => ("", "ok".to_string()),
            };
            // Blacklisted ports keep their status but wear the ⊘ marker:
            // whatever it says, nothing here is waiting on a human.
            let (marker, mut tail) = if row.blacklisted {
                ("⊘", " [blacklisted]".to_string())
            } else {
                (marker, String::new())
            };
            // The loop itself is spelled out on stderr; the row only says
            // that this port is caught in one.
            if row.in_loop {
                tail.push_str(" [dependency loop]");
            }
            // Pad first, tint after: the escapes must not count as width.
            let cell = match marker_color(marker).filter(|_| color) {
                Some(c) => format!("{c}{marker:<2}{}", ansi::RESET),
                None => format!("{marker:<2}"),
            };
            println!("{cell} {key:<40} {pkgname:<32} {text}{tail}");
            if cli.verbose {
                if !row.state.is_empty() {
                    println!("     options: {}", row.state);
                }
                for w in &row.warnings {
                    println!("     warning: {w}");
                }
            }
        }
    }

    // Loops are counted, not re-listed: run_scan already named every one.
    let loop_note = match dep_loops.len() {
        0 => String::new(),
        n => format!(" · {n} dependency loop{}", if n == 1 { "" } else { "s" }),
    };
    eprintln!(
        "{} ports with options ({} unconfigured, {} stale, {} conflict; \
         {} awaiting a decision) · {} without options{} · \
         {} queried, {} cached · {:.1}s",
        rows.len(),
        unconfigured,
        stale,
        conflict,
        attention,
        hidden,
        loop_note,
        queried,
        from_cache,
        elapsed
    );
    Ok(pending)
}

/// One write pass, whatever assembled it: the files to write, the entries to
/// remove, and everything that stopped the pass from being complete.
struct WritePass {
    writes: Vec<apply::PendingWrite>,
    removals: Vec<clean::Removal>,
    warnings: Vec<String>,
    rejected: Vec<report::RejectedEntry>,
    conflicts: Vec<report::ConflictEntry>,
    unknown: Vec<String>,
}

impl WritePass {
    fn new(writes: Vec<apply::PendingWrite>, removals: Vec<clean::Removal>, warnings: Vec<String>) -> Self {
        WritePass {
            writes,
            removals,
            warnings,
            rejected: Vec::new(),
            conflicts: Vec::new(),
            unknown: Vec::new(),
        }
    }

    /// A pass that could not be carried out as asked writes nothing at all:
    /// a caller driving this from a script must never be left guessing which
    /// half of its plan landed.
    fn blocked(&self) -> bool {
        !self.rejected.is_empty() || !self.conflicts.is_empty() || !self.unknown.is_empty()
    }

    fn nothing_to_do(&self) -> bool {
        self.writes.is_empty() && self.removals.is_empty()
    }
}

/// Carry out a write pass — unless it is a dry run or blocked — and assemble
/// the report `sync` and `decide` both answer with. Failures are collected
/// rather than raised: a per-file problem must not hide the rest of the pass.
fn commit_writes(
    pass: &WritePass,
    ports: &std::collections::BTreeMap<model::origin::PortKey, model::port::PortInfo>,
    options_dir: &std::path::Path,
    dry_run: bool,
) -> report::WriteReport {
    let act = !dry_run && !pass.blocked();
    let mut errors: Vec<report::ErrorEntry> = Vec::new();
    let mut written = 0usize;
    let mut removed = 0usize;
    let mut notes: Vec<String> = Vec::new();

    if act {
        let summary = apply::apply(&pass.writes);
        written = summary.written;
        for (key, message) in summary.failed {
            errors.push(report::ErrorEntry { subject: key.to_string(), message });
        }
        for r in &pass.removals {
            match clean::remove_entry(r) {
                Ok(note) => {
                    removed += 1;
                    if let Some(note) = note {
                        notes.push(note);
                    }
                }
                Err(e) => errors.push(report::ErrorEntry {
                    subject: r.options_name.clone(),
                    message: e.to_string(),
                }),
            }
        }
    }

    let mut warnings = pass.warnings.clone();
    warnings.extend(notes);
    report::WriteReport {
        options_dir: options_dir.display().to_string(),
        dry_run,
        applied: written > 0 || removed > 0,
        writes: pass
            .writes
            .iter()
            .map(|w| report::write_entry(w, ports.get(&w.key)))
            .collect(),
        removals: pass
            .removals
            .iter()
            .map(|r| report::RemovalEntry {
                options_name: r.options_name.clone(),
                reason: r.reason.clone(),
            })
            .collect(),
        rejected: pass.rejected.clone(),
        conflicts: pass.conflicts.clone(),
        unknown: pass.unknown.clone(),
        warnings,
        summary: report::WriteSummary {
            written,
            removed,
            failed: errors.len(),
            rejected: pass.rejected.len(),
            conflicts: pass.conflicts.len(),
        },
        errors,
    }
}

fn cmd_sync(cli: &Cli, rs: &RootSet, dry_run: bool, json: bool) -> Result<()> {
    let scanned = run_scan(cli, rs)?;
    let (settings, result) = (&scanned.settings, &scanned.result);
    let paint = stderr_color(cli);

    let staged = result.ports.iter().map(|(key, info)| {
        let saved =
            SavedOptionsFile::load(&settings.options_dir.join(&info.options_name).join("options"));
        (key, info, apply::sync_enabled_set(info, saved.as_ref()))
    });
    let planned = apply::plan_writes(staged, &settings.options_dir, cli.minimal);

    // A port that lost ALL its options never reaches plan_writes; its
    // leftover file is dead configuration and must go too (unless another
    // flavor sharing the file still has options). --minimal adds files whose
    // content defaults + make.conf already dictate.
    let mut stale_files = apply::plan_stale_removals(&result.ports, &settings.options_dir);
    stale_files.extend(planned.removals);
    stale_files.sort_by(|a, b| a.options_name.cmp(&b.options_name));

    let pass = WritePass::new(planned.writes, stale_files, planned.warnings);
    for w in &pass.warnings {
        eprintln!("{} {w}", tint(paint, ansi::YELLOW, "warning:"));
    }

    if pass.nothing_to_do() && !json {
        eprintln!("everything up to date, nothing to write");
        return Ok(());
    }
    // stdout carries either the listing or the JSON object, never both.
    if !json && !cli.quiet {
        for r in &pass.removals {
            println!("{}  removing options file ({})", r.options_name, r.reason);
        }
        for w in &pass.writes {
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
    }

    let report = commit_writes(&pass, &result.ports, &settings.options_dir, dry_run);
    if json {
        report::print(&report)?;
    }
    for e in &report.errors {
        eprintln!("{} {}: {}", tint(paint, ansi::RED, "error:"), e.subject, e.message);
    }
    if dry_run {
        eprintln!(
            "dry run: {} file(s) would be written, {} removed in {}",
            pass.writes.len(),
            pass.removals.len(),
            settings.options_dir.display()
        );
        return Ok(());
    }
    eprintln!(
        "{} file(s) written, {} removed in {}{}",
        report.summary.written,
        report.summary.removed,
        settings.options_dir.display(),
        if report.summary.failed == 0 {
            String::new()
        } else {
            format!(", {} failed", report.summary.failed)
        }
    );
    Ok(())
}

/// Apply a JSON plan of option values read on stdin. Answers with one JSON
/// object on stdout and exits 1 when the plan was not honoured in full, so a
/// script can branch on the status and read the reasons from the report.
fn cmd_decide(cli: &Cli, args: &cli::DecideArgs) -> Result<usize> {
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
            roots: plan.roots(),
            notes: vec!["no port list given; the plan's own ports are the closure roots".into()],
        }
    } else {
        RootSet { roots: cli::collect_roots(&args.roots.origins, &cli.files)?, notes: Vec::new() }
    };

    let scanned = run_scan(cli, &rs)?;
    let settings = scanned.settings;
    let paint = stderr_color(cli);
    let ports = scanned.result.ports.clone();
    let mut sess = session::Session::new(
        scanned.result.ports,
        scanned.result.aliases,
        &rs.roots,
        &settings.options_dir,
        cli.minimal,
    );

    let outcome = decide::apply_plan(&mut sess, &plan);

    // Only the files of the ports the plan named are rewritten; `sync`
    // refreshes the rest. Every closure port sharing a touched options file
    // is handed to the planner, so the default flavor still owns the write.
    let touched_names: std::collections::BTreeSet<String> = outcome
        .touched
        .iter()
        .filter_map(|k| sess.ports.get(k))
        .map(|i| i.options_name.clone())
        .collect();
    let staged = sess.ports.iter().filter_map(|(key, info)| {
        if !touched_names.contains(&info.options_name) {
            return None;
        }
        let state = sess.state(info)?;
        Some((key, info, state.staged.clone()))
    });
    let planned = apply::plan_writes(staged, &settings.options_dir, cli.minimal);

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
    pass.unknown = outcome.unknown.iter().map(|k| k.to_string()).collect();
    // A file that would record a state the port's own constraints forbid is
    // not written, however the plan got there.
    for name in &touched_names {
        for (key, info) in &sess.ports {
            if info.options_name != *name {
                continue;
            }
            let violations = sess.violations(info);
            if !violations.is_empty() {
                pass.conflicts
                    .push(report::ConflictEntry { port: key.to_string(), violations });
            }
        }
    }

    let report = commit_writes(&pass, &ports, &settings.options_dir, cli.dry_run);
    report::print(&report)?;
    for w in &report.warnings {
        eprintln!("{} {w}", tint(paint, ansi::YELLOW, "warning:"));
    }
    for e in &report.errors {
        eprintln!("{} {}: {}", tint(paint, ansi::RED, "error:"), e.subject, e.message);
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
    if cli.dry_run {
        eprintln!(
            "dry run: {} file(s) would be written, {} removed in {}",
            pass.writes.len(),
            pass.removals.len(),
            settings.options_dir.display()
        );
    } else {
        eprintln!(
            "{} file(s) written, {} removed in {}",
            report.summary.written,
            report.summary.removed,
            settings.options_dir.display()
        );
    }
    Ok(if report.summary.failed > 0 { 1 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::split_raw_origins;
    use super::parse_installed;

    #[test]
    fn installed_list_flavors_and_repo_filter() {
        let origins = "www/nginx\tpoudriere\n\
                       graphics/ImageMagick7\tSynth\n\
                       devel/binutils\tSynth\n\
                       www/nginx\tpoudriere\n\
                       not-an-origin\tSynth\n";
        let annots = "graphics/ImageMagick7\tflavor\tnox11\n\
                      devel/binutils\tflavor\tnative\n\
                      www/nginx\tcpe\tsomething\n";
        // Unfiltered: three ports, flavors applied, dup and junk dropped.
        let all = parse_installed(origins, annots, &[]);
        let names: Vec<String> = all.iter().map(|k| k.to_string()).collect();
        assert_eq!(
            names,
            vec!["devel/binutils@native", "graphics/ImageMagick7@nox11", "www/nginx"]
        );
        // Repo filter keeps only Synth-built packages.
        let synth = parse_installed(origins, annots, &["Synth".to_string()]);
        let names: Vec<String> = synth.iter().map(|k| k.to_string()).collect();
        assert_eq!(names, vec!["devel/binutils@native", "graphics/ImageMagick7@nox11"]);
        // Several repositories: a package from any of them is kept.
        let both =
            parse_installed(origins, annots, &["Synth".to_string(), "poudriere".to_string()]);
        assert_eq!(both.len(), 3);
        // Unknown repo: empty (caller turns this into an error).
        assert!(parse_installed(origins, annots, &["nope".to_string()]).is_empty());
    }

    #[test]
    fn split_raw_origins_forms() {
        let raw: Vec<String> = ["www/nginx", "-f", "list1", "mail/dovecot", "--file=list2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (origins, files) = split_raw_origins(&raw).unwrap();
        assert_eq!(origins, vec!["www/nginx", "mail/dovecot"]);
        assert_eq!(files.len(), 2);
        assert!(files[0].ends_with("list1") && files[1].ends_with("list2"));
    }

    #[test]
    fn split_raw_origins_rejects_stray_flags_and_dangling_f() {
        let raw = vec!["www/nginx".to_string(), "-J".to_string(), "8".to_string()];
        assert!(split_raw_origins(&raw).is_err());
        let raw = vec!["www/nginx".to_string(), "-f".to_string()];
        assert!(split_raw_origins(&raw).is_err());
    }
}
