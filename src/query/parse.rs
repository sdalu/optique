use std::collections::BTreeSet;

use anyhow::{bail, Result};

use crate::model::options::{GroupKind, OptionDef, OptionGroup, PortOptions};
use crate::model::origin::PortKey;
use crate::model::port::{DepEdge, PortInfo};

/// Parse the combined stdout+stderr of a wrapper invocation into a PortInfo.
/// `requested` is the key the query was made for.
pub fn parse_dump(requested: &PortKey, text: &str) -> Result<PortInfo> {
    let mut pkgname = String::new();
    let mut flavors: Vec<String> = Vec::new();
    let mut flavor = String::new();
    let mut options_name = String::new();
    let mut opts = PortOptions::default();
    let mut deps: Vec<DepEdge> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut broken = String::new();
    let mut ignore = String::new();
    let mut deprecated = String::new();
    let mut default_versions: Vec<String> = Vec::new();
    let mut saw_sentinel = false;

    for line in text.lines() {
        let Some(pos) = line.find("OPTIQUE|") else { continue };
        saw_sentinel = true;
        let rest = &line[pos + "OPTIQUE|".len()..];
        let (tag, value) = rest.split_once('|').unwrap_or((rest, ""));
        match tag {
            "PKGNAME" => pkgname = value.trim().to_string(),
            "FLAVORS" => flavors = words(value),
            "FLAVOR" => flavor = value.trim().to_string(),
            "OPTIONS_NAME" => options_name = value.trim().to_string(),
            "COMPLETE" => {
                // preserve order, drop duplicates
                let mut seen = BTreeSet::new();
                opts.complete =
                    words(value).into_iter().filter(|o| seen.insert(o.clone())).collect();
            }
            "DEFAULT" => opts.defaults = wordset(value),
            "PORT_OPTIONS" => opts.effective = wordset(value),
            "DEPENDS" => (deps, warnings) = parse_depends(value),
            "MC_SET" => opts.mc_set = wordset(value),
            "MC_UNSET" => opts.mc_unset = wordset(value),
            "PORT_SET" => opts.port_set = wordset(value),
            "PORT_UNSET" => opts.port_unset = wordset(value),
            "FORCE_SET" => opts.force_set = wordset(value),
            "FORCE_UNSET" => opts.force_unset = wordset(value),
            "DEFAULT_VERSIONS" => default_versions = words(value),
            "BROKEN" => broken = value.trim().to_string(),
            "IGNORE" => ignore = value.trim().to_string(),
            "DEPRECATED" => deprecated = value.trim().to_string(),
            "GROUP" | "SINGLE" | "RADIO" | "MULTI" => {
                let kind = match tag {
                    "GROUP" => GroupKind::Group,
                    "SINGLE" => GroupKind::Single,
                    "RADIO" => GroupKind::Radio,
                    _ => GroupKind::Multi,
                };
                let (name, members) = value.split_once('|').unwrap_or((value, ""));
                opts.groups.push(OptionGroup {
                    kind,
                    name: name.trim().to_string(),
                    desc: String::new(),
                    members: words(members),
                });
            }
            "DESC" | "IMPLIES" | "PREVENTS" | "PREVENTS_MSG" | "OPT_BROKEN" | "OPT_IGNORE" => {
                let Some((name, v)) = value.split_once('|') else { continue };
                let name = name.trim();
                let v = v.trim();
                // Group descriptions land on the group, everything else on defs.
                if tag == "DESC" {
                    if let Some(g) = opts.groups.iter_mut().find(|g| g.name == name) {
                        g.desc = v.to_string();
                        continue;
                    }
                }
                let def = opts.defs.entry(name.to_string()).or_insert_with(OptionDef::default);
                match tag {
                    "DESC" => def.desc = v.to_string(),
                    "IMPLIES" => def.implies = words(v),
                    "PREVENTS" => def.prevents = words(v),
                    "PREVENTS_MSG" => def.prevents_msg = Some(v.to_string()),
                    "OPT_BROKEN" => def.broken = Some(v.to_string()),
                    _ => def.ignore = Some(v.to_string()),
                }
            }
            _ => {}
        }
    }

    if !saw_sentinel {
        bail!("no OPTIQUE output found (make failed?):\n{}", tail(text, 6));
    }
    if pkgname.is_empty() {
        bail!("wrapper dump incomplete (no PKGNAME):\n{}", tail(text, 6));
    }

    let canonical = PortKey::new(
        requested.origin.clone(),
        if flavors.is_empty() || flavor.is_empty() { None } else { Some(flavor.clone()) },
    );

    Ok(PortInfo {
        key: requested.clone(),
        canonical,
        pkgname,
        flavors,
        options_name,
        options: opts,
        deps,
        broken: non_empty(broken),
        ignore: non_empty(ignore),
        deprecated: non_empty(deprecated),
        default_versions,
        warnings,
    })
}

/// Parse `_UNIFIED_DEPENDS`: whitespace-separated `spec:origin[@flavor][:target]`.
pub fn parse_depends(value: &str) -> (Vec<DepEdge>, Vec<String>) {
    let mut edges: Vec<DepEdge> = Vec::new();
    let mut warnings = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in value.split_whitespace() {
        let fields: Vec<&str> = entry.split(':').collect();
        // The origin is the first field after the spec that parses as one;
        // specs may be absolute paths (no ':' inside in practice).
        let target = fields.iter().skip(1).find_map(|f| PortKey::parse(f));
        match target {
            Some(t) => {
                if seen.insert(t.clone()) {
                    edges.push(DepEdge { target: t, spec: fields[0].to_string() });
                }
            }
            None => warnings.push(format!("unparsable dependency entry: {entry}")),
        }
    }
    (edges, warnings)
}

fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

fn wordset(s: &str) -> BTreeSet<String> {
    s.split_whitespace().map(str::to_string).collect()
}

fn non_empty(s: String) -> Option<String> {
    if s.trim().is_empty() { None } else { Some(s) }
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depends_forms() {
        let (edges, warns) = parse_depends(
            "/usr/local/sbin/pkg:ports-mgmt/pkg libpcre2-8.so:devel/pcre2 \
             py312-foo>=1:devel/py-foo@py312 msgfmt:devel/gettext:build garbage",
        );
        let targets: Vec<String> = edges.iter().map(|e| e.target.to_string()).collect();
        assert_eq!(targets, vec!["ports-mgmt/pkg", "devel/pcre2", "devel/py-foo@py312", "devel/gettext"]);
        assert_eq!(warns.len(), 1);
    }

    #[test]
    fn depends_edge_cases() {
        // Empty input, duplicate targets collapse, flavored target kept.
        let (edges, warns) = parse_depends("");
        assert!(edges.is_empty() && warns.is_empty());
        let (edges, _) = parse_depends("a.so:x/y b>0:x/y c:x/y@f");
        assert_eq!(edges.len(), 2, "same origin deduped, flavored is distinct");
        // Path specs with slashes must not be mistaken for origins.
        let (edges, _) = parse_depends("/usr/local/bin/gmake:devel/gmake");
        assert_eq!(edges[0].target.to_string(), "devel/gmake");
        assert_eq!(edges[0].spec, "/usr/local/bin/gmake");
    }

    #[test]
    fn desc_with_pipe_survives() {
        let text = "\
make: /dev/stdin:2: OPTIQUE|PKGNAME|foo-1.0
make: /dev/stdin:3: OPTIQUE|COMPLETE|A
make: /dev/stdin:4: OPTIQUE|DESC|A|use X | Y syntax
";
        let key = PortKey::parse("cat/foo").unwrap();
        let info = parse_dump(&key, text).unwrap();
        assert_eq!(info.options.defs["A"].desc, "use X | Y syntax");
    }

    #[test]
    fn failures_are_errors_not_panics() {
        let key = PortKey::parse("cat/foo").unwrap();
        // No sentinel at all (make crashed early).
        assert!(parse_dump(&key, "make: error: something exploded").is_err());
        // Sentinel but no PKGNAME.
        assert!(parse_dump(&key, "x OPTIQUE|COMPLETE|A B").is_err());
    }

    #[test]
    fn canonical_flavor_resolution() {
        let key = PortKey::parse("devel/py-foo").unwrap();
        let text = "\
OPTIQUE|PKGNAME|py312-foo-1.0
OPTIQUE|FLAVORS|py312 py311
OPTIQUE|FLAVOR|py312
";
        let info = parse_dump(&key, text).unwrap();
        assert_eq!(info.canonical.to_string(), "devel/py-foo@py312");
        // Unflavored port: canonical == plain origin.
        let text2 = "OPTIQUE|PKGNAME|bar-1.0\nOPTIQUE|FLAVORS|\nOPTIQUE|FLAVOR|\n";
        let key2 = PortKey::parse("cat/bar").unwrap();
        assert_eq!(parse_dump(&key2, text2).unwrap().canonical, key2);
    }

    #[test]
    fn port_level_flags_and_layers() {
        let key = PortKey::parse("cat/foo").unwrap();
        let text = "\
OPTIQUE|PKGNAME|foo-1.0
OPTIQUE|COMPLETE|A B C
OPTIQUE|MC_SET|A GLOBAL_OTHER
OPTIQUE|MC_UNSET|B
OPTIQUE|PORT_SET|C
OPTIQUE|FORCE_SET|A
OPTIQUE|BROKEN|does not build on 15.0
OPTIQUE|IGNORE|requires X
OPTIQUE|DEPRECATED|use cat/bar
OPTIQUE|IMPLIES|A|B
OPTIQUE|PREVENTS|A|C
OPTIQUE|PREVENTS_MSG|A|A and C clash
OPTIQUE|OPT_BROKEN|B|B is busted
";
        let info = parse_dump(&key, text).unwrap();
        let o = &info.options;
        assert!(o.mc_set.contains("A") && o.mc_set.contains("GLOBAL_OTHER"));
        assert!(o.mc_unset.contains("B"));
        assert!(o.port_set.contains("C"));
        assert!(o.force_set.contains("A"));
        assert_eq!(info.broken.as_deref(), Some("does not build on 15.0"));
        assert_eq!(info.ignore.as_deref(), Some("requires X"));
        assert_eq!(info.deprecated.as_deref(), Some("use cat/bar"));
        assert_eq!(o.defs["A"].implies, vec!["B"]);
        assert_eq!(o.defs["A"].prevents, vec!["C"]);
        assert_eq!(o.defs["A"].prevents_msg.as_deref(), Some("A and C clash"));
        assert_eq!(o.defs["B"].broken.as_deref(), Some("B is busted"));
    }

    #[test]
    fn complete_list_dedups_preserving_order() {
        let key = PortKey::parse("cat/foo").unwrap();
        let text = "OPTIQUE|PKGNAME|foo-1\nOPTIQUE|COMPLETE|B A B C A\n";
        let info = parse_dump(&key, text).unwrap();
        assert_eq!(info.options.complete, vec!["B", "A", "C"]);
    }

    #[test]
    fn dump_smoke() {
        let text = "\
make: /dev/stdin:2: OPTIQUE|PKGNAME|nginx-1.30.4,3
make: /dev/stdin:3: OPTIQUE|FLAVORS|
make: /dev/stdin:4: OPTIQUE|FLAVOR|
make: /dev/stdin:5: OPTIQUE|OPTIONS_NAME|www_nginx
make: /dev/stdin:6: OPTIQUE|COMPLETE|DSO IPV6 LUA GSSAPI_MIT GSSAPI_HEIMDAL
make: /dev/stdin:7: OPTIQUE|DEFAULT|DSO IPV6
make: /dev/stdin:8: OPTIQUE|PORT_OPTIONS|DSO IPV6
make: /dev/stdin:9: OPTIQUE|DEPENDS|libpcre2-8.so:devel/pcre2
make: /dev/stdin:10: OPTIQUE|RADIO|GSSAPI|GSSAPI_HEIMDAL GSSAPI_MIT
make: /dev/stdin:11: OPTIQUE|DESC|LUA|3rd party lua module
make: /dev/stdin:12: OPTIQUE|DESC|GSSAPI|GSSAPI implementation
make: /dev/stdin:13: OPTIQUE|IMPLIES|GSSAPI_MIT|IPV6
make: /dev/stdin:14: OPTIQUE|BROKEN|
";
        let key = PortKey::parse("www/nginx").unwrap();
        let info = parse_dump(&key, text).unwrap();
        assert_eq!(info.pkgname, "nginx-1.30.4,3");
        assert_eq!(info.canonical, key);
        assert_eq!(info.options_name, "www_nginx");
        assert_eq!(info.options.complete.len(), 5);
        assert_eq!(info.options.groups.len(), 1);
        assert_eq!(info.options.groups[0].desc, "GSSAPI implementation");
        assert_eq!(info.options.defs["LUA"].desc, "3rd party lua module");
        assert_eq!(info.options.defs["GSSAPI_MIT"].implies, vec!["IPV6"]);
        assert_eq!(info.deps.len(), 1);
        assert!(info.broken.is_none());
    }
}
