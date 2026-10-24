//! The commands, and the scan they share: settings resolved, roots read
//! through MOVED, the closure queried, what the run found said on stderr.

use std::io::Write as _;
use std::path::Path;
use std::time::Instant;

use anyhow::Result;

use crate::cli::Cli;
use crate::color::{ansi, stderr_color, tint};
use crate::moved::Moved;
use crate::query::makerunner::QueryCtx;
use crate::query::scanner::{self, ScanResult};
use crate::{cache, cli, config, deps, model, moved, optfiles, roots, synth};

mod clean;
mod decide;
mod scan;
mod sync;
mod tui;
mod write;

pub(crate) use clean::cmd_clean;
pub(crate) use decide::cmd_decide;
pub(crate) use scan::cmd_scan;
pub(crate) use sync::cmd_sync;
pub(crate) use tui::cmd_tui;

/// What the make queries need from the settings; `port_dbdir` is where they
/// read options files from.
fn query_ctx(settings: &config::Settings, port_dbdir: std::path::PathBuf) -> QueryCtx {
    QueryCtx {
        portsdir: settings.portsdir.clone(),
        make_conf: settings.make_conf.clone(),
        port_dbdir,
        overlays: settings.overlay_paths(),
        identity: settings.env.identity(),
        framework: settings.framework.clone(),
    }
}

/// The resolved root list plus notes destined for the startup banner's
/// aligned `note:` block (nothing is printed here).
pub(crate) struct RootSet {
    roots: Vec<roots::Listed>,
    notes: Vec<String>,
    /// The roots are synth's implicit list, the installed packages, which
    /// the scan matches against the tree as synth does.
    installed: bool,
}

/// The roots a list given names. Under -s too it is read with poudriere's
/// syntax: entries synth's stricter one refuses are synth's input to fix,
/// not optique's to report.
pub(super) fn listed_roots(origins: &[String], files: &[std::path::PathBuf]) -> Result<RootSet> {
    Ok(RootSet {
        roots: cli::collect_roots(origins, files)?,
        notes: Vec::new(),
        installed: false,
    })
}

/// Roots for a subcommand: the given list, or — in synth mode only — the
/// installed packages when nothing was given.
pub(super) fn roots_or_installed(cli: &Cli, origins: &[String]) -> Result<RootSet> {
    if origins.is_empty() && cli.files.is_empty() && cli.synth.is_some() {
        let roots = synth::installed_roots(&cli.repo)?;
        let note = if cli.repo.is_empty() {
            format!(
                "no ports given; using {} installed package(s) as the list",
                roots.len()
            )
        } else {
            format!(
                "no ports given; using {} installed package(s) from {}",
                roots.len(),
                synth::repos_phrase(&cli.repo)
            )
        };
        let roots = roots.into_iter().map(roots::Listed::port).collect();
        return Ok(RootSet {
            roots,
            notes: vec![note],
            installed: true,
        });
    }
    listed_roots(origins, &cli.files)
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
    /// The listed origins MOVED moves (already said on stderr): followed
    /// under --follow-moved, else left out of the closure and pending.
    moves: Vec<moved::ListedMove>,
    jobs: usize,
}

impl Scanned {
    /// The listed origins MOVED moves that were not followed: each one
    /// pending, the list needing a fix.
    fn flagged_moves(&self) -> usize {
        self.moves.iter().filter(|m| !m.followed).count()
    }
}

/// Parallel `make` jobs: -J, else min(16, ncpu).
fn default_jobs(cli: &Cli) -> usize {
    cli.jobs.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(std::num::NonZero::get)
            .unwrap_or(4)
            .min(16)
    })
}

/// Open the persistent query cache for this tree + make.conf pair (or a
/// no-op cache under --no-cache). Carrying entries over an edit to
/// make.conf asks make for the option variables it touched, under the old
/// text and the new, each staged in a temporary file.
fn new_cache(cli: &Cli, settings: &config::Settings) -> cache::Cache {
    if cli.no_cache {
        return cache::Cache::disabled();
    }
    let trees = cache::trees(&settings.portsdir, &settings.overlay_paths());
    let identity = settings.env.identity();
    let dir = config::probe_dir(&settings.portsdir);
    let probe = |text: &str, vars: &[String]| {
        let file = tempfile::NamedTempFile::new().ok()?;
        std::fs::write(file.path(), text).ok()?;
        crate::query::makerunner::make_conf_values(identity, file.path(), &dir, vars)
    };
    let conf = cache::Conf {
        hash: &settings.conf_hash,
        text: &settings.conf_text,
        rest: &settings.conf_rest,
        probe: Some(&probe),
        carry: !cli.no_carry_over,
    };
    cache::Cache::open_trees(&cache::default_cache_dir(), &trees, conf)
}

/// MOVED of the main tree and every overlay, its warnings said once.
fn load_moved(cli: &Cli, settings: &config::Settings) -> Moved {
    let overlays = settings.overlay_paths();
    let trees: Vec<&Path> = std::iter::once(settings.portsdir.as_path())
        .chain(overlays.iter().map(std::path::PathBuf::as_path))
        .collect();
    let (moved, warnings) = Moved::load_all(&trees);
    let paint = stderr_color(cli);
    for w in warnings {
        eprintln!("{} {w}", tint(paint, ansi::YELLOW, "warning:"));
    }
    moved
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
        print_banner(paint, rs, &settings, &cache, jobs);
    }
    // Writing to the options dir is harmless, only useless: said, even
    // under -q, and the run goes on (scan counts it among its errors).
    if let Some(e) = settings.port_dbdir_error() {
        eprintln!("{} {e}", tint(paint, ansi::RED, "error:"));
    }

    let t0 = Instant::now();
    let files = optfiles::OptionsFiles::new(&settings.options_dir);
    let mut given = rs.roots.clone();
    if rs.installed {
        let installed: Vec<_> = given.iter().map(|l| l.key.clone()).collect();
        let (kept, skipped) = synth::installed_in_index(&installed, &ctx, &files, jobs, &mut cache);
        if !skipped.is_empty() && !cli.quiet {
            let list: Vec<String> = skipped
                .iter()
                .map(|(k, why)| format!("{k} ({why})"))
                .collect();
            eprintln!(
                "  {}        synth skips {} installed package(s) its flavor index does not match: {}",
                tint(paint, ansi::YELLOW, "note:"),
                skipped.len(),
                list.join(", ")
            );
        }
        given = kept.into_iter().map(roots::Listed::port).collect();
    }
    // MOVED, read for the listed origins as the build reads it, each time:
    // never cached, as the cache's carry-over takes MOVED to be read per run.
    let policy = roots::MovedPolicy::from(settings.env);
    let exists = |o: &str| ctx.locate(o).is_some();
    let listing = roots::apply_moved(&given, policy, cli.follow_moved, &moved, exists);
    if cli.follow_moved && policy == roots::MovedPolicy::Synth && !cli.quiet {
        eprintln!(
            "  {}        --follow-moved has no effect under -s: synth does not read MOVED",
            tint(paint, ansi::YELLOW, "note:")
        );
    }
    let roots = roots::resolve(
        &listing.roots,
        settings.flavor_default_all,
        &ctx,
        &files,
        jobs,
        &mut cache,
    );
    let mut result = scanner::scan(
        &roots,
        &ctx,
        &files,
        jobs,
        &mut cache,
        &moved,
        &settings.blacklist,
        move |p| {
            let line = format!(
                "scanning… {}/{} ports ({} cached)",
                p.done, p.discovered, p.from_cache
            );
            eprint!("\r{}", tint(paint, ansi::GRAY, &line));
            let _ = std::io::stderr().flush();
        },
    );
    eprintln!();

    result.errors.splice(0..0, listing.errors);
    for m in &listing.moves {
        eprintln!("{} {}", tint(paint, ansi::MAGENTA, "moved:"), m.message());
    }
    for (key, msg) in &result.errors {
        eprintln!("{} {key}: {msg}", tint(paint, ansi::RED, "error:"));
    }

    say_overridden(paint, &result);
    let read = say_moved_files(paint, &result);
    say_options_files(paint, &result, &files, &read);

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
        moves: listing.moves,
        jobs,
    })
}

/// The startup banner: the tree and the jobs, the notes, the overlays, then
/// where options, make.conf, the environment and the blacklist come from.
fn print_banner(
    paint: bool,
    rs: &RootSet,
    settings: &config::Settings,
    cache: &cache::Cache,
    jobs: usize,
) {
    eprintln!(
        "{} ports tree {} · {} jobs",
        tint(paint, ansi::BOLD, "optique:"),
        settings.portsdir.display(),
        jobs
    );
    let notes = rs
        .notes
        .iter()
        .chain(settings.notes.iter())
        .map(String::as_str);
    for note in notes.chain(cache.note()) {
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
            if !settings.options_dir.is_dir() {
                " (new, created on apply)"
            } else {
                ""
            }
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
    if let Some(env) = settings.env.description() {
        eprintln!("  {} {env}", tint(paint, ansi::CYAN, "environment:"));
    }
    if !settings.blacklist.is_empty() {
        for (i, src) in settings.blacklist.sources.iter().enumerate() {
            let label = if i == 0 { "blacklist:" } else { "          " };
            eprintln!("  {}   {}", tint(paint, ansi::CYAN, label), src.display());
        }
    }
}

/// The ports make.conf decides outright with OPTIONS_OVERRIDE, named once.
fn say_overridden(paint: bool, result: &ScanResult) {
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
            if overridden.len() == 1 {
                "its"
            } else {
                "their"
            }
        );
    }
}

/// The ports whose OPTIONS_FILE lies outside the options dir, each said
/// once; returns the options files make still reads.
fn say_moved_files(paint: bool, result: &ScanResult) -> std::collections::BTreeSet<&str> {
    // bsd.options.mk:183: OPTIONS_FILE names a file outside the options dir
    // (make.conf, or the port's Makefile, set it), so make reads neither the
    // file optique manages nor its options.local. Per port: a flavor moved
    // elsewhere leaves the shared file to those still reading it
    // (`session::moved`). One inside the options dir is a file optique
    // manages like any other (`makerunner::adopt_options_file`).
    let with_options = || result.ports.values().filter(|i| i.options.has_options());
    let read: std::collections::BTreeSet<&str> = with_options()
        .filter(|i| i.options.moved_file.is_none())
        .map(|i| i.options_name.as_str())
        .collect();
    let mut moved_files: std::collections::BTreeMap<&str, Vec<(String, &str)>> = Default::default();
    for info in with_options() {
        if let Some(path) = &info.options.moved_file {
            moved_files
                .entry(info.options_name.as_str())
                .or_default()
                .push((info.canonical.to_string(), path));
        }
    }
    for (name, moved) in &moved_files {
        let mut said = std::collections::BTreeSet::new();
        for (port, path) in moved {
            if read.contains(name) {
                eprintln!(
                    "{} OPTIONS_FILE is {path} for {port}, outside the options dir: make reads \
                     that file, not {}, for it, and optique leaves its options alone",
                    tint(paint, ansi::YELLOW, "warning:"),
                    optfiles::key_label(name),
                );
            } else if said.insert(*path) {
                eprintln!(
                    "{} OPTIONS_FILE is {path}, outside the options dir: make does not read {}, \
                     and optique leaves it alone",
                    tint(paint, ansi::YELLOW, "warning:"),
                    optfiles::key_label(name),
                );
            }
        }
    }
    read
}

/// What each options file make reads holds that a build may not expect:
/// lines optique cannot model, an options.local, options with no line.
fn say_options_files(
    paint: bool,
    result: &ScanResult,
    files: &optfiles::OptionsFiles,
    read: &std::collections::BTreeSet<&str>,
) {
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
        if !read.contains(name) {
            continue;
        }
        let both = files.combined(name);
        let file = optfiles::key_label(name);
        // make reads every line of both files, a `!=` running its shell
        // command; a line optique cannot model leaves the file to the user
        // (apply::plan_writes), each line named once.
        for u in &both.unmodeled {
            eprintln!(
                "{} {} line {} {}: optique leaves {file} alone",
                tint(paint, ansi::YELLOW, "warning:"),
                if u.file == "options" {
                    file.clone()
                } else {
                    format!("{file}.local")
                },
                u.line,
                u.what,
            );
        }
        if !both.unmodeled.is_empty() {
            continue;
        }
        // An options.local assigning a list afresh drops the file's lines:
        // optique leaves the file alone (apply::plan_writes), said once.
        if let Some((_, why)) = both.unmanaged() {
            eprintln!("{} {file}: {why}", tint(paint, ansi::YELLOW, "warning:"));
            continue;
        }
        let mut adds = Vec::new();
        for (verb, list) in [("sets", &both.local_set), ("unsets", &both.local_unset)] {
            if !list.is_empty() {
                adds.push(format!(
                    "{verb} {}",
                    list.iter().cloned().collect::<Vec<_>>().join(" ")
                ));
            }
        }
        if !adds.is_empty() {
            eprintln!(
                "{} {file}.local also {}; sync leaves it alone",
                tint(paint, ansi::CYAN, "note:"),
                adds.join(" and ")
            );
        }
        let Some(saved) = files.saved(name) else {
            continue;
        };
        let unlined = saved.unlined();
        if !unlined.is_empty() {
            eprintln!(
                "{} {file} has no SET or UNSET line for {}: make reads them at \
                 their defaults; sync writes them out",
                tint(paint, ansi::YELLOW, "warning:"),
                unlined.join(" ")
            );
        }
    }
}
