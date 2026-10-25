//! `optique clean`: vanished, unused and redundant options-dir entries.

use std::io::Write as _;

use anyhow::Result;

use crate::cli::Cli;
use crate::color::{ansi, stderr_color, tint};
use crate::model::origin::PortKey;
use crate::model::port::PortInfo;
use crate::moved::Moved;
use crate::{cache, clean, cli, config, optfiles, optionsfile, report, session};

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
        errln!(
            "{} --minimal does not affect clean; its counterpart here is --redundant",
            tint(stderr_color(cli), ansi::YELLOW, "note:")
        );
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
    // The entries the closure reads: a key one entry deep
    // (`devel_git/lite-options`) uses its entry, and itself, an entry of
    // its own to clean (`clean::classify_entries`).
    let used = scanned
        .result
        .ports
        .values()
        .flat_map(|i| {
            [
                optfiles::key_entry(&i.options_name).to_string(),
                i.options_name.clone(),
            ]
        })
        .collect();
    // Sorted by port, so the port each note names is the same every run.
    let mut infos: Vec<&PortInfo> = scanned.result.ports.values().collect();
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
    let CleanCtx {
        settings,
        moved,
        jobs,
        cache: open_cache,
        used,
        mut read,
        _staging,
    } = ctx;
    let paint = stderr_color(cli);
    if !cli.quiet {
        errln!(
            "{} options dir {}",
            tint(paint, ansi::BOLD, "optique clean:"),
            settings.options_dir.display()
        );
        for note in &settings.notes {
            errln!("  {}        {note}", tint(paint, ansi::YELLOW, "note:"));
        }
    }
    // Said even under -q, as run_scan does; --unused had it say it already.
    if used.is_none()
        && let Some(e) = settings.port_dbdir_error()
    {
        errln!("{} {e}", tint(paint, ansi::RED, "error:"));
    }

    let (vanished, live, mut warnings) = clean::classify_entries(
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
        errln!("{} {w}", tint(paint, ansi::YELLOW, "warning:"));
    }
    let pruned = clean::prune(vanished, live, used.as_ref(), relocated.is_some());
    let (mut removals, live) = (pruned.removals, pruned.live);
    if cli.verbose {
        let why = "make.conf relocates options files";
        kept_entries.extend(pruned.held.into_iter().map(|name| (name, why.to_string())));
    }

    // Optionally find files that only repeat defaults + make.conf. An entry
    // a live port's flavor may read is not judged from that port's
    // answers, which are another file's.
    if args.redundant {
        let (judged, flavor_files): (Vec<_>, Vec<_>) =
            live.into_iter().partition(|e| !e.flavor_file);
        if cli.verbose {
            kept_entries.extend(flavor_files.iter().map(|e| {
                let why = format!(
                    "a flavor of {} may read it (an OPTIONS_FILE its Makefile sets); not judged",
                    e.key
                );
                (e.options_name.clone(), why)
            }));
        }
        let live = judged;
        let cache = open_cache.unwrap_or_else(|| new_cache(cli, &settings));
        let found = find_redundant(cli, &settings, jobs, cache, &live);
        removals.extend(found.removals);
        warnings.extend(found.warnings);
        kept_entries.extend(found.kept);
        for (entry, port) in found.read {
            read.entry(entry).or_insert(port);
        }
    }
    // An entry a moved OPTIONS_FILE has make read is kept whatever its
    // name says, even where the make.conf probe saw no move.
    let before: Vec<String> = removals.iter().map(|r| r.options_name.clone()).collect();
    for note in clean::keep_read(&mut removals, &read) {
        errln!("{} {note}", tint(paint, ansi::YELLOW, "warning:"));
        warnings.push(note);
    }
    if cli.verbose {
        for name in before {
            if let Some(port) = read
                .get(&name)
                .filter(|_| !removals.iter().any(|r| r.options_name == name))
            {
                kept_entries.push((
                    name,
                    format!("make reads {port}'s options from it (OPTIONS_FILE)"),
                ));
            }
        }
    }

    // Obsolete, unused and redundant removals were collected separately.
    removals.sort_by(|a, b| a.options_name.cmp(&b.options_name));
    // A file optique cannot model is the user's, whatever its port became.
    let files = optfiles::OptionsFiles::new(&settings.options_dir);
    removals.retain(|r| match files.combined(&r.options_name).unmanaged() {
        Some((_, why)) => {
            let note = format!("{}: {why}; kept", r.options_name);
            errln!("{} {note}", tint(paint, ansi::YELLOW, "warning:"));
            warnings.push(note);
            if cli.verbose {
                kept_entries.push((r.options_name.clone(), why));
            }
            false
        }
        None => true,
    });

    // Kept as the last filter left it: a held entry the redundancy pass
    // removes after all is not kept, one a later filter took back is.
    kept_entries.retain(|(name, _)| !removals.iter().any(|r| &r.options_name == name));
    kept_entries.sort();
    kept_entries.dedup_by(|a, b| a.0 == b.0);
    if cli.verbose && !args.json {
        for (name, why) in &kept_entries {
            outln!("keep  {name:<38} {why}")?;
        }
    }

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
            errln!("{} {note}", tint(paint, ansi::YELLOW, "note:"));
            warnings.push(note);
        }
        for (subject, message) in done.failed {
            errln!("{} {subject}: {message}", tint(paint, ansi::RED, "error:"));
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
        errln!("nothing to clean ({total_entries} entries kept)");
    } else if cli.dry_run {
        errln!(
            "dry run: {} of {total_entries} entries would be removed from {}",
            removals.len(),
            settings.options_dir.display()
        );
    } else {
        errln!(
            "{removed} entry(ies) removed from {}",
            settings.options_dir.display()
        );
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

    let paint = stderr_color(cli);
    let runner = MakeRunner::new(query_ctx(settings, settings.options_dir.clone()), jobs);
    let mut pass = RedundancyPass {
        verbose: cli.verbose,
        paint,
        options_dir: &settings.options_dir,
        by_key: live.iter().map(|e| (e.key.clone(), e)).collect(),
        files: optfiles::OptionsFiles::new(&settings.options_dir),
        pending: Default::default(),
        found: Redundancy::default(),
    };
    let mut ask: Vec<PortKey> = live.iter().map(|e| e.key.clone()).collect();
    let mut ready: std::collections::VecDeque<Answer> = Default::default();
    let mut in_flight = 0usize;
    let mut done = 0usize;
    let mut total = live.len();
    loop {
        for key in ask.drain(..) {
            let since = std::time::SystemTime::now();
            match cache.lookup(&key, &pass.files) {
                Some(info) => ready.push_back(Ok(info)),
                None => {
                    runner.submit_since(key, since);
                    in_flight += 1;
                }
            }
        }
        while let Some(result) = ready.pop_front() {
            done += 1;
            let others = pass.answer(result);
            total += others.len();
            ask.extend(others);
        }
        if !ask.is_empty() {
            continue;
        }
        if in_flight == 0 {
            break;
        }
        match runner.events.recv() {
            Ok(ScanEvent::PortDone(info, since)) => {
                in_flight -= 1;
                cache.insert(&info, &pass.files, since);
                ready.push_back(Ok(*info));
            }
            Ok(ScanEvent::PortError { key, msg }) => {
                in_flight -= 1;
                ready.push_back(Err((key, msg)));
            }
            Err(_) => break,
        }
        errp!(
            "\r{}",
            tint(
                paint,
                ansi::GRAY,
                &format!("checking… {done}/{total} ports")
            )
        );
        let _ = std::io::stderr().flush();
    }
    if done > 0 {
        errln!();
    }
    // An entry found redundant for its own port may still be read by a
    // sibling origin its Makefile.common sends there: asked last, for the
    // few entries about to go.
    let trees: Vec<std::path::PathBuf> = settings
        .overlay_paths()
        .into_iter()
        .chain(std::iter::once(settings.portsdir.clone()))
        .collect();
    let mut rescued = Vec::new();
    for removal in &pass.found.removals {
        let local = pass
            .files
            .under_local(&removal.options_name, Default::default());
        let file = pass.files.combined(&removal.options_name);
        for sibling in sibling_origins(&trees, &removal.options_name.replacen('_', "/", 1)) {
            let key = PortKey::new(sibling, None);
            let since = std::time::SystemTime::now();
            let info = match cache.lookup(&key, &pass.files) {
                Some(info) => info,
                None => {
                    runner.submit_since(key.clone(), since);
                    match runner.events.recv() {
                        Ok(ScanEvent::PortDone(info, since)) => {
                            cache.insert(&info, &pass.files, since);
                            *info
                        }
                        // Whether it reads the file is unknown: kept.
                        _ => {
                            rescued.push((
                                removal.options_name.clone(),
                                format!("{key}: query failed, left alone"),
                            ));
                            break;
                        }
                    }
                }
            };
            if info.options_name != removal.options_name || !info.options.has_options() {
                continue;
            }
            if let Some(why) = shared_file_deviation(std::slice::from_ref(&info), &local, &file) {
                rescued.push((
                    removal.options_name.clone(),
                    format!("{key} reads it: {why}"),
                ));
                break;
            }
        }
    }
    pass.found
        .removals
        .retain(|r| !rescued.iter().any(|(n, _)| *n == r.options_name));
    if pass.verbose {
        pass.found.kept.extend(rescued);
    }
    runner.shutdown();
    pass.found.kept.sort();
    pass.found
}

/// A port's query: what make said of it, or why it failed.
type Answer = std::result::Result<PortInfo, (PortKey, String)>;

/// An entry whose other flavors are still being queried.
struct Flavors<'a> {
    entry: &'a clean::LiveEntry,
    /// The flavors reading this entry's file, default flavor first.
    infos: Vec<PortInfo>,
    waiting: usize,
    failed: Option<String>,
}

/// The redundancy pass between the queries: each entry's default flavor
/// answers first and either settles it or names the other flavors to ask,
/// whose answers are gathered until the last one settles it.
struct RedundancyPass<'a> {
    verbose: bool,
    paint: bool,
    options_dir: &'a std::path::Path,
    by_key: std::collections::HashMap<PortKey, &'a clean::LiveEntry>,
    files: optfiles::OptionsFiles,
    pending: std::collections::HashMap<PortKey, Flavors<'a>>,
    found: Redundancy,
}

/// The origins `<category>/<port>-<suffix>` beside `origin` in `trees`: a
/// family sharing a Makefile.common may send every member to one options
/// file (multimedia/avidemux-qt5, -cli and -plugins read
/// multimedia_avidemux), which is not redundant while one of them would
/// build otherwise without it.
fn sibling_origins(trees: &[std::path::PathBuf], origin: &str) -> Vec<String> {
    let Some((category, port)) = origin.split_once('/') else {
        return Vec::new();
    };
    let prefix = format!("{port}-");
    let mut out: Vec<String> = trees
        .iter()
        .filter_map(|t| std::fs::read_dir(t.join(category)).ok())
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with(&prefix))
        .map(|n| format!("{category}/{n}"))
        .filter(|o| trees.iter().any(|t| t.join(o).join("Makefile").is_file()))
        .collect();
    out.sort();
    out.dedup();
    out
}

impl<'a> RedundancyPass<'a> {
    fn warn(&mut self, note: String) {
        errln!("{} {note}", tint(self.paint, ansi::YELLOW, "warning:"));
        self.found.warnings.push(note);
    }

    /// One port's answer; returns the other flavors of its entry still to
    /// ask, when it is an entry's default flavor and does not settle it.
    fn answer(&mut self, result: Answer) -> Vec<PortKey> {
        let key = match &result {
            Ok(info) => info.key.clone(),
            Err((key, _)) => key.clone(),
        };
        if let Ok(info) = &result {
            // Answers arrive in any order: the smallest port names it. A
            // file one entry deep is the port's own key (its moved path
            // adopted, `makerunner::adopt_options_file`): read as surely.
            let natural = info.key.origin.replacen('/', "_", 1);
            let own = [
                info.options_name.clone(),
                optfiles::key_entry(&info.options_name).to_string(),
            ]
            .into_iter()
            .filter(|entry| *entry != natural)
            .map(|entry| (entry, info.key.to_string()));
            for (entry, port) in clean::read_through_moved(self.options_dir, [info])
                .into_iter()
                .chain(own)
            {
                let named = self.found.read.entry(entry).or_insert_with(|| port.clone());
                if port < *named {
                    *named = port;
                }
            }
        }
        if key.flavor.is_none() {
            return self.default_answer(key, result);
        }
        self.flavor_answer(&key, result);
        Vec::new()
    }

    /// The answer of an entry's default flavor: a verdict, or the other
    /// flavors reading the file, to ask before there can be one.
    fn default_answer(&mut self, key: PortKey, result: Answer) -> Vec<PortKey> {
        let Some(&entry) = self.by_key.get(&key) else {
            return Vec::new();
        };
        let info = match result {
            Ok(info) => info,
            Err((_, msg)) => {
                self.warn(format!("{key}: query failed, left alone ({msg})"));
                return Vec::new();
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
            self.warn(note);
            return Vec::new();
        }
        let others: Vec<PortKey> = info
            .flavors
            .iter()
            .filter(|f| info.canonical.flavor.as_ref() != Some(*f))
            .map(|f| PortKey::new(key.origin.clone(), Some(f.clone())))
            .collect();
        let f = Flavors {
            entry,
            infos: vec![info],
            waiting: others.len(),
            failed: None,
        };
        // A deviating default flavor settles it: no need to ask the
        // others. Not one reading a file of its own (`session::moved`).
        let local = self
            .files
            .under_local(&entry.options_name, Default::default());
        let file = self.files.combined(&entry.options_name);
        let deviates = session::moved(&f.infos[0]).is_none()
            && shared_file_deviation(&f.infos, &local, &file).is_some();
        if others.is_empty() || deviates {
            self.settle(f);
            return Vec::new();
        }
        self.pending
            .insert(PortKey::new(key.origin.clone(), None), f);
        others
    }

    /// The answer of another flavor of a pending entry, settling it once the
    /// last one is in.
    fn flavor_answer(&mut self, key: &PortKey, result: Answer) {
        let plain = PortKey::new(key.origin.clone(), None);
        let Some(f) = self.pending.get_mut(&plain) else {
            return;
        };
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
                    format!(
                        "{}: {key}: query failed, left alone ({msg})",
                        f.entry.options_name
                    )
                });
            }
        }
        if f.waiting == 0 {
            let f = self
                .pending
                .remove(&plain)
                .expect("pending entry just seen");
            self.settle(f);
        }
    }

    /// The verdict once every flavor reading the file is known.
    fn settle(&mut self, mut f: Flavors<'a>) {
        if let Some(why) = f.failed {
            self.warn(why);
            return;
        }
        // Answers arrive in any order; the reason names the same flavor each run.
        f.infos[1..].sort_by(|a, b| a.key.cmp(&b.key));
        // A flavor make.conf moves OPTIONS_FILE for reads none of the file
        // (`session::moved`); the file is judged for the others alone, and
        // kept when none is left.
        let (moved, readers): (Vec<PortInfo>, Vec<PortInfo>) = f
            .infos
            .into_iter()
            .partition(|i| session::moved(i).is_some());
        if readers.is_empty() {
            if let (true, Some(u)) = (self.verbose, moved.first().and_then(session::moved)) {
                self.found.kept.push((f.entry.options_name.clone(), u.why));
            }
            return;
        }
        // A file optique cannot model: not its to remove.
        if let Some(u) = session::unmanaged_file(&self.files.combined(&f.entry.options_name)) {
            if self.verbose {
                self.found.kept.push((f.entry.options_name.clone(), u.why));
            }
            return;
        }
        let local = self
            .files
            .under_local(&f.entry.options_name, Default::default());
        let file = self.files.combined(&f.entry.options_name);
        match shared_file_deviation(&readers, &local, &file) {
            None => self.found.removals.push(clean::Removal {
                options_name: f.entry.options_name.clone(),
                dir: f.entry.dir.clone(),
                reason: "redundant: repeats defaults + make.conf".to_string(),
            }),
            Some(why) if self.verbose => self.found.kept.push((f.entry.options_name.clone(), why)),
            Some(_) => {}
        }
    }
}

/// Why a shared options file is not redundant, judged for every flavor that
/// reads it (`infos`, default flavor first, each queried with the file in
/// place): the first flavor it moves away from defaults + make.conf. None
/// when the file changes nothing for any of them.
fn shared_file_deviation(
    infos: &[PortInfo],
    local: &optionsfile::SavedOptionsFile,
    file: &optionsfile::SavedOptionsFile,
) -> Option<String> {
    // bsd.options.mk:300-304: a flavor with an OPTIONS_OVERRIDE never reads
    // the file; it is kept, not judged redundant, whatever it says.
    if let Some(info) = infos.iter().find(|i| i.options.r#override.is_some()) {
        return Some(format!(
            "{}: make.conf OPTIONS_OVERRIDE decides it, left alone",
            info.key
        ));
    }
    infos.iter().find_map(|info| {
        let diff = clean::redundancy_diff(info, local, file);
        if diff.is_empty() {
            None
        } else if info.is_default_flavor() {
            Some(format!(
                "deviates from defaults + make.conf: {}",
                diff.join(" ")
            ))
        } else {
            Some(format!(
                "deviates from defaults + make.conf for {}: {}",
                info.key,
                diff.join(" ")
            ))
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
        assert_eq!(
            super::shared_file_deviation(
                std::slice::from_ref(&mysql),
                &Default::default(),
                &Default::default()
            ),
            None
        );
        let why =
            super::shared_file_deviation(&[mysql, pgsql], &Default::default(), &Default::default())
                .expect("@pgsql reads MYSQL from the file");
        assert!(
            why.contains("www/sogo@pgsql") && why.contains("+MYSQL -PGSQL"),
            "{why}"
        );
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
        let why = super::shared_file_deviation(
            std::slice::from_ref(&info),
            &Default::default(),
            &Default::default(),
        )
        .expect("an override must never be judged redundant");
        assert!(why.contains("OPTIONS_OVERRIDE"), "{why}");
    }
}
