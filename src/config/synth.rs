//! A synth(1) build: synth.ini read as synth reads it, the profile's
//! make.conf under synth's scan header, and the tree synth falls back to.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::{checked_portsdir, is_ports_tree, stage_make_conf, QueryEnv, Request, Settings};

/// What synth(1) writes at the top of its scan slave's make.conf, ahead of
/// `<profile>-make.conf` (`create_make_conf` in its replicant.adb), as far as
/// it bears on evaluating a Makefile: the slave's PORTSDIR, PORT_DBDIR and
/// friends are chroot paths optique sets its own way. synth gives
/// `PACKAGE_BUILDING` alone, never `PACKAGE_BUILDING_FLAVORS`. The profile
/// line comes first and the ccache lines last, from [`synth_prelude`].
const SYNTH_SCAN_PRELUDE: &str = "\
USE_PACKAGE_DEPENDS_ONLY=yes
PACKAGE_BUILDING=yes
BATCH=yes
";

/// synth reads ports in a chroot whose /usr/local is an empty tmpfs or
/// nullfs (replicant.adb), so the host's packages answer no `exists()`
/// probe there: Mk/Uses/php.mk's php.conf and httpd checks, iconv.mk.
const SYNTH_SCAN_TRAILER: &str = "\
# --- synth scan environment (optique) ---
LOCALBASE=/nonexistent
";

/// What synth adds to that header when the profile's `Directory_ccache`
/// exists (replicant.adb:754-757): ports then gain devel/ccache as a build
/// dependency.
const SYNTH_SCAN_CCACHE: &str = "\
WITH_CCACHE_BUILD=yes
CCACHE_DIR=/ccache
";

/// synth's header for `profile`, as its scan slave's make.conf starts,
/// with the ccache lines when `ccache` is on.
fn synth_prelude(profile: &str, ccache: bool) -> String {
    let ccache = if ccache { SYNTH_SCAN_CCACHE } else { "" };
    format!(
        "# --- synth scan environment (optique) ---\nSYNTHPROFILE={profile}\n\
         {SYNTH_SCAN_PRELUDE}{ccache}"
    )
}

/// Does synth turn ccache on for a profile whose `Directory_ccache` is
/// `setting`? Only when that path exists; unset is synth's `disabled`.
fn synth_ccache(setting: Option<&Path>) -> bool {
    setting.is_some_and(|dir| dir != Path::new("disabled") && dir.exists())
}

const SYNTH_DIR: &str = "/usr/local/etc/synth";
/// The trees synth tries, in order, after `PORTSDIR` and make's answer
/// (`std_dports_loc`, `std_ports_loc` in its parameters.ads).
const SYNTH_STANDARD_PORTSDIRS: &[&str] = &["/usr/dports", "/usr/ports"];
/// Section of synth.ini whose settings apply to every profile.
const SYNTH_GLOBAL_SECTION: &str = "Global Configuration";

/// The synth build `profile` names (empty for synth's own selection), as
/// synth reads this host: synth.ini under /usr/local/etc/synth, and the
/// environment's `SYNTHPROFILE` and `PORTSDIR`.
pub(super) fn resolve(profile: &str, req: &Request, staging_dir: &Path) -> Result<Settings> {
    let env_profile = std::env::var("SYNTHPROFILE").ok();
    let env_portsdir = std::env::var_os("PORTSDIR").filter(|v| !v.is_empty()).map(PathBuf::from);
    let make_portsdir = || synth_make_portsdir(None);
    let host = SynthHost {
        env_profile: env_profile.as_deref(),
        env_portsdir: env_portsdir.as_deref(),
        make_portsdir: &make_portsdir,
        standard_portsdirs: SYNTH_STANDARD_PORTSDIRS,
    };
    resolve_synth(Path::new(SYNTH_DIR), profile, &host, req.portsdir, req.options_dir, staging_dir)
}

/// synth(1) layout: one `synth.ini` describing every profile, the profile's
/// own `<profile>-make.conf` (no layering), and no blacklist at all. The
/// options dir is a documented synth setting (`Directory_options`), so unlike
/// poudriere there is nothing to probe for. `host` holds what synth reads
/// from the host besides synth.ini.
fn resolve_synth(
    synth_dir: &Path,
    profile: &str,
    host: &SynthHost,
    portsdir_flag: Option<&Path>,
    options_dir_flag: Option<&Path>,
    staging_dir: &Path,
) -> Result<Settings> {
    let ini = read_synth_ini(&synth_dir.join("synth.ini"))?;

    let mut notes = Vec::new();
    // A bare -s (empty profile) follows synth's own selection.
    let selected;
    let profile = if profile.is_empty() {
        let (name, source) = synth_selected_profile(&ini, host.env_profile);
        notes.push(format!("synth profile {name} ({source})"));
        selected = name;
        selected.as_str()
    } else {
        profile
    };
    let path_setting = |key: &str| synth_ini_path(&ini, profile, key);
    notes.extend(synth_ini_misreads(&ini));
    // A named profile that synth.ini doesn't know silently gets built-in
    // defaults — say so, it is usually a typo.
    if !ini.is_empty() && !synth_ini_has_section(&ini, profile) {
        notes.push(format!(
            "profile '{profile}' has no section in synth.ini; using built-in defaults"
        ));
    }

    let portsdir = match (portsdir_flag, path_setting("Directory_portsdir")) {
        (Some(dir), _) => checked_portsdir(dir)?,
        (None, Some(p)) => {
            if !is_ports_tree(&p) {
                bail!(
                    "synth profile '{profile}' points Directory_portsdir at {} \
                     which is not a ports tree",
                    p.display()
                );
            }
            p
        }
        (None, None) => {
            let Some((dir, source)) = synth_default_portsdir(host) else {
                bail!(
                    "synth profile '{profile}' sets no Directory_portsdir, and none of \
                     PORTSDIR, make's PORTSDIR, {} exists",
                    host.standard_portsdirs.join(", ")
                );
            };
            if !is_ports_tree(&dir) {
                bail!(
                    "synth profile '{profile}' sets no Directory_portsdir, and {} \
                     ({source}) is not a ports tree",
                    dir.display()
                );
            }
            notes.push(format!("no Directory_portsdir: ports tree {} ({source})", dir.display()));
            dir
        }
    };

    let options_dir = match options_dir_flag {
        Some(d) => d.to_path_buf(),
        None => path_setting("Directory_options").unwrap_or_else(|| PathBuf::from("/var/db/ports")),
    };
    if options_dir == Path::new("/var/db/ports") {
        notes.push(
            "options dir is the live /var/db/ports: minimal/redundant cleanups \
             also affect plain `make config` workflows"
                .to_string(),
        );
    }

    // synth reads ports in a chroot with nothing installed, under its own
    // header and the profile's make.conf alone: never the host's
    // /etc/make.conf, whose absence the always non-empty staged file keeps.
    let ccache = synth_ccache(path_setting("Directory_ccache").as_deref());
    let staged = stage_make_conf(
        synth_prelude(profile, ccache),
        &[synth_dir.join(format!("{profile}-make.conf"))],
        SYNTH_SCAN_TRAILER,
        staging_dir,
    )?;

    Ok(Settings::new(portsdir, options_dir, staged, QueryEnv::Synth, notes))
}

/// What synth reads from the host besides synth.ini.
struct SynthHost<'a> {
    /// The environment's `SYNTHPROFILE`, which a bare -s heeds.
    env_profile: Option<&'a str>,
    /// The environment's `PORTSDIR`, when non-empty.
    env_portsdir: Option<&'a Path>,
    /// `PORTSDIR` as synth asks make for it ([`synth_make_portsdir`]), when
    /// make gives one; asked only when the profile names no tree.
    make_portsdir: &'a dyn Fn() -> Option<PathBuf>,
    /// The trees synth tries last, in order: /usr/dports, /usr/ports.
    standard_portsdirs: &'a [&'a str],
}

/// synth.ini as bytes, as synth reads it: missing is empty, as for synth,
/// which then writes a fresh one; any other read error stops the run, as
/// synth stops on it ("Configuration failed to load"). Bytes that are not
/// UTF-8 stay as they are: a path keeps them ([`synth_ini_path`]), text
/// decodes them lossily.
fn read_synth_ini(path: &Path) -> Result<Vec<u8>> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e)
            .with_context(|| format!("reading {} (synth will not run without it)", path.display())),
    }
}

/// `PORTSDIR` as synth asks make for it (parameters.adb `query_portsdir`):
/// `make -f /usr/share/mk/bsd.port.mk -V PORTSDIR`, from the directory
/// synth was run in (it never changes directory; `cwd` None is optique's
/// own), with its environment. That file takes the environment's or
/// /etc/make.conf's `PORTSDIR`, else a tree found at or up to three levels
/// above that directory, else /usr/ports, and includes that tree's
/// bsd.port.mk: make fails when the tree is missing. None when make fails
/// or answers nothing.
fn synth_make_portsdir(cwd: Option<&Path>) -> Option<PathBuf> {
    let mut cmd = std::process::Command::new("/usr/bin/make");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    cmd.args(["-f", "/usr/share/mk/bsd.port.mk", "-V", "PORTSDIR"])
        .env_remove("MAKEFLAGS")
        .env_remove("MAKELEVEL")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let dir = text.lines().next()?.trim();
    (!dir.is_empty()).then(|| PathBuf::from(dir))
}

/// The ports tree synth takes for a profile naming none, and what gave it
/// (parameters.adb `determine_portsdirs`): the first that exists of the
/// environment's `PORTSDIR`, make's `PORTSDIR` as synth asks for it
/// ([`synth_make_portsdir`]: /usr/ports when nothing sets one, so
/// /usr/dports is reached only when make fails), /usr/dports, /usr/ports.
/// synth's query fails, stopping synth with a "blank PORTSDIR", whenever
/// the tree it names is missing; optique goes on to the next candidate
/// instead.
fn synth_default_portsdir(host: &SynthHost) -> Option<(PathBuf, &'static str)> {
    if let Some(dir) = host.env_portsdir.filter(|d| d.exists()) {
        return Some((dir.to_path_buf(), "from PORTSDIR"));
    }
    if let Some(dir) = (host.make_portsdir)().filter(|d| d.exists()) {
        return Some((dir, "from make's PORTSDIR"));
    }
    host.standard_portsdirs
        .iter()
        .map(Path::new)
        .find(|d| d.exists())
        .map(|d| (d.to_path_buf(), "synth's default"))
}

/// The profile synth itself runs with, and what chose it
/// (parameters.adb:64-71, `load_configuration`): `env_profile`, the
/// environment's `SYNTHPROFILE`, when synth.ini has a section for it with
/// `Directory_packages` set, else `profile_selected`, else `LiveSystem`.
fn synth_selected_profile(
    ini: &(impl AsRef<[u8]> + ?Sized),
    env_profile: Option<&str>,
) -> (String, &'static str) {
    if let Some(env) = env_profile.filter(|p| !p.is_empty())
        && synth_ini_lookup(ini, env, "Directory_packages").is_some()
    {
        return (env.to_string(), "SYNTHPROFILE");
    }
    match synth_ini_lookup(ini, SYNTH_GLOBAL_SECTION, "profile_selected") {
        Some(p) => (p, "synth.ini profile_selected"),
        None => ("LiveSystem".to_string(), "synth's default"),
    }
}

/// The part of a synth.ini data line before the first `;` or `#`: both start
/// a comment that runs to the end of the line (config.adb's `Get_Value`,
/// its `Comment_Ind := Index (..., Set => To_Set ("#;"))`, read only for a
/// line that is not itself a section header).
fn ini_strip_comment(line: &[u8]) -> &[u8] {
    match line.iter().position(|b| matches!(b, b';' | b'#')) {
        Some(i) => &line[..i],
        None => line,
    }
}

/// synth.ini's lines as synth reads them (config.adb's `Get_Value`): bytes,
/// as synth reads them, split at LF alone, a CR left in place, and a line
/// of fewer than two bytes skipped.
fn ini_lines(text: &[u8]) -> impl Iterator<Item = (usize, &[u8])> {
    text.split(|&b| b == b'\n').enumerate().map(|(n, l)| (n + 1, l)).filter(|(_, l)| l.len() > 1)
}

/// `s` without the leading and trailing bytes `trim` matches.
fn trim_bytes(s: &[u8], trim: impl Fn(u8) -> bool) -> &[u8] {
    let start = s.iter().position(|&b| !trim(b)).unwrap_or(s.len());
    let end = s.iter().rposition(|&b| !trim(b)).map_or(start, |e| e + 1);
    &s[start..end]
}

/// The UTF-8 byte-order mark, which synth reads as part of the first line.
const BOM: &[u8] = b"\xEF\xBB\xBF";

/// A synth.ini section header as synth reads it: a `[` in column 1, the
/// name running untrimmed to the first `]`, anything after it ignored. An
/// indented `[name]` is no header, and `[ C ]` names the section ` C `.
/// These follow synth's INI reader rather than what the line seems to say
/// (an upstream defect that decides which settings synth uses, so optique
/// reads the file as synth does, and `synth_ini_misreads` names each such
/// line). A header with no `]` is no header: it makes synth fail to load
/// its configuration, which is the user's file to fix, not optique's to
/// report.
fn ini_section_header(line: &[u8]) -> Option<&[u8]> {
    let rest = line.strip_prefix(b"[")?;
    rest.iter().position(|&b| b == b']').map(|end| &rest[..end])
}

/// Trim a key or value as config.adb does: spaces alone (`Index_Non_Blank`),
/// a tab or a CRLF file's CR staying part of it, as for synth.
fn ini_trim(s: &[u8]) -> &[u8] {
    trim_bytes(s, |b| b == b' ')
}

/// The synth.ini lines synth reads otherwise than they seem to say, each as
/// a note: a tab around a key or value is part of it, the spaces of
/// `[ name ]` are part of the name, an indented `[name]` is no header but a
/// data line of the section above, and a CRLF file's CR stays in each key
/// or value, and a UTF-8 byte-order mark is part of the first line, so a
/// header there is no header and the lines after it are in no section.
/// optique reads them as synth does (`ini_section_header`, `ini_trim`), so
/// a profile, key or path may not be the one meant; the note says why, for
/// the user to fix the file.
///
/// A line with no data once its comment is cut (blank, tab-only, a
/// tab-indented comment) holds nothing synth could misread; a header
/// ignores its CR, which comes after the `]`.
fn synth_ini_misreads(text: &(impl AsRef<[u8]> + ?Sized)) -> Vec<String> {
    const WHY: &str = "read as synth reads it (a synth defect)";
    let text = text.as_ref();
    let mut notes = Vec::new();
    let mut crlf = Vec::new();
    for (n, raw) in ini_lines(text) {
        if let Some(rest) = raw.strip_prefix(BOM).filter(|_| n == 1) {
            let hidden = match ini_section_header(rest) {
                Some(name) => format!(
                    ", so [{}] there is no section header and the lines after it are in no section",
                    String::from_utf8_lossy(name)
                ),
                None => String::new(),
            };
            notes.push(format!(
                "synth.ini line 1: synth reads the UTF-8 byte-order mark as part of the line{hidden}; {WHY}"
            ));
            continue;
        }
        if let Some(name) = ini_section_header(raw) {
            if name.trim_ascii() != name {
                let name = String::from_utf8_lossy(name);
                notes.push(format!(
                    "synth.ini line {n}: synth keeps the spaces of the section name '{name}'; {WHY}"
                ));
            }
            continue;
        }
        if raw.trim_ascii_start().starts_with(b"[") && raw.contains(&b']') {
            notes.push(format!(
                "synth.ini line {n}: synth takes no indented [name] for a section header, \
                 the lines after it staying in the section above; {WHY}"
            ));
            continue;
        }
        if raw.starts_with(b";") || raw.starts_with(b"#") {
            continue;
        }
        let data = ini_strip_comment(raw);
        if data.trim_ascii().is_empty() {
            continue;
        }
        // A comment cut drops the CR with it.
        if data.ends_with(b"\r") {
            crlf.push(n);
        }
        let (k, v) = match data.iter().position(|&b| b == b'=') {
            Some(at) => (&data[..at], &data[at + 1..]),
            None => (data, &b""[..]),
        };
        let tabbed = |s: &[u8]| trim_bytes(s, |b| b == b' ' || b == b'\r') != s.trim_ascii();
        if tabbed(k) || tabbed(v) {
            notes.push(format!("synth.ini line {n}: synth keeps a tab around the key or value; {WHY}"));
        }
    }
    if !crlf.is_empty() {
        let (s, list) = (if crlf.len() == 1 { "" } else { "s" }, line_ranges(&crlf));
        notes.push(format!(
            "synth.ini line{s} {list}: synth keeps the CR of a CRLF line end in the key or value; {WHY}"
        ));
    }
    notes
}

/// Ascending line numbers as runs: `1-3, 5`.
fn line_ranges(lines: &[usize]) -> String {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for &n in lines {
        match runs.last_mut() {
            Some((_, end)) if *end + 1 == n => *end = n,
            _ => runs.push((n, n)),
        }
    }
    runs.iter()
        .map(|&(a, b)| if a == b { a.to_string() } else { format!("{a}-{b}") })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Does synth.ini contain a `[name]` section header?
fn synth_ini_has_section(text: &(impl AsRef<[u8]> + ?Sized), name: &str) -> bool {
    ini_lines(text.as_ref()).any(|(_, line)| ini_section_header(line) == Some(name.as_bytes()))
}

/// Minimal INI reader for synth.ini: the value of `key` in section
/// `[profile]` alone, as synth reads a profile's settings
/// (parameters.adb:200-271, `load_specific_profile`, over config.adb's
/// `Get_Value`): no other section, `[Global Configuration]` included,
/// stands in for it. Keys are case-sensitive, `;` and `#` start a comment,
/// and spaces alone around the key and the value are trimmed (`ini_trim`).
/// A key's first match within the section wins — `Get_Value` exits its scan
/// of the whole file the moment one is found, matched or not, so even a
/// bare key with no `=`, or one given an empty value, ends the search
/// there, giving the default rather than reading on to a later, real
/// assignment of the same key. The value comes as text, any byte that is
/// not UTF-8 decoded lossily; [`synth_ini_path`] keeps a path's bytes.
fn synth_ini_lookup(text: &(impl AsRef<[u8]> + ?Sized), profile: &str, key: &str) -> Option<String> {
    synth_ini_value(text.as_ref(), profile, key).map(|v| String::from_utf8_lossy(v).into_owned())
}

/// [`synth_ini_lookup`] for a setting that is a path (`Directory_*`): its
/// bytes as synth hands them to the system, so a path in Latin-1 is still
/// the directory synth uses.
fn synth_ini_path(text: &(impl AsRef<[u8]> + ?Sized), profile: &str, key: &str) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    synth_ini_value(text.as_ref(), profile, key).map(|v| PathBuf::from(std::ffi::OsStr::from_bytes(v)))
}

/// The bytes of [`synth_ini_lookup`]'s value.
fn synth_ini_value<'a>(text: &'a [u8], profile: &str, key: &str) -> Option<&'a [u8]> {
    let (profile, key) = (profile.as_bytes(), key.as_bytes());
    let mut in_section = false;
    for (_, line) in ini_lines(text) {
        if line.starts_with(b"[") {
            // A header with no `]` is skipped, the section going on.
            if let Some(name) = ini_section_header(line) {
                in_section = name == profile;
            }
            continue;
        }
        if !in_section || line.starts_with(b";") || line.starts_with(b"#") {
            continue;
        }
        let data = ini_strip_comment(line);
        let Some(at) = data.iter().position(|&b| b == b'=') else {
            // No `=` at all: config.adb still compares the whole
            // (trimmed) line against `key`, as a bare mark with no value.
            return if ini_trim(data) == key { None } else { continue };
        };
        if ini_trim(&data[..at]) != key {
            continue;
        }
        let v = ini_trim(&data[at + 1..]);
        return if v.is_empty() { None } else { Some(v) };
    }
    None
}

/// Stage synth's scan make.conf for `profile` over `profile_make_conf`.
#[cfg(test)]
pub(crate) fn stage_synth_make_conf(
    staging_dir: &Path,
    profile: &str,
    profile_make_conf: &Path,
) -> Result<PathBuf> {
    let (path, _, _) = stage_make_conf(
        synth_prelude(profile, false),
        &[profile_make_conf.to_path_buf()],
        SYNTH_SCAN_TRAILER,
        staging_dir,
    )?;
    Ok(path.expect("the synth environment is never empty"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{key_installed_packages, make_values};
    use crate::config::testutil::*;

    /// synth sees no host package, so the pkg database keys nothing for it;
    /// its profile names a cache generation of its own.
    #[test]
    fn synth_generation_ignores_the_pkg_db() {
        let tmp = tempfile::tempdir().unwrap();
        let synth = tmp.path().join("synth");
        let tree = tmp.path().join("ports");
        fake_tree(&tree);
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(synth.join("synth.ini"), format!("[A]\nDirectory_portsdir={}\n", tree.display()))
            .unwrap();
        let resolve = |profile| resolve_synth(&synth, profile, &synth_host(None), None, None, &stage).unwrap();

        let pkg_db = tmp.path().join("local.sqlite");
        fs::write(&pkg_db, "one package").unwrap();
        let s = resolve("A");
        let before = s.conf_hash.clone();
        assert_eq!(key_installed_packages(s, &pkg_db).conf_hash, before);
        assert_ne!(resolve("B").conf_hash, before, "SYNTHPROFILE differs");
    }

    #[test]
    fn synth_ini_lookup_reads_the_profile_section_alone() {
        let ini = "\
; leading comment
[Global Configuration]
Directory_portsdir= /global/ports
Directory_options = /global/options   # trailing comment

[LiveSystem]
Directory_portsdir=/live/ports
Operating_system= FreeBSD

[Other]
Directory_portsdir = /other/ports
";
        // Profile section wins over the global one.
        assert_eq!(
            synth_ini_lookup(ini, "LiveSystem", "Directory_portsdir").as_deref(),
            Some("/live/ports")
        );
        // Key absent from the profile: unset, as synth reads it — the
        // global section never stands in for a profile.
        assert_eq!(synth_ini_lookup(ini, "LiveSystem", "Directory_options"), None);
        // Sections are not confused with each other.
        assert_eq!(
            synth_ini_lookup(ini, "Other", "Directory_portsdir").as_deref(),
            Some("/other/ports")
        );
        // Nor for an unknown profile; the global section is read by name.
        assert_eq!(synth_ini_lookup(ini, "Nope", "Directory_portsdir"), None);
        assert_eq!(
            synth_ini_lookup(ini, SYNTH_GLOBAL_SECTION, "Directory_portsdir").as_deref(),
            Some("/global/ports")
        );
        // An unknown key is None, and keys are case-sensitive.
        assert_eq!(synth_ini_lookup(ini, "LiveSystem", "Directory_packages"), None);
        assert_eq!(synth_ini_lookup(ini, "LiveSystem", "directory_portsdir"), None);
    }

    #[test]
    fn synth_ini_lookup_ignores_comments_and_junk() {
        let ini = "\
#[LiveSystem]
#Directory_portsdir=/commented/out
[LiveSystem]
; Directory_options=/also/commented
Directory_options=
   Directory_portsdir   =   /spaced/ports
not a key value line
";
        // A commented-out section header never opens a section, and a
        // commented-out or empty assignment counts as unset.
        assert_eq!(
            synth_ini_lookup(ini, "LiveSystem", "Directory_portsdir").as_deref(),
            Some("/spaced/ports")
        );
        assert_eq!(synth_ini_lookup(ini, "LiveSystem", "Directory_options"), None);
        // Nothing at all: no panic, no value.
        assert_eq!(synth_ini_lookup("", "LiveSystem", "Directory_portsdir"), None);
    }

    /// config.adb's own syntax (`Get_Value`), each claim checked on its own,
    /// optique reading it as synth does and naming each line that does not
    /// say what it seems to: a section header's `[` must open the line
    /// (column 1), its name runs untrimmed to the first `]`, anything after
    /// it ignored; a key and its value are trimmed of spaces only, a tab left
    /// in place; and a key's first match within its section ends the whole
    /// search there — `exit Read_File` — even a bare key with no `=` or one
    /// given an empty value, so a later, real assignment is never reached.
    #[test]
    fn synth_ini_reads_as_synth_and_names_what_it_misreads() {
        assert_eq!(ini_section_header(b"[B] extra"), Some(&b"B"[..]), "anything after ] is ignored");
        assert_eq!(ini_section_header(b"[ C ]"), Some(&b" C "[..]), "the name is not trimmed");
        assert_eq!(ini_section_header(b"   [P]"), None, "an indented header is none");

        let ini = "[A]\nk\t= d\n[ C ]\nDirectory_portsdir=/c\n[P]\n  [Q]\nkey=2\n";
        assert_eq!(synth_ini_lookup(ini, "A", "k"), None, "the key is `k\\t`");
        assert_eq!(synth_ini_lookup(ini, "A", "k\t").as_deref(), Some("d"));
        assert_eq!(synth_ini_lookup(ini, "C", "Directory_portsdir"), None);
        assert_eq!(synth_ini_lookup(ini, " C ", "Directory_portsdir").as_deref(), Some("/c"));
        assert_eq!(synth_ini_lookup(ini, "Q", "key"), None, "no section Q");
        assert_eq!(synth_ini_lookup(ini, "P", "key").as_deref(), Some("2"), "still in [P]");
        assert!(!synth_ini_has_section(ini, "C") && synth_ini_has_section(ini, " C "));
        // Each line synth reads otherwise than it seems is named.
        let why = "read as synth reads it (a synth defect)";
        assert_eq!(
            synth_ini_misreads(ini),
            [
                format!("synth.ini line 2: synth keeps a tab around the key or value; {why}"),
                format!("synth.ini line 3: synth keeps the spaces of the section name ' C '; {why}"),
                format!(
                    "synth.ini line 6: synth takes no indented [name] for a section header, \
                     the lines after it staying in the section above; {why}"
                ),
            ]
        );
        assert!(synth_ini_misreads("[A]\nk = v\n").is_empty(), "spaces are fine");

        // The format's own rule: the first match ends the search, an empty
        // value giving the default rather than a later assignment.
        let ini = "[A]\nDirectory_portsdir=\nDirectory_portsdir=/x\n";
        assert_eq!(synth_ini_lookup(ini, "A", "Directory_portsdir"), None);
    }

    /// The host as a test sees it: no PORTSDIR, make answering none, and
    /// synth's own standard trees, of which /usr/ports may exist here.
    fn synth_host(env_profile: Option<&str>) -> SynthHost<'_> {
        fn no_make() -> Option<PathBuf> {
            None
        }
        SynthHost {
            env_profile,
            env_portsdir: None,
            make_portsdir: &no_make,
            standard_portsdirs: SYNTH_STANDARD_PORTSDIRS,
        }
    }

    /// A line with nothing left once its comment is cut is no misread, tab
    /// or not; a data line's CR, which synth keeps and optique with it, is,
    /// all such lines in one note; a comment cut drops the CR, a header
    /// ignores it, and a CR alone is a line synth skips.
    #[test]
    fn synth_ini_misreads_skip_empty_lines_and_name_crlf() {
        let ini = "[A]\n\t\n\t# comment\n \t; comment\nk = v ; tab\there\n";
        assert!(synth_ini_misreads(ini).is_empty(), "{:?}", synth_ini_misreads(ini));

        let ini = "[A]\r\nk=v\r\nj=w\r\n\r\n; c\r\nm=x ; c\r\nn=y\r\n";
        let why = "read as synth reads it (a synth defect)";
        assert_eq!(
            synth_ini_misreads(ini),
            [format!("synth.ini lines 2-3, 7: synth keeps the CR of a CRLF line end in the key or value; {why}")]
        );
        assert_eq!(
            synth_ini_misreads("[A]\nk=v\r\n"),
            [format!("synth.ini line 2: synth keeps the CR of a CRLF line end in the key or value; {why}")]
        );
        // As synth reads it: the value keeps the CR, a comment cut drops it.
        assert_eq!(synth_ini_lookup(ini, "A", "k").as_deref(), Some("v\r"));
        assert_eq!(synth_ini_lookup(ini, "A", "m").as_deref(), Some("x"));
        assert_eq!(line_ranges(&[1, 2, 3, 5, 7, 8]), "1-3, 5, 7-8");
    }

    /// A header with no `]` is no header: skipped, the section above going
    /// on, and nothing said of it. synth fails to load such a file, but the
    /// file is the user's, not something optique wrote.
    #[test]
    fn synth_ini_header_without_bracket_is_skipped() {
        let ini = "[A]\nk=v\n[B\nj=w\n";
        assert_eq!(synth_ini_lookup(ini, "A", "k").as_deref(), Some("v"));
        assert_eq!(synth_ini_lookup(ini, "A", "j").as_deref(), Some("w"));
        assert!(!synth_ini_has_section(ini, "B"));
        assert!(synth_ini_misreads(ini).is_empty(), "{:?}", synth_ini_misreads(ini));
    }

    /// A missing synth.ini is none; one that cannot be read stops the run,
    /// as it stops synth; bytes that are not UTF-8 cost only themselves.
    #[test]
    fn synth_ini_read_errors_stop_and_bad_bytes_are_kept_lossily() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_synth_ini(&tmp.path().join("absent")).unwrap().is_empty());
        let err = read_synth_ini(tmp.path()).unwrap_err();
        assert!(format!("{err:#}").contains("synth will not run"), "{err:#}");

        let ini = tmp.path().join("synth.ini");
        fs::write(&ini, b"; caf\xe9\n[A]\nDirectory_options=/opts\n").unwrap();
        let text = read_synth_ini(&ini).unwrap();
        assert_eq!(synth_ini_lookup(&text, "A", "Directory_options").as_deref(), Some("/opts"));

        // Through the resolution: an unreadable synth.ini is an error.
        let synth = tmp.path().join("synth");
        fs::create_dir_all(synth.join("synth.ini")).unwrap();
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&stage).unwrap();
        let tree = tmp.path().join("ports");
        fake_tree(&tree);
        assert!(resolve_synth(&synth, "A", &synth_host(None), Some(&tree), Some(&tree), &stage).is_err());
    }

    /// synth reads a UTF-8 byte-order mark as part of the first line, so
    /// the header there is none: its settings are in no section, and the
    /// profile gets its defaults. optique reads it so, and says why.
    #[test]
    fn synth_ini_bom_is_named() {
        let ini = b"\xEF\xBB\xBF[A]\nDirectory_options=/opts\n[B]\nDirectory_options=/b\n";
        assert_eq!(synth_ini_lookup(ini, "A", "Directory_options"), None);
        assert!(!synth_ini_has_section(ini, "A"));
        assert_eq!(synth_ini_lookup(ini, "B", "Directory_options").as_deref(), Some("/b"));
        assert_eq!(
            synth_ini_misreads(ini),
            ["synth.ini line 1: synth reads the UTF-8 byte-order mark as part of the line, so [A] there \
              is no section header and the lines after it are in no section; read as synth reads it (a synth defect)"]
        );
        let comment = synth_ini_misreads(b"\xEF\xBB\xBF; settings\n[A]\nk=v\n");
        assert_eq!(
            comment,
            ["synth.ini line 1: synth reads the UTF-8 byte-order mark as part of the line; \
              read as synth reads it (a synth defect)"]
        );
        assert!(synth_ini_misreads(b"[A]\nk=\xEF\xBB\xBFv\n").is_empty(), "only a leading one");
    }

    /// A Latin-1 path in synth.ini is the directory synth uses, byte for
    /// byte, never its lossy decoding; a line of one Latin-1 byte is one
    /// byte long, and skipped as synth skips it.
    #[test]
    fn synth_ini_latin1_path_is_kept() {
        use std::os::unix::ffi::OsStrExt as _;
        let ini = b"[A]\nDirectory_options=/opt/r\xe9glages\n\xe9\nDirectory_portsdir=/p\n";
        let path = synth_ini_path(ini, "A", "Directory_options").unwrap();
        assert_eq!(path.as_os_str().as_bytes(), b"/opt/r\xe9glages");
        assert_eq!(synth_ini_path(ini, "A", "Directory_portsdir").as_deref(), Some(Path::new("/p")));
        assert!(synth_ini_misreads(ini).is_empty());

        // Through the resolution: the options dir is the Latin-1 path.
        let tmp = tempfile::tempdir().unwrap();
        let [synth, stage, tree] = ["synth", "stage", "ports"].map(|d| tmp.path().join(d));
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fake_tree(&tree);
        let mut text = b"[A]\nDirectory_options=".to_vec();
        text.extend_from_slice(tmp.path().as_os_str().as_bytes());
        text.extend_from_slice(b"/r\xe9glages\n");
        fs::write(synth.join("synth.ini"), &text).unwrap();
        let s = resolve_synth(&synth, "A", &synth_host(None), Some(&tree), None, &stage).unwrap();
        assert!(s.options_dir.as_os_str().as_bytes().ends_with(b"/r\xe9glages"), "{:?}", s.options_dir);
    }

    /// With no Directory_portsdir, synth takes the first that exists of
    /// PORTSDIR, make's PORTSDIR, /usr/dports, /usr/ports; -P still comes
    /// first, and the banner says which gave the tree.
    #[test]
    fn synth_portsdir_falls_back_as_synth_determines_it() {
        let tmp = tempfile::tempdir().unwrap();
        let [env, make, dports, ports] =
            ["env", "make", "dports", "ports"].map(|d| tmp.path().join(d));
        let absent = tmp.path().join("absent");
        let standard = [dports.to_str().unwrap(), ports.to_str().unwrap()];
        let answer = std::cell::RefCell::new(Some(make.clone()));
        let make_portsdir = || answer.borrow().clone();
        let host = |env_portsdir| SynthHost {
            env_profile: None,
            env_portsdir,
            make_portsdir: &make_portsdir,
            standard_portsdirs: &standard,
        };
        let pick = |env_portsdir| synth_default_portsdir(&host(env_portsdir));

        assert_eq!(pick(Some(&env)), None, "nothing exists yet");
        fake_tree(&ports);
        assert_eq!(pick(Some(&env)), Some((ports.clone(), "synth's default")));
        fake_tree(&dports);
        assert_eq!(pick(Some(&env)), Some((dports.clone(), "synth's default")));
        fake_tree(&make);
        assert_eq!(pick(Some(&env)), Some((make.clone(), "from make's PORTSDIR")));
        fake_tree(&env);
        assert_eq!(pick(Some(&env)), Some((env.clone(), "from PORTSDIR")));
        assert_eq!(pick(Some(&absent)), Some((make.clone(), "from make's PORTSDIR")));
        *answer.borrow_mut() = None;
        assert_eq!(pick(None), Some((dports.clone(), "synth's default")));

        // Through the resolution: noted, -P first, and a candidate that is
        // no tree is an error naming its source.
        let synth = tmp.path().join("synth");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(synth.join("synth.ini"), "[A]\n").unwrap();
        let s = resolve_synth(&synth, "A", &host(Some(&env)), None, Some(&stage), &stage).unwrap();
        assert_eq!(s.portsdir, env);
        let note = format!("no Directory_portsdir: ports tree {} (from PORTSDIR)", env.display());
        assert!(s.notes.contains(&note), "{:?}", s.notes);
        let s = resolve_synth(&synth, "A", &host(Some(&env)), Some(&ports), Some(&stage), &stage).unwrap();
        assert_eq!(s.portsdir, ports);
        assert!(!s.notes.iter().any(|n| n.contains("Directory_portsdir")), "{:?}", s.notes);
        let err = resolve_synth(&synth, "A", &host(Some(&stage)), None, Some(&stage), &stage)
            .unwrap_err()
            .to_string();
        assert!(err.contains("(from PORTSDIR) is not a ports tree"), "{err}");
    }

    #[test]
    fn synth_notes_flag_missing_profile_and_live_options_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let synth = tmp.path().join("synth");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(
            synth.join("synth.ini"),
            "[Global Configuration]\nprofile_selected= Live\n[Live]\n",
        )
        .unwrap();

        // Unknown named profile: noted, defaults still apply (/var/db/ports
        // triggers the live-dir note too). /usr/ports exists on this host.
        let s = resolve_synth(&synth, "typo", &synth_host(None), None, None, &stage).unwrap();
        assert!(s.notes.iter().any(|n| n.contains("'typo' has no section")), "{:?}", s.notes);
        assert!(s.notes.iter().any(|n| n.contains("live /var/db/ports")), "{:?}", s.notes);

        // Bare -s honours profile_selected; known section → no missing note.
        let s = resolve_synth(&synth, "", &synth_host(None), None, None, &stage).unwrap();
        assert!(s.notes.iter().any(|n| n.contains("profile Live (synth.ini")), "{:?}", s.notes);
        assert!(!s.notes.iter().any(|n| n.contains("no section")), "{:?}", s.notes);

        // -o override: no live-dir note.
        let o = tmp.path().join("other");
        let s = resolve_synth(&synth, "Live", &synth_host(None), None, Some(&o), &stage).unwrap();
        assert!(!s.notes.iter().any(|n| n.contains("live /var/db/ports")), "{:?}", s.notes);
    }

    /// synth has no jail: nothing stages the jail system lines poudriere's
    /// own `jail_system` writes for -j, so the staged synth make.conf never
    /// sets OSVERSION, ARCH or _OSRELEASE.
    #[test]
    fn synth_make_conf_never_sets_the_jail_system_vars() {
        let tmp = tempfile::tempdir().unwrap();
        let synth = tmp.path().join("synth");
        let tree = tmp.path().join("ports");
        fake_tree(&tree);
        let stage = tmp.path().join("stage");
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(synth.join("synth.ini"), format!("[A]\nDirectory_portsdir={}\n", tree.display()))
            .unwrap();
        fs::write(synth.join("A-make.conf"), "X=1\n").unwrap();

        let s = resolve_synth(&synth, "A", &synth_host(None), None, None, &stage).unwrap();
        let conf = fs::read_to_string(s.make_conf.unwrap()).unwrap();
        for var in ["OSVERSION", "ARCH", "_OSRELEASE"] {
            assert!(!conf.contains(var), "{var} in synth make.conf: {conf}");
        }
    }

    /// A bare -s runs with the profile synth itself would: SYNTHPROFILE's
    /// when its section sets Directory_packages, else profile_selected,
    /// else LiveSystem; the banner says which chose it. A named -s wins.
    #[test]
    fn synth_profile_follows_synthprofile_then_the_ini() {
        let tmp = tempfile::tempdir().unwrap();
        let synth = tmp.path().join("synth");
        let stage = tmp.path().join("stage");
        let o = tmp.path().join("other");
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(
            synth.join("synth.ini"),
            "[Global Configuration]\nprofile_selected= Live\n[Live]\n\
             [Env]\nDirectory_packages=/pkgs\n[Bare]\nDirectory_options=/x\n",
        )
        .unwrap();
        let picked = |profile, env| {
            let s = resolve_synth(&synth, profile, &synth_host(env), None, Some(&o), &stage).unwrap();
            let staged = fs::read_to_string(s.make_conf.unwrap()).unwrap();
            let name = staged.lines().find_map(|l| l.strip_prefix("SYNTHPROFILE=")).unwrap().to_string();
            (name, s.notes)
        };

        let (name, notes) = picked("", Some("Env"));
        assert_eq!(name, "Env");
        assert!(notes.iter().any(|n| n == "synth profile Env (SYNTHPROFILE)"), "{notes:?}");
        // A section without Directory_packages, an unknown one, an empty
        // value: synth ignores the variable.
        for env in [Some("Bare"), Some("Nope"), Some(""), None] {
            let (name, notes) = picked("", env);
            assert_eq!(name, "Live", "{env:?}");
            assert!(notes.iter().any(|n| n == "synth profile Live (synth.ini profile_selected)"), "{notes:?}");
        }
        assert_eq!(picked("Bare", Some("Env")).0, "Bare");

        fs::write(synth.join("synth.ini"), "[LiveSystem]\n").unwrap();
        let (name, notes) = picked("", None);
        assert_eq!(name, "LiveSystem");
        assert!(notes.iter().any(|n| n == "synth profile LiveSystem (synth's default)"), "{notes:?}");
    }

    #[test]
    fn synth_resolution_follows_the_ini() {
        let tmp = tempfile::tempdir().unwrap();
        let synth = tmp.path().join("synth");
        let tree = tmp.path().join("ports");
        let opts = tmp.path().join("db-ports");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(tree.join("Mk")).unwrap();
        fs::write(tree.join("Mk/bsd.port.mk"), "# fake\n").unwrap();
        fs::create_dir_all(&opts).unwrap();
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(
            synth.join("synth.ini"),
            format!(
                "[Global Configuration]\nprofile_selected= LiveSystem\n\n\
                 [LiveSystem]\nDirectory_portsdir= {}\nDirectory_options={}\n",
                tree.display(),
                opts.display()
            ),
        )
        .unwrap();
        fs::write(synth.join("LiveSystem-make.conf"), "DEFAULT_VERSIONS+=ssl=openssl\n").unwrap();
        // A second profile's make.conf must not be picked up.
        fs::write(synth.join("Other-make.conf"), "A=1\n").unwrap();

        let s = resolve_synth(&synth, "LiveSystem", &synth_host(None), None, None, &stage).unwrap();
        assert_eq!(s.portsdir, tree);
        assert_eq!(s.options_dir, opts);
        assert!(s.options_dir.is_dir());
        assert_eq!(s.make_conf_sources, vec![synth.join("LiveSystem-make.conf")]);
        assert_eq!(s.env, QueryEnv::Synth);
        let staged = fs::read_to_string(s.make_conf.as_ref().unwrap()).unwrap();
        assert!(staged.contains("ssl=openssl"), "{staged}");
        assert!(!staged.contains("A=1"), "other profile leaked in: {staged}");
        // synth's scan slave: its header before the profile's make.conf,
        // PACKAGE_BUILDING without _FLAVORS, and an empty LOCALBASE after it
        // so the profile cannot undo it.
        let at = |needle: &str| staged.find(needle).unwrap_or_else(|| panic!("{needle}: {staged}"));
        assert!(at("SYNTHPROFILE=LiveSystem\n") < at("ssl=openssl"), "{staged}");
        assert!(at("PACKAGE_BUILDING=yes\n") < at("ssl=openssl"), "{staged}");
        assert!(!staged.contains("PACKAGE_BUILDING_FLAVORS"), "{staged}");
        assert!(at("ssl=openssl") < at("LOCALBASE=/nonexistent\n"), "{staged}");
        assert!(staged.ends_with("LOCALBASE=/nonexistent\n"), "{staged}");
        // synth has no blacklist, and nothing to note.
        assert!(s.blacklist.is_empty());
        assert!(s.notes.is_empty());
        assert!(!s.conf_hash.is_empty());

        // -o wins over Directory_options, -P over Directory_portsdir.
        let elsewhere = tmp.path().join("elsewhere");
        fake_tree(&elsewhere);
        let s = resolve_synth(&synth, "LiveSystem", &synth_host(None), Some(&elsewhere), Some(&elsewhere), &stage)
            .unwrap();
        assert_eq!((s.portsdir.as_path(), s.options_dir.as_path()), (elsewhere.as_path(), elsewhere.as_path()));
        assert!(s.notes.is_empty(), "{:?}", s.notes);
    }

    /// synth's header gains its ccache lines, after the rest of it and
    /// before the profile's make.conf, only when Directory_ccache exists.
    #[test]
    fn synth_ccache_follows_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let synth = tmp.path().join("synth");
        let tree = tmp.path().join("ports");
        let ccache = tmp.path().join("ccache");
        let stage = tmp.path().join("stage");
        fake_tree(&tree);
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        fs::write(
            synth.join("synth.ini"),
            format!(
                "[A]\nDirectory_portsdir={}\nDirectory_options={}\nDirectory_ccache={}\n",
                tree.display(),
                tmp.path().display(),
                ccache.display()
            ),
        )
        .unwrap();
        fs::write(synth.join("A-make.conf"), "X=1\n").unwrap();
        let staged = || {
            let s = resolve_synth(&synth, "A", &synth_host(None), None, None, &stage).unwrap();
            fs::read_to_string(s.make_conf.unwrap()).unwrap()
        };

        let absent = staged();
        assert!(!absent.contains("CCACHE"), "{absent}");

        fs::create_dir_all(&ccache).unwrap();
        let present = staged();
        let at = |needle: &str| present.find(needle).unwrap_or_else(|| panic!("{needle}: {present}"));
        assert!(at("BATCH=yes\n") < at("WITH_CCACHE_BUILD=yes\nCCACHE_DIR=/ccache\n"), "{present}");
        assert!(at("CCACHE_DIR=/ccache\n") < at("X=1\n"), "{present}");

        // Unset is synth's "disabled", never a path looked up.
        assert!(!synth_ccache(None));
        assert!(!synth_ccache(Some(Path::new("disabled"))));
    }

    #[test]
    fn synth_resolution_without_profile_files() {
        let tmp = tempfile::tempdir().unwrap();
        let synth = tmp.path().join("synth");
        let tree = tmp.path().join("ports");
        let opts = tmp.path().join("db-ports");
        let stage = tmp.path().join("stage");
        fs::create_dir_all(tree.join("Mk")).unwrap();
        fs::write(tree.join("Mk/bsd.port.mk"), "# fake\n").unwrap();
        fs::create_dir_all(&synth).unwrap();
        fs::create_dir_all(&stage).unwrap();
        // No <profile>-make.conf at all.
        fs::write(
            synth.join("synth.ini"),
            format!(
                "[LiveSystem]\nDirectory_portsdir={}\nDirectory_options={}\n",
                tree.display(),
                opts.display()
            ),
        )
        .unwrap();

        let s = resolve_synth(&synth, "LiveSystem", &synth_host(None), None, None, &stage).unwrap();
        assert_eq!(s.portsdir, tree);
        // Nonexistent Directory_options is still the target, not created yet.
        assert_eq!(s.options_dir, opts);
        assert!(!s.options_dir.is_dir());
        // No profile make.conf still stages synth's header: left unset, make
        // would read the host's (/etc/make.conf or the environment's
        // __MAKE_CONF), which a synth builder never does.
        assert_eq!(s.make_conf.as_deref(), Some(stage.join("make.conf").as_path()));
        assert!(s.make_conf_sources.is_empty());
        let staged = fs::read_to_string(stage.join("make.conf")).unwrap();
        assert_eq!(staged, format!("{}{SYNTH_SCAN_TRAILER}", synth_prelude("LiveSystem", false)));

        // Directory_portsdir naming a non-tree is an error, not a fallback.
        fs::write(
            synth.join("synth.ini"),
            format!("[LiveSystem]\nDirectory_portsdir={}/nope\n", tmp.path().display()),
        )
        .unwrap();
        let err = resolve_synth(&synth, "LiveSystem", &synth_host(None), None, Some(&opts), &stage)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not a ports tree"), "{err}");

        // A setting in [Global Configuration] alone is no profile's: synth
        // reads Directory_options from the profile's section only.
        fs::write(
            synth.join("synth.ini"),
            format!(
                "[Global Configuration]\nDirectory_options={}\n\
                 [LiveSystem]\nDirectory_portsdir={}\n",
                opts.display(),
                tree.display()
            ),
        )
        .unwrap();
        let s = resolve_synth(&synth, "LiveSystem", &synth_host(None), None, None, &stage).unwrap();
        assert_eq!(s.options_dir, Path::new("/var/db/ports"));

        // No synth.ini: options dir falls back to /var/db/ports (the portsdir
        // fallback is /usr/ports, whose presence is host-dependent, so it is
        // only exercised through -o here).
        fs::remove_file(synth.join("synth.ini")).unwrap();
        if is_ports_tree(Path::new("/usr/ports")) {
            let s = resolve_synth(&synth, "LiveSystem", &synth_host(None), None, None, &stage).unwrap();
            assert_eq!(s.options_dir, Path::new("/var/db/ports"));
        }
    }

    /// synth asks make for PORTSDIR through /usr/share/mk/bsd.port.mk from
    /// the directory it runs in: a tree found above that directory, else
    /// /usr/ports, never /usr/dports; a missing tree fails the query.
    #[test]
    fn synth_default_portsdir_asks_make_as_synth_does() {
        if std::env::var_os("PORTSDIR").is_some()
            || make_values(&["PORTSDIR"], None, &[]).is_some_and(|t| !t.trim().is_empty())
        {
            eprintln!("PORTSDIR set by the environment or /etc/make.conf; skipped");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let tree = tmp.path().join("tree");
        fs::create_dir_all(tree.join("Mk")).unwrap();
        fs::create_dir_all(tree.join("cat/port/files")).unwrap();
        fs::write(tree.join("Mk/bsd.port.mk"), "# fake\n").unwrap();
        let real = tree.canonicalize().unwrap();
        assert_eq!(synth_make_portsdir(Some(&tree.join("cat/port"))), Some(real.clone()));
        assert_eq!(synth_make_portsdir(Some(&tree.join("cat/port/files"))), Some(real));
        // Outside any tree: /usr/ports, which make reads, or a failure.
        let outside = synth_make_portsdir(Some(tmp.path()));
        if Path::new("/usr/ports/Mk/bsd.port.mk").is_file() {
            assert_eq!(outside.as_deref(), Some(Path::new("/usr/ports")));
        } else {
            assert_eq!(outside, None);
        }
    }
}
