use std::collections::HashSet;
use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::model::origin::PortKey;

/// Sentinel for a bare `-s`: the profile then comes from synth.ini's
/// `profile_selected`, falling back to LiveSystem.
pub const SYNTH_DEFAULT_PROFILE: &str = "";

/// Subcommand names, which `-s` must never mistake for a profile.
const SUBCOMMANDS: [&str; 6] = ["tui", "scan", "sync", "clean", "decide", "help"];

/// Short flags that take no value, so may precede `s` in a cluster; after
/// any other (`-zs`) the `s` is that flag's value.
const BOOLEAN_SHORTS: [char; 7] = ['l', 'n', 'm', 'v', 'q', 'r', 'u'];

/// `-s` takes an *optional* profile name, so `optique scan -s www/nginx` or
/// `optique -s scan` is ambiguous: clap would greedily read the next word as
/// the profile. Rewrite a bare
/// `-s`/`--synth` into the attached `--synth=<default>` form whenever the
/// following word cannot be a profile name — it is missing, another option, a
/// subcommand name, or contains `/` (a port origin or a path). A profile that
/// really looks like one of those can still be selected as `--synth=name`.
/// Words after a literal `--` are left untouched. A cluster of boolean short
/// flags ending in `s` (`-ns`) gets the same treatment, split into the
/// cluster without the `s` and the rewritten `-s`.
pub fn disambiguate_synth<I: IntoIterator<Item = OsString>>(args: I) -> Vec<OsString> {
    let mut out: Vec<OsString> = Vec::new();
    let mut args = args.into_iter().peekable();
    let mut rewriting = true;
    while let Some(arg) = args.next() {
        // The flags of `-<flags>s`, when every one of them is a boolean.
        let cluster = arg
            .to_str()
            .and_then(|a| a.strip_prefix('-')?.strip_suffix('s'))
            .filter(|f| !f.is_empty() && f.chars().all(|c| BOOLEAN_SHORTS.contains(&c)));
        if rewriting && arg == "--" {
            rewriting = false;
        } else if rewriting && (arg == "-s" || arg == "--synth" || cluster.is_some()) {
            let next_is_profile = match args.peek() {
                None => false,
                // A non-UTF-8 word is neither a flag nor a subcommand.
                Some(next) => next.to_str().is_none_or(|s| {
                    !s.starts_with('-') && !SUBCOMMANDS.contains(&s) && !s.contains('/')
                }),
            };
            if !next_is_profile {
                if let Some(flags) = cluster {
                    out.push(OsString::from(format!("-{flags}")));
                }
                out.push(OsString::from(format!("--synth={SYNTH_DEFAULT_PROFILE}")));
                continue;
            }
        }
        out.push(arg);
    }
    out
}

/// Short options that take a value; in a cluster (`-nz ws`) the value is the
/// rest of the word, or the next word when the option ends it.
const VALUE_SHORTS: [char; 8] = ['z', 'j', 'p', 'P', 'O', 'o', 'f', 'J'];

/// Long options that take a value, given as the next word unless attached
/// with `=`.
const VALUE_LONGS: [&str; 11] = [
    "--set", "--jail", "--ports-tree", "--portsdir", "--overlay", "--overlay-dir",
    "--options-dir", "--file", "--jobs", "--repo", "--color",
];

/// The action comes first: `optique scan -z ws -f list`. With no action the
/// options open the TUI (`optique -z ws www/nginx`), but an option before an
/// action would read as two places to look for one setting, and clap checks
/// the two levels apart; so it is refused, with the command as it should
/// read. Run on the arguments after `disambiguate_synth`, where a bare `-s`
/// left alone (or a cluster ending in it) is followed by its profile.
pub fn check_action_first(args: &[OsString]) -> Result<(), String> {
    let word = |a: &OsString| a.to_string_lossy().into_owned();
    if args.iter().skip(1).any(|a| ["-h", "--help", "-V", "--version"].contains(&a.to_str().unwrap_or(""))) {
        return Ok(());
    }
    let mut i = 1;
    let mut options_at = None;
    while i < args.len() {
        let a = word(&args[i]);
        if a == "--" {
            return Ok(());
        }
        if SUBCOMMANDS.contains(&a.as_str()) {
            let Some(first) = options_at else { return Ok(()) };
            // The bare -s disambiguate_synth rewrote reads as typed.
            let typed = |a: &OsString| match word(a) {
                w if w == format!("--synth={SYNTH_DEFAULT_PROFILE}") => "-s".to_string(),
                w => w,
            };
            let mut fixed = vec!["optique".to_string(), a];
            fixed.extend(args[first..i].iter().map(typed));
            fixed.extend(args[i + 1..].iter().map(typed));
            return Err(format!("options go after the action: {}", fixed.join(" ")));
        }
        if !a.starts_with('-') || a == "-" {
            // A port origin: the TUI's list starts here, no action follows.
            return Ok(());
        }
        options_at.get_or_insert(i);
        let takes_next = if let Some(long) = a.strip_prefix("--") {
            !long.contains('=') && (VALUE_LONGS.contains(&a.as_str()) || a == "--synth")
        } else {
            // The first value option of a cluster takes the rest of the word,
            // or the next word when it ends the cluster; a trailing `s` left
            // by disambiguate_synth takes its profile.
            let flags: Vec<char> = a[1..].chars().collect();
            match flags.iter().position(|c| VALUE_SHORTS.contains(c)) {
                Some(p) => p == flags.len() - 1,
                None => flags.last() == Some(&'s'),
            }
        };
        i += if takes_next { 2 } else { 1 };
    }
    Ok(())
}

/// Parse the (already rewritten and checked) arguments, merging -O and
/// --overlay-dir into one list in command-line order: which overlay shadows
/// which depends on it, and clap keeps the two apart.
pub fn parse(args: Vec<OsString>) -> Cli {
    use clap::{CommandFactory as _, FromArgMatches as _};
    let matches = Cli::command().get_matches_from(args);
    let mut cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    // The options sit with the action when there is one, else at the top.
    let level = matches
        .subcommand()
        .map(|(_, sub)| sub)
        .filter(|sub| sub.try_contains_id("overlay").is_ok())
        .unwrap_or(&matches);
    let indices = |id: &str| -> Vec<usize> {
        level.indices_of(id).map(Iterator::collect).unwrap_or_default()
    };
    let mut specs: Vec<(usize, crate::config::OverlaySpec)> = Vec::new();
    specs.extend(
        indices("overlay")
            .into_iter()
            .zip(cli.overlay.iter())
            .map(|(i, n)| (i, crate::config::OverlaySpec::Name(n.clone()))),
    );
    specs.extend(
        indices("overlay_dir")
            .into_iter()
            .zip(cli.overlay_dir.iter())
            .map(|(i, d)| (i, crate::config::OverlaySpec::Dir(d.clone()))),
    );
    specs.sort_by_key(|(i, _)| *i);
    cli.overlays = specs.into_iter().map(|(_, s)| s).collect();
    cli
}

#[derive(Parser, Debug)]
#[command(
    name = "optique",
    version,
    about = "Fast FreeBSD ports options configurator for poudriere, synth and local builds",
    override_usage = "optique [OPTIONS] [PORT]...\n       optique <COMMAND> [OPTIONS] [PORT]...",
    max_term_width = 80
)]
pub struct Cli {
    /// Poudriere set (options land in poudriere.d/<SET>-options)
    #[arg(short = 'z', long = "set", global = true)]
    pub set: Option<String>,

    /// Poudriere jail (only used for make.conf layering)
    #[arg(short = 'j', long = "jail", global = true)]
    pub jail: Option<String>,

    /// Poudriere ports tree name, located through
    /// poudriere.d/ports/<TREE>/mnt [default: default]
    #[arg(short = 'p', long = "ports-tree", global = true)]
    pub tree: Option<String>,

    /// Explicit ports tree, overriding the poudriere, synth or local
    /// resolution (e.g. /usr/ports)
    #[arg(short = 'P', long = "portsdir", global = true, value_name = "DIR")]
    pub portsdir: Option<PathBuf>,

    /// Poudriere ports tree laid over the main one, as poudriere bulk -O;
    /// repeatable, the first given shadows the next and the main tree
    #[arg(short = 'O', long = "overlay", global = true, value_name = "NAME")]
    pub overlay: Vec<String>,

    /// Directory laid over the ports tree as an overlay, the counterpart of
    /// -O as -P is of -p; repeatable, in order with -O
    #[arg(long = "overlay-dir", global = true, value_name = "DIR")]
    pub overlay_dir: Vec<PathBuf>,

    /// -O and --overlay-dir merged in command-line order (set by `parse`).
    #[arg(skip)]
    pub overlays: Vec<crate::config::OverlaySpec>,

    /// Configure for a local build on this host, ignoring poudriere.d even
    /// when it exists: ports tree from PORTSDIR, options dir from
    /// PORT_DBDIR (environment or /etc/make.conf, else /usr/ports and
    /// /var/db/ports), /etc/make.conf, the installed packages
    #[arg(short = 'l', long = "local", global = true)]
    pub local: bool,

    /// Use the synth(1) layout instead of poudriere's: options dir and ports
    /// tree from /usr/local/etc/synth/synth.ini, make.conf from
    /// <PROFILE>-make.conf [default: synth.ini's profile_selected, else
    /// LiveSystem]
    #[arg(
        short = 's',
        long = "synth",
        global = true,
        value_name = "PROFILE",
        num_args = 0..=1,
        default_missing_value = SYNTH_DEFAULT_PROFILE
    )]
    pub synth: Option<String>,

    /// With -s and no ports given, only consider installed packages coming
    /// from these pkg(8) repositories (e.g. Synth) when building the
    /// implicit list; repeatable, a package matching any given name is
    /// kept. See `pkg query %R` for the names
    #[arg(long = "repo", global = true, value_name = "NAME")]
    pub repo: Vec<String>,

    /// Explicit options dir, overriding the poudriere, synth or local
    /// resolution (e.g. /var/db/ports)
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

    /// Show what sync/clean would change without touching anything;
    /// the TUI previews changes in its apply dialog instead and ignores this
    #[arg(short = 'n', long = "dry-run", global = true)]
    pub dry_run: bool,

    /// Persist only deviations: on sync/apply, a port whose final
    /// configuration equals what defaults + make.conf already produce gets
    /// no options file (an existing identical one is removed), and bare
    /// defaults count as decided — scan's gate and the TUI attention views
    /// then only flag deviations and conflicts. clean's counterpart is
    /// --redundant
    #[arg(short = 'm', long = "minimal", global = true)]
    pub minimal: bool,

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
    /// Tints the scan status markers on stdout and the informational stderr
    /// output (banner labels, notes, warnings, errors, progress); stdout and
    /// stderr are judged for tty-ness separately. The TUI is unaffected.
    #[arg(long = "color", global = true, value_name = "WHEN", default_value = "auto")]
    pub color: ColorChoice,

    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    /// The constraints between global options, checked on the parsed values
    /// in one list rather than as clap attributes: clap checks those at each
    /// command level before globals are merged, which let a pair straddling
    /// the subcommand through when options could sit on both sides of it.
    /// `check_action_first` now keeps them on one side; this stays the one
    /// place a new constraint goes, failing as clap's own usage errors do.
    pub fn check_globals(&self) -> Result<(), clap::Error> {
        let given = |name: &str| match name {
            "local" => self.local,
            "synth" => self.synth.is_some(),
            "set" => self.set.is_some(),
            "jail" => self.jail.is_some(),
            "ports-tree" => self.tree.is_some(),
            "portsdir" => self.portsdir.is_some(),
            "overlay" => !self.overlay.is_empty(),
            "overlay-dir" => !self.overlay_dir.is_empty(),
            "repo" => !self.repo.is_empty(),
            "verbose" => self.verbose,
            "quiet" => self.quiet,
            _ => unreachable!("unknown option {name}"),
        };
        const CONFLICTS: &[(&str, &str)] = &[
            ("local", "synth"),
            ("local", "set"),
            ("local", "jail"),
            ("local", "ports-tree"),
            ("local", "overlay"),
            ("synth", "set"),
            ("synth", "jail"),
            ("synth", "ports-tree"),
            ("synth", "overlay"),
            ("synth", "overlay-dir"),
            ("ports-tree", "portsdir"),
            ("verbose", "quiet"),
        ];
        const REQUIRES: &[(&str, &str)] = &[("repo", "synth")];
        let error = |kind, msg: String| {
            use clap::CommandFactory as _;
            Err(Cli::command().error(kind, msg))
        };
        for &(a, b) in CONFLICTS {
            if given(a) && given(b) {
                return error(
                    clap::error::ErrorKind::ArgumentConflict,
                    format!("the argument '--{a}' cannot be used with '--{b}'"),
                );
            }
        }
        for &(a, b) in REQUIRES {
            if given(a) && !given(b) {
                return error(
                    clap::error::ErrorKind::MissingRequiredArgument,
                    format!("the argument '--{a}' requires '--{b}'"),
                );
            }
        }
        // poudriere would mount an overlay given twice twice, and the tree
        // itself as its own overlay: both are mistakes.
        let tree = self.tree.as_deref().unwrap_or("default");
        for (i, name) in self.overlay.iter().enumerate() {
            if self.overlay[..i].contains(name) {
                return error(
                    clap::error::ErrorKind::ArgumentConflict,
                    format!("overlay '{name}' given twice"),
                );
            }
            if name == tree && self.portsdir.is_none() {
                return error(
                    clap::error::ErrorKind::ArgumentConflict,
                    format!("overlay '{name}' is the ports tree itself"),
                );
            }
        }
        Ok(())
    }
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
    Sync(SyncArgs),

    /// Set option values from a JSON plan read on stdin, for scripts and
    /// agents: {"www/nginx": {"LUA": true}}. Applies the same group, IMPLIES
    /// and FORCE rules as the TUI, writes only the named ports' files, and
    /// writes nothing unless the whole plan is honoured. Reports on stdout as
    /// JSON; -n previews
    Decide(DecideArgs),

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
pub struct DecideArgs {
    #[command(flatten)]
    pub roots: RootsArgs,
}

#[derive(Args, Debug)]
pub struct SyncArgs {
    #[command(flatten)]
    pub roots: RootsArgs,

    /// Report on stdout as one JSON object instead of the per-file listing
    /// (the summary stays on stderr); -q is ignored
    #[arg(long = "json")]
    pub json: bool,
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

    /// Report on stdout as one JSON object instead of the per-entry listing
    /// (the summary stays on stderr); -q is ignored
    #[arg(long = "json")]
    pub json: bool,
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

    /// With --json, add every port's full per-option detail: value, default,
    /// saved state, make.conf verdict, locks, group, IMPLIES/PREVENTS,
    /// description and the dependencies each option adds — everything a
    /// decision needs without re-running make
    #[arg(long = "options", requires = "json")]
    pub options: bool,
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
    let mut push = |spec: &str, source: &str, from_pkglist: bool| -> anyhow::Result<()> {
        // Strip inline comments and surrounding whitespace.
        let spec = spec.split('#').next().unwrap_or("").trim();
        if spec.is_empty() {
            return Ok(());
        }
        // poudriere(8) reads a pkglist line with `read`: only its first
        // whitespace-separated word is the origin, and one stray leading or
        // trailing '/' (a common typo) is stripped (common.sh ~9590-9600).
        let spec = if from_pkglist {
            let word = spec.split_whitespace().next().unwrap_or(spec);
            let word = word.strip_prefix('/').unwrap_or(word);
            word.strip_suffix('/').unwrap_or(word)
        } else {
            spec
        };
        match PortKey::parse(spec) {
            Some(k) => {
                if seen.insert(k.clone()) {
                    out.push(k);
                }
                Ok(())
            }
            None => {
                let msg = format!("malformed port origin {spec:?} in {source}");
                if from_pkglist {
                    // Bad data inside a -f pkglist file, not bad usage of
                    // the command line itself: man/optique.8's "Invalid
                    // command line usage exits 2" does not cover it.
                    anyhow::bail!(msg);
                }
                use clap::CommandFactory as _;
                Cli::command().error(clap::error::ErrorKind::ValueValidation, msg).exit()
            }
        }
    };
    for o in origins {
        push(o, "arguments", false)?;
    }
    for f in files {
        let text = std::fs::read_to_string(f)
            .map_err(|e| anyhow::anyhow!("cannot read list file {}: {e}", f.display()))?;
        let source = f.display().to_string();
        for line in text.lines() {
            push(line, &source, true)?;
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

    /// The tables `disambiguate_synth` and `check_action_first` read before
    /// clap does are kept by hand; a flag added to the derive and not to them
    /// would go wrong without a word, so they are checked against it here.
    #[test]
    fn hand_kept_tables_match_the_clap_definition() {
        use clap::CommandFactory as _;
        let root = Cli::command();
        let mut args: Vec<clap::Arg> = root.get_arguments().cloned().collect();
        for sub in root.get_subcommands() {
            args.extend(sub.get_arguments().cloned());
        }
        // --synth/-s takes an optional value: it is the case the tables exist for.
        args.retain(|a| a.get_id() != "synth" && !a.is_positional());
        let takes = |a: &clap::Arg| a.get_action().takes_values();
        let shorts = |value: bool| -> std::collections::BTreeSet<char> {
            args.iter().filter(|a| takes(a) == value).filter_map(clap::Arg::get_short).collect()
        };
        assert_eq!(shorts(true), VALUE_SHORTS.into_iter().collect());
        assert_eq!(shorts(false), BOOLEAN_SHORTS.into_iter().collect());
        let longs: std::collections::BTreeSet<String> = args
            .iter()
            .filter(|a| takes(a))
            .flat_map(|a| {
                let aliases = a.get_all_aliases().unwrap_or_default();
                a.get_long().into_iter().chain(aliases).map(|l| format!("--{l}")).collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(longs, VALUE_LONGS.iter().map(|l| l.to_string()).collect());
        let mut subs: Vec<&str> = root.get_subcommands().map(clap::Command::get_name).collect();
        subs.push("help");
        subs.sort_unstable();
        let mut table = SUBCOMMANDS.to_vec();
        table.sort_unstable();
        assert_eq!(subs, table);
    }

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
    fn pkglist_lines_take_first_word_and_strip_slashes() {
        // poudriere(8) reads a pkglist with `read`: a trailing or leading
        // '/' is a stray typo it strips, and anything after the first
        // whitespace-separated word (a trailing description, say) is not
        // part of the origin.
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "sysutils/tmux/\nsysutils/screen  terminal multiplexer\n/mail/dovecot")
            .unwrap();

        let roots = collect_roots(&[], &[f.path().to_path_buf()]).unwrap();
        let names: Vec<String> = roots.iter().map(|k| k.to_string()).collect();
        assert_eq!(names, vec!["sysutils/tmux", "sysutils/screen", "mail/dovecot"]);
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
    fn overlays_keep_command_line_order() {
        use crate::config::OverlaySpec::{Dir, Name};
        let parse = |args: &[&str]| parse(args.iter().map(OsString::from).collect()).overlays;
        let want = vec![Name("a".into()), Dir("/d".into()), Name("b".into())];
        assert_eq!(parse(&["optique", "scan", "-O", "a", "--overlay-dir", "/d", "-O", "b", "x/y"]), want);
        assert_eq!(parse(&["optique", "-O", "a", "--overlay-dir=/d", "--overlay", "b", "x/y"]), want);
        assert!(parse(&["optique", "scan", "x/y"]).is_empty());
    }

    #[test]
    fn the_action_comes_first() {
        let check = |args: &[&str]| -> Result<(), String> {
            let args: Vec<OsString> = args.iter().map(OsString::from).collect();
            check_action_first(&disambiguate_synth(args))
        };
        // Accepted: the action first, or no action at all.
        for ok in [
            &["optique", "scan", "-z", "ws", "-f", "l"][..],
            &["optique", "-z", "ws", "-f", "l"],
            &["optique", "-z", "ws", "www/nginx"],
            &["optique", "-o", "scan", "www/nginx"],
            &["optique", "-nz", "scan"],
            &["optique", "--options-dir", "sync"],
            &["optique", "-s", "Release", "www/nginx"],
            &["optique", "-z", "ws", "--help", "scan"],
            &["optique", "--", "-z", "scan"],
        ] {
            assert_eq!(check(ok), Ok(()), "{ok:?}");
        }
        // Refused, with the command as it should read.
        for (bad, fixed) in [
            (&["optique", "-z", "ws", "scan", "-f", "l"][..], "optique scan -z ws -f l"),
            (&["optique", "-l", "sync"], "optique sync -l"),
            (&["optique", "-s", "scan"], "optique scan -s"),
            (&["optique", "-s", "Release", "scan"], "optique scan -s Release"),
            (&["optique", "--set=ws", "-n", "clean", "-r"], "optique clean --set=ws -n -r"),
            (&["optique", "-nzws", "decide"], "optique decide -nzws"),
        ] {
            assert_eq!(check(bad), Err(format!("options go after the action: {fixed}")), "{bad:?}");
        }
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
        // A cluster of boolean flags ending in s follows the same rule.
        assert_eq!(fix(&["optique", "-ns", "scan"]), ["optique", "-n", &dflt, "scan"]);
        assert_eq!(fix(&["optique", "-vs", "scan"]), ["optique", "-v", &dflt, "scan"]);
        assert_eq!(fix(&["optique", "-qs", "scan"]), ["optique", "-q", &dflt, "scan"]);
        assert_eq!(fix(&["optique", "-nms"]), ["optique", "-nm", &dflt]);
        assert_eq!(fix(&["optique", "-ns", "Release", "scan"]), ["optique", "-ns", "Release", "scan"]);
        // After a flag that takes a value, the s is that value, not -s.
        assert_eq!(fix(&["optique", "-zs", "scan"]), ["optique", "-zs", "scan"]);
        assert_eq!(fix(&["optique", "-nzs", "scan"]), ["optique", "-nzs", "scan"]);
        assert_eq!(fix(&["optique", "--", "-ns", "scan"]), ["optique", "--", "-ns", "scan"]);
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
