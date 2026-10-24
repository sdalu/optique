//! What synth(1) does with the ports optique scans: with no list given,
//! the installed packages it roots, matched against its own flavor index;
//! before each build, the check that refuses a saved options file whose
//! entries are not the port's options.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};

use crate::cache::Cache;
use crate::model::origin::PortKey;
use crate::model::port::PortInfo;
use crate::optfiles::OptionsFiles;
use crate::query::makerunner::QueryCtx;
use crate::roots;

/// Origins of everything installed, from pkg(8) — synth's natural root set
/// when no list is given (synth builds what is installed). Flavors come from
/// the pkg "flavor" annotation; `repo` restricts to packages installed from
/// that pkg repository (%R).
pub fn installed_roots(repos: &[String]) -> Result<Vec<PortKey>> {
    let origins = pkg_query("%n\t%o\t%R")?;
    // One line per annotation; only the "flavor" ones matter.
    let annots = pkg_query("%n\t%At\t%Av")?;
    let roots = parse_installed(&origins, &annots, repos);
    if roots.is_empty() {
        if repos.is_empty() {
            bail!("no installed packages with port origins found (pkg query -a %o)");
        }
        bail!(
            "no installed packages from {} \
             (check the names with: pkg query -a %R | sort -u)",
            repos_phrase(repos)
        );
    }
    Ok(roots)
}

/// `pkg query -a FORMAT`, its stdout; a failing query fails the run rather
/// than reading as no packages (or no flavors).
fn pkg_query(format: &str) -> Result<String> {
    let out = std::process::Command::new("pkg")
        .args(["query", "-a", format])
        .output()
        .map_err(|e| anyhow!("cannot run pkg query: {e}"))?;
    if !out.status.success() {
        bail!("pkg query failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// "repository 'a'" or "repositories 'a', 'b'".
pub fn repos_phrase(repos: &[String]) -> String {
    let noun = if repos.len() == 1 { "repository" } else { "repositories" };
    format!("{noun} '{}'", repos.join("', '"))
}

/// Pure part of installed_roots: `origins` lines are
/// `package\torigin\trepository`, `annots` lines are
/// `package\tannotation\tvalue`. Both are keyed by package, not origin:
/// several flavors of one origin can be installed at once (polkit-qt-1 for
/// qt5 and qt6), each its own root. Unparsable origins are skipped,
/// duplicates collapse, the result is sorted.
fn parse_installed(
    origins: &str,
    annots: &str,
    repos: &[String],
) -> Vec<PortKey> {
    let mut flavor: HashMap<&str, &str> = Default::default();
    for line in annots.lines() {
        let mut f = line.split('\t');
        if let (Some(pkg), Some("flavor"), Some(value)) = (f.next(), f.next(), f.next()) {
            flavor.insert(pkg, value);
        }
    }
    let mut roots = BTreeSet::new();
    for line in origins.lines() {
        let mut f = line.split('\t');
        let (Some(pkg), Some(origin), pkg_repo) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let origin = origin.trim();
        if origin.is_empty() {
            continue;
        }
        if !repos.is_empty() {
            let from = pkg_repo.map(str::trim).unwrap_or("");
            if !repos.iter().any(|r| r == from) {
                continue;
            }
        }
        let spec = match flavor.get(pkg) {
            Some(fl) => format!("{origin}@{fl}"),
            None => origin.to_string(),
        };
        // Packages not built from ports may carry unparsable origins: skip.
        if let Some(key) = PortKey::parse(&spec) {
            roots.insert(key);
        }
    }
    roots.into_iter().collect()
}

/// The installed packages synth roots, and those it skips with why
/// ([`match_installed`]): an origin counts only when it is in synth's own
/// index of `ctx`'s trees, each category Makefile's `SUBDIR` entries
/// ([`flavor_index`]), not merely a directory holding a Makefile. The
/// FLAVORS of the origins indexed are queried, the cache first.
pub fn installed_in_index(
    installed: &[PortKey],
    ctx: &QueryCtx,
    files: &OptionsFiles,
    jobs: usize,
    cache: &mut Cache,
) -> (Vec<PortKey>, Vec<(PortKey, &'static str)>) {
    let index = flavor_index(&ctx.portsdir, &ctx.overlays);
    let exists = |o: &str| index.contains(o);
    let origins = installed.iter().map(|k| k.origin.clone()).filter(|o| exists(o)).collect();
    let flavors = roots::query_flavors(&origins, ctx, files, jobs, cache);
    match_installed(installed, exists, &flavors)
}

/// The installed packages synth roots (`installed`: origin and flavor
/// annotation of each), and those it skips with why. synth matches each
/// against its flavor index, where a port without FLAVORS is its bare
/// origin and a flavored one is listed only as `origin@flavor`
/// (portscan-pilot.adb ~1137-1173, portscan.adb ~1726-1762): an origin not
/// in [`flavor_index`] is skipped as "missing from ports" — absent from
/// synth's own index, not merely a directory without a Makefile; a port
/// without FLAVORS is rooted as its bare origin, whatever the annotation;
/// a flavored port only when the annotation names one of its FLAVORS.
/// `exists` says whether the index has an origin, `flavors` gives the
/// FLAVORS of those queried; a port whose FLAVORS are unknown (a failed
/// query) is kept as given for the scan to report.
pub fn match_installed(
    installed: &[PortKey],
    exists: impl Fn(&str) -> bool,
    flavors: &HashMap<String, Vec<String>>,
) -> (Vec<PortKey>, Vec<(PortKey, &'static str)>) {
    let mut kept = Vec::new();
    let mut seen = HashSet::new();
    let mut skipped = Vec::new();
    for key in installed {
        let root = if !exists(&key.origin) {
            Err("missing from ports")
        } else {
            match flavors.get(&key.origin) {
                None => Ok(key.clone()),
                Some(fl) if fl.is_empty() => Ok(PortKey::new(key.origin.clone(), None)),
                Some(fl) => match &key.flavor {
                    Some(f) if fl.contains(f) => Ok(key.clone()),
                    _ => Err("package unmatched"),
                },
            }
        };
        match root {
            Ok(k) => {
                if seen.insert(k.clone()) {
                    kept.push(k);
                }
            }
            Err(why) => skipped.push((key.clone(), why)),
        }
    }
    (kept, skipped)
}

/// synth's flavor index, built the way `prescan_ports_tree` /
/// `grep_Makefile` build it (portscan.adb ~1045, ~1266): `category/name`
/// for every `SUBDIR` entry of every category Makefile under `portsdir` and
/// each of `overlays` (optique's own notion of the tree; synth itself knows
/// none). A category with no Makefile of its own is not read — synth's
/// `walk_all_subdirectories` fallback for that case is outside this scan's
/// scope. An origin absent from this index is "missing from ports" to
/// synth, whatever is on disk at its path.
pub fn flavor_index(portsdir: &Path, overlays: &[PathBuf]) -> HashSet<String> {
    let mut out = HashSet::new();
    for tree in overlays.iter().map(PathBuf::as_path).chain(std::iter::once(portsdir)) {
        for category in categories(tree) {
            let Ok(text) = fs::read_to_string(tree.join(&category).join("Makefile")) else {
                continue;
            };
            for line in text.lines() {
                if let Some(name) = subdir_entry(line) {
                    out.insert(format!("{category}/{name}"));
                }
            }
        }
    }
    out
}

/// The categories synth's prescan reads from one ports tree, the way
/// `prescan_ports_tree` lists them (portscan.adb ~1045): every directory
/// entry starting with a lowercase ASCII letter, except the ports
/// framework's own `base`, `distfiles` and `packages`. Sorted, as synth's
/// own walk is.
fn categories(portsdir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(portsdir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.chars().next().is_some_and(|c| c.is_ascii_lowercase()))
        .filter(|name| !matches!(name.as_str(), "base" | "distfiles" | "packages"))
        .collect();
    out.sort();
    out
}

/// One line's `SUBDIR` entry, the way synth's `grep_Makefile` regex reads
/// it (portscan.adb ~1266:
/// `^SUBDIR[[:space:]]*[:+:]=[[:space:]]*([[:graph:]]*)`, applied to the
/// line already trimmed): `SUBDIR`, optional whitespace, one `:` or `+`,
/// `=`, optional whitespace, then the first run of non-space graphic
/// characters — a second name on the same line is never read.
fn subdir_entry(line: &str) -> Option<&str> {
    let is_blank = |c: char| c == ' ' || c == '\t';
    let rest = line.trim().strip_prefix("SUBDIR")?.trim_start_matches(is_blank);
    let rest = rest.strip_prefix(['+', ':'])?.strip_prefix('=')?.trim_start_matches(is_blank);
    let name = rest.split_whitespace().next().unwrap_or("");
    (!name.is_empty()).then_some(name)
}

/// Why synth would refuse to build with the saved options of a port whose
/// options are `options`, `text` being the file synth reads for it; None
/// when it would not. synth's check (portscan-packages.adb ~808-916,
/// `passed_options_cache_check`, run before every build by
/// portscan-pilot.adb ~418-424) counts every line with `+=` right after
/// `OPTIONS_FILE_SET` or `OPTIONS_FILE_UNSET`'s length, and fails the file
/// when an entry names an option the port lacks or when the count is not
/// the port's number of options, more or fewer.
pub fn refuses(text: &[u8], options: &[String]) -> Option<String> {
    let options: BTreeSet<&str> = options.iter().map(String::as_str).collect();
    let mut count = 0usize;
    for line in text.split(|&b| b == b'\n') {
        let name = if line.get(16..18) == Some(b"+=") {
            &line[18..]
        } else if line.get(18..20) == Some(b"+=") {
            &line[20..]
        } else {
            continue;
        };
        count += 1;
        let name = String::from_utf8_lossy(name);
        if !options.contains(name.as_ref()) {
            return Some(format!("saved {name} is no option of the port"));
        }
    }
    (count != options.len())
        .then(|| format!("{count} saved entries for the port's {} options", options.len()))
}

/// The file synth checks for `info` under `files`' options dir, and what it
/// says of it ([`refuses`]): `<category>_<name>/options` by origin,
/// for a port without flavors; a flavored port's name carries its
/// `@flavor` there, a file that never exists, so it is never refused.
pub fn refuses_port(files: &OptionsFiles, info: &PortInfo) -> Option<String> {
    if !info.flavors.is_empty() {
        return None;
    }
    let bytes = files.bytes(&info.key.origin.replace('/', "_"))?;
    refuses(&bytes, &info.options.complete)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(specs: &[&str]) -> Vec<PortKey> {
        specs.iter().map(|s| PortKey::parse(s).unwrap()).collect()
    }

    /// synth's flavor index comes from each category Makefile's `SUBDIR`
    /// entries (`prescan_ports_tree` / `grep_Makefile`, portscan.adb ~1045,
    /// ~1266), not merely a directory with a Makefile: a port present on
    /// disk but left out of its category's `SUBDIR` list is "missing from
    /// ports" to synth, exactly as one truly absent from the tree would be.
    #[test]
    fn synth_installed_checks_the_subdir_index_not_the_makefile() {
        let tmp = tempfile::tempdir().unwrap();
        let portsdir = tmp.path().join("ports");
        std::fs::create_dir_all(portsdir.join("misc/other")).unwrap();
        std::fs::create_dir_all(portsdir.join("misc/local")).unwrap();
        std::fs::write(portsdir.join("misc/Makefile"), "SUBDIR += other\n").unwrap();
        std::fs::write(portsdir.join("misc/other/Makefile"), "PKGNAME=other-1.0\n").unwrap();
        std::fs::write(portsdir.join("misc/local/Makefile"), "PKGNAME=local-1.0\n").unwrap();

        let index = flavor_index(&portsdir, &[]);
        let exists = |o: &str| index.contains(o);
        let (kept, skipped) =
            match_installed(&keys(&["misc/local", "misc/other"]), exists, &HashMap::new());
        assert_eq!(kept, keys(&["misc/other"]));
        let skipped: Vec<String> = skipped.iter().map(|(k, why)| format!("{k} {why}")).collect();
        assert_eq!(skipped, ["misc/local missing from ports"]);
    }

    /// synth roots an installed package only when its flavor index matches
    /// it: a stale flavor, a flavored port's unannotated package and an
    /// origin gone from the tree are skipped, an unflavored port is rooted
    /// bare whatever its annotation says.
    #[test]
    fn synth_installed_keeps_what_the_flavor_index_matches() {
        let flavors: HashMap<String, Vec<String>> = [
            ("misc/flav".to_string(), vec!["full".into(), "nox".into()]),
            ("misc/plain".to_string(), Vec::new()),
        ]
        .into();
        let exists = |o: &str| o != "misc/gone";
        let (kept, skipped) = match_installed(
            &keys(&[
                "misc/flav@nox",
                "misc/flav@old",
                "misc/flav",
                "misc/plain@py39",
                "misc/plain",
                "misc/gone",
                "misc/broken@x",
            ]),
            exists,
            &flavors,
        );
        assert_eq!(kept, keys(&["misc/flav@nox", "misc/plain", "misc/broken@x"]));
        let skipped: Vec<String> = skipped.iter().map(|(k, why)| format!("{k} {why}")).collect();
        assert_eq!(
            skipped,
            [
                "misc/flav@old package unmatched",
                "misc/flav package unmatched",
                "misc/gone missing from ports",
            ]
        );
    }

    /// synth refuses a file with an entry the port lacks, or with more or
    /// fewer entries than the port has options, every SET or UNSET line
    /// counted.
    #[test]
    fn refuses_a_file_whose_entries_are_not_the_options() {
        let opts: Vec<String> = ["A", "B"].iter().map(|s| s.to_string()).collect();
        let file = |body: &str| {
            format!("# This file is auto-generated by 'make config'.\n_OPTIONS_READ=x-1.0\n{body}")
        };
        let both = file("OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n");
        assert_eq!(refuses(both.as_bytes(), &opts), None);
        let gone = file("OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=GONE\n");
        assert_eq!(
            refuses(gone.as_bytes(), &opts).as_deref(),
            Some("saved GONE is no option of the port")
        );
        let short = file("OPTIONS_FILE_SET+=A\n");
        assert_eq!(
            refuses(short.as_bytes(), &opts).as_deref(),
            Some("1 saved entries for the port's 2 options")
        );
        // A repeated line is one more entry, as synth counts it.
        let twice = file("OPTIONS_FILE_SET+=A\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n");
        assert_eq!(
            refuses(twice.as_bytes(), &opts).as_deref(),
            Some("3 saved entries for the port's 2 options")
        );
        // A port with no options at all and a stray entry.
        assert!(refuses(both.as_bytes(), &[]).is_some());
    }

    /// `refuses_port` reads `<category>_<name>/options` by origin,
    /// never by the port's own `OPTIONS_NAME` (which a flavor or an
    /// OPTIONS_SUB port names differently): synth's own lookup
    /// (portscan-packages.adb ~816) builds that path from the catport, not
    /// from anything the port's Makefile sets.
    #[test]
    fn synth_refuses_port_reads_by_origin_not_options_name() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("misc_port")).unwrap();
        std::fs::write(
            tmp.path().join("misc_port/options"),
            b"# This file is auto-generated by 'make config'.\n\
              _OPTIONS_READ=x-1.0\n\
              OPTIONS_FILE_SET+=A\n\
              OPTIONS_FILE_UNSET+=GONE\n",
        )
        .unwrap();
        let files = OptionsFiles::new(tmp.path());
        let mut info = PortInfo::stub("misc/port");
        info.options.complete = vec!["A".to_string()];
        // A deliberately different OPTIONS_NAME: the file above must still
        // be read, by the origin alone.
        info.options_name = "something_else".to_string();
        assert_eq!(
            refuses_port(&files, &info).as_deref(),
            Some("saved GONE is no option of the port")
        );
    }

    /// A flavored port's file, named `<category>_<name>@<flavor>/options`,
    /// never exists under the plain by-origin path synth checks, so it is
    /// never refused — whatever that path holds, even a file that would
    /// trip a flavorless port.
    #[test]
    fn synth_refuses_port_never_refuses_a_flavored_port() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("misc_port")).unwrap();
        std::fs::write(tmp.path().join("misc_port/options"), b"short\n").unwrap();
        let files = OptionsFiles::new(tmp.path());
        let mut info = PortInfo::stub("misc/port");
        info.flavors = vec!["full".to_string()];
        assert_eq!(refuses_port(&files, &info), None);
    }

    #[test]
    fn installed_list_flavors_and_repo_filter() {
        let origins = "nginx\twww/nginx\tpoudriere\n\
                       ImageMagick7-nox11\tgraphics/ImageMagick7\tSynth\n\
                       binutils\tdevel/binutils\tSynth\n\
                       nginx\twww/nginx\tpoudriere\n\
                       junk\tnot-an-origin\tSynth\n";
        let annots = "ImageMagick7-nox11\tflavor\tnox11\n\
                      binutils\tflavor\tnative\n\
                      nginx\tcpe\tsomething\n";
        // Unfiltered: three ports, flavors applied, dup and junk dropped.
        let all = parse_installed(origins, annots, &[]);
        let names: Vec<String> = all.iter().map(|k| k.to_string()).collect();
        assert_eq!(
            names,
            vec!["devel/binutils@native", "graphics/ImageMagick7@nox11", "www/nginx"]
        );
        // Repo filter keeps only Synth-built packages.
        let synth = parse_installed(origins, annots, &["Synth".to_string()]);
        let names: Vec<String> = synth.iter().map(|k| k.to_string()).collect();
        assert_eq!(names, vec!["devel/binutils@native", "graphics/ImageMagick7@nox11"]);
        // Several repositories: a package from any of them is kept.
        let both =
            parse_installed(origins, annots, &["Synth".to_string(), "poudriere".to_string()]);
        assert_eq!(both.len(), 3);
        // Unknown repo: empty (caller turns this into an error).
        assert!(parse_installed(origins, annots, &["nope".to_string()]).is_empty());
    }

    /// Several flavors of one origin installed side by side are each a
    /// root: keyed by origin, all but the last annotated one were dropped
    /// (183 of 2230 packages on one host).
    #[test]
    fn installed_list_keeps_every_flavor_of_one_origin() {
        let origins = "polkit-qt5-1\tsysutils/polkit-qt-1\tSynth\n\
                       polkit-qt6-1\tsysutils/polkit-qt-1\tSynth\n\
                       gpu-firmware-amd-kmod-aldebaran\tgraphics/gpu-firmware-amd-kmod\tSynth\n\
                       gpu-firmware-amd-kmod-arcturus\tgraphics/gpu-firmware-amd-kmod\tSynth\n";
        let annots = "polkit-qt5-1\tflavor\tqt5\n\
                      polkit-qt6-1\tflavor\tqt6\n\
                      gpu-firmware-amd-kmod-aldebaran\tflavor\taldebaran\n\
                      gpu-firmware-amd-kmod-arcturus\tflavor\tarcturus\n";
        let names: Vec<String> =
            parse_installed(origins, annots, &[]).iter().map(|k| k.to_string()).collect();
        assert_eq!(
            names,
            vec![
                "graphics/gpu-firmware-amd-kmod@aldebaran",
                "graphics/gpu-firmware-amd-kmod@arcturus",
                "sysutils/polkit-qt-1@qt5",
                "sysutils/polkit-qt-1@qt6",
            ]
        );
    }
}
