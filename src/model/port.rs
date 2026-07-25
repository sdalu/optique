use serde::{Deserialize, Serialize};

use super::options::PortOptions;
use super::origin::PortKey;
use crate::optionsfile::SavedOptionsFile;

/// One dependency edge as extracted from _UNIFIED_DEPENDS.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DepEdge {
    pub target: PortKey,
    /// The raw dependency spec (lib.so, pkgname>=x, path, ...).
    pub spec: String,
}

/// Everything queried from the ports tree for one port@flavor.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortInfo {
    /// The key as it was requested (dep edge / command line form).
    pub key: PortKey,
    /// Canonical identity: origin plus the flavor make actually selected
    /// (None when the port has no flavors).
    pub canonical: PortKey,
    pub pkgname: String,
    pub flavors: Vec<String>,
    pub options_name: String,
    pub options: PortOptions,
    pub deps: Vec<DepEdge>,
    pub broken: Option<String>,
    pub ignore: Option<String>,
    pub deprecated: Option<String>,
    pub default_versions: Vec<String>,
    /// Non-fatal oddities (unparsable dep entries, ...).
    pub warnings: Vec<String>,
}

/// Configuration state of a port relative to the target options dir.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortStatus {
    /// No options at all, or saved file matches the current option list.
    Ok,
    /// Port has options but no saved options file.
    Unconfigured,
    /// Saved file's option list differs from the current one.
    Stale { added: Vec<String>, removed: Vec<String> },
}

impl PortInfo {
    /// Derive the status from the saved options file (if any).
    pub fn status(&self, saved: Option<&SavedOptionsFile>) -> PortStatus {
        if !self.options.has_options() {
            return PortStatus::Ok;
        }
        let Some(saved) = saved else {
            return PortStatus::Unconfigured;
        };
        let current: std::collections::BTreeSet<&str> =
            self.options.complete.iter().map(String::as_str).collect();
        let recorded: std::collections::BTreeSet<&str> =
            saved.complete.iter().map(String::as_str).collect();
        if current == recorded {
            PortStatus::Ok
        } else {
            PortStatus::Stale {
                added: current.difference(&recorded).map(|s| s.to_string()).collect(),
                removed: recorded.difference(&current).map(|s| s.to_string()).collect(),
            }
        }
    }
}
