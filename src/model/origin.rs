//! Port identity: [`PortKey`], an origin with an optional flavor, and the
//! check of a bare origin.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A port identified by its origin and optional flavor: `category/name[@flavor]`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PortKey {
    pub origin: String,
    pub flavor: Option<String>,
}

impl PortKey {
    pub fn new(origin: impl Into<String>, flavor: Option<String>) -> Self {
        PortKey {
            origin: origin.into(),
            flavor,
        }
    }

    /// Parse `category/name[@flavor]`. Returns None on malformed input.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let (origin, flavor) = match s.split_once('@') {
            Some((o, f)) if !f.is_empty() => (o, Some(f.to_string())),
            Some((o, _)) => (o, None),
            None => (s, None),
        };
        if !valid_origin(origin) {
            return None;
        }
        Some(PortKey {
            origin: origin.to_string(),
            flavor,
        })
    }

    /// A dependency's `origin[@flavor][~subpkg]` as the framework's
    /// `Mk/Scripts/do-depends.sh` splits it: an `@` is tested first and its
    /// flavor runs to the end, a `~subpkg` after it staying part of that
    /// flavor (a framework defect that changes which flavor is built, so it
    /// is followed); without an `@`, a `~subpkg` is dropped. Every
    /// subpackage comes out of the one port build, with one options file,
    /// so the origin is what has options.
    pub fn parse_dependency(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.contains('@') {
            return Self::parse(s);
        }
        Self::parse(
            s.rsplit_once('~')
                .filter(|(_, sub)| !sub.contains('/'))
                .map_or(s, |(o, _)| o),
        )
    }

    /// A listed `origin[@flavor][~subpkg]` as poudriere's
    /// `originspec_decode` splits it: the `~subpkg` first, dropped, then the
    /// `@flavor`. A subpackage names one of the packages the port's build
    /// makes, all from one options file.
    pub fn parse_listed(s: &str) -> Option<Self> {
        let s = s.trim();
        Self::parse(s.split_once('~').map_or(s, |(o, _)| o))
    }
}

impl fmt::Display for PortKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.flavor {
            Some(fl) => write!(f, "{}@{}", self.origin, fl),
            None => f.write_str(&self.origin),
        }
    }
}

/// Validate a `category/name` port origin (no flavor suffix).
pub fn valid_origin(s: &str) -> bool {
    let mut parts = s.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(cat), Some(name), None) => {
            !cat.is_empty()
                && !name.is_empty()
                && cat
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain() {
        let k = PortKey::parse("www/nginx").unwrap();
        assert_eq!(k.origin, "www/nginx");
        assert_eq!(k.flavor, None);
        assert_eq!(k.to_string(), "www/nginx");
    }

    #[test]
    fn parse_flavored() {
        let k = PortKey::parse("devel/py-Automat@py312").unwrap();
        assert_eq!(k.origin, "devel/py-Automat");
        assert_eq!(k.flavor.as_deref(), Some("py312"));
        assert_eq!(k.to_string(), "devel/py-Automat@py312");
    }

    /// A subpackage is dropped where it names the port: a dependency's
    /// `origin~sub`, and a listed `origin~sub` or `origin@fl~sub`. A
    /// dependency's `origin@fl~sub` keeps `fl~sub` as its flavor, as
    /// do-depends.sh splits it.
    #[test]
    fn subpackages_name_their_port() {
        let dep = |s| PortKey::parse_dependency(s).map(|k| k.to_string());
        assert_eq!(dep("devel/bar~lib").as_deref(), Some("devel/bar"));
        assert_eq!(
            dep("devel/bar@py311~lib").as_deref(),
            Some("devel/bar@py311~lib")
        );
        assert_eq!(dep("devel/bar@py311").as_deref(), Some("devel/bar@py311"));
        assert_eq!(dep("devel/bar").as_deref(), Some("devel/bar"));
        let listed = |s| PortKey::parse_listed(s).map(|k| k.to_string());
        assert_eq!(listed("devel/bar~lib").as_deref(), Some("devel/bar"));
        assert_eq!(
            listed("devel/bar@py311~lib").as_deref(),
            Some("devel/bar@py311")
        );
        assert_eq!(listed("devel/bar@all").as_deref(), Some("devel/bar@all"));
        assert!(
            PortKey::parse("devel/bar~lib").is_none(),
            "plain parse still refuses it"
        );
    }

    #[test]
    fn reject_malformed() {
        assert!(PortKey::parse("nginx").is_none());
        assert!(PortKey::parse("/usr/local/bin/perl").is_none());
        assert!(PortKey::parse("a/b/c").is_none());
        assert!(PortKey::parse("").is_none());
    }
}
