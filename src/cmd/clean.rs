//! `optique clean`: vanished, unused and redundant options-dir entries.

use std::io::Write as _;

use anyhow::Result;

use crate::cli::Cli;
use crate::color::{ansi, stderr_color, tint};
use crate::moved::Moved;
use crate::{cache, clean, cli, config, model, optfiles, optionsfile, report, session};

use super::{
    default_jobs, load_moved, new_cache, query_ctx, resolve_settings, roots_or_installed, run_scan,
};

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
    /// Entries a closure port's moved OPTIONS_FILE has make read, each with
    /// that port (`clean::read_through_moved`); empty without --unused.
    read: std::collections::BTreeMap<String, String>,
    /// Holds the layered make.conf alive until the pass is done.
    _staging: tempfile::TempDir,
}

/// Remove obsolete (and optionally redundant or unused) options files from
/// the resolved options dir. Unlike scan/sync this walks the directory
/// itself; a package list is only consulted for --unused.
pub(crate) fn cmd_clean(cli: &Cli, args: &cli::CleanArgs) -> Result<()> {
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
            read: Default::default(),
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
        // A port that failed to query, or a dependency on a nonexistent
        // origin, leaves the closure short: what it misses would be pruned
        // as unused — refuse rather than delete on partial knowledge.
        anyhow::bail!(
            "{} error(s): the dependency closure is incomplete, \
             refusing to prune entries",
            scanned.result.errors.len()
        );
    }
    if scanned.flagged_moves() > 0 {
        // The port a listed origin moved to is not in the closure, so its
        // options would go as unused while the list only needs a fix.
        anyhow::bail!(
            "{} listed origin(s) moved (see above): the list needs a fix; refusing to prune entries",
            scanned.flagged_moves()
        );
    }
    let used = scanned.result.ports.values().map(|i| i.options_name.clone()).collect();
    // Sorted by port, so the port each note names is the same every run.
    let mut infos: Vec<&model::port::PortInfo> = scanned.result.ports.values().collect();
    infos.sort_by(|a, b| a.key.cmp(&b.key));
    let read = clean::read_through_moved(&scanned.settings.options_dir, infos);
    let ctx = CleanCtx {
        settings: scanned.settings,
        moved: scanned.moved,
        jobs: scanned.jobs,
        cache: Some(scanned.cache),
        used: Some(used),
        read,
        _staging: scanned.staging,
    };
    clean_options_dir(cli, args, ctx)
}

fn clean_options_dir(cli: &Cli, args: &cli::CleanArgs, ctx: CleanCtx) -> Result<()> {
    let CleanCtx { settings, moved, jobs, cache: open_cache, used, mut read, _staging } = ctx;
    let paint = stderr_color(cli);
    eprintln!("{} options dir {}", tint(paint, ansi::BOLD, "optique clean:"), settings.options_dir.display());
    for note in &settings.notes {
        eprintln!("  {}        {note}", tint(paint, ansi::YELLOW, "note:"));
    }
    // --unused had run_scan's banner say it already.
    if used.is_none()
        && let Some(e) = settings.port_dbdir_error()
    {
        eprintln!("{} {e}", tint(paint, ansi::RED, "error:"));
    }

    let (vanished, live, mut warnings) =
        clean::classify_entries(
            &settings.options_dir,
            &settings.portsdir,
            &settings.overlay_paths(),
            &moved,
        );
    let total_entries = vanished.len() + live.len();
    // Only --verbose fills this in; JSON reports it as `kept`.
    let mut kept_entries: Vec<(String, String)> = Vec::new();

    // Entries whose port vanished go, and so do, under --unused, those
    // nobody in the closure reads, unless make.conf relocates options
    // files; the redundancy pass below then only has to look at what is
    // still live.
    let relocated = config::relocated_options(settings.options_place.as_ref());
    warnings.extend(relocated.clone());
    for w in &warnings {
        eprintln!("{} {w}", tint(paint, ansi::YELLOW, "warning:"));
    }
    let pruned = clean::prune(vanished, live, used.as_ref(), relocated.is_some());
    let (mut removals, live) = (pruned.removals, pruned.live);
    if cli.verbose {
        let why = "make.conf relocates options files";
        kept_entries.extend(pruned.held.into_iter().map(|name| (name, why.to_string())));
    }

    // Optionally find files that only repeat defaults + make.conf.
    if args.redundant {
        let cache = open_cache.unwrap_or_else(|| new_cache(cli, &settings));
        let found = find_redundant(cli, &settings, jobs, cache, &live);
        removals.extend(found.removals);
        warnings.extend(found.warnings);
        kept_entries.extend(found.kept);
        for (entry, port) in found.read {
            read.entry(entry).or_insert(port);
        }
    }
    kept_entries.sort();
    if cli.verbose && !args.json {
        for (name, why) in &kept_entries {
            outln!("keep  {name:<38} {why}")?;
        }
    }

    // An entry a moved OPTIONS_FILE has make read is kept whatever its
    // name says, even where the make.conf probe saw no move.
    for note in clean::keep_read(&mut removals, &read) {
        eprintln!("{} {note}", tint(paint, ansi::YELLOW, "warning:"));
        warnings.push(note);
    }

    // Obsolete, unused and redundant removals were collected separately.
    removals.sort_by(|a, b| a.options_name.cmp(&b.options_name));
    // A file optique cannot model is the user's, whatever its port became.
    let files = optfiles::OptionsFiles::new(&settings.options_dir);
    removals.retain(|r| match files.combined(&r.options_name).unmanaged() {
        Some((_, why)) => {
            let note = format!("{}: {why}; kept", r.options_name);
            eprintln!("{} {note}", tint(paint, ansi::YELLOW, "warning:"));
            warnings.push(note);
            false
        }
        None => true,
    });

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
    /// Entries a queried port's moved OPTIONS_FILE has make read, each with
    /// the first such port (`clean::read_through_moved`).
    read: std::collections::BTreeMap<String, String>,
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
        // A flavor make.conf moves OPTIONS_FILE for reads none of the file
        // (`session::moved`); the file is judged for the others alone, and
        // kept when none is left.
        let (moved, readers): (Vec<PortInfo>, Vec<PortInfo>) =
            f.infos.into_iter().partition(|i| session::moved(i).is_some());
        if readers.is_empty() {
            if let (true, Some(u)) = (cli.verbose, moved.first().and_then(session::moved)) {
                found.kept.push((f.entry.options_name.clone(), u.why));
            }
            return;
        }
        // A file optique cannot model: not its to remove.
        if let Some(u) = session::unmanaged_file(&files.combined(&f.entry.options_name)) {
            if cli.verbose {
                found.kept.push((f.entry.options_name.clone(), u.why));
            }
            return;
        }
        let local = files.under_local(&f.entry.options_name, Default::default());
        match shared_file_deviation(&readers, &local) {
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
            if let Ok(info) = &result {
                // Answers arrive in any order: the smallest port names it.
                for (entry, port) in clean::read_through_moved(&settings.options_dir, [info]) {
                    let named = found.read.entry(entry).or_insert_with(|| port.clone());
                    if port < *named {
                        *named = port;
                    }
                }
            }
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
                // A deviating default flavor settles it: no need to ask the
                // others. Not one reading a file of its own (`session::moved`).
                let local = files.under_local(&entry.options_name, Default::default());
                let deviates = session::moved(&f.infos[0]).is_none()
                    && shared_file_deviation(&f.infos, &local).is_some();
                if others.is_empty() || deviates {
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
    infos.iter().find_map(|info| {
        let diff = clean::redundancy_diff(info, local);
        if diff.is_empty() {
            None
        } else if info.is_default_flavor() {
            Some(format!("deviates from defaults + make.conf: {}", diff.join(" ")))
        } else {
            Some(format!("deviates from defaults + make.conf for {}: {}", info.key, diff.join(" ")))
        }
    })
}

#[cfg(test)]
mod tests {
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
}
