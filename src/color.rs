//! Colors on the terminal: when to paint a stream, and the few SGR
//! escapes the banner, the scan table and the notes use.

use crate::cli::{self, Cli};

/// SGR escapes for the scan marker column, matching the TUI's marker colors.
/// A handful of constants beats a dependency for four markers.
pub(crate) mod ansi {
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
pub(crate) fn stderr_color(cli: &Cli) -> bool {
    use std::io::IsTerminal as _;
    color_for(cli, std::io::stderr().is_terminal())
}

/// Whether to paint a stream, given whether it is a terminal.
pub(crate) fn color_for(cli: &Cli, is_terminal: bool) -> bool {
    cli::use_color(cli.color, is_terminal, std::env::var("NO_COLOR").ok().as_deref())
}

/// Wrap `text` in a color when painting is enabled.
pub(crate) fn tint(on: bool, color: &str, text: &str) -> String {
    if on && !text.is_empty() {
        format!("{color}{text}{}", ansi::RESET)
    } else {
        text.to_string()
    }
}

/// Color for a scan status marker; None for the unmarked ok rows.
pub(crate) fn marker_color(marker: &str) -> Option<&'static str> {
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
pub(crate) fn stdout_color(cli: &Cli) -> bool {
    use std::io::IsTerminal as _;
    color_for(cli, std::io::stdout().is_terminal())
}
