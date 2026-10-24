//! `optique scan`: the table, or the JSON, and the gate.

use anyhow::Result;

use crate::cli::Cli;
use crate::color::{ansi, marker_color, stdout_color};
use crate::model::port::PortInfo;
use crate::session::UiStatus;
use crate::{cli, config, deps, optfiles, report, session, synth};

use super::{run_scan, RootSet};


/// Does a scan row make `scan` exit 1? A conflict always does (saved
/// options violate the port's own constraints), and so does a file synth
/// would refuse (`synth::refuses`, under -s), whatever the status; missing
/// or outdated configuration only does when make.conf leaves something
/// open (`mc_covered` false). Blacklisted ports never do — they are never
/// built here.
fn needs_attention(
    status: &UiStatus,
    blacklisted: bool,
    mc_covered: bool,
    synth_refuses: bool,
) -> bool {
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
fn shown_ok(status: &UiStatus, minimal: bool, covered: bool) -> bool {
    match status {
        UiStatus::Ok => true,
        UiStatus::Unconfigured | UiStatus::Stale => minimal && covered,
        _ => false,
    }
}

/// A port with options, as scan reports it.
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


/// Scan and report. Returns how many things a human still has to look at —
/// ports needing a decision (see `Row::needs_attention`), dependency loops
/// that are not entirely blacklisted, and ports whose query failed — which
/// main turns into exit code 1.
pub(crate) fn cmd_scan(cli: &Cli, rs: &RootSet, args: &cli::ScanArgs) -> Result<usize> {
    let scanned = run_scan(cli, rs)?;
    let (queried, from_cache, elapsed) =
        (scanned.result.queried, scanned.result.from_cache, scanned.elapsed);
    let flagged_moves = scanned.flagged_moves();
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
    let (rows, hidden) = build_rows(cli, &sess, &settings, &scanned.files, &dep_loops, args.options);

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
    // shut until it can be (run_scan already named each one on stderr). So
    // does a dependency on a nonexistent origin, which no build gets past.
    // A make.conf sending the build to another options dir comes first:
    // the report is still valid, but for files the build never reads.
    let errors: Vec<report::ErrorEntry> = settings
        .port_dbdir_error()
        .map(|message| report::ErrorEntry { subject: "make.conf".to_string(), message })
        .into_iter()
        .chain(scanned.result.errors.iter().map(|(key, msg)| report::ErrorEntry {
            subject: key.to_string(),
            message: msg.clone(),
        }))
        .collect();
    // The gate: ports owing a decision, loops a human has to untangle,
    // listed origins MOVED moves that were not followed (the list needs a
    // fix), and errors: ports that could not be checked, or depend on no
    // port, and a make.conf moving the options dir.
    let error_count = errors.len();
    let pending = attention + blocking_loops + flagged_moves + error_count;
    let summary = report::ScanSummary {
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
        moved: flagged_moves,
        errors: error_count,
        pending,
    };
    let line = summary_line(&summary, queried, from_cache, elapsed);

    if args.json {
        // stdout must stay pure JSON: one object, no table, quiet ignored.
        let report = report::ScanReport {
            options_dir: settings.options_dir.display().to_string(),
            ports_tree: settings.portsdir.display().to_string(),
            overlays: settings.overlays.iter().map(|o| o.path.display().to_string()).collect(),
            ports: rows.iter().map(port_report).collect(),
            loops: dep_loops
                .iter()
                .map(|dl| report::LoopReport {
                    ports: dl.members.iter().map(ToString::to_string).collect(),
                    cycle: dl.cycle.iter().map(ToString::to_string).collect(),
                    blacklisted: loop_blacklisted(dl),
                })
                .collect(),
            summary,
            moved: report::moved_entries(&scanned.moves),
            errors,
        };
        report::print(&report)?;
    } else if !cli.quiet {
        print_table(cli, &rows)?;
    }
    eprintln!("{line}");
    Ok(pending)
}

/// The rows of the ports with options (and of any file synth would refuse),
/// problems first, and how many ports have no options at all.
fn build_rows<'a>(
    cli: &Cli,
    sess: &session::Session,
    settings: &'a config::Settings,
    files: &optfiles::OptionsFiles,
    dep_loops: &[deps::DepLoop],
    with_options: bool,
) -> (Vec<Row<'a>>, usize) {
    let mut rows: Vec<Row> = Vec::new();
    let mut hidden = 0usize;
    for (key, info) in &sess.ports {
        // synth refuses the whole build over one such file, options or not.
        let synth_refuses = (settings.env == config::QueryEnv::Synth)
            .then(|| synth::refuses_port(files, info))
            .flatten();
        if !info.options.has_options() && synth_refuses.is_none() {
            hidden += 1;
            continue;
        }
        let status = sess.status(info);
        let (added, removed) = if status == UiStatus::Stale {
            sess.stale_delta(info).unwrap_or_default()
        } else {
            (Vec::new(), Vec::new())
        };
        let undecided = sess.undecided(info);
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
                options: report::option_reports(sess, info),
            }),
            synth_refuses,
        });
    }
    rows.sort_by(|a, b| {
        (a.shown_ok(cli.minimal), &a.key).cmp(&(b.shown_ok(cli.minimal), &b.key))
    });
    (rows, hidden)
}

/// One row as the JSON gives it.
fn port_report(r: &Row) -> report::PortReport {
    report::PortReport {
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
    }
}

/// The table on stdout, a row per port, problems first.
fn print_table(cli: &Cli, rows: &[Row]) -> Result<()> {
    let color = stdout_color(cli);
    for row in rows {
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
    Ok(())
}

/// The line scan ends with on stderr: the counts, the loops, the moves and
/// the errors, what was queried and how long it took.
fn summary_line(s: &report::ScanSummary, queried: usize, from_cache: usize, elapsed: f32) -> String {
    // Loops are counted, not re-listed: run_scan already named every one.
    let mut loop_note = match s.loops {
        0 => String::new(),
        n => format!(" · {n} dependency loop{}", if n == 1 { "" } else { "s" }),
    };
    if s.moved > 0 {
        loop_note.push_str(&format!(" · {} listed moved (see above)", s.moved));
    }
    match s.errors {
        0 => {}
        n => loop_note.push_str(&format!(" · {n} error{} (see above)", if n == 1 { "" } else { "s" })),
    }
    format!(
        "{} ports with options ({} unconfigured, {} stale, {} conflict; \
         {} awaiting a decision) · {} without options{} · \
         {} queried, {} cached · {:.1}s",
        s.total,
        s.unconfigured,
        s.stale,
        s.conflict,
        s.attention,
        s.optionless,
        loop_note,
        queried,
        from_cache,
        elapsed
    )
}

#[cfg(test)]
mod tests {
    /// A file synth refuses trips scan's gate whatever its status.
    #[test]
    fn a_file_synth_refuses_trips_the_gate() {
        use super::needs_attention;
        use crate::session::UiStatus;
        // The gate: a refused file trips it even when the port is ok or
        // make.conf covers what is open; a blacklisted port never does.
        assert!(needs_attention(&UiStatus::Ok, false, true, true));
        assert!(needs_attention(&UiStatus::Stale, false, true, true));
        assert!(!needs_attention(&UiStatus::Ok, false, true, false));
        assert!(!needs_attention(&UiStatus::Stale, false, true, false));
        assert!(needs_attention(&UiStatus::Conflict, false, true, false));
        assert!(!needs_attention(&UiStatus::Conflict, true, true, true));
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
}
