use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "optique", version, about = "Fast FreeBSD ports options/dependency configurator")]
pub struct Cli {
    /// Poudriere set (options land in poudriere.d/<SET>-options)
    #[arg(short = 'z', long = "set", global = true)]
    pub set: Option<String>,

    /// Poudriere jail (only used for make.conf layering)
    #[arg(short = 'j', long = "jail", global = true)]
    pub jail: Option<String>,

    /// Poudriere ports tree name
    #[arg(short = 'p', long = "ports-tree", default_value = "default", global = true)]
    pub tree: String,

    /// Explicit options dir (overrides poudriere resolution; e.g. /var/db/ports)
    #[arg(short = 'o', long = "options-dir", global = true)]
    pub options_dir: Option<PathBuf>,

    /// Parallel make jobs (default: min(16, ncpu))
    #[arg(short = 'J', long = "jobs", global = true)]
    pub jobs: Option<usize>,

    /// Disable the persistent query cache
    #[arg(long = "no-cache", global = true)]
    pub no_cache: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Interactive TUI to review and edit options across the closure
    Tui(RootsArgs),
    /// Scan the dependency closure and print each port's option status
    Scan(RootsArgs),
    /// Refresh options files non-interactively: keep saved choices, adopt
    /// defaults for new options, drop removed ones (like `poudriere options -C`
    /// but headless and fast)
    Sync(SyncArgs),
}

#[derive(Args, Debug)]
pub struct SyncArgs {
    #[command(flatten)]
    pub roots: RootsArgs,

    /// Show what would be written without touching anything
    #[arg(short = 'n', long = "dry-run")]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct RootsArgs {
    /// Port origins (category/name[@flavor])
    pub origins: Vec<String>,

    /// File(s) with one origin per line (poudriere pkglist format)
    #[arg(short = 'f', long = "file")]
    pub files: Vec<PathBuf>,
}

impl RootsArgs {
    /// Merge positional origins and list files into parsed keys.
    pub fn roots(&self) -> anyhow::Result<Vec<crate::model::origin::PortKey>> {
        let mut out = Vec::new();
        let mut push = |spec: &str| -> anyhow::Result<()> {
            let spec = spec.trim();
            if spec.is_empty() || spec.starts_with('#') {
                return Ok(());
            }
            match crate::model::origin::PortKey::parse(spec) {
                Some(k) => {
                    out.push(k);
                    Ok(())
                }
                None => anyhow::bail!("malformed port origin: {spec:?}"),
            }
        };
        for o in &self.origins {
            push(o)?;
        }
        for f in &self.files {
            let text = std::fs::read_to_string(f)
                .map_err(|e| anyhow::anyhow!("cannot read list file {}: {e}", f.display()))?;
            for line in text.lines() {
                push(line)?;
            }
        }
        if out.is_empty() {
            anyhow::bail!("no port origins given (arguments or -f listfile)");
        }
        Ok(out)
    }
}
