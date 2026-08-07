use serde::{Deserialize, Serialize};

use super::options::PortOptions;
use super::origin::PortKey;

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
    /// The port's pkg-help file (`${PKGHELP}`, i.e. `${PKGDIR}/pkg-help`) when
    /// it exists — the free-form port notes `make config` offers behind its
    /// Help button. Only a minority of ports ship one. Existence is settled at
    /// query time: a cache generation is one ports-tree commit, so the file
    /// cannot appear or vanish under a cached entry.
    #[serde(default)]
    pub pkg_help: Option<String>,
    pub default_versions: Vec<String>,
    /// Non-fatal oddities (unparsable dep entries, ...).
    pub warnings: Vec<String>,
}

// NOTE: per-port staleness deliberately has no direct API here — flavors
// share options files owned by the default flavor, so status must be asked
// of session::Session, which knows the owner.
