use serde::{Deserialize, Serialize};

use super::options::PortOptions;
use super::origin::PortKey;

/// One dependency edge as extracted from _UNIFIED_DEPENDS.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DepEdge {
    pub target: PortKey,
    /// The raw dependency spec (lib.so, pkgname>=x, path, ...).
    pub spec: String,
    /// The port asks for this one only as a `TEST_DEPENDS`. `poudriere bulk`
    /// builds those only under `-t`, so such an edge belongs to the closure
    /// but not to the build order.
    pub test_only: bool,
}

/// The port's own description: `COMMENT`, `WWW` and its pkg-descr file
/// (`${DESCR}`), kept like pkg-help — the path only when the file exists,
/// the contents read when shown.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PortAbout {
    pub comment: String,
    /// Space-separated, as the Makefile gives it; some ports list several.
    pub www: String,
    pub pkg_descr: Option<String>,
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
    /// query time: an entry is carried over a tree update only when nothing
    /// in the port's directory changed (`cache::Invalidation`), so the file
    /// cannot appear or vanish under a cached entry.
    #[serde(default)]
    pub pkg_help: Option<String>,
    /// What the port says it is, for the editor's description panel.
    #[serde(default)]
    pub about: PortAbout,
    pub default_versions: Vec<String>,
    /// Non-fatal oddities (unparsable dep entries, ...).
    pub warnings: Vec<String>,
    /// The main tree's files make read for the query (`.MAKE.MAKEFILES`),
    /// relative to it, sorted: what a cache entry is carried over a tree
    /// update on (`cache::Invalidation`).
    #[serde(default)]
    pub makefiles: Vec<String>,
}

impl PortInfo {
    /// Is `canonical`'s flavor the port's default (the first of `flavors`)?
    /// Poudriere's blacklist (`Blacklist::matches`) treats the default
    /// flavor specially (`origin@-` also matches it); a flavorless port has
    /// no flavor to be default or not, so this is vacuously true for one.
    pub fn is_default_flavor(&self) -> bool {
        self.canonical.flavor.as_deref() == self.flavors.first().map(String::as_str)
    }
}

#[cfg(test)]
impl PortInfo {
    /// A port with nothing but its origin (`cat/port` or `cat/port@flavor`):
    /// named as the framework would name it, `port-1.0` and `cat_port`, and
    /// otherwise empty. Tests override what they need.
    pub fn stub(origin: &str) -> Self {
        let key = PortKey::parse(origin).expect("test origin parses");
        PortInfo {
            canonical: key.clone(),
            pkgname: format!("{}-1.0", key.origin.rsplit('/').next().unwrap_or_default()),
            options_name: key.origin.replace('/', "_"),
            key,
            flavors: vec![],
            options: PortOptions::default(),
            deps: vec![],
            broken: None,
            ignore: None,
            deprecated: None,
            pkg_help: None,
            about: PortAbout::default(),
            default_versions: vec![],
            warnings: vec![],
            makefiles: vec![],
        }
    }
}

// NOTE: per-port staleness deliberately has no direct API here — flavors
// share options files owned by the default flavor, so status must be asked
// of session::Session, which knows the owner.
