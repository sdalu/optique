//! Reading what a query printed: the wrapper's `OPTIQUE|...` lines made a
//! [`PortInfo`], `_UNIFIED_DEPENDS` made dependency edges.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Result, bail};

use crate::model::options::{GroupKind, OptionGroup, PortOptions};
use crate::model::origin::PortKey;
use crate::model::port::{DepEdge, PortAbout, PortInfo};

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
    let mut pkg_help = String::new();
    let mut about = PortAbout::default();
    let mut descr = String::new();
    let mut build_deps = String::new();
    let mut subpackage_deps = Vec::new();
    let mut default_versions: Vec<String> = Vec::new();
    let mut makefiles: Vec<String> = Vec::new();
    let mut saw_sentinel = false;

    for line in text.lines() {
        let Some(pos) = line.find("OPTIQUE|") else {
            continue;
        };
        saw_sentinel = true;
        let rest = &line[pos + "OPTIQUE|".len()..];
        let (tag, value) = rest.split_once('|').unwrap_or((rest, ""));
        match tag {
            "PKGNAME" => pkgname = value.trim().to_string(),
            "FLAVORS" => flavors = words(value),
            "FLAVOR" => flavor = value.trim().to_string(),
            "OPTIONS_NAME" => {
                options_name = value.trim().to_string();
                opts.name = options_name.clone();
            }
            // Dumped only when make.conf moved it (bsd.options.mk:183).
            "OPTIONS_FILE" => opts.moved_file = Some(value.trim().to_string()),
            "COMPLETE" => {
                // preserve order, drop duplicates
                let mut seen = BTreeSet::new();
                opts.complete = words::<Vec<String>>(value)
                    .into_iter()
                    .filter(|o| seen.insert(o.clone()))
                    .collect();
            }
            "DEFAULT" => opts.defaults = words(value),
            "ALL" => opts.all = words(value),
            "PORT_OPTIONS" => opts.effective = words(value),
            // Dumped only `.if defined(OPTIONS_OVERRIDE)` (bsd.options.mk:
            // 300-304): an absent tag leaves `override` None (undefined), a
            // present one with an empty value is Some(empty) (defined
            // empty) — the two are not the same decision.
            "OVERRIDE" => opts.r#override = Some(words(value)),
            "DEPENDS" => {
                (deps, warnings) = parse_depends(value);
                subpackage_deps = parse_subpackage_deps(value);
            }
            "BUILDDEPS" => build_deps = value.to_string(),
            "MC_SET" => opts.mc_set = words(value),
            "MC_UNSET" => opts.mc_unset = words(value),
            "PORT_SET" => opts.port_set = words(value),
            "PORT_UNSET" => opts.port_unset = words(value),
            "FORCE_SET" => opts.force_set = words(value),
            "FORCE_UNSET" => opts.force_unset = words(value),
            "PORT_FORCE_SET" => opts.port_force_set = words(value),
            "PORT_FORCE_UNSET" => opts.port_force_unset = words(value),
            "SLAVE" => opts.slave = words(value),
            "WITH" => opts.with = words(value),
            "WITHOUT" => opts.without = words(value),
            "DEFAULT_VERSIONS" => default_versions = words(value),
            // As make names them; `makerunner` keeps the main tree's.
            "MAKEFILES" => makefiles = words(value),
            "BROKEN" => broken = shown(value),
            "IGNORE" => ignore = shown(value),
            "DEPRECATED" => deprecated = shown(value),
            "PKGHELP" => pkg_help = value.trim().to_string(),
            "COMMENT" => about.comment = shown(value),
            "WWW" => about.www = words::<Vec<String>>(&shown(value)).join(" "),
            "DESCR" => descr = value.trim().to_string(),
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
                let Some((name, v)) = value.split_once('|') else {
                    continue;
                };
                let name = name.trim();
                let v = &shown(v);
                // Group descriptions land on the group, everything else on defs.
                if tag == "DESC"
                    && let Some(g) = opts.groups.iter_mut().find(|g| g.name == name)
                {
                    g.desc = v.to_string();
                    continue;
                }
                let def = opts.defs.entry(name.to_string()).or_default();
                match tag {
                    "DESC" => def.desc = v.to_string(),
                    "IMPLIES" => def.implies = words(v),
                    "PREVENTS" => def.prevents = words(v),
                    "PREVENTS_MSG" => def.prevents_msg = Some(v.to_string()),
                    "OPT_BROKEN" => def.broken = Some(v.to_string()),
                    _ => def.ignore = Some(v.to_string()),
                }
            }
            // `OPTIQUE|OPT_DEP|<opt>|<class>|<value>`: the dependency lists (and
            // USES) a single option contributes. Exotic specs are silently
            // skipped — option-level noise does not belong in info.warnings.
            "OPT_DEP" => {
                let Some((name, rest)) = value.split_once('|') else {
                    continue;
                };
                let Some((class, v)) = rest.split_once('|') else {
                    continue;
                };
                let name = name.trim();
                let class = class.trim();
                let def = opts.defs.entry(name.to_string()).or_default();
                if class == "uses" {
                    def.uses = words(v);
                    continue;
                }
                let (edges, _) = parse_depends(v);
                if edges.is_empty() {
                    continue;
                }
                let origins: Vec<String> = edges.iter().map(|e| e.target.to_string()).collect();
                match def.deps.iter_mut().find(|(c, _)| c == class) {
                    Some((_, list)) => {
                        for o in origins {
                            if !list.contains(&o) {
                                list.push(o);
                            }
                        }
                    }
                    None => def.deps.push((class.to_string(), origins)),
                }
            }
            _ => {}
        }
    }

    if !saw_sentinel {
        bail!(
            "no OPTIQUE output found (make failed?):\n{}",
            last_lines(text, 6, "\n")
        );
    }
    if pkgname.is_empty() {
        bail!(
            "wrapper dump incomplete (no PKGNAME):\n{}",
            last_lines(text, 6, "\n")
        );
    }

    // bsd.options.mk:288: "Some options are always enabled by default" —
    // DOCS/NLS/EXAMPLES/IPV6 are force-added to PORT_OPTIONS when the port
    // carries them, independent of OPTIONS_DEFAULT. Fold them into defaults
    // so the no-file outcome and the def:on|off display match the framework.
    for opt in ["DOCS", "NLS", "EXAMPLES", "IPV6"] {
        if opts.complete.iter().any(|o| o == opt) {
            opts.defaults.insert(opt.to_string());
        }
    }

    // On a tree before ports e7679b71cf89 _UNIFIED_DEPENDS folds
    // TEST_DEPENDS in, so mark the edges no other dependency list asks for:
    // they are in the closure, but poudriere builds them only under
    // `bulk -t` (on a current tree none is). Warnings from this pass are
    // dropped — the unified list already reported every unparsable entry.
    let built: BTreeSet<PortKey> = parse_depends(&build_deps)
        .0
        .into_iter()
        .map(|e| e.target)
        .collect();
    for edge in &mut deps {
        edge.test_only = !built.contains(&edge.target);
    }

    // A flavor the port does not have: make only sets IGNORE (bsd.port.mk
    // ~1470), poudriere refuses it and gathers none of its dependencies; so
    // does optique, rather than walk a build that never happens. An
    // `@flavor~subpkg` spelling keeps its `~subpkg` in the flavor, as
    // do-depends.sh does (docs/conformance.md), and is left to that.
    if let Some(asked) = &requested.flavor
        && !asked.contains('~')
        && !flavors.iter().any(|f| f == asked)
    {
        if flavors.is_empty() {
            bail!("has an unknown flavor '{asked}', this port does not have any flavors");
        }
        bail!(
            "has an unknown flavor '{asked}', possible flavors: {}",
            flavors.join(" ")
        );
    }

    let canonical = PortKey::new(
        requested.origin.clone(),
        if flavors.is_empty() || flavor.is_empty() {
            None
        } else {
            Some(flavor)
        },
    );

    Ok(PortInfo {
        key: requested.clone(),
        canonical,
        pkgname,
        flavors,
        options_name,
        options: opts,
        deps,
        subpackage_deps,
        broken: non_empty(broken),
        ignore: non_empty(ignore),
        deprecated: non_empty(deprecated),
        // ${PKGHELP} is always defined; only the minority of ports that ship
        // the file get the marker, so the path is kept only when it is there.
        pkg_help: existing_file(pkg_help),
        about: PortAbout {
            pkg_descr: existing_file(descr),
            ..about
        },
        default_versions,
        warnings,
        makefiles,
        outside_files: Vec::new(),
        sub_make: false,
    })
}

/// Parse `_UNIFIED_DEPENDS`: whitespace-separated
/// `spec:origin[@flavor][~subpkg][:target]`, each origin read as
/// do-depends.sh reads it (`PortKey::parse_dependency`).
pub fn parse_depends(value: &str) -> (Vec<DepEdge>, Vec<String>) {
    let mut edges: Vec<DepEdge> = Vec::new();
    let mut warnings = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in value.split_whitespace() {
        let fields: Vec<&str> = entry.split(':').collect();
        // The origin is the first field after the spec that parses as one;
        // specs may be absolute paths (no ':' inside in practice).
        let target = fields
            .iter()
            .skip(1)
            .find_map(|f| PortKey::parse_dependency(f));
        match target {
            Some(t) => {
                if seen.insert(t.clone()) {
                    // Set by parse_dump, which alone knows the other lists.
                    edges.push(DepEdge {
                        target: t,
                        spec: fields[0].to_string(),
                        test_only: false,
                    });
                }
            }
            None => warnings.push(format!("unparsable dependency entry: {entry}")),
        }
    }
    (edges, warnings)
}

/// The targets of `_UNIFIED_DEPENDS` written `origin~subpkg`, without
/// a flavor, where [`parse_depends`] keeps that form for the edge (its
/// first entry for the target): the edge names the origin, but
/// poudriere's `deps_sanity` hands `check_moved` the originspec whole.
fn parse_subpackage_deps(value: &str) -> Vec<PortKey> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for entry in value.split_whitespace() {
        let fields: Vec<&str> = entry.split(':').collect();
        let Some((field, target)) = fields
            .iter()
            .skip(1)
            .find_map(|f| PortKey::parse_dependency(f).map(|t| (*f, t)))
        else {
            continue;
        };
        if seen.insert(target.clone()) && !field.contains('@') && field.contains('~') {
            out.push(target);
        }
    }
    out
}

/// Whitespace-separated words of `s`, collected into whatever the call site
/// needs — a `Vec<String>` keeping order, or a `BTreeSet<String>` deduping.
fn words<C: FromIterator<String>>(s: &str) -> C {
    s.split_whitespace().map(str::to_string).collect()
}

/// A free-text value (a description, a reason) as it may be shown: a
/// control character a Makefile put in it, an escape sequence or a
/// carriage return, would reach the terminal raw and redraw it, so each one
/// is a space.
fn shown(s: &str) -> String {
    printable(&s.replace('\t', " ")).trim().to_string()
}

/// `s` with every control character but a tab or a newline made a space:
/// for text read from a port's files (pkg-help, pkg-descr, MOVED), whose
/// tabs are laid out where it is drawn and lines split there.
pub(crate) fn printable(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() && c != '\t' && c != '\n' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

fn non_empty(s: String) -> Option<String> {
    if s.trim().is_empty() { None } else { Some(s) }
}

/// `s`, when non-empty and naming a file that exists: the shared shape of
/// `pkg_help` and `pkg_descr`, neither of which the framework guarantees.
fn existing_file(s: String) -> Option<String> {
    non_empty(s).filter(|p| Path::new(p).is_file())
}

/// The last `n` lines of `text`, joined with `sep`.
pub(crate) fn last_lines(text: &str, n: usize, sep: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join(sep)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::options::OptionDef;

    #[test]
    fn depends_forms() {
        let (edges, warns) = parse_depends(
            "/usr/local/sbin/pkg:ports-mgmt/pkg libpcre2-8.so:devel/pcre2 \
             py312-foo>=1:devel/py-foo@py312 msgfmt:devel/gettext:build garbage",
        );
        let targets: Vec<String> = edges.iter().map(|e| e.target.to_string()).collect();
        assert_eq!(
            targets,
            vec![
                "ports-mgmt/pkg",
                "devel/pcre2",
                "devel/py-foo@py312",
                "devel/gettext"
            ]
        );
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
        // A subpackage names its port; after a flavor it stays in it.
        let (edges, warns) = parse_depends("bar>0:devel/bar~lib baz>0:devel/baz@fl~lib");
        let targets: Vec<String> = edges.iter().map(|e| e.target.to_string()).collect();
        assert_eq!(targets, ["devel/bar", "devel/baz@fl~lib"]);
        assert!(warns.is_empty(), "{warns:?}");
        let (edges, _) = parse_depends("/usr/local/bin/gmake:devel/gmake");
        assert_eq!(edges[0].target.to_string(), "devel/gmake");
        assert_eq!(edges[0].spec, "/usr/local/bin/gmake");
    }

    /// A dependency written `origin~sub`, without a flavor, is noted for
    /// MOVED; one with a flavor keeps its `~sub` in it, and an edge first
    /// written as the plain origin stays plain.
    #[test]
    fn subpackage_dependencies_are_noted() {
        let got = parse_subpackage_deps(
            "a>0:devel/a~lib b>0:devel/b@fl~lib c>0:devel/c c.so:devel/c~lib d:devel/d~x d.so:devel/d",
        );
        let got: Vec<String> = got.iter().map(ToString::to_string).collect();
        assert_eq!(got, ["devel/a", "devel/d"]);
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

    /// A Makefile's escape sequence or carriage return in a description or
    /// a reason never reaches the terminal: each control character is a
    /// space, the text around it kept.
    #[test]
    fn control_characters_in_text_are_spaces() {
        let text = "\
make: /dev/stdin:2: OPTIQUE|PKGNAME|foo-1.0
make: /dev/stdin:3: OPTIQUE|COMPLETE|A
make: /dev/stdin:4: OPTIQUE|DESC|A|red\x1b[31m\rtext
make: /dev/stdin:5: OPTIQUE|COMMENT|a\x07b\r
make: /dev/stdin:6: OPTIQUE|BROKEN|x\x1b]0;t\x07y
make: /dev/stdin:7: OPTIQUE|OPT_IGNORE|A|z\x1bw
make: /dev/stdin:8: OPTIQUE|WWW|https://a\x1b[2J/ https://b/
";
        let key = PortKey::parse("cat/foo").unwrap();
        let info = parse_dump(&key, text).unwrap();
        assert_eq!(info.options.defs["A"].desc, "red [31m text");
        assert_eq!(info.about.comment, "a b");
        assert_eq!(info.broken.as_deref(), Some("x ]0;t y"));
        assert_eq!(info.options.defs["A"].ignore.as_deref(), Some("z w"));
        assert_eq!(info.about.www, "https://a [2J/ https://b/");
    }

    /// PKGHELP is always defined by the framework; only the ports that really
    /// ship the file may claim one, so the path is kept only when it is there.
    #[test]
    fn pkg_help_is_kept_only_when_the_file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let present = tmp.path().join("pkg-help");
        std::fs::write(&present, "notes\n").unwrap();
        let dump = |path: &Path| {
            format!(
                "make: /dev/stdin:2: OPTIQUE|PKGNAME|foo-1.0\n\
                 make: /dev/stdin:3: OPTIQUE|PKGHELP|{}\n",
                path.display()
            )
        };
        let key = PortKey::parse("cat/foo").unwrap();
        let info = parse_dump(&key, &dump(&present)).unwrap();
        assert_eq!(info.pkg_help.as_deref(), Some(present.to_str().unwrap()));

        let missing = tmp.path().join("absent/pkg-help");
        let info = parse_dump(&key, &dump(&missing)).unwrap();
        assert_eq!(info.pkg_help, None);
    }

    /// COMMENT and WWW are kept as given; pkg-descr, like pkg-help, only
    /// when the file is there.
    #[test]
    fn about_keeps_comment_www_and_an_existing_pkg_descr() {
        let tmp = tempfile::tempdir().unwrap();
        let present = tmp.path().join("pkg-descr");
        std::fs::write(&present, "What foo is.\n").unwrap();
        let dump = |path: &Path| {
            format!(
                "make: /dev/stdin:2: OPTIQUE|PKGNAME|foo-1.0\n\
                 make: /dev/stdin:3: OPTIQUE|COMMENT|Does foo things \n\
                 make: /dev/stdin:4: OPTIQUE|WWW|https://a.example/  https://b.example/\n\
                 make: /dev/stdin:5: OPTIQUE|DESCR|{}\n",
                path.display()
            )
        };
        let key = PortKey::parse("cat/foo").unwrap();
        let info = parse_dump(&key, &dump(&present)).unwrap();
        assert_eq!(info.about.comment, "Does foo things");
        assert_eq!(info.about.www, "https://a.example/ https://b.example/");
        assert_eq!(
            info.about.pkg_descr.as_deref(),
            Some(present.to_str().unwrap())
        );

        let info = parse_dump(&key, &dump(&tmp.path().join("absent"))).unwrap();
        assert_eq!(info.about.pkg_descr, None);
    }

    /// Every tag of the dump lands in its own field: a tag the parser drops
    /// silently loses a make.conf layer or a group, so each is checked.
    #[test]
    fn every_dump_tag_lands_in_its_field() {
        let text = "\
OPTIQUE|PKGNAME|foo-1.0
OPTIQUE|COMPLETE|A B C D E F G H I J K
OPTIQUE|DEFAULT|A
OPTIQUE|ALL|A B
OPTIQUE|PORT_OPTIONS|A B
OPTIQUE|MC_SET|C
OPTIQUE|MC_UNSET|D
OPTIQUE|PORT_SET|E
OPTIQUE|PORT_UNSET|F
OPTIQUE|FORCE_SET|G
OPTIQUE|FORCE_UNSET|H
OPTIQUE|PORT_FORCE_SET|I
OPTIQUE|PORT_FORCE_UNSET|J
OPTIQUE|SLAVE|K
OPTIQUE|WITH|B
OPTIQUE|WITHOUT|C*
OPTIQUE|OVERRIDE|A
OPTIQUE|OPTIONS_FILE|/elsewhere/options
OPTIQUE|DEFAULT_VERSIONS|ssl=base python=3.12
OPTIQUE|GROUP|FREE|A B
OPTIQUE|SINGLE|ONE|C D
OPTIQUE|RADIO|MAYBE|E F
OPTIQUE|MULTI|SOME|G H
";
        let set = |s: &[&str]| s.iter().map(ToString::to_string).collect::<BTreeSet<_>>();
        let key = PortKey::parse("cat/foo").unwrap();
        let info = parse_dump(&key, text).unwrap();
        let o = &info.options;
        assert_eq!(o.defaults, set(&["A"]));
        assert_eq!(o.all, set(&["A", "B"]));
        assert_eq!(o.effective, set(&["A", "B"]));
        assert_eq!(o.mc_set, set(&["C"]));
        assert_eq!(o.mc_unset, set(&["D"]));
        assert_eq!(o.port_set, set(&["E"]));
        assert_eq!(o.port_unset, set(&["F"]));
        assert_eq!(o.force_set, set(&["G"]));
        assert_eq!(o.force_unset, set(&["H"]));
        assert_eq!(o.port_force_set, set(&["I"]));
        assert_eq!(o.port_force_unset, set(&["J"]));
        assert_eq!(o.slave, set(&["K"]));
        assert_eq!(o.with, set(&["B"]));
        assert_eq!(o.without, set(&["C*"]));
        assert_eq!(o.r#override, Some(set(&["A"])));
        assert_eq!(o.moved_file.as_deref(), Some("/elsewhere/options"));
        assert_eq!(info.default_versions, ["ssl=base", "python=3.12"]);
        // Absent, OPTIONS_FILE is the file optique manages.
        assert_eq!(
            parse_dump(&key, "OPTIQUE|PKGNAME|foo-1.0\n")
                .unwrap()
                .options
                .moved_file,
            None
        );
        let groups: Vec<(GroupKind, &str, Vec<String>)> = o
            .groups
            .iter()
            .map(|g| (g.kind, g.name.as_str(), g.members.clone()))
            .collect();
        assert_eq!(
            groups,
            [
                (
                    GroupKind::Group,
                    "FREE",
                    vec!["A".to_string(), "B".to_string()]
                ),
                (
                    GroupKind::Single,
                    "ONE",
                    vec!["C".to_string(), "D".to_string()]
                ),
                (
                    GroupKind::Radio,
                    "MAYBE",
                    vec!["E".to_string(), "F".to_string()]
                ),
                (
                    GroupKind::Multi,
                    "SOME",
                    vec!["G".to_string(), "H".to_string()]
                ),
            ]
        );
    }

    /// bsd.options.mk:300-304: the wrapper dumps OVERRIDE only `.if
    /// defined(OPTIONS_OVERRIDE)`, so an absent line (undefined) and a
    /// present, empty one (defined empty) must parse to different values.
    #[test]
    fn override_undefined_differs_from_defined_empty() {
        let key = PortKey::parse("cat/foo").unwrap();
        let undefined = parse_dump(&key, "OPTIQUE|PKGNAME|foo-1.0\n").unwrap();
        assert_eq!(undefined.options.r#override, None);
        let defined_empty =
            parse_dump(&key, "OPTIQUE|PKGNAME|foo-1.0\nOPTIQUE|OVERRIDE|\n").unwrap();
        assert_eq!(defined_empty.options.r#override, Some(BTreeSet::new()));
    }

    /// A FLAVOR on a port with no FLAVORS is no flavor of it.
    #[test]
    fn a_flavor_without_flavors_is_dropped() {
        let key = PortKey::parse("cat/foo").unwrap();
        let info = parse_dump(&key, "OPTIQUE|PKGNAME|foo-1.0\nOPTIQUE|FLAVOR|x\n").unwrap();
        assert_eq!(info.canonical, key);
    }

    /// A flavor the port does not have is refused, as poudriere refuses it:
    /// none of its dependencies is gathered.
    #[test]
    fn an_unknown_flavor_is_an_error() {
        let key = PortKey::parse("devel/py-six@py311").unwrap();
        let text = "OPTIQUE|PKGNAME|py312-six-1.0\nOPTIQUE|FLAVORS|py312\nOPTIQUE|FLAVOR|py311\n";
        let err = parse_dump(&key, text).unwrap_err().to_string();
        assert!(
            err.contains("unknown flavor 'py311', possible flavors: py312"),
            "{err}"
        );
        let key = PortKey::parse("devel/gmake@bogus").unwrap();
        let err = parse_dump(&key, "OPTIQUE|PKGNAME|gmake-4\nOPTIQUE|FLAVOR|bogus\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not have any flavors"), "{err}");
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
    fn always_on_framework_defaults_are_folded_in() {
        let key = PortKey::parse("cat/foo").unwrap();
        let text = "OPTIQUE|PKGNAME|foo-1\nOPTIQUE|COMPLETE|DOCS NLS X11\nOPTIQUE|DEFAULT|X11\n";
        let info = parse_dump(&key, text).unwrap();
        for opt in ["DOCS", "NLS", "X11"] {
            assert!(
                info.options.defaults.contains(opt),
                "{opt} must be a default"
            );
        }
        // EXAMPLES/IPV6 are not in this port's option list: not added.
        assert!(!info.options.defaults.contains("EXAMPLES"));
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
make: /dev/stdin:15: OPTIQUE|OPT_DEP|LUA|lib|libluajit-5.1.so:lang/luajit-openresty
make: /dev/stdin:16: OPTIQUE|OPT_DEP|LUA|run|lua-resty-core>0:www/lua-resty-core lua-resty-core>0:www/lua-resty-core
make: /dev/stdin:17: OPTIQUE|OPT_DEP|GSSAPI_MIT|uses|gssapi:mit
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
        let lua = &info.options.defs["LUA"];
        assert_eq!(
            lua.deps,
            vec![
                ("lib".to_string(), vec!["lang/luajit-openresty".to_string()]),
                ("run".to_string(), vec!["www/lua-resty-core".to_string()]),
            ],
            "per-class origins, deduped, in emission order"
        );
        assert!(lua.uses.is_empty());
        assert_eq!(info.options.defs["GSSAPI_MIT"].uses, vec!["gssapi:mit"]);
    }

    /// `_UNIFIED_DEPENDS` folds `TEST_DEPENDS` in; the second list says which
    /// edges the other dependency kinds also ask for.
    #[test]
    fn test_only_dependencies_are_marked() {
        let key = PortKey::parse("cat/foo").unwrap();
        let text = "\
OPTIQUE|PKGNAME|foo-1
OPTIQUE|DEPENDS|a.so:cat/lib b>0:cat/tester c:cat/both
OPTIQUE|BUILDDEPS|a.so:cat/lib c:cat/both
";
        let info = parse_dump(&key, text).unwrap();
        let flags: Vec<(String, bool)> = info
            .deps
            .iter()
            .map(|d| (d.target.to_string(), d.test_only))
            .collect();
        assert_eq!(
            flags,
            vec![
                ("cat/lib".to_string(), false),
                ("cat/tester".to_string(), true),
                ("cat/both".to_string(), false),
            ]
        );
    }

    /// A dump with no BUILDDEPS line at all (a schema older than the cache
    /// generation should never reach us, but nothing may pretend an edge is
    /// build-relevant on no evidence).
    #[test]
    fn without_a_build_list_every_edge_counts_as_test_only() {
        let key = PortKey::parse("cat/foo").unwrap();
        let text = "OPTIQUE|PKGNAME|foo-1\nOPTIQUE|DEPENDS|a.so:cat/lib\n";
        let info = parse_dump(&key, text).unwrap();
        assert!(info.deps[0].test_only);
    }

    #[test]
    fn opt_dep_skips_unparsable_and_merges_repeats() {
        let key = PortKey::parse("cat/foo").unwrap();
        let text = "\
OPTIQUE|PKGNAME|foo-1
OPTIQUE|COMPLETE|A
OPTIQUE|OPT_DEP|A|build|garbage
OPTIQUE|OPT_DEP|A|run|x:cat/one
OPTIQUE|OPT_DEP|A|run|y:cat/two x:cat/one
";
        let info = parse_dump(&key, text).unwrap();
        let def = &info.options.defs["A"];
        assert_eq!(def.deps.len(), 1, "unparsable-only record adds no class");
        assert_eq!(def.deps[0].0, "run");
        assert_eq!(def.deps[0].1, vec!["cat/one", "cat/two"]);
        assert!(
            info.warnings.is_empty(),
            "option-level noise stays out of warnings"
        );
    }

    /// Cached PortInfos are read back from JSONL written by older versions:
    /// an OptionDef without the newer fields must still deserialize.
    #[test]
    fn option_def_deserializes_without_new_fields() {
        let json = r#"{"desc":"x","implies":[],"prevents":[],"prevents_msg":null,
                       "broken":null,"ignore":null}"#;
        let def: OptionDef = serde_json::from_str(json).unwrap();
        assert_eq!(def.desc, "x");
        assert!(def.deps.is_empty());
        assert!(def.uses.is_empty());
    }
}
