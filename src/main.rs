mod apply;
mod cache;
mod clean;
mod cli;
mod config;
mod draft;
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
            let roots = cli::collect_roots(&args.roots.origins, &cli.files)?;
            cmd_tui(&cli, &roots, args.drive)
        }
        Some(Command::Scan(args)) => {
            let roots = cli::collect_roots(&args.roots.origins, &cli.files)?;
            // Exit code is the cron/CI gate: 1 = decisions pending, 0 = clean.
            // Real errors keep travelling up the anyhow path (nonzero, 1 from
            // clap's runner) — only the *clean* run may return 0.
            let attention = cmd_scan(&cli, &roots, args.json)?;
            if attention > 0 {
                let _ = std::io::stdout().flush();
                let _ = std::io::stderr().flush();
                std::process::exit(1);
            }
            Ok(())
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
            cmd_tui(&cli, &roots, false)
        }
        None => {
            if cli.files.is_empty() {
                anyhow::bail!(
                    "no ports given; try `optique -z <set> category/port…` or `optique -f pkglist`"
                );
            }
            let roots = cli::collect_roots(&[], &cli.files)?;
            cmd_tui(&cli, &roots, false)
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
    let has_list = !args.origins.is_empty() || !cli.files.is_empty();
    if args.unused && !has_list {
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
    let roots = cli::collect_roots(&args.origins, &cli.files)?;
    let scanned = run_scan(cli, &roots)?;
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
    eprintln!("optique clean: options dir {}", settings.options_dir.display());
    for note in &settings.notes {
        eprintln!("  note:        {note}");
    }

    let (mut removals, live, warnings) =
        clean::classify_entries(&settings.options_dir, &settings.portsdir, &moved);
    let total_entries = removals.len() + live.len();
    for w in &warnings {
        eprintln!("warning: {w}");
    }

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
        if cli.verbose && !cli.quiet {
            kept.sort();
            for (name, why) in &kept {
                println!("keep  {name:<38} {why}");
            }
        }
    }

    // Obsolete, unused and redundant removals were collected separately.
    removals.sort_by(|a, b| a.options_name.cmp(&b.options_name));

    if removals.is_empty() {
        eprintln!("nothing to clean ({total_entries} entries kept)");
        return Ok(());
    }
    if !cli.quiet {
        for r in &removals {
            println!("{:<44} {}", r.options_name, r.reason);
        }
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

fn cmd_tui(cli: &Cli, roots: &[model::origin::PortKey], drive: bool) -> Result<()> {
    // Fail before the (possibly minute-long) scan, not after. The headless
    // driver renders into memory, so it has no use for a terminal.
    if !drive {
        tui::ensure_terminal()?;
    }
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
    let blacklist = scanned.settings.blacklist;
    if drive {
        tui::run_driver(session, options_dir, db, refresher, blacklist)
    } else {
        tui::run(session, options_dir, db, refresher, blacklist)
    }
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

fn run_scan(cli: &Cli, roots: &[model::origin::PortKey]) -> Result<Scanned> {
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

    if !cli.quiet {
        eprintln!("optique: ports tree {} · {} jobs", settings.portsdir.display(), jobs);
        for note in &settings.notes {
            eprintln!("  note:        {note}");
        }
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
        if !settings.blacklist.is_empty() {
            for (i, src) in settings.blacklist.sources.iter().enumerate() {
                eprintln!(
                    "  {}   {}",
                    if i == 0 { "blacklist:" } else { "          " },
                    src.display()
                );
            }
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

/// SGR escapes for the scan marker column, matching the TUI's marker colors.
/// A handful of constants beats a dependency for four markers.
mod ansi {
    pub const RESET: &str = "\x1b[0m";
    pub const RED: &str = "\x1b[31m";
    pub const LIGHT_RED: &str = "\x1b[91m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const GRAY: &str = "\x1b[90m";
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

/// Scan and report. Returns the number of ports needing a *human* decision,
/// which main turns into exit code 1 (see `Row::needs_attention`).
fn cmd_scan(cli: &Cli, roots: &[model::origin::PortKey], json: bool) -> Result<usize> {
    use crate::session::UiStatus;

    let scanned = run_scan(cli, roots)?;
    let (queried, from_cache, elapsed) =
        (scanned.result.queried, scanned.result.from_cache, scanned.elapsed);
    let settings = scanned.settings;
    // Session gives owner-aware statuses (flavors sharing an options file
    // are judged against the default flavor's view).
    let sess = session::Session::new(
        scanned.result.ports,
        scanned.result.aliases,
        roots,
        &settings.options_dir,
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
        let undecided = session::undecided_options(info, saved);
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
        });
    }
    rows.sort_by_key(|r| (r.status == UiStatus::Ok, r.key.clone()));

    let unconfigured = rows.iter().filter(|r| r.status == UiStatus::Unconfigured).count();
    let stale = rows.iter().filter(|r| r.status == UiStatus::Stale).count();
    let conflict = rows.iter().filter(|r| r.status == UiStatus::Conflict).count();
    let ok = rows.iter().filter(|r| r.status_str() == "ok").count();
    let blacklisted = rows.iter().filter(|r| r.blacklisted).count();
    let attention = rows.iter().filter(|r| r.needs_attention()).count();

    if json {
        // stdout must stay pure JSON: one object, no table, quiet ignored.
        #[derive(serde::Serialize)]
        struct JsonPort<'a> {
            port: &'a str,
            pkgname: &'a str,
            status: &'static str,
            undecided: &'a [String],
            added: &'a [String],
            removed: &'a [String],
            mc_covered: bool,
            blacklisted: bool,
        }
        #[derive(serde::Serialize)]
        struct JsonSummary {
            total: usize,
            unconfigured: usize,
            stale: usize,
            conflict: usize,
            ok: usize,
            optionless: usize,
            blacklisted: usize,
            attention: usize,
        }
        #[derive(serde::Serialize)]
        struct JsonReport<'a> {
            options_dir: String,
            ports_tree: String,
            ports: Vec<JsonPort<'a>>,
            summary: JsonSummary,
        }
        let report = JsonReport {
            options_dir: settings.options_dir.display().to_string(),
            ports_tree: settings.portsdir.display().to_string(),
            ports: rows
                .iter()
                .map(|r| JsonPort {
                    port: &r.key,
                    pkgname: &r.pkgname,
                    status: r.status_str(),
                    undecided: &r.undecided,
                    added: &r.added,
                    removed: &r.removed,
                    mc_covered: r.mc_covered(),
                    blacklisted: r.blacklisted,
                })
                .collect(),
            summary: JsonSummary {
                total: rows.len(),
                unconfigured,
                stale,
                conflict,
                ok,
                optionless: hidden,
                blacklisted,
                attention,
            },
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if !cli.quiet {
        let color = stdout_color(cli);
        for row in &rows {
            let (key, pkgname) = (&row.key, &row.pkgname);
            let decision = if row.mc_covered() {
                " [mc-covered ≈]".to_string()
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
            let (marker, tail) = if row.blacklisted {
                ("⊘", " [blacklisted]")
            } else {
                (marker, "")
            };
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

    eprintln!(
        "{} ports with options ({} unconfigured, {} stale, {} conflict; \
         {} awaiting a decision) · {} without options · \
         {} queried, {} cached · {:.1}s",
        rows.len(),
        unconfigured,
        stale,
        conflict,
        attention,
        hidden,
        queried,
        from_cache,
        elapsed
    );
    Ok(attention)
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

    // A port that lost ALL its options never reaches plan_writes; its
    // leftover file is dead configuration and must go too (unless another
    // flavor sharing the file still has options).
    let stale_files = apply::plan_stale_removals(&result.ports, &settings.options_dir);

    if writes.is_empty() && stale_files.is_empty() {
        eprintln!("everything up to date, nothing to write");
        return Ok(());
    }
    if !cli.quiet {
        for r in &stale_files {
            println!("{}  removing options file ({})", r.options_name, r.reason);
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
    }
    if dry_run {
        eprintln!(
            "dry run: {} file(s) would be written, {} removed in {}",
            writes.len(),
            stale_files.len(),
            settings.options_dir.display()
        );
        return Ok(());
    }
    let summary = apply::apply(&writes);
    for (key, msg) in &summary.failed {
        eprintln!("error: {key}: {msg}");
    }
    let mut removed = 0usize;
    for r in &stale_files {
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
    eprintln!(
        "{} file(s) written, {} removed in {}{}",
        summary.written,
        removed,
        settings.options_dir.display(),
        if summary.failed.is_empty() { String::new() } else { format!(", {} failed", summary.failed.len()) }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::split_raw_origins;

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
