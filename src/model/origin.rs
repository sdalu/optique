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
        PortKey { origin: origin.into(), flavor }
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
        Some(PortKey { origin: origin.to_string(), flavor })
    }

    pub fn with_origin(&self, origin: &str) -> Self {
        PortKey { origin: origin.to_string(), flavor: self.flavor.clone() }
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
                && name.chars().all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
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

    #[test]
    fn reject_malformed() {
        assert!(PortKey::parse("nginx").is_none());
        assert!(PortKey::parse("/usr/local/bin/perl").is_none());
        assert!(PortKey::parse("a/b/c").is_none());
        assert!(PortKey::parse("").is_none());
    }
}
