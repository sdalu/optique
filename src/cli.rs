use std::collections::HashSet;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::model::origin::PortKey;

#[derive(Parser, Debug)]
#[command(name = "optique", version, about = "Fast FreeBSD ports options/dependency configurator")]
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

    /// More detail: scan shows each port's effective options and query
    /// warnings, sync shows the full final state per written file, clean
    /// explains why kept entries are kept
    #[arg(short = 'v', long = "verbose", global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Interactive TUI to review and edit options across the closure (default)
    Tui(RootsArgs),
    /// Scan the dependency closure and print each port's option status
    Scan(RootsArgs),
    /// Refresh options files non-interactively: keep saved choices, adopt
    /// defaults for new options, drop removed ones (like `poudriere options -C`
    /// but headless and fast)
    Sync(SyncArgs),

    /// Remove obsolete options files from the resolved options dir: ports
    /// that vanished from the tree, and optionally files that only repeat
    /// what defaults + make.conf already dictate
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

    /// Show what would be removed without touching anything
    #[arg(short = 'n', long = "dry-run")]
    pub dry_run: bool,
}

#[derive(Args, Debug, Default)]
pub struct RootsArgs {
    /// Port origins (category/name[@flavor])
    pub origins: Vec<String>,
}

#[derive(Args, Debug)]
pub struct SyncArgs {
    #[command(flatten)]
    pub roots: RootsArgs,

    /// Show what would be written without touching anything
    #[arg(short = 'n', long = "dry-run")]
    pub dry_run: bool,
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
}
