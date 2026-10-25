//! The entry point: the arguments read and checked, the cache cleared when
//! asked, one command run, and its outcome made an exit status (1 for
//! decisions pending, 2 for bad usage, 3 for a failed run).

/// `println!` for the stdout payload, handing back the write's error instead
/// of panicking on it: a reader that went away (`optique scan | head`) must
/// end the run with a documented status, not Rust's 101.
macro_rules! outln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        writeln!(std::io::stdout(), $($arg)*)
    }};
}

/// `eprintln!` that never panics: stderr gone (`optique scan 2>&1 | head`)
/// is no reason to end on Rust's 101; what is left to say is dropped.
macro_rules! errln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

/// `eprint!`, likewise (`errln!`).
macro_rules! errp {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = write!(std::io::stderr(), $($arg)*);
    }};
}

mod apply;
mod cache;
mod clean;
mod cli;
mod cmd;
mod color;
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
mod synth;
mod tui;

use std::io::Write as _;

use anyhow::Result;

use crate::cli::{Cli, Command};
use crate::cmd::{
    cmd_clean, cmd_decide, cmd_scan, cmd_sync, cmd_tui, listed_roots, roots_or_installed,
};

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
            errln!("Error: {e:?}");
        }
        let _ = std::io::stdout().flush();
        std::process::exit(EXIT_ERROR);
    }
}

fn run() -> Result<()> {
    let args = cli::disambiguate_synth(std::env::args_os());
    if let Err(msg) = cli::check_action_first(&args) {
        use clap::CommandFactory as _;
        Cli::command()
            .error(clap::error::ErrorKind::ArgumentConflict, msg)
            .exit();
    }
    let cli = cli::parse(args);
    if let Err(e) = cli.check_globals() {
        e.exit();
    }
    refuse_unused_repo(&cli);
    if cli.clear_cache {
        let dir = cache::default_cache_dir();
        let (files, bytes) = cache::clear(&dir);
        errln!(
            "cache cleared: {files} file(s), {} KiB in {}",
            bytes / 1024,
            dir.display()
        );
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
            let attention = cmd_scan(&cli, &rs, args)?;
            exit_pending(attention > 0);
            Ok(())
        }
        Some(Command::Sync(args)) => {
            let rs = roots_or_installed(&cli, &args.roots.origins)?;
            cmd_sync(&cli, &rs, args)
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
            let rs = listed_roots(&origins, &all_files)?;
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

/// --repo only filters the implicit installed-package list: refuse it on
/// every path that cannot use that list, not just where the list is built.
fn refuse_unused_repo(cli: &Cli) {
    if cli.repo.is_empty() {
        return;
    }
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

/// Exit 1 when something is left to do — the gate status scan and decide
/// share — once both streams are flushed.
fn exit_pending(pending: bool) {
    if pending {
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        std::process::exit(1);
    }
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

#[cfg(test)]
mod tests {
    use super::split_raw_origins;
    #[test]
    fn split_raw_origins_forms() {
        let raw: Vec<String> = ["www/nginx", "-f", "list1", "mail/dovecot", "--file=list2"]
            .iter()
            .map(ToString::to_string)
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
