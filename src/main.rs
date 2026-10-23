/// `println!` for the stdout payload, handing back the write's error instead
/// of panicking on it: a reader that went away (`optique scan | head`) must
/// end the run with a documented status, not Rust's 101.
macro_rules! outln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        writeln!(std::io::stdout(), $($arg)*)
    }};
}

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
mod optfiles;
mod optionsfile;
mod query;
mod report;
mod roots;
mod session;
mod staging;
mod tui;

use std::io::Write as _;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context as _, Result};

use crate::cli::{Cli, Command};
use crate::model::port::PortInfo;
use crate::moved::Moved;
use crate::query::makerunner::QueryCtx;
use crate::query::scanner::{self, ScanResult};

/// Exit status of a run that failed outright. 1 is taken: scan and decide
/// answer "something is left to do" with it, so a failure must not look the
/// same to a gate. 2 is clap's, for bad usage.
const EXIT_ERROR: i32 = 3;

fn main() {
    if let Err(e) = run() {
        // A closed stdout has no reader left to tell; it still fails the run.
        let broken_pipe = e.chain().any(|c| {
            c.downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::BrokenPipe)
        });
        if !broken_pipe {
            // As the standard library would print it, causes included.
            eprintln!("Error: {e:?}");
        }
        let _ = std::io::stdout().flush();
        std::process::exit(EXIT_ERROR);
    }
}

fn run() -> Result<()> {
    let args = cli::disambiguate_synth(std::env::args_os());
    if let Err(msg) = cli::check_action_first(&args) {
        use clap::CommandFactory as _;
        Cli::command().error(clap::error::ErrorKind::ArgumentConflict, msg).exit();
    }
    let cli = cli::parse(args);
    if let Err(e) = cli.check_globals() {
        e.exit();
    }
    // --repo only filters the implicit installed-package list: refuse it on
    // every path that cannot use that list, not just where the list is built.
    if !cli.repo.is_empty() {
        let explicit = !cli.files.is_empty()
            || match &cli.command {
                Some(Command::Tui(a)) => !a.roots.origins.is_empty(),
                Some(Command::Scan(a)) => !a.roots.origins.is_empty(),
                Some(Command::Sync(a)) => !a.roots.origins.is_empty(),
                Some(Command::Clean(a)) => !a.unused || !a.origins.is_empty(),
                Some(Command::Decide(_)) | Some(Command::Origins(_)) => true,
                None => false,
            };
        if explicit {
            use clap::CommandFactory as _;
            Cli::command()
                .error(
                    clap::error::ErrorKind::ArgumentConflict,
                    "--repo filters the installed-package list, which is only used when \
                     no ports are given",
                )
                .exit();
        }
    }
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
            exit_pending(attention > 0);
            Ok(())
        }
        Some(Command::Sync(args)) => {
            let rs = roots_or_installed(&cli, &args.roots.origins)?;
            cmd_sync(&cli, &rs, cli.dry_run, args.json)
        }
        Some(Command::Decide(args)) => {
            // Same gate contract as scan: 1 means "not done", not "broke".
            let code = cmd_decide(&cli, args)?;
            exit_pending(code != 0);
            Ok(())
        }
        Some(Command::Clean(args)) => cmd_clean(&cli, args),
        Some(Command::Origins(raw)) => {
            // Bare origins (default TUI). Tolerate -f/--file mixed in after
            // the first origin, where clap no longer parses flags.
            let (origins, files) = split_raw_origins(raw).unwrap_or_else(|e| e.exit());
            let mut all_files = cli.files.clone();
            all_files.extend(files);
            let rs = RootSet {
                roots: cli::collect_roots(&origins, &all_files)?,
                notes: Vec::new(),
                installed: false,
            };
            cmd_tui(&cli, &rs, false)
        }
        None => {
            if cli.files.is_empty() && cli.synth.is_none() {
                // No build-specific flag in the hint: it must hold for
                // poudriere, local and any flags already given.
                anyhow::bail!(
                    "no ports given; name them (`category/port…`) or a list (`-f pkglist`)"
                );
            }
            let rs = roots_or_installed(&cli, &[])?;
            cmd_tui(&cli, &rs, false)
        }
    }
}

/// Exit 1 when something is left to do — the gate status scan and decide
/// share — once both streams are flushed.
fn exit_pending(pending: bool) {
    if pending {
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        std::process::exit(1);
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
        use clap::CommandFactory as _;
        Cli::command()
            .error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "clean --unused needs a package list to compare against: \
                 give port origins or -f pkglist",
            )
            .exit();
    }
    if !args.unused && has_list {
        use clap::CommandFactory as _;
        Cli::command()
            .error(
                clap::error::ErrorKind::ArgumentConflict,
                "clean walks the whole options dir; port origins and -f pkglist \
                 only mean something together with --unused",
            )
            .exit();
    }

    if !args.unused {
        let staging = tempfile::tempdir()?;
        let settings = resolve_settings(cli, staging.path())?;
        let moved = load_moved(cli, &settings);
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
    let CleanCtx { settings, moved, jobs, cache: open_cache, used, _staging } = ctx;
    let paint = stderr_color(cli);
    eprintln!("{} options dir {}", tint(paint, ansi::BOLD, "optique clean:"), settings.options_dir.display());
    for note in &settings.notes {
        eprintln!("  {}        {note}", tint(paint, ansi::YELLOW, "note:"));
    }

    let (mut removals, live, mut warnings) =
        clean::classify_entries(
            &settings.options_dir,
            &settings.portsdir,
            &settings.overlay_paths(),
            &moved,
        );
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
        let cache = open_cache.unwrap_or_else(|| new_cache(cli, &settings));
        let found = find_redundant(cli, &settings, jobs, cache, &live);
        removals.extend(found.removals);
        warnings.extend(found.warnings);
        if cli.verbose && !args.json {
            for (name, why) in &found.kept {
                outln!("keep  {name:<38} {why}")?;
            }
        }
        kept_entries = found.kept;
    }

    // Obsolete, unused and redundant removals were collected separately.
    removals.sort_by(|a, b| a.options_name.cmp(&b.options_name));

    // stdout carries either the listing or the JSON object, never both.
    if !args.json && !cli.quiet {
        for r in &removals {
            outln!("{:<44} {}", r.options_name, r.reason)?;
        }
    }

    let mut errors: Vec<report::ErrorEntry> = Vec::new();
    let mut removed = 0usize;
    if !cli.dry_run {
        let done = clean::remove_all(&removals);
        removed = done.count;
        for note in done.notes {
            eprintln!("{} {note}", tint(paint, ansi::YELLOW, "note:"));
            warnings.push(note);
        }
        for (subject, message) in done.failed {
            eprintln!("{} {subject}: {message}", tint(paint, ansi::RED, "error:"));
            errors.push(report::ErrorEntry { subject, message });
        }
    }

    if args.json {
        report::print(&report::CleanReport {
            options_dir: settings.options_dir.display().to_string(),
            dry_run: cli.dry_run,
            removals: removals.iter().map(report::RemovalEntry::from).collect(),
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

/// What the redundancy pass found among the live entries.
#[derive(Default)]
struct Redundancy {
    removals: Vec<clean::Removal>,
    /// Entries kept and why, sorted; only --verbose fills it in.
    kept: Vec<(String, String)>,
    warnings: Vec<String>,
}

/// Query the port of every live entry and mark the files that only repeat
/// defaults + make.conf. Flavors of one origin share its file, and a flavor
/// can set its own defaults, so the default flavor's verdict is not enough:
/// every other flavor in its FLAVORS is queried as well, one make query each,
/// and the file goes only when none of them deviates. An entry one of whose
/// queries fails is left alone.
fn find_redundant(
    cli: &Cli,
    settings: &config::Settings,
    jobs: usize,
    mut cache: cache::Cache,
    live: &[clean::LiveEntry],
) -> Redundancy {
    use crate::query::makerunner::{MakeRunner, ScanEvent};
    use model::origin::PortKey;
    use model::port::PortInfo;

    /// An entry whose other flavors are still being queried.
    struct Flavors<'a> {
        entry: &'a clean::LiveEntry,
        /// The flavors reading this entry's file, default flavor first.
        infos: Vec<PortInfo>,
        waiting: usize,
        failed: Option<String>,
    }

    let paint = stderr_color(cli);
    let runner = MakeRunner::new(query_ctx(settings, settings.options_dir.clone()), jobs);
    let by_key: std::collections::HashMap<_, _> = live.iter().map(|e| (e.key.clone(), e)).collect();
    let files = optfiles::OptionsFiles::new(&settings.options_dir);
    let mut found = Redundancy::default();
    let mut warn = |found: &mut Redundancy, note: String| {
        eprintln!("{} {note}", tint(paint, ansi::YELLOW, "warning:"));
        found.warnings.push(note);
    };
    let mut pending: std::collections::HashMap<PortKey, Flavors> = Default::default();
    // The verdict once every flavor reading the file is known.
    let settle = |mut f: Flavors, found: &mut Redundancy, warn: &mut dyn FnMut(&mut Redundancy, String)| {
        if let Some(why) = f.failed {
            warn(found, why);
            return;
        }
        // Answers arrive in any order; the reason names the same flavor each run.
        f.infos[1..].sort_by(|a, b| a.key.cmp(&b.key));
        // An options.local assigning a list afresh: not optique's to remove.
        if let Some(why) = files.combined(&f.entry.options_name).local_reset() {
            if cli.verbose {
                found.kept.push((f.entry.options_name.clone(), why));
            }
            return;
        }
        let local = files.under_local(&f.entry.options_name, Default::default());
        match shared_file_deviation(&f.infos, &local) {
            None => found.removals.push(clean::Removal {
                options_name: f.entry.options_name.clone(),
                dir: f.entry.dir.clone(),
                reason: "redundant: repeats defaults + make.conf".to_string(),
            }),
            Some(why) if cli.verbose => found.kept.push((f.entry.options_name.clone(), why)),
            Some(_) => {}
        }
    };

    let mut ask: Vec<PortKey> = live.iter().map(|e| e.key.clone()).collect();
    let mut ready: std::collections::VecDeque<std::result::Result<PortInfo, (PortKey, String)>> =
        Default::default();
    let mut in_flight = 0usize;
    let mut done = 0usize;
    let mut total = live.len();
    loop {
        for key in ask.drain(..) {
            match cache.lookup(&key, &files) {
                Some(info) => ready.push_back(Ok(info)),
                None => {
                    runner.submit(key);
                    in_flight += 1;
                }
            }
        }
        while let Some(result) = ready.pop_front() {
            done += 1;
            let key = match &result {
                Ok(info) => info.key.clone(),
                Err((key, _)) => key.clone(),
            };
            let plain = PortKey::new(key.origin.clone(), None);
            if key.flavor.is_none() {
                let Some(entry) = by_key.get(&key) else { continue };
                let info = match result {
                    Ok(info) => info,
                    Err((_, msg)) => {
                        warn(&mut found, format!("{key}: query failed, left alone ({msg})"));
                        continue;
                    }
                };
                // The verdict below is only valid for the file this port
                // actually reads; a custom/legacy OPTIONS_NAME means the entry
                // belongs to some other port — leave it alone.
                if info.options_name != entry.options_name {
                    let note = format!(
                        "{}: {} uses options name {} — not this entry, left alone",
                        entry.options_name, info.key, info.options_name
                    );
                    warn(&mut found, note);
                    continue;
                }
                let others: Vec<PortKey> = info
                    .flavors
                    .iter()
                    .filter(|f| info.canonical.flavor.as_ref() != Some(*f))
                    .map(|f| PortKey::new(key.origin.clone(), Some(f.clone())))
                    .collect();
                let f = Flavors { entry, infos: vec![info], waiting: others.len(), failed: None };
                // A deviating default flavor settles it: no need to ask the others.
                let local = files.under_local(&entry.options_name, Default::default());
                if others.is_empty() || shared_file_deviation(&f.infos, &local).is_some() {
                    settle(f, &mut found, &mut warn);
                    continue;
                }
                total += others.len();
                ask.extend(others);
                pending.insert(plain, f);
                continue;
            }
            let Some(f) = pending.get_mut(&plain) else { continue };
            f.waiting -= 1;
            match result {
                // A flavor with an options name of its own, or with no
                // options at all (glib20@bootstrap), does not read this file.
                Ok(info) if info.options_name == f.entry.options_name && info.options.has_options() => {
                    f.infos.push(info)
                }
                Ok(_) => {}
                Err((_, msg)) => {
                    f.failed.get_or_insert_with(|| {
                        format!("{}: {key}: query failed, left alone ({msg})", f.entry.options_name)
                    });
                }
            }
            if f.waiting == 0 {
                let f = pending.remove(&plain).expect("pending entry just seen");
                settle(f, &mut found, &mut warn);
            }
        }
        if !ask.is_empty() {
            continue;
        }
        if in_flight == 0 {
            break;
        }
        match runner.events.recv() {
            Ok(ScanEvent::PortDone(info)) => {
                in_flight -= 1;
                cache.insert(&info, &files);
                ready.push_back(Ok(*info));
            }
            Ok(ScanEvent::PortError { key, msg }) => {
                in_flight -= 1;
                ready.push_back(Err((key, msg)));
            }
            Err(_) => break,
        }
        eprint!("\r{}", tint(paint, ansi::GRAY, &format!("checking… {done}/{total} ports")));
        let _ = std::io::stderr().flush();
    }
    if done > 0 {
        eprintln!();
    }
    runner.shutdown();
    found.kept.sort();
    found
}

/// Why a shared options file is not redundant, judged for every flavor that
/// reads it (`infos`, default flavor first, each queried with the file in
/// place): the first flavor it moves away from defaults + make.conf. None
/// when the file changes nothing for any of them.
fn shared_file_deviation(infos: &[model::port::PortInfo], local: &optionsfile::SavedOptionsFile) -> Option<String> {
    // bsd.options.mk:300-304: a flavor with an OPTIONS_OVERRIDE never reads
    // the file; it is kept, not judged redundant, whatever it says.
    if let Some(info) = infos.iter().find(|i| i.options.r#override.is_some()) {
        return Some(format!("{}: make.conf OPTIONS_OVERRIDE decides it, left alone", info.key));
    }
    infos.iter().enumerate().find_map(|(i, info)| {
        let diff = clean::redundancy_diff(info, local);
        if diff.is_empty() {
            None
        } else if i == 0 {
            Some(format!("deviates from defaults + make.conf: {}", diff.join(" ")))
        } else {
            Some(format!("deviates from defaults + make.conf for {}: {}", info.key, diff.join(" ")))
        }
    })
}

/// What the make queries need from the settings; `port_dbdir` is where they
/// read options files from.
fn query_ctx(settings: &config::Settings, port_dbdir: std::path::PathBuf) -> QueryCtx {
    QueryCtx {
        portsdir: settings.portsdir.clone(),
        make_conf: settings.make_conf.clone(),
        port_dbdir,
        overlays: settings.overlay_paths(),
    }
}

/// Origins of everything installed, from pkg(8) — synth's natural root set
/// when no list is given (synth builds what is installed). Flavors come from
/// the pkg "flavor" annotation; `repo` restricts to packages installed from
/// that pkg repository (%R).
fn installed_roots(repos: &[String]) -> Result<Vec<model::origin::PortKey>> {
    let origins = pkg_query("%n\t%o\t%R")?;
    // One line per annotation; only the "flavor" ones matter.
    let annots = pkg_query("%n\t%At\t%Av")?;
    let roots = parse_installed(&origins, &annots, repos);
    if roots.is_empty() {
        if repos.is_empty() {
            anyhow::bail!("no installed packages with port origins found (pkg query -a %o)");
        }
        anyhow::bail!(
            "no installed packages from {} \
             (check the names with: pkg query -a %R | sort -u)",
            repos_phrase(repos)
        );
    }
    Ok(roots)
}

/// `pkg query -a FORMAT`, its stdout; a failing query fails the run rather
/// than reading as no packages (or no flavors).
fn pkg_query(format: &str) -> Result<String> {
    let out = std::process::Command::new("pkg")
        .args(["query", "-a", format])
        .output()
        .map_err(|e| anyhow::anyhow!("cannot run pkg query: {e}"))?;
    if !out.status.success() {
        anyhow::bail!("pkg query failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// "repository 'a'" or "repositories 'a', 'b'".
fn repos_phrase(repos: &[String]) -> String {
    let noun = if repos.len() == 1 { "repository" } else { "repositories" };
    format!("{noun} '{}'", repos.join("', '"))
}

/// Pure part of installed_roots: `origins` lines are
/// `package\torigin\trepository`, `annots` lines are
/// `package\tannotation\tvalue`. Both are keyed by package, not origin:
/// several flavors of one origin can be installed at once (polkit-qt-1 for
/// qt5 and qt6), each its own root. Unparsable origins are skipped,
/// duplicates collapse, the result is sorted.
fn parse_installed(
    origins: &str,
    annots: &str,
    repos: &[String],
) -> Vec<model::origin::PortKey> {
    let mut flavor: std::collections::HashMap<&str, &str> = Default::default();
    for line in annots.lines() {
        let mut f = line.split('\t');
        if let (Some(pkg), Some("flavor"), Some(value)) = (f.next(), f.next(), f.next()) {
            flavor.insert(pkg, value);
        }
    }
    let mut roots = std::collections::BTreeSet::new();
    for line in origins.lines() {
        let mut f = line.split('\t');
        let (Some(pkg), Some(origin), pkg_repo) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let origin = origin.trim();
        if origin.is_empty() {
            continue;
        }
        if !repos.is_empty() {
            let from = pkg_repo.map(str::trim).unwrap_or("");
            if !repos.iter().any(|r| r == from) {
                continue;
            }
        }
        let spec = match flavor.get(pkg) {
            Some(fl) => format!("{origin}@{fl}"),
            None => origin.to_string(),
        };
        // Packages not built from ports may carry unparsable origins: skip.
        if let Some(key) = model::origin::PortKey::parse(&spec) {
            roots.insert(key);
        }
    }
    roots.into_iter().collect()
}

/// The resolved root list plus notes destined for the startup banner's
/// aligned `note:` block (nothing is printed here).
struct RootSet {
    roots: Vec<model::origin::PortKey>,
    notes: Vec<String>,
    /// The roots are synth's implicit list, the installed packages, which
    /// the scan matches against the tree as synth does.
    installed: bool,
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
                "no ports given; using {} installed package(s) from {}",
                roots.len(),
                repos_phrase(&cli.repo)
            )
        };
        return Ok(RootSet { roots, notes: vec![note], installed: true });
    }
    Ok(RootSet {
        roots: cli::collect_roots(origins, &cli.files)?,
        notes: Vec::new(),
        installed: false,
    })
}

/// Split an external-subcommand argument vector into origins and -f/--file
/// values (clap stops parsing flags once the first bare origin appears).
/// Usage errors here are returned, not raised through clap's own `.exit()`,
/// so a caller testing this in-process (unit tests) sees a plain `Err`
/// rather than the process vanishing; `run()` calls `.exit()` on it, which
/// is where the exit-2 contract actually takes effect.
pub(crate) fn split_raw_origins(
    raw: &[String],
) -> std::result::Result<(Vec<String>, Vec<std::path::PathBuf>), clap::Error> {
    use clap::CommandFactory as _;
    let mut origins = Vec::new();
    let mut files = Vec::new();
    let mut it = raw.iter();
    while let Some(arg) = it.next() {
        if arg == "-f" || arg == "--file" {
            let Some(path) = it.next() else {
                return Err(Cli::command().error(
                    clap::error::ErrorKind::MissingRequiredArgument,
                    format!("{arg} needs a pkglist file argument"),
                ));
            };
            files.push(path.into());
        } else if let Some(path) = arg.strip_prefix("--file=") {
            files.push(path.into());
        } else if arg.starts_with('-') {
            return Err(Cli::command().error(
                clap::error::ErrorKind::UnknownArgument,
                format!(
                    "unexpected flag {arg} after port origins; put flags before the first origin"
                ),
            ));
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
    let mut session = session::Session::new(
        scanned.result.ports,
        scanned.result.aliases,
        &scanned.roots,
        &scanned.files,
        cli.minimal,
    );
    session.blacklist = scanned.settings.blacklist.clone();

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
        tui::run_driver(session, files, db, refresher, blacklist, overlays, cli.minimal)
    } else {
        tui::run(session, files, db, refresher, blacklist, overlays, cli.minimal)
    }
    // scanned.staging (make.conf + staging db) lives until here
}

/// Shared setup + closure scan used by every subcommand.
struct Scanned {
    settings: config::Settings,
    /// The roots the scan started from: the given ones, each `origin@all`
    /// expanded to its port's flavors (every bare one too under
    /// FLAVOR_DEFAULT_ALL), `origin@-` read as the bare origin.
    roots: Vec<model::origin::PortKey>,
    result: ScanResult,
    /// Dependency loops found in the scanned closure (already reported on
    /// stderr by `run_scan`; kept for the table, the JSON and the gate).
    dep_loops: Vec<deps::DepLoop>,
    elapsed: f32,
    /// Holds the layered make.conf (and the TUI's staging db) alive.
    staging: tempfile::TempDir,
    /// The options dir's files as the scan read them; the session and the
    /// TUI's staging copy read them from here, not from disk again.
    files: optfiles::OptionsFiles,
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
        let tree_key = cache::trees_key(&settings.portsdir, &settings.overlay_paths());
        cache::Cache::open(&cache::default_cache_dir(), &tree_key, &settings.conf_hash)
    }
}

/// MOVED of the main tree and every overlay, its warnings said once.
fn load_moved(cli: &Cli, settings: &config::Settings) -> Moved {
    let overlays = settings.overlay_paths();
    let trees: Vec<&Path> =
        std::iter::once(settings.portsdir.as_path()).chain(overlays.iter().map(|p| p.as_path())).collect();
    let (moved, warnings) = Moved::load_all(&trees);
    let paint = stderr_color(cli);
    for w in warnings {
        eprintln!("{} {w}", tint(paint, ansi::YELLOW, "warning:"));
    }
    moved
}

/// Why synth would refuse to build with the saved options of a port whose
/// options are `options`, `text` being the file synth reads for it; None
/// when it would not. synth's check (portscan-packages.adb ~808-916,
/// `passed_options_cache_check`, run before every build by
/// portscan-pilot.adb ~418-424) counts every line with `+=` right after
/// `OPTIONS_FILE_SET` or `OPTIONS_FILE_UNSET`'s length, and fails the file
/// when an entry names an option the port lacks or when the count is not
/// the port's number of options, more or fewer. That check indexes every
/// line at a fixed byte offset (`Line (17 .. 18)` in Ada, 1-indexed) with no
/// exception handler of its own or in anything that calls it up to the run
/// that invokes it once per build: a line shorter than 18 bytes raises
/// `CONSTRAINT_ERROR` and aborts synth outright, rather than being skipped,
/// and so does one of 18 or 19 bytes without the marker at 17-18, whose
/// second probe (`Line (19 .. 20)`) runs past its end.
fn synth_refuses(text: &[u8], options: &[String]) -> Option<String> {
    let options: std::collections::BTreeSet<&str> = options.iter().map(String::as_str).collect();
    // A single trailing newline ends the last real line, as Ada's line-based
    // Get_Line reads it (no extra, empty line after it); any other short
    // line is one synth would choke on, for real.
    let mut lines: Vec<&[u8]> = text.split(|&b| b == b'\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let mut count = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if line.len() < 18 {
            return Some(format!("synth aborts on a line shorter than 18 bytes (line {})", i + 1));
        }
        if line.len() < 20 && line.get(16..18) != Some(b"+=") {
            return Some(format!(
                "synth aborts on a line of {} bytes without += at 17-18 (line {})",
                line.len(),
                i + 1
            ));
        }
        let name = if line.get(16..18) == Some(b"+=") {
            &line[18..]
        } else if line.get(18..20) == Some(b"+=") {
            &line[20..]
        } else {
            continue;
        };
        count += 1;
        let name = String::from_utf8_lossy(name);
        if !options.contains(name.as_ref()) {
            return Some(format!("saved {name} is no option of the port"));
        }
    }
    (count != options.len())
        .then(|| format!("{count} saved entries for the port's {} options", options.len()))
}

/// The file synth checks for `info` under `files`' options dir, and what it
/// says of it ([`synth_refuses`]): `<category>_<name>/options` by origin,
/// for a port without flavors; a flavored port's name carries its
/// `@flavor` there, a file that never exists, so it is never refused.
fn synth_refuses_port(files: &optfiles::OptionsFiles, info: &PortInfo) -> Option<String> {
    if !info.flavors.is_empty() {
        return None;
    }
    let bytes = files.bytes(&info.key.origin.replace('/', "_"))?;
    synth_refuses(&bytes, &info.options.complete)
}

/// Does a scan row make `scan` exit 1? A conflict always does (saved
/// options violate the port's own constraints), and so does a file synth
/// would refuse (`synth_refuses`, under -s), whatever the status; missing
/// or outdated configuration only does when make.conf leaves something
/// open (`mc_covered` false). Blacklisted ports never do — they are never
/// built here.
fn needs_attention(
    status: &session::UiStatus,
    blacklisted: bool,
    mc_covered: bool,
    synth_refuses: bool,
) -> bool {
    use session::UiStatus;
    if blacklisted {
        return false;
    }
    if synth_refuses {
        return true;
    }
    match status {
        UiStatus::Conflict => true,
        UiStatus::Unconfigured | UiStatus::Stale => !mc_covered,
        _ => false,
    }
}

/// Whether scan shows a row as ok: its status is, or -m makes the defaults
/// and make.conf the decision and they leave nothing open for this
/// unconfigured or stale port. Rows sort on it, so the ones shown as
/// problems come first.
fn shown_ok(status: &session::UiStatus, minimal: bool, covered: bool) -> bool {
    use session::UiStatus;
    match status {
        UiStatus::Ok => true,
        UiStatus::Unconfigured | UiStatus::Stale => minimal && covered,
        _ => false,
    }
}

/// The resolution the command line asks for.
fn resolve_settings(cli: &Cli, staging_dir: &Path) -> Result<config::Settings> {
    config::resolve(
        &config::Request {
            tree: cli.tree.as_deref(),
            jail: cli.jail.as_deref(),
            set: cli.set.as_deref(),
            synth: cli.synth.as_deref(),
            local: cli.local,
            portsdir: cli.portsdir.as_deref(),
            options_dir: cli.options_dir.as_deref(),
            overlays: &cli.overlays,
        },
        staging_dir,
    )
}

fn run_scan(cli: &Cli, rs: &RootSet) -> Result<Scanned> {
    let staging = tempfile::tempdir()?;
    let settings = resolve_settings(cli, staging.path())?;

    let jobs = default_jobs(cli);
    let mut cache = new_cache(cli, &settings);
    let moved = load_moved(cli, &settings);

    let ctx = query_ctx(&settings, settings.options_dir.clone());

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
        // In lookup order: the first holding a port answers for it.
        for (i, o) in settings.overlays.iter().enumerate() {
            let label = if i == 0 { "overlay:" } else { "        " };
            let what = match &o.name {
                Some(name) => format!("{name} ({})", o.path.display()),
                None => o.path.display().to_string(),
            };
            eprintln!("  {}     {what}", tint(paint, ansi::CYAN, label));
        }
        eprintln!(
            "  {} {}{}",
            tint(paint, ansi::CYAN, "options dir:"),
            settings.options_dir.display(),
            tint(
                paint,
                ansi::YELLOW,
                if !settings.options_dir.is_dir() { " (new, created on apply)" } else { "" }
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
        match settings.env {
            config::QueryEnv::Jail { package_building } => eprintln!(
                "  {} poudriere jail (host packages under /usr/local not seen{})",
                tint(paint, ansi::CYAN, "environment:"),
                if package_building { "" } else { "; NO_PACKAGE_BUILDING set" }
            ),
            config::QueryEnv::Synth => eprintln!(
                "  {} synth scan slave (host packages under /usr/local not seen)",
                tint(paint, ansi::CYAN, "environment:")
            ),
            config::QueryEnv::Host => {}
        }
        if !settings.blacklist.is_empty() {
            for (i, src) in settings.blacklist.sources.iter().enumerate() {
                let label = if i == 0 { "blacklist:" } else { "          " };
                eprintln!("  {}   {}", tint(paint, ansi::CYAN, label), src.display());
            }
        }
    }

    let t0 = Instant::now();
    let files = optfiles::OptionsFiles::new(&settings.options_dir);
    let mut given = rs.roots.clone();
    if rs.installed {
        // synth roots only the installed packages its flavor index matches:
        // present only when its origin is in synth's own index — each
        // category Makefile's SUBDIR entries (roots::synth_index) — not
        // merely a directory holding a Makefile.
        let synth_index = roots::synth_index(&ctx.portsdir, &ctx.overlays);
        let exists = |o: &str| synth_index.contains(o);
        let origins = given.iter().map(|k| k.origin.clone()).filter(|o| exists(o)).collect();
        let flavors = roots::query_flavors(&origins, &ctx, &files, jobs, &mut cache);
        let (kept, skipped) = roots::synth_installed(&given, exists, &flavors);
        if !skipped.is_empty() && !cli.quiet {
            let list: Vec<String> = skipped.iter().map(|(k, why)| format!("{k} ({why})")).collect();
            eprintln!(
                "  {}        synth skips {} installed package(s) its flavor index does not match: {}",
                tint(paint, ansi::YELLOW, "note:"),
                skipped.len(),
                list.join(", ")
            );
        }
        given = kept;
    }
    let roots =
        roots::resolve(&given, settings.flavor_default_all, &ctx, &files, jobs, &mut cache, &moved);
    let result = scanner::scan(&roots, &ctx, &files, jobs, &mut cache, &moved, &settings.blacklist, move |p| {
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

    // bsd.options.mk:300-304: make.conf may define OPTIONS_OVERRIDE for a
    // port, which decides it outright and leaves its options file unread;
    // named once here so it is not mistaken for one sync merely left alone.
    let overridden: Vec<String> = result
        .ports
        .iter()
        .filter(|(_, i)| i.options.has_options() && i.options.r#override.is_some())
        .map(|(key, _)| key.to_string())
        .collect();
    if !overridden.is_empty() {
        eprintln!(
            "{} make.conf OPTIONS_OVERRIDE decides {} ({} options file not read)",
            tint(paint, ansi::YELLOW, "note:"),
            overridden.join(", "),
            if overridden.len() == 1 { "its" } else { "their" }
        );
    }

    // A file missing lines reads as its defaults today and follows them
    // tomorrow; said once per file, whatever the command (sync completes it).
    let names: std::collections::BTreeSet<&str> = result
        .ports
        .values()
        .filter(|i| i.options.has_options())
        .map(|i| i.options_name.as_str())
        .collect();
    for name in names {
        // make reads options.local after the file (bsd.options.mk:341-363),
        // and so does optique; sync never writes it. Its lines join the
        // file's, UNSET winning across both, so it overrides nothing by
        // itself: the note says what it adds.
        let both = files.combined(name);
        // make runs a `!=` line's shell command; optique does not.
        for (file, line) in &both.shell {
            eprintln!(
                "{} {name}/{file} line {line} assigns with != (a shell command): optique ignores it",
                tint(paint, ansi::YELLOW, "warning:"),
            );
        }
        // An options.local assigning a list afresh drops the file's lines:
        // optique leaves the file alone (apply::plan_writes), said once.
        if let Some(why) = both.local_reset() {
            eprintln!("{} {name}/options: {why}", tint(paint, ansi::YELLOW, "warning:"));
            continue;
        }
        let mut adds = Vec::new();
        for (verb, list) in [("sets", &both.local_set), ("unsets", &both.local_unset)] {
            if !list.is_empty() {
                adds.push(format!("{verb} {}", list.iter().cloned().collect::<Vec<_>>().join(" ")));
            }
        }
        if !adds.is_empty() {
            eprintln!(
                "{} {name}/options.local also {}; sync leaves it alone",
                tint(paint, ansi::CYAN, "note:"),
                adds.join(" and ")
            );
        }
        let Some(saved) = files.saved(name) else { continue };
        let unlined = saved.unlined();
        if !unlined.is_empty() {
            eprintln!(
                "{} {name}/options has no SET or UNSET line for {}: make reads them at \
                 their defaults; sync writes them out",
                tint(paint, ansi::YELLOW, "warning:"),
                unlined.join(" ")
            );
        }
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
        roots,
        result,
        dep_loops,
        elapsed: t0.elapsed().as_secs_f32(),
        staging,
        files,
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
    color_for(cli, std::io::stderr().is_terminal())
}

/// Whether to paint a stream, given whether it is a terminal.
fn color_for(cli: &Cli, is_terminal: bool) -> bool {
    cli::use_color(cli.color, is_terminal, std::env::var("NO_COLOR").ok().as_deref())
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
    color_for(cli, std::io::stdout().is_terminal())
}

/// Scan and report. Returns how many things a human still has to look at —
/// ports needing a decision (see `Row::needs_attention`), dependency loops
/// that are not entirely blacklisted, and ports whose query failed — which
/// main turns into exit code 1.
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
        &scanned.roots,
        &scanned.files,
        cli.minimal,
    );

    struct Row<'a> {
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
        /// The overlay answering for the port, when not the main tree.
        overlay: Option<&'a config::Overlay>,
        /// Only under `--options`: what a caller needs to decide a value.
        detail: Option<PortDetail>,
        /// Under -s: why synth would refuse to build with the saved file.
        synth_refuses: Option<String>,
    }
    /// The `--options` payload of one port.
    struct PortDetail {
        violations: Vec<String>,
        options: Vec<report::OptionReport>,
    }
    impl Row<'_> {
        /// Listed as done: see `shown_ok`.
        fn shown_ok(&self, minimal: bool) -> bool {
            shown_ok(&self.status, minimal, self.mc_covered())
        }
        /// make.conf already dictates every option this port still owes an
        /// answer for, so no human has to decide anything.
        fn mc_covered(&self) -> bool {
            self.undecided.is_empty()
        }
        /// Does this row make `scan` exit 1? See [`needs_attention`].
        fn needs_attention(&self) -> bool {
            needs_attention(
                &self.status,
                self.blacklisted,
                self.mc_covered(),
                self.synth_refuses.is_some(),
            )
        }
        fn status_str(&self) -> &'static str {
            match self.status {
                UiStatus::Conflict => "conflict",
                UiStatus::Unconfigured => "unconfigured",
                UiStatus::Stale => "stale",
                // McDeviation needs the TUI's w view, which a scan never has.
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
        // synth refuses the whole build over one such file, options or not.
        let synth_refuses = (settings.env == config::QueryEnv::Synth)
            .then(|| synth_refuses_port(&scanned.files, info))
            .flatten();
        if !info.options.has_options() && synth_refuses.is_none() {
            hidden += 1;
            continue;
        }
        let saved = sess.state(info).and_then(|s| s.saved.as_ref());
        let status = sess.status(info);
        let (added, removed) = if status == UiStatus::Stale {
            sess.stale_delta(info).unwrap_or_default()
        } else {
            (Vec::new(), Vec::new())
        };
        let undecided = session::undecided_options(info, saved, cli.minimal);
        let state = if cli.verbose {
            session::signed_options(&info.options.complete, &info.options.effective)
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
            blacklisted: settings.blacklist.matches(key, info.is_default_flavor()),
            in_loop: dep_loops.iter().any(|dl| dl.contains(key)),
            options_name: info.options_name.clone(),
            overlay: settings.overlay_of(&key.origin),
            detail: with_options.then(|| PortDetail {
                violations: sess.violations(info),
                options: report::option_reports(&sess, info),
            }),
            synth_refuses,
        });
    }
    rows.sort_by(|a, b| {
        (a.shown_ok(cli.minimal), &a.key).cmp(&(b.shown_ok(cli.minimal), &b.key))
    });

    let unconfigured = rows.iter().filter(|r| r.status == UiStatus::Unconfigured).count();
    let stale = rows.iter().filter(|r| r.status == UiStatus::Stale).count();
    let conflict = rows.iter().filter(|r| r.status == UiStatus::Conflict).count();
    let ok = rows.iter().filter(|r| r.status_str() == "ok").count();
    let blacklisted = rows.iter().filter(|r| r.blacklisted).count();
    // A loop every member of which is blacklisted blocks nothing here:
    // poudriere never builds any of them.
    let loop_blacklisted = |dl: &deps::DepLoop| {
        dl.members.iter().all(|k| {
            let default_flavor = sess.ports.get(k).is_none_or(PortInfo::is_default_flavor);
            settings.blacklist.matches(k, default_flavor)
        })
    };
    let blocking_loops = dep_loops.iter().filter(|dl| !loop_blacklisted(dl)).count();
    let attention = rows.iter().filter(|r| r.needs_attention()).count();
    // A port whose query failed is missing from the closure, with whatever
    // it depends on: neither it nor they were checked, so the gate stays
    // shut until it can be (run_scan already named each one on stderr).
    let query_errors = &scanned.result.errors;
    // The gate: ports owing a decision, loops a human has to untangle, and
    // ports that could not be checked at all.
    let pending = attention + blocking_loops + query_errors.len();

    if json {
        // stdout must stay pure JSON: one object, no table, quiet ignored.
        let report = report::ScanReport {
            options_dir: settings.options_dir.display().to_string(),
            ports_tree: settings.portsdir.display().to_string(),
            overlays: settings.overlays.iter().map(|o| o.path.display().to_string()).collect(),
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
                    overlay: r.overlay.map(|o| o.path.display().to_string()),
                    synth_refuses: r.synth_refuses.clone(),
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
                errors: query_errors.len(),
                pending,
            },
            errors: query_errors
                .iter()
                .map(|(key, msg)| report::ErrorEntry { subject: key.to_string(), message: msg.clone() })
                .collect(),
        };
        report::print(&report)?;
    } else if !cli.quiet {
        let color = stdout_color(cli);
        for row in &rows {
            let (key, pkgname) = (&row.key, &row.pkgname);
            let decision = if row.mc_covered() {
                " [mc-covered ≈]".to_string()
            } else {
                format!(" undecided: {}", row.undecided.join(" "))
            };
            let (marker, text) = match (&row.status, row.shown_ok(cli.minimal)) {
                // Under -m the defaults are the decision: a port they settle
                // is done, its file (or lack of one) left as sync will.
                (UiStatus::Unconfigured, true) => {
                    ("", "ok (no file: defaults + make.conf)".to_string())
                }
                (UiStatus::Stale, true) => {
                    ("", format!("ok (stale file{}: defaults + make.conf decide)", row.stale_detail()))
                }
                (UiStatus::Unconfigured, false) => ("?", format!("UNCONFIGURED{decision}")),
                (UiStatus::Stale, false) => {
                    ("!", format!("STALE{}{decision}", row.stale_detail()))
                }
                (UiStatus::Conflict, _) => {
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
            // Only where an overlay, not the main tree, answers for it.
            if let Some(why) = &row.synth_refuses {
                tail.push_str(&format!(" [synth refuses the file: {why}]"));
            }
            if let Some(o) = row.overlay {
                tail.push_str(&format!(" [overlay {}]", o.label()));
            }
            // Pad first, tint after: the escapes must not count as width.
            let cell = match marker_color(marker).filter(|_| color) {
                Some(c) => format!("{c}{marker:<2}{}", ansi::RESET),
                None => format!("{marker:<2}"),
            };
            outln!("{cell} {key:<40} {pkgname:<32} {text}{tail}")?;
            if cli.verbose {
                if !row.state.is_empty() {
                    outln!("     options: {}", row.state)?;
                }
                for w in &row.warnings {
                    outln!("     warning: {w}")?;
                }
            }
        }
    }

    // Loops are counted, not re-listed: run_scan already named every one.
    let mut loop_note = match dep_loops.len() {
        0 => String::new(),
        n => format!(" · {n} dependency loop{}", if n == 1 { "" } else { "s" }),
    };
    match query_errors.len() {
        0 => {}
        n => loop_note.push_str(&format!(" · {n} port{} not checked (query failed)", if n == 1 { "" } else { "s" })),
    }
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
/// A file that cannot be written stops the whole pass (see `apply::apply`).
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
        let done = apply::commit(&pass.writes, &pass.removals);
        written = done.summary.written;
        removed = done.removed.count;
        notes = done.removed.notes;
        let failed = done.summary.failed.into_iter().map(|(key, m)| (key.to_string(), m));
        for (subject, message) in failed.chain(done.removed.failed) {
            errors.push(report::ErrorEntry { subject, message });
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
            .map(report::RemovalEntry::from)
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

#[cfg(test)]
mod commit_writes_tests {
    /// A pass whose write failed removes nothing either: the options dir is
    /// left exactly as it was found.
    #[test]
    fn a_failed_write_removes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dead = tmp.path().join("cat_dead");
        std::fs::create_dir_all(&dead).unwrap();
        std::fs::write(dead.join("options"), "x\n").unwrap();
        std::fs::create_dir_all(tmp.path().join("cat_port/.options.optique.tmp")).unwrap();
        let write = crate::apply::PendingWrite {
            key: crate::model::origin::PortKey::parse("cat/port").unwrap(),
            options_name: "cat_port".into(),
            path: tmp.path().join("cat_port/options"),
            old: None,
            enabled: Default::default(),
            complete: vec![],
            kept: vec![],
            content: "x\n".into(),
        };
        let removal = crate::clean::Removal {
            options_name: "cat_dead".into(),
            dir: dead.clone(),
            reason: "test".into(),
        };
        let pass = super::WritePass::new(vec![write], vec![removal], Vec::new());
        let report = super::commit_writes(&pass, &Default::default(), tmp.path(), false);
        assert_eq!(report.summary.failed, 1);
        assert_eq!(report.summary.removed, 0);
        assert!(dead.join("options").exists(), "the removal waited for the writes");
    }
}

fn cmd_sync(cli: &Cli, rs: &RootSet, dry_run: bool, json: bool) -> Result<()> {
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
        eprintln!("{} {w}", tint(paint, ansi::YELLOW, "warning:"));
    }

    if pass.nothing_to_do() && !json {
        eprintln!("everything up to date, nothing to write");
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
                outln!("     final: {}", session::signed_options(&w.complete, &w.enabled))?;
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
    print_write_summary(&pass, &report, &settings.options_dir, dry_run);
    Ok(())
}

/// The closing stderr line of a write pass, for sync and decide alike.
fn print_write_summary(pass: &WritePass, report: &report::WriteReport, dir: &Path, dry_run: bool) {
    if dry_run {
        eprintln!(
            "dry run: {} file(s) would be written, {} removed in {}",
            pass.writes.len(),
            pass.removals.len(),
            dir.display()
        );
        return;
    }
    let failed = match report.summary.failed {
        0 => String::new(),
        n => format!(", {n} failed"),
    };
    eprintln!(
        "{} file(s) written, {} removed in {}{failed}",
        report.summary.written,
        report.summary.removed,
        dir.display()
    );
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
            installed: false,
        }
    } else {
        RootSet {
            roots: cli::collect_roots(&args.roots.origins, &cli.files)?,
            notes: Vec::new(),
            installed: false,
        }
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

    let report = commit_writes(&pass, &sess.ports, &settings.options_dir, cli.dry_run);
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
    print_write_summary(&pass, &report, &settings.options_dir, cli.dry_run);
    Ok(if report.summary.failed > 0 { 1 } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::split_raw_origins;

    /// synth refuses a file with an entry the port lacks, or with more or
    /// fewer entries than the port has options, every SET or UNSET line
    /// counted; such a file trips scan's gate whatever its status.
    #[test]
    fn a_file_synth_refuses_trips_the_gate() {
        use super::{needs_attention, synth_refuses};
        use crate::session::UiStatus;
        let opts: Vec<String> = ["A", "B"].iter().map(|s| s.to_string()).collect();
        let file = |body: &str| {
            format!("# This file is auto-generated by 'make config'.\n_OPTIONS_READ=xport-1.0\n{body}")
        };
        let both = file("OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n");
        assert_eq!(synth_refuses(both.as_bytes(), &opts), None);
        let gone = file("OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=GONE\n");
        assert_eq!(
            synth_refuses(gone.as_bytes(), &opts).as_deref(),
            Some("saved GONE is no option of the port")
        );
        let short = file("OPTIONS_FILE_SET+=A\n");
        assert_eq!(
            synth_refuses(short.as_bytes(), &opts).as_deref(),
            Some("1 saved entries for the port's 2 options")
        );
        // A repeated line is one more entry, as synth counts it.
        let twice = file("OPTIONS_FILE_SET+=A\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n");
        assert_eq!(
            synth_refuses(twice.as_bytes(), &opts).as_deref(),
            Some("3 saved entries for the port's 2 options")
        );
        // A port with no options at all and a stray entry.
        assert!(synth_refuses(both.as_bytes(), &[]).is_some());

        // The gate: a refused file trips it even when the port is ok or
        // make.conf covers what is open; a blacklisted port never does.
        assert!(needs_attention(&UiStatus::Ok, false, true, true));
        assert!(needs_attention(&UiStatus::Stale, false, true, true));
        assert!(!needs_attention(&UiStatus::Ok, false, true, false));
        assert!(!needs_attention(&UiStatus::Stale, false, true, false));
        assert!(needs_attention(&UiStatus::Conflict, false, true, false));
        assert!(!needs_attention(&UiStatus::Conflict, true, true, true));
    }

    /// `passed_options_cache_check` indexes every line at a fixed byte
    /// offset with no exception handler anywhere up to the run that calls
    /// it once per build (portscan-packages.adb ~839): a line shorter than
    /// 18 bytes aborts synth outright, so optique refuses the file rather
    /// than silently skipping that line as too short to hold a marker.
    #[test]
    fn synth_refuses_aborts_on_a_short_line() {
        use super::synth_refuses;
        let opts: Vec<String> = vec!["A".to_string()];
        let file = "# This file is auto-generated by 'make config'.\n\
                     _OPTIONS_READ=xport-1.0\n\
                     short\n\
                     OPTIONS_FILE_SET+=A\n";
        assert_eq!(
            synth_refuses(file.as_bytes(), &opts).as_deref(),
            Some("synth aborts on a line shorter than 18 bytes (line 3)")
        );
        // A single trailing newline is not an extra, empty line: it must
        // not itself be read as "shorter than 18 bytes".
        let clean = "# This file is auto-generated by 'make config'.\n\
                      _OPTIONS_READ=xport-1.0\n\
                      OPTIONS_FILE_SET+=A\n";
        assert_eq!(synth_refuses(clean.as_bytes(), &opts), None);
        // 18 or 19 bytes: the second probe, `Line (19 .. 20)`, runs past
        // the end unless the first one found the marker.
        let nineteen = "_OPTIONS_READ=x-1.0\nOPTIONS_FILE_SET+=A\n";
        assert_eq!(
            synth_refuses(nineteen.as_bytes(), &opts).as_deref(),
            Some("synth aborts on a line of 19 bytes without += at 17-18 (line 1)")
        );
    }

    /// `synth_refuses_port` reads `<category>_<name>/options` by origin,
    /// never by the port's own `OPTIONS_NAME` (which a flavor or an
    /// OPTIONS_SUB port names differently): synth's own lookup
    /// (portscan-packages.adb ~816) builds that path from the catport, not
    /// from anything the port's Makefile sets.
    #[test]
    fn synth_refuses_port_reads_by_origin_not_options_name() {
        use super::synth_refuses_port;
        use crate::model::port::PortInfo;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("misc_port")).unwrap();
        std::fs::write(
            tmp.path().join("misc_port/options"),
            b"# This file is auto-generated by 'make config'.\n\
              _OPTIONS_READ=xport-1.0\n\
              OPTIONS_FILE_SET+=A\n\
              OPTIONS_FILE_UNSET+=GONE\n",
        )
        .unwrap();
        let files = crate::optfiles::OptionsFiles::new(tmp.path());
        let mut info = PortInfo::stub("misc/port");
        info.options.complete = vec!["A".to_string()];
        // A deliberately different OPTIONS_NAME: the file above must still
        // be read, by the origin alone.
        info.options_name = "something_else".to_string();
        assert_eq!(
            synth_refuses_port(&files, &info).as_deref(),
            Some("saved GONE is no option of the port")
        );
    }

    /// A flavored port's file, named `<category>_<name>@<flavor>/options`,
    /// never exists under the plain by-origin path synth checks, so it is
    /// never refused — whatever that path holds, even a file that would
    /// trip a flavorless port.
    #[test]
    fn synth_refuses_port_never_refuses_a_flavored_port() {
        use super::synth_refuses_port;
        use crate::model::port::PortInfo;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("misc_port")).unwrap();
        std::fs::write(tmp.path().join("misc_port/options"), b"short\n").unwrap();
        let files = crate::optfiles::OptionsFiles::new(tmp.path());
        let mut info = PortInfo::stub("misc/port");
        info.flavors = vec!["full".to_string()];
        assert_eq!(synth_refuses_port(&files, &info), None);
    }

    /// A shared file is redundant only when no flavor reading it deviates:
    /// www/sogo@pgsql defaults to PGSQL, the file turns MYSQL on instead.
    #[test]
    fn a_shared_file_deviating_for_another_flavor_is_not_redundant() {
        use crate::model::port::PortInfo;
        let flavor = |f: &str, default: &str| {
            let mut info = PortInfo::stub(&format!("www/sogo@{f}"));
            info.flavors = vec!["mysql".into(), "pgsql".into()];
            info.options.complete = vec!["MYSQL".into(), "PGSQL".into()];
            info.options.defaults = [default.to_string()].into();
            // Queried with the file in place: MYSQL on, PGSQL off.
            info.options.effective = ["MYSQL".to_string()].into();
            info
        };
        let (mysql, pgsql) = (flavor("mysql", "MYSQL"), flavor("pgsql", "PGSQL"));
        assert_eq!(super::shared_file_deviation(std::slice::from_ref(&mysql), &Default::default()), None);
        let why = super::shared_file_deviation(&[mysql, pgsql], &Default::default()).expect("@pgsql reads MYSQL from the file");
        assert!(why.contains("www/sogo@pgsql") && why.contains("+MYSQL -PGSQL"), "{why}");
    }

    /// bsd.options.mk:300-304: a flavor with an OPTIONS_OVERRIDE never
    /// reads its options file, so `clean --redundant` must never judge it
    /// redundant — even though its effective set, computed the override's
    /// own way, would otherwise match defaults + make.conf exactly, which
    /// is precisely what "redundant" means for every other port.
    #[test]
    fn an_overridden_flavors_file_is_never_redundant() {
        use crate::model::port::PortInfo;
        let mut info = PortInfo::stub("misc/cond");
        info.options.complete = vec!["DOCS".into()];
        info.options.defaults = ["DOCS".into()].into();
        info.options.effective = ["DOCS".into()].into();
        info.options.r#override = Some(["DOCS".to_string()].into());
        let why = super::shared_file_deviation(std::slice::from_ref(&info), &Default::default())
            .expect("an override must never be judged redundant");
        assert!(why.contains("OPTIONS_OVERRIDE"), "{why}");
    }

    /// Under -m a settled unconfigured or stale port shows as ok and sorts
    /// with the ok ones: a conflict comes before it, not among it.
    #[test]
    fn minimal_scan_lists_problems_first() {
        use super::shown_ok;
        use crate::session::UiStatus;
        let mut rows = [
            ("a/nofile", UiStatus::Unconfigured, true),
            ("m/stale", UiStatus::Stale, true),
            ("z/conflict", UiStatus::Conflict, false),
            ("b/ok", UiStatus::Ok, true),
        ];
        rows.sort_by_key(|(k, st, cov)| (shown_ok(st, true, *cov), k.to_string()));
        let order: Vec<&str> = rows.iter().map(|r| r.0).collect();
        assert_eq!(order, ["z/conflict", "a/nofile", "b/ok", "m/stale"]);
        // Without -m an unconfigured port is a problem, covered or not.
        assert!(!shown_ok(&UiStatus::Unconfigured, false, true));
        assert!(!shown_ok(&UiStatus::Unconfigured, true, false), "-m, something open");
        assert!(!shown_ok(&UiStatus::Conflict, true, true));
    }
    use super::parse_installed;

    #[test]
    fn installed_list_flavors_and_repo_filter() {
        let origins = "nginx\twww/nginx\tpoudriere\n\
                       ImageMagick7-nox11\tgraphics/ImageMagick7\tSynth\n\
                       binutils\tdevel/binutils\tSynth\n\
                       nginx\twww/nginx\tpoudriere\n\
                       junk\tnot-an-origin\tSynth\n";
        let annots = "ImageMagick7-nox11\tflavor\tnox11\n\
                      binutils\tflavor\tnative\n\
                      nginx\tcpe\tsomething\n";
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

    /// Several flavors of one origin installed side by side are each a
    /// root: keyed by origin, all but the last annotated one were dropped
    /// (183 of 2230 packages on one host).
    #[test]
    fn installed_list_keeps_every_flavor_of_one_origin() {
        let origins = "polkit-qt5-1\tsysutils/polkit-qt-1\tSynth\n\
                       polkit-qt6-1\tsysutils/polkit-qt-1\tSynth\n\
                       gpu-firmware-amd-kmod-aldebaran\tgraphics/gpu-firmware-amd-kmod\tSynth\n\
                       gpu-firmware-amd-kmod-arcturus\tgraphics/gpu-firmware-amd-kmod\tSynth\n";
        let annots = "polkit-qt5-1\tflavor\tqt5\n\
                      polkit-qt6-1\tflavor\tqt6\n\
                      gpu-firmware-amd-kmod-aldebaran\tflavor\taldebaran\n\
                      gpu-firmware-amd-kmod-arcturus\tflavor\tarcturus\n";
        let names: Vec<String> =
            parse_installed(origins, annots, &[]).iter().map(|k| k.to_string()).collect();
        assert_eq!(
            names,
            vec![
                "graphics/gpu-firmware-amd-kmod@aldebaran",
                "graphics/gpu-firmware-amd-kmod@arcturus",
                "sysutils/polkit-qt-1@qt5",
                "sysutils/polkit-qt-1@qt6",
            ]
        );
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
