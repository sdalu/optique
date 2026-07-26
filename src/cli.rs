use std::collections::HashSet;
use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::model::origin::PortKey;

/// synth(1) profile assumed when `-s` is given without a name.
pub const SYNTH_DEFAULT_PROFILE: &str = "LiveSystem";

/// Subcommand names, which `-s` must never mistake for a profile.
const SUBCOMMANDS: [&str; 5] = ["tui", "scan", "sync", "clean", "help"];

/// `-s` takes an *optional* profile name, so `optique -s scan -f list` is
/// ambiguous: clap would greedily read `scan` as the profile. Rewrite a bare
/// `-s`/`--synth` into the attached `--synth=<default>` form whenever the
/// following word cannot be a profile name — it is missing, another option, a
/// subcommand name, or contains `/` (a port origin or a path). A profile that
/// really looks like one of those can still be selected as `--synth=name`.
/// Words after a literal `--` are left untouched.
pub fn disambiguate_synth<I: IntoIterator<Item = OsString>>(args: I) -> Vec<OsString> {
    let mut out: Vec<OsString> = Vec::new();
    let mut args = args.into_iter().peekable();
    let mut rewriting = true;
    while let Some(arg) = args.next() {
        if rewriting && arg == "--" {
            rewriting = false;
        } else if rewriting && (arg == "-s" || arg == "--synth") {
            let next_is_profile = match args.peek() {
                None => false,
                // A non-UTF-8 word is neither a flag nor a subcommand.
                Some(next) => next.to_str().is_none_or(|s| {
                    !s.starts_with('-') && !SUBCOMMANDS.contains(&s) && !s.contains('/')
                }),
            };
            if !next_is_profile {
                out.push(OsString::from(format!("--synth={SYNTH_DEFAULT_PROFILE}")));
                continue;
            }
        }
        out.push(arg);
    }
    out
}

#[derive(Parser, Debug)]
#[command(name = "optique", version, about = "Fast FreeBSD ports options/dependency configurator", max_term_width = 80)]
pub struct Cli {
    /// Poudriere set (options land in poudriere.d/<SET>-options)
    #[arg(short = 'z', long = "set", global = true)]
    pub set: Option<String>,

    /// Poudriere jail (only used for make.conf layering)
    #[arg(short = 'j', long = "jail", global = true)]
    pub jail: Option<String>,

    /// Poudriere ports tree name [default: default]
    #[arg(short = 'p', long = "ports-tree", global = true)]
    pub tree: Option<String>,

    /// Use the synth(1) layout instead of poudriere's: options dir and ports
    /// tree from /usr/local/etc/synth/synth.ini, make.conf from
    /// <PROFILE>-make.conf [default profile: LiveSystem]
    #[arg(
        short = 's',
        long = "synth",
        global = true,
        value_name = "PROFILE",
        num_args = 0..=1,
        default_missing_value = SYNTH_DEFAULT_PROFILE,
        conflicts_with_all = ["set", "jail"]
    )]
    pub synth: Option<String>,

    /// Explicit options dir (overrides poudriere resolution; e.g. /var/db/ports)
    #[arg(short = 'o', long = "options-dir", global = true)]
    pub options_dir: Option<PathBuf>,

    /// Port list file, one origin[@flavor] per line, '#' comments
    /// (poudriere pkglist format); repeatable, duplicates are dropped
    #[arg(short = 'f', long = "file", global = true, value_name = "PKGLIST")]
    pub files: Vec<PathBuf>,

    /// Parallel make jobs (default: min(16, ncpu))
    #[arg(short = 'J', long = "jobs", global = true)]
    pub jobs: Option<usize>,

    /// Disable the persistent query cache
    #[arg(long = "no-cache", global = true)]
    pub no_cache: bool,

    /// Delete the persistent query cache before running
    /// (given alone: clear it and exit)
    #[arg(long = "clear-cache", global = true)]
    pub clear_cache: bool,

    /// Show what sync/clean would change without touching anything
    /// (the only commands that write)
    #[arg(short = 'n', long = "dry-run", global = true)]
    pub dry_run: bool,

    /// More detail: scan shows each port's effective options and query
    /// warnings, sync shows the full final state per written file, clean
    /// explains why kept entries are kept
    #[arg(short = 'v', long = "verbose", global = true)]
    pub verbose: bool,

    /// Quiet: drop the startup banner and the per-port/per-file listings on
    /// stdout (scan rows, sync writes, clean entries); the summary line and
    /// all warnings/errors on stderr stay. Ignored by `scan --json`.
    #[arg(short = 'q', long = "quiet", global = true)]
    pub quiet: bool,

    /// Colorize output: auto (a terminal with NO_COLOR unset), always, never.
    /// Today this only tints the scan status marker column; it is honoured by
    /// any colored CLI output added later. The TUI is unaffected.
    #[arg(long = "color", global = true, value_name = "WHEN", default_value = "auto")]
    pub color: ColorChoice,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(ValueEnum, Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum ColorChoice {
    #[default]
    Auto,
    Always,
    Never,
}

/// Should ANSI colors be emitted on stdout? `always` is an explicit request
/// and wins over NO_COLOR; under `auto` any NO_COLOR value (including an
/// empty one) and any non-terminal stdout mean plain text, so piped output
/// and the tests never see escapes.
pub fn use_color(choice: ColorChoice, is_tty: bool, no_color_env: Option<&str>) -> bool {
    match choice {
        ColorChoice::Never => false,
        ColorChoice::Always => true,
        ColorChoice::Auto => is_tty && no_color_env.is_none(),
    }
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Interactive TUI to review and edit options across the closure (default)
    Tui(TuiArgs),
    /// Scan the dependency closure and print each port's option status;
    /// exits 1 when a human decision is pending, 0 when nothing is
    Scan(ScanArgs),
    /// Refresh options files non-interactively: keep saved choices, adopt
    /// defaults for new options, drop removed ones (like `poudriere options -C`
    /// but headless and fast)
    Sync(RootsArgs),

    /// Remove obsolete options files from the resolved options dir: ports
    /// that vanished from the tree, and optionally files that only repeat
    /// what defaults + make.conf already dictate or belong to ports outside
    /// a package list's dependency closure
    Clean(CleanArgs),

    /// Bare origins with no subcommand open the TUI: `optique -z set www/nginx`
    #[command(external_subcommand)]
    Origins(Vec<String>),
}

#[derive(Args, Debug)]
pub struct CleanArgs {
    /// Also remove files whose recorded configuration equals the
    /// defaults + make.conf overlay outcome (removing them changes nothing)
    #[arg(short = 'r', long = "redundant")]
    pub redundant: bool,

    /// Also remove entries for ports outside the dependency closure of the
    /// given package list (positional origins and/or -f pkglist)
    #[arg(short = 'u', long = "unused")]
    pub unused: bool,

    /// Port origins for --unused closure computation
    pub origins: Vec<String>,
}

#[derive(Args, Debug)]
pub struct TuiArgs {
    #[command(flatten)]
    pub roots: RootsArgs,

    /// Headless driver for debugging/testing: read commands from stdin,
    /// render into an in-memory terminal (see DRIVE PROTOCOL in optique(8))
    #[arg(long)]
    pub drive: bool,
}

#[derive(Args, Debug)]
pub struct ScanArgs {
    #[command(flatten)]
    pub roots: RootsArgs,

    /// Print one machine-readable JSON object on stdout instead of the
    /// table (progress and the summary stay on stderr)
    #[arg(long = "json")]
    pub json: bool,
}

#[derive(Args, Debug, Default)]
pub struct RootsArgs {
    /// Port origins (category/name[@flavor])
    pub origins: Vec<String>,
}


/// Merge positional origins and pkglist files into parsed root keys,
/// dropping duplicates while preserving first-seen order.
pub fn collect_roots(origins: &[String], files: &[PathBuf]) -> anyhow::Result<Vec<PortKey>> {
    let mut out: Vec<PortKey> = Vec::new();
    let mut seen: HashSet<PortKey> = HashSet::new();
    let mut push = |spec: &str, source: &str| -> anyhow::Result<()> {
        // Strip inline comments and surrounding whitespace.
        let spec = spec.split('#').next().unwrap_or("").trim();
        if spec.is_empty() {
            return Ok(());
        }
        match PortKey::parse(spec) {
            Some(k) => {
                if seen.insert(k.clone()) {
                    out.push(k);
                }
                Ok(())
            }
            None => anyhow::bail!("malformed port origin {spec:?} in {source}"),
        }
    };
    for o in origins {
        push(o, "arguments")?;
    }
    for f in files {
        let text = std::fs::read_to_string(f)
            .map_err(|e| anyhow::anyhow!("cannot read list file {}: {e}", f.display()))?;
        for line in text.lines() {
            push(line, &f.display().to_string())?;
        }
    }
    if out.is_empty() {
        anyhow::bail!("no port origins given (arguments or -f pkglist)");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn dedup_and_comments() {
        let mut f1 = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            f1,
            "# workstation list\nwww/nginx\ndevel/py-Automat@py312   # keep\n\nmail/dovecot"
        )
        .unwrap();
        let mut f2 = tempfile::NamedTempFile::new().unwrap();
        writeln!(f2, "www/nginx\nmail/dovecot\nsysutils/tmux").unwrap();

        let roots = collect_roots(
            &["www/nginx".to_string(), "editors/vim".to_string()],
            &[f1.path().to_path_buf(), f2.path().to_path_buf()],
        )
        .unwrap();
        let names: Vec<String> = roots.iter().map(|k| k.to_string()).collect();
        assert_eq!(
            names,
            vec![
                "www/nginx",
                "editors/vim",
                "devel/py-Automat@py312",
                "mail/dovecot",
                "sysutils/tmux"
            ]
        );
    }

    #[test]
    fn malformed_line_names_the_file() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "not-an-origin").unwrap();
        let err = collect_roots(&[], &[f.path().to_path_buf()]).unwrap_err().to_string();
        assert!(err.contains("not-an-origin"));
        assert!(err.contains(&f.path().display().to_string()));
    }

    #[test]
    fn empty_is_an_error() {
        assert!(collect_roots(&[], &[]).is_err());
    }

    #[test]
    fn synth_only_swallows_real_profile_names() {
        let fix = |args: &[&str]| -> Vec<String> {
            disambiguate_synth(args.iter().map(OsString::from))
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let dflt = format!("--synth={SYNTH_DEFAULT_PROFILE}");
        // A subcommand, an option or nothing at all: -s stands alone.
        assert_eq!(fix(&["optique", "-s", "scan", "-f", "l"]), ["optique", &dflt, "scan", "-f", "l"]);
        assert_eq!(fix(&["optique", "--synth", "clean"]), ["optique", &dflt, "clean"]);
        assert_eq!(fix(&["optique", "scan", "-s", "-o", "/d"]), ["optique", "scan", &dflt, "-o", "/d"]);
        assert_eq!(fix(&["optique", "scan", "x/y", "-s"]), ["optique", "scan", "x/y", &dflt]);
        // A port origin (or any path) is never mistaken for a profile.
        assert_eq!(fix(&["optique", "scan", "-s", "www/nginx"]), ["optique", "scan", &dflt, "www/nginx"]);
        // A plain word is one, and the attached forms are left alone.
        assert_eq!(fix(&["optique", "-s", "Release", "scan"]), ["optique", "-s", "Release", "scan"]);
        assert_eq!(fix(&["optique", "--synth=scan", "scan"]), ["optique", "--synth=scan", "scan"]);
        assert_eq!(fix(&["optique", "-sRelease", "scan"]), ["optique", "-sRelease", "scan"]);
        // Nothing is rewritten past a literal --.
        assert_eq!(fix(&["optique", "--", "-s", "scan"]), ["optique", "--", "-s", "scan"]);
        // Untouched when -s is absent.
        assert_eq!(fix(&["optique", "-z", "ws", "scan"]), ["optique", "-z", "ws", "scan"]);
    }

    #[test]
    fn color_decision_table() {
        use ColorChoice::*;
        // auto: only a terminal without NO_COLOR gets escapes.
        assert!(use_color(Auto, true, None));
        assert!(!use_color(Auto, false, None));
        assert!(!use_color(Auto, true, Some("1")));
        assert!(!use_color(Auto, true, Some("")), "NO_COLOR= still means no color");
        assert!(!use_color(Auto, false, Some("1")));
        // never always loses, always always wins (explicit beats the env).
        assert!(!use_color(Never, true, None));
        assert!(!use_color(Never, false, Some("1")));
        assert!(use_color(Always, false, None));
        assert!(use_color(Always, false, Some("1")));
    }
}
