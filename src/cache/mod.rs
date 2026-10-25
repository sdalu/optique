//! The query cache: one generation file per ports tree and make.conf, an
//! entry per port valid while its options files are unchanged, and the
//! entries a new commit of a git tree carries over from the one before.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::origin::PortKey;
use crate::model::port::PortInfo;
use crate::optfiles::OptionsFiles;

mod makeconf;

/// Persistent per-generation query cache.
///
/// A generation is one (ports tree and its state, layered make.conf) pair;
/// any change to either, another tree at the same commit included, starts
/// a fresh file. Within a generation, an entry is valid as long as the
/// port's options file and `options.local` (in the PORT_DBDIR the query
/// saw), or the pair make.conf moved OPTIONS_FILE to, are unchanged — their
/// content is part of the entry. A new
/// generation of a git tree starts with the entries of the one before it
/// that the commits between them leave alone ([`Invalidation`]).
pub struct Cache {
    map: HashMap<PortKey, CacheEntry>,
    file: Option<File>,
    enabled: bool,
    /// What opening carried over from an earlier generation, for the banner.
    note: Option<String>,
    /// The make.conf files queries read in place, each with its size and
    /// mtime when the generation was opened (`watch`): once one changes,
    /// answers come from another text than the generation's, and none is
    /// stored any more.
    watched: Vec<(PathBuf, Option<(u64, std::time::SystemTime)>)>,
    /// A watched file changed: nothing more is stored this run.
    frozen: bool,
}

/// The options files a query of `info` read: the entry's file and its
/// `.local`, and the file make.conf moved OPTIONS_FILE to with its own.
fn read_files(info: &PortInfo, files: &OptionsFiles) -> Vec<PathBuf> {
    let mut out = vec![
        files.path(&info.options_name),
        files.local_path(&info.options_name),
    ];
    if let Some(moved) = info.options.moved_file.as_deref().filter(|f| !f.is_empty()) {
        out.push(PathBuf::from(moved));
        out.push(PathBuf::from(format!("{moved}.local")));
    }
    out
}

/// Was `path` written since `since`? Its ctime, which a write or a rename
/// into place always moves and nothing sets back (an mtime `cp -p` or
/// `touch -r` can). A missing file has none, and is fine.
fn changed_since(path: &Path, since: std::time::SystemTime) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let Ok(m) = fs::metadata(path) else {
        return false;
    };
    let Ok(secs) = u64::try_from(m.ctime()) else {
        return false;
    };
    let nanos = u32::try_from(m.ctime_nsec()).unwrap_or(0);
    std::time::UNIX_EPOCH + std::time::Duration::new(secs, nanos) >= since
}

/// `path`'s size and mtime; None when it cannot be stat'ed.
fn stamp(path: &Path) -> Option<(u64, std::time::SystemTime)> {
    let m = fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()?))
}

/// A generation file's first line: what it was made for, which is what a
/// later generation needs to know to carry its entries over.
#[derive(Serialize, Deserialize)]
struct Header {
    generation: Generation,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
struct Generation {
    /// The main tree's canonical path: two trees at one commit (poudriere's
    /// `-p default` and `-p dev`, a clone) are two generations, since each
    /// holds its own uncommitted edits and its entries name its own files.
    portsdir: String,
    /// The main tree's git commit; None for a tree without one.
    commit: Option<String>,
    /// The overlays' part of the tree key (see [`Trees`]).
    overlays: String,
    conf: String,
    /// The staged make.conf text `conf` was made from, and a signature of
    /// everything else folded into it ([`Conf`]): what carrying entries over
    /// an edit to make.conf needs.
    #[serde(default)]
    conf_text: String,
    #[serde(default)]
    conf_rest: String,
}

/// Values make gives variables (in order) under a make.conf of the given
/// text, outside any port; None when make cannot say.
pub type Probe<'a> = &'a dyn Fn(&str, &[String]) -> Option<Vec<String>>;

/// The make.conf a generation is opened for: the hash naming it, and what
/// carrying entries over an edit to it needs.
pub struct Conf<'a> {
    pub hash: &'a str,
    /// The staged make.conf text the hash was made from.
    pub text: &'a str,
    /// A signature of everything else folded into the hash: the host's
    /// installed packages, the invoking user, the framework's host values.
    pub rest: &'a str,
    /// Asked for the option variables an edit touched, under each text.
    pub probe: Option<Probe<'a>>,
    /// Whether a new generation may start with an earlier one's entries
    /// (`--no-carry-over` says no).
    pub carry: bool,
}

impl<'a> From<&'a str> for Conf<'a> {
    /// A make.conf known by its hash alone, which no entry is carried over
    /// an edit to.
    fn from(hash: &'a str) -> Self {
        Conf {
            hash,
            text: "",
            rest: "",
            probe: None,
            carry: true,
        }
    }
}

/// The trees a generation is opened for: its key, and what carrying entries
/// over from an earlier revision of the main tree needs.
pub struct Trees {
    /// The tree part of the generation's name ([`trees_key`]).
    pub key: String,
    /// The main tree, canonical where it can be made so.
    portsdir: PathBuf,
    /// The main tree's git commit, when [`tree_key`] read one.
    commit: Option<String>,
    /// Each overlay's path and identity, one `\n<path> <key>` each.
    overlays: String,
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    options_name: String,
    options_hash: String,
    /// The `options.local` make read after the options file, hashed alike.
    local_hash: String,
    /// When make.conf moved OPTIONS_FILE (`PortOptions::moved_file`), the
    /// file make reads instead and its `.local`, hashed alike: make reads
    /// those, not the two above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    moved_hashes: Option<(String, String)>,
    /// The hash of each of `info.outside_files` when the query was made, in
    /// order: what make.conf `.include`s, which no other key covers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    outside_hashes: Vec<String>,
    info: PortInfo,
}

impl CacheEntry {
    /// Do the files outside every other key still hold what they held for
    /// the query? `now` memoizes each file's hash, read once per opening.
    fn outside_unchanged(&self, now: &mut HashMap<String, String>) -> bool {
        self.info.outside_files.len() == self.outside_hashes.len()
            && self
                .info
                .outside_files
                .iter()
                .zip(&self.outside_hashes)
                .all(|(file, then)| {
                    now.entry(file.clone())
                        .or_insert_with(|| file_hash(Path::new(file)))
                        == then
                })
    }
}

impl Cache {
    /// A cache that never hits and never stores (--no-cache).
    pub fn disabled() -> Self {
        Cache {
            map: HashMap::new(),
            file: None,
            enabled: false,
            note: None,
            watched: Vec::new(),
            frozen: false,
        }
    }

    /// The generation for a tree known by its key alone, which nothing is
    /// carried over into.
    #[cfg(test)]
    pub fn open(cache_dir: &Path, tree_key: &str, conf_hash: &str) -> Self {
        let trees = Trees {
            key: tree_key.to_string(),
            portsdir: PathBuf::new(),
            commit: None,
            overlays: String::new(),
        };
        Self::open_trees(cache_dir, &trees, conf_hash)
    }

    /// The generation for `trees` under `conf_hash`. A new one starts with
    /// what [`carry_over`] keeps of the latest earlier one.
    pub fn open_trees<'a>(cache_dir: &Path, trees: &Trees, conf: impl Into<Conf<'a>>) -> Self {
        let conf = conf.into();
        let conf_hash = conf.hash;
        let generation = format!("v{SCHEMA}-{}-{}.jsonl", short(&trees.key), short(conf_hash));
        let path = cache_dir.join(&generation);
        let _ = fs::create_dir_all(cache_dir);
        let here = Generation {
            portsdir: trees.portsdir.to_string_lossy().into_owned(),
            commit: trees.commit.clone(),
            overlays: trees.overlays.clone(),
            conf: conf_hash.to_string(),
            conf_text: conf.text.to_string(),
            conf_rest: conf.rest.to_string(),
        };
        let mut note = None;
        let fresh = fs::metadata(&path).map_or(true, |m| m.len() == 0);
        if fresh && let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
            let mut text = serde_json::to_string(&Header {
                generation: here.clone(),
            })
            .map(|h| h + "\n")
            .unwrap_or_default();
            let carried = conf
                .carry
                .then(|| carry_over(cache_dir, &generation, &here, &trees.portsdir, conf.probe))
                .flatten();
            if let Some(carried) = carried {
                for line in &carried.lines {
                    text.push_str(line);
                    text.push('\n');
                }
                note = Some(carried.note);
            }
            let _ = f.write_all(text.as_bytes());
        }
        prune_old_generations(cache_dir, &generation);

        let mut map = HashMap::new();
        // An entry whose make.conf includes changed since its query is
        // left out: the latest line for a port decides, kept or not.
        let mut now = HashMap::new();
        let mut stale = HashSet::new();
        if let Ok(text) = fs::read_to_string(&path) {
            for line in text.lines() {
                if let Ok(entry) = serde_json::from_str::<CacheEntry>(line) {
                    let key = entry.info.key.clone();
                    if entry.outside_unchanged(&mut now) {
                        stale.remove(&key);
                        map.insert(key, entry);
                    } else {
                        map.remove(&key);
                        stale.insert(key);
                    }
                }
            }
        }
        if !stale.is_empty() {
            let dropped = format!(
                "{} entries dropped: a file make.conf includes changed",
                stale.len()
            );
            note = Some(match note {
                Some(carried) => format!("{carried}; {dropped}"),
                None => format!("cache: {dropped}"),
            });
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        // A line cut short (a process gone mid-write) would glue the next
        // one to it: end it first.
        if let Some(f) = &mut file
            && fs::read(&path).is_ok_and(|b| !b.is_empty() && !b.ends_with(b"\n"))
        {
            let _ = f.write_all(b"\n");
        }
        Cache {
            map,
            file,
            enabled: true,
            note,
            watched: Vec::new(),
            frozen: false,
        }
    }

    /// Watch `files`, make.conf files the queries read in place (a local
    /// build's; a builder's staged copy is fixed for the run): the
    /// generation is keyed on the text they had at startup, so an answer
    /// made after one is edited is not stored under it.
    pub fn watch(&mut self, files: &[PathBuf]) {
        self.watched = files.iter().map(|f| (f.clone(), stamp(f))).collect();
    }

    /// What was carried over from an earlier generation, if anything was
    /// looked for: a line for the startup banner.
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Return the cached PortInfo if the options file and `options.local` it
    /// was computed against are still byte-identical, and so are the file
    /// make.conf moved OPTIONS_FILE to and its `.local`, when it did.
    pub fn lookup(&self, key: &PortKey, files: &OptionsFiles) -> Option<PortInfo> {
        if !self.enabled {
            return None;
        }
        let entry = self.map.get(key)?;
        if files.hash(&entry.options_name) == entry.options_hash
            && files.local_hash(&entry.options_name) == entry.local_hash
            && moved_hashes(&entry.info) == entry.moved_hashes
        {
            Some(entry.info.clone())
        } else {
            None
        }
    }

    pub fn insert(&mut self, info: &PortInfo, files: &OptionsFiles, since: std::time::SystemTime) {
        if !self.enabled || self.frozen {
            return;
        }
        // A file the query read that changed since it started may hold other
        // bytes than make read: the answer is not stored under them. (A port
        // with an entry had its files read before the query, by `lookup`;
        // the others, and a moved OPTIONS_FILE, are read only now.)
        if read_files(info, files)
            .iter()
            .any(|f| changed_since(f, since))
        {
            return;
        }
        if let Some((path, _)) = self.watched.iter().find(|(f, at)| stamp(f) != *at) {
            errln!(
                "warning: {} changed during the run: no more answers are cached",
                path.display()
            );
            self.frozen = true;
            return;
        }
        let entry = CacheEntry {
            options_name: info.options_name.clone(),
            options_hash: files.hash(&info.options_name),
            local_hash: files.local_hash(&info.options_name),
            moved_hashes: moved_hashes(info),
            outside_hashes: info
                .outside_files
                .iter()
                .map(|f| file_hash(Path::new(f)))
                .collect(),
            info: info.clone(),
        };
        // One write(2) per line: O_APPEND keeps a single write whole, so a
        // second optique appending to the generation cannot land inside it.
        if let Some(f) = &mut self.file
            && let Ok(line) = serde_json::to_string(&entry)
        {
            let _ = f.write_all(format!("{line}\n").as_bytes());
        }
        self.map.insert(entry.info.key.clone(), entry);
    }
}

/// The hashes of the file make.conf moved `info`'s OPTIONS_FILE to and of
/// its `.local`, which bsd.options.mk reads in place of the options dir's
/// pair: the path as make expanded it, so one under PORT_DBDIR names the
/// directory the query saw. None when OPTIONS_FILE was not moved.
fn moved_hashes(info: &PortInfo) -> Option<(String, String)> {
    let file = info
        .options
        .moved_file
        .as_deref()
        .filter(|f| !f.is_empty())?;
    Some((
        file_hash(Path::new(file)),
        file_hash(Path::new(&format!("{file}.local"))),
    ))
}

/// A file's hash as [`OptionsFiles::hash`] records one: its SHA-256,
/// `ABSENT` or `UNREADABLE`.
fn file_hash(path: &Path) -> String {
    match fs::read(path) {
        Ok(bytes) => sha256_hex(&bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => crate::optfiles::ABSENT.to_string(),
        Err(_) => crate::optfiles::UNREADABLE.to_string(),
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// A file's modification time since the epoch; zero when the platform
/// cannot tell, which only weakens a key, never breaks one.
pub(crate) fn mtime(meta: &fs::Metadata) -> std::time::Duration {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .unwrap_or_default()
}

/// Identity of the ports tree: the git HEAD commit, falling back for non-git
/// trees to the mtimes of the files an update rewrites (`mtime-<a>-<b>-<c>`).
pub fn tree_key(portsdir: &Path) -> String {
    // HEAD only: a `git status` dirty-check would stat the whole tree
    // (~10s cold on /usr/ports). Uncommitted local edits to the tree are
    // invisible to the cache — use --no-cache when hacking on ports.
    if let Some(head) = git_head(portsdir) {
        return head;
    }
    // An update touches MOVED or UPDATING far more often than the framework,
    // so the framework alone would keep a generation across most updates.
    let mtimes: Vec<String> = ["Mk/bsd.port.mk", "MOVED", "UPDATING"]
        .iter()
        .map(|file| {
            fs::metadata(portsdir.join(file))
                .map_or(0, |m| mtime(&m).as_secs())
                .to_string()
        })
        .collect();
    format!("mtime-{}", mtimes.join("-"))
}

/// Identity of the ports tree and the overlays laid over it, in order: a
/// digest of the main tree's `tree_key`, its canonical path and each
/// overlay's path and identity. The path is part of it: two trees at one
/// commit (poudriere's `-p default` and `-p dev`, a clone) differ in their
/// uncommitted edits and in the absolute paths their entries hold.
#[cfg(test)]
pub fn trees_key(portsdir: &Path, overlays: &[PathBuf]) -> String {
    trees(portsdir, overlays).key
}

/// The trees' identity ([`trees_key`]) with its parts kept apart.
pub fn trees(portsdir: &Path, overlays: &[PathBuf]) -> Trees {
    let main = tree_key(portsdir);
    let portsdir = fs::canonicalize(portsdir).unwrap_or_else(|_| portsdir.to_path_buf());
    let overlays: String = overlays
        .iter()
        .map(|o| format!("\n{} {}", o.display(), overlay_key(o)))
        .collect();
    let key = sha256_hex(format!("{main}\n{portsdir:?}{overlays}").as_bytes());
    Trees {
        key,
        portsdir,
        commit: object_id(&main),
        overlays,
    }
}

/// What [`carry_over`] found for a new generation.
struct Carried {
    /// The kept entries' lines, as the earlier generation holds them.
    lines: Vec<String>,
    note: String,
}

/// The entries of the latest earlier generation that a new one, `here`
/// (`name`), can start with: one made for the same make.conf and overlays
/// at another commit of the same git tree, at the same path (another tree
/// at that commit holds its own edits), keeping what the commits since
/// leave alone ([`carry_over_tree`]); or one made for the same tree, its
/// overlays and everything else folded into the make.conf hash, under a
/// make.conf differing only in option policy, keeping what that edit cannot
/// reach ([`carry_over_conf`]). None when there is no such generation, or
/// what changed cannot be told.
fn carry_over(
    cache_dir: &Path,
    name: &str,
    here: &Generation,
    portsdir: &Path,
    probe: Option<Probe>,
) -> Option<Carried> {
    let prefix = format!("v{SCHEMA}-");
    let tree_part = |n: &str| n.split('-').nth(1).map(str::to_string);
    let mut others: Vec<(std::time::SystemTime, PathBuf, String)> = fs::read_dir(cache_dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let candidate = n.starts_with(&prefix) && n.ends_with(".jsonl") && n != name;
            candidate.then(|| Some((e.metadata().ok()?.modified().ok()?, e.path(), n)))?
        })
        .collect();
    others.sort_by_key(|(t, _, _)| std::cmp::Reverse(*t));
    others.into_iter().find_map(|(_, path, n)| {
        let text = fs::read_to_string(path).ok()?;
        let header: Header = serde_json::from_str(text.lines().next()?).ok()?;
        let g = header.generation;
        if g.portsdir != here.portsdir || g.overlays != here.overlays {
            return None;
        }
        if g.conf == here.conf {
            let (old, new) = (g.commit?, here.commit.clone()?);
            return (old != new)
                .then(|| carry_over_tree(&text, &old, &new, portsdir))
                .flatten();
        }
        let same_tree = g.commit == here.commit && tree_part(&n) == tree_part(name);
        let comparable = same_tree && g.conf_rest == here.conf_rest && !g.conf_text.is_empty();
        comparable
            .then(|| carry_over_conf(&text, &g.conf_text, &here.conf_text, probe?))
            .flatten()
    })
}

/// [`carry_over`] from `text`, a generation of commit `old`, to commit
/// `new` of the same tree.
fn carry_over_tree(text: &str, old: &str, new: &str, portsdir: &Path) -> Option<Carried> {
    let mut changes = git_changes(portsdir, old, new)?;
    let changed = changes.len();
    changes.extend(
        parse_time_readers(portsdir, &changes)
            .into_iter()
            .map(|mk| ('M', mk)),
    );
    let invalidation = Invalidation::of(&changes);
    let from = short(old);
    if let Some(path) = &invalidation.all {
        return Some(Carried {
            lines: Vec::new(),
            note: format!("cache: nothing carried over from tree {from}: {path} changed since"),
        });
    }
    let (mut lines, mut dropped) = (Vec::new(), 0);
    for line in text.lines().skip(1) {
        match serde_json::from_str::<CacheEntry>(line) {
            // A port running make on another one at parse time read a port
            // its makefiles never name (`PortInfo::sub_make`).
            Ok(entry) if invalidation.keeps(&entry.info) && !entry.info.sub_make => {
                lines.push(line.to_string());
            }
            Ok(_) => dropped += 1,
            Err(_) => {}
        }
    }
    let note = format!(
        "cache: {} entries carried over from tree {from}, {dropped} dropped by {changed} changed paths",
        lines.len(),
    );
    Some(Carried { lines, note })
}

/// [`carry_over`] from `text`, a generation of the same tree made under the
/// make.conf `old`, to the make.conf `new`: when the edit changes option
/// policy alone (`makeconf::option_edit`), each entry it cannot reach, its
/// dump patched to the new values, which `probe` asks make for under each
/// text. None for any other edit, or when make cannot say.
fn carry_over_conf(text: &str, old: &str, new: &str, probe: Probe) -> Option<Carried> {
    let vars = makeconf::option_edit(old, new)?;
    let reach = makeconf::Reach::of(&vars, &probe(old, &vars)?, &probe(new, &vars)?);
    let (mut lines, mut dropped) = (Vec::new(), 0);
    for line in text.lines().skip(1) {
        let Ok(mut entry) = serde_json::from_str::<CacheEntry>(line) else {
            continue;
        };
        match reach.carry(&entry.info) {
            Some(info) => {
                entry.info = info;
                lines.extend(serde_json::to_string(&entry).ok());
            }
            None => dropped += 1,
        }
    }
    let note = format!(
        "cache: {} entries carried over from the make.conf before, {dropped} dropped by its edit to {}",
        lines.len(),
        vars.join(" "),
    );
    Some(Carried { lines, note })
}

/// The paths that differ between commits `old` and `new` of the git tree at
/// `portsdir`, each with its status letter (`A`, `D`, `M`, ...): `git
/// diff-tree`, plumbing, so no user configuration changes its output. The
/// tree is commonly root's and optique not, which git refuses as dubious
/// ownership; nothing here reads the work tree or runs a hook, and optique
/// runs this tree's Makefiles anyway, so every directory is let through.
/// None when git is missing or fails (an old commit gone, a shallow clone).
fn git_changes(portsdir: &Path, old: &str, new: &str) -> Option<Vec<(char, String)>> {
    let out = std::process::Command::new("git")
        .args(["--no-pager", "-c", "safe.directory=*", "-C"])
        .arg(portsdir)
        .args([
            "diff-tree",
            "-r",
            "-z",
            "--no-renames",
            "--name-status",
            old,
            new,
        ])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let text = String::from_utf8(out.stdout).ok()?;
    let mut fields = text.split('\0').filter(|f| !f.is_empty());
    let mut changes = Vec::new();
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        changes.push((status.chars().next()?, path.to_string()));
    }
    Some(changes)
}

/// The `Mk/*.mk` files that run one of the changed files under `Mk/` that
/// is not itself a makefile (a script) while make parses: such a change
/// counts as one to the makefile, whose readers it then takes. A script is
/// run at parse time when a command make runs then (`VAR!= cmd`,
/// `${:!cmd!}`, `${VAR:sh}`) names it once the variables it uses are
/// expanded from `Mk/`'s own assignments: `Uses/cargo.mk` runs three awk
/// scripts through `${_CARGO_AWK}`, and the crates they list add build
/// dependencies. Most scripts are only run by targets (`do-fetch.sh`,
/// `qa.sh`), and a change to one takes nothing. `Mk/` is read only when
/// such a file changed.
fn parse_time_readers(portsdir: &Path, changes: &[(char, String)]) -> Vec<String> {
    let scripts: Vec<&str> = changes
        .iter()
        .filter(|(_, path)| path.starts_with("Mk/") && !path.ends_with(".mk"))
        .filter_map(|(_, path)| path.rsplit('/').next())
        .collect();
    if scripts.is_empty() {
        return Vec::new();
    }
    let makefiles = mk_files(&portsdir.join("Mk"));
    let mut assigned: HashMap<String, String> = HashMap::new();
    let mut commands: Vec<(String, String)> = Vec::new();
    for (rel, text) in &makefiles {
        for line in logical_lines(text) {
            if line.starts_with('\t') {
                continue; // a target's recipe
            }
            if let Some((name, value)) = assignment(&line) {
                let entry = assigned.entry(name.to_string()).or_default();
                entry.push(' ');
                entry.push_str(value);
            }
            for command in parse_commands(&line) {
                commands.push((format!("Mk/{rel}"), command));
            }
        }
    }
    // `${VAR:sh}` runs VAR's value: its command is in `assigned`.
    let mut readers: Vec<String> = commands
        .into_iter()
        .filter(|(_, command)| {
            let expanded = expand(command, &assigned, 4);
            scripts.iter().any(|script| expanded.contains(script))
        })
        .map(|(mk, _)| mk)
        .collect();
    readers.sort();
    readers.dedup();
    readers
}

/// Does a makefile with this `text` run make while make parses it (`VAR!=
/// ${MAKE} -C ../other -V X`)? That child make reads another port, which
/// `.MAKE.MAKEFILES` never names: www/caddy-custom takes its version from
/// www/caddy, mail/claws-mail-plugins its options from each plugin. Judged
/// at query time (`PortInfo::sub_make`), for the port's makefiles in the
/// tree or an overlay alike.
pub(crate) fn makefile_runs_make(text: &str) -> bool {
    logical_lines(text)
        .iter()
        .filter(|line| !line.starts_with('\t'))
        .flat_map(|line| parse_commands(line))
        .any(|command| runs_make(&command))
}

/// Is `command` a make run on a port: `${MAKE}`, `${MAKE_CMD}` or `make`,
/// with `-C` or `-V`? A plain `!=` that runs no make reads no other port.
fn runs_make(command: &str) -> bool {
    let words: Vec<&str> = command.split_whitespace().collect();
    let make = words.iter().any(|w| {
        matches!(
            *w,
            "${MAKE}" | "$(MAKE)" | "${MAKE_CMD}" | "$(MAKE_CMD)" | "make"
        ) || w.ends_with("/make")
    });
    make && words
        .iter()
        .any(|w| w.starts_with("-C") || w.starts_with("-V"))
}

/// Every `*.mk` under `dir`, relative to it, with its text.
fn mk_files(dir: &Path) -> Vec<(String, String)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else if path.extension().is_some_and(|e| e == "mk")
                && let (Ok(rel), Ok(text)) = (path.strip_prefix(base), fs::read(&path))
            {
                let text = String::from_utf8_lossy(&text).into_owned();
                out.push((rel.to_string_lossy().into_owned(), text));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

/// A makefile's lines with their backslash continuations joined.
fn logical_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        match line.strip_suffix('\\') {
            Some(head) => {
                current.push_str(head);
                current.push(' ');
            }
            None => {
                current.push_str(line);
                lines.push(std::mem::take(&mut current));
            }
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// `NAME=value` in any of make's assignment forms (`=`, `+=`, `?=`, `:=`,
/// `!=`), with the name and the value.
fn assignment(line: &str) -> Option<(&str, &str)> {
    let at = line.find('=')?;
    let name = line[..at].trim_end_matches(['+', '?', ':', '!']).trim();
    let word = |b: u8| b.is_ascii_alphanumeric() || b"_.-".contains(&b);
    let ok = !name.is_empty() && name.bytes().all(word);
    ok.then(|| (name, line[at + 1..].trim()))
}

/// The commands a line has make run while it parses: a `!=` assignment's
/// value, every `${...:!cmd!}`, and the variable of every `${VAR:sh}`.
fn parse_commands(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(at) = line.find("!=")
        && assignment(line).is_some_and(|(name, _)| line[..at].trim() == name)
    {
        out.push(line[at + 2..].to_string());
    }
    let mut rest = line;
    while let Some(at) = rest.find(":!") {
        let tail = &rest[at + 2..];
        let end = tail
            .find("!}")
            .or_else(|| tail.find("!:"))
            .unwrap_or(tail.len());
        out.push(tail[..end].to_string());
        rest = &tail[end..];
    }
    let mut rest = line;
    while let Some(at) = rest.find(":sh}") {
        let head = &rest[..at];
        if let Some(open) = head.rfind("${") {
            out.push(format!("${{{}}}", &head[open + 2..]));
        }
        rest = &rest[at + 4..];
    }
    out
}

/// `text` with each `${NAME}` or `${NAME:...}` followed into what `Mk/`
/// assigns NAME, kept as well, `depth` levels down.
fn expand(text: &str, assigned: &HashMap<String, String>, depth: usize) -> String {
    let mut out = text.to_string();
    if depth == 0 {
        return out;
    }
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        let tail = &rest[at + 2..];
        let end = tail.find(['}', ':']).unwrap_or(tail.len());
        if let Some(value) = assigned.get(&tail[..end]) {
            out.push(' ');
            out.push_str(&expand(value, assigned, depth - 1));
        }
        rest = &tail[end..];
    }
    out
}

/// Top-level files of the tree that no query reads: the framework names
/// UIDs and GIDs only in a target's shell, and the top Makefile is the
/// tree's own SUBDIR walk.
const UNREAD_TOP: [&str; 10] = [
    "CHANGES",
    "CONTRIBUTING.md",
    "COPYRIGHT",
    "GIDs",
    "LEGAL",
    "MOVED",
    "Makefile",
    "README",
    "UIDs",
    "UPDATING",
];

/// What a set of changed paths takes from a cache generation. An entry
/// goes when make read a changed file for it, which `.MAKE.MAKEFILES`
/// lists (`PortInfo::makefiles`), or when anything changed in a directory
/// it owns, the port's own and, for a slave, its master's (the directories
/// whose `Makefile` it read): bsd.port.mk probes there with `exists()`
/// (`pkg-help`, `Makefile.local`, `files/`) what it never includes. A
/// change in a category's own files, other than its SUBDIR Makefile, takes
/// the entries owning a port there (bsd.port.mk probes `../Makefile.inc`).
/// Everything goes for a top-level file a query may read (PORTSDIR's
/// `Makefile.inc`, probed too) and for a new `.mk` anywhere under `Mk/`:
/// `bsd.local.mk` is `.sinclude`d, and a `Uses/X.mk` a port named before
/// it existed was only probed with `exists()`, the query answering with an
/// ERROR no entry's makefiles record. A `Mk/` script run while make parses counts as its
/// makefile ([`parse_time_readers`]); the rest of `Mk/`, `Tools/`,
/// `Templates/` and `Keywords/` only reach make through a target.
struct Invalidation {
    /// The changed path that takes everything.
    all: Option<String>,
    files: HashSet<String>,
    dirs: HashSet<String>,
    categories: HashSet<String>,
}

impl Invalidation {
    fn of(changes: &[(char, String)]) -> Self {
        let mut inv = Invalidation {
            all: None,
            files: HashSet::new(),
            dirs: HashSet::new(),
            categories: HashSet::new(),
        };
        for (status, path) in changes {
            inv.files.insert(path.clone());
            let parts: Vec<&str> = path.split('/').collect();
            let everything = match parts.as_slice() {
                [first, ..] if first.starts_with('.') => false,
                ["Mk", .., file] if *status == 'A' && file.ends_with(".mk") => true,
                ["Mk" | "Tools" | "Templates" | "Keywords", ..] => false,
                [file] => !UNREAD_TOP.contains(file),
                [_, "Makefile"] => false,
                [category, _] => {
                    inv.categories.insert(category.to_string());
                    false
                }
                [category, port, ..] => {
                    inv.dirs.insert(format!("{category}/{port}"));
                    false
                }
                [] => false,
            };
            if everything && inv.all.is_none() {
                inv.all = Some(path.clone());
            }
        }
        inv
    }

    /// Does the entry made with `info` survive these changes?
    fn keeps(&self, info: &PortInfo) -> bool {
        if self.all.is_some() || info.makefiles.is_empty() {
            return false;
        }
        info.makefiles.iter().all(|file| {
            if self.files.contains(file) {
                return false;
            }
            let owned = file
                .strip_suffix("/Makefile")
                .filter(|d| d.matches('/').count() == 1);
            let Some(dir) = owned else { return true };
            let category = dir.split('/').next().unwrap_or_default();
            !self.dirs.contains(dir) && !self.categories.contains(category)
        })
    }
}

/// An overlay's identity: the count and newest mtime of what it holds down
/// to `category/port/file` (and `Mk/Uses/*.mk`, `MOVED`). Never its git
/// HEAD, even when it is a checkout of its own: an overlay is a handful of
/// ports edited in place before any commit, and an edit to a port's
/// Makefile touches the file, not its directory; a commit or a checkout
/// touches the files too.
fn overlay_key(overlay: &Path) -> String {
    fn walk(dir: &Path, depth: usize, count: &mut u64, newest: &mut u128) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_name() == ".git" {
                continue;
            }
            // Through a symlink, as the lookup goes: a symlinked port is
            // queried from its target, so its target's files are its key.
            // The depth limit stops a symlink loop.
            let Ok(meta) = fs::metadata(entry.path()) else {
                continue;
            };
            *count += 1;
            *newest = (*newest).max(mtime(&meta).as_nanos());
            if meta.is_dir() && depth > 1 {
                walk(&entry.path(), depth - 1, count, newest);
            }
        }
    }
    let (mut count, mut newest) = (0, 0);
    walk(overlay, 3, &mut count, &mut newest);
    format!("walk-{count}-{newest}")
}

/// The commit HEAD points at, read straight from the .git directory — no
/// `git` subprocess (a fork+exec per invocation, and git may not even be
/// installed on a host that only ever fetched a portsnap/distfile tree).
/// None whenever anything is missing or unrecognized; the caller then falls
/// back to the mtime key.
fn git_head(portsdir: &Path) -> Option<String> {
    let gitdir = resolve_gitdir(portsdir)?;
    let head = fs::read_to_string(gitdir.join("HEAD")).ok()?;
    let head = head.trim();
    let Some(refname) = head.strip_prefix("ref:") else {
        // Detached HEAD: the file holds the commit itself.
        return object_id(head);
    };
    let refname = refname.trim();
    // Only real refs, so a malformed HEAD cannot point outside the gitdir.
    if !refname.starts_with("refs/") {
        return None;
    }
    // Linked worktrees keep their own HEAD but share refs/ and packed-refs
    // with the main repository, named by the `commondir` file.
    let common = fs::read_to_string(gitdir.join("commondir"))
        .ok()
        .map(|c| resolve_relative(&gitdir, c.trim()));
    for dir in [Some(&gitdir), common.as_ref()].into_iter().flatten() {
        if let Some(id) = fs::read_to_string(dir.join(refname))
            .ok()
            .and_then(|t| object_id(t.trim()))
        {
            return Some(id);
        }
        if let Some(id) = packed_ref(dir, refname) {
            return Some(id);
        }
    }
    None
}

/// The repository directory backing `portsdir`: `.git` itself when it is a
/// directory, or the `gitdir: <path>` it names when it is a file (linked
/// worktree or submodule checkout).
fn resolve_gitdir(portsdir: &Path) -> Option<PathBuf> {
    let dot = portsdir.join(".git");
    if fs::metadata(&dot).ok()?.is_dir() {
        return Some(dot);
    }
    let text = fs::read_to_string(&dot).ok()?;
    let path = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("gitdir:"))?
        .trim();
    (!path.is_empty()).then(|| resolve_relative(portsdir, path))
}

/// Resolve a path a git file pointed at: absolute as given, relative to `base`.
fn resolve_relative(base: &Path, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// Look `refname` up in `gitdir/packed-refs` (`<id> <ref>` lines; '#' headers
/// and '^' peeled-tag lines are not refs).
fn packed_ref(gitdir: &Path, refname: &str) -> Option<String> {
    let text = fs::read_to_string(gitdir.join("packed-refs")).ok()?;
    for line in text.lines() {
        if line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        let Some((id, name)) = line.split_once(' ') else {
            continue;
        };
        if name.trim() == refname {
            return object_id(id);
        }
    }
    None
}

/// Accept a bare object id (sha1 or sha256 hex), rejecting anything else so a
/// stray file cannot become a cache generation name.
fn object_id(s: &str) -> Option<String> {
    let s = s.trim();
    let ok = matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit());
    ok.then(|| s.to_string())
}

/// Delete every cache generation file (drafts are left alone).
/// Returns (files removed, bytes freed).
pub fn clear(cache_dir: &Path) -> (usize, u64) {
    let mut files = 0;
    let mut bytes = 0;
    if let Ok(entries) = fs::read_dir(cache_dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().ends_with(".jsonl") {
                bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
                if fs::remove_file(entry.path()).is_ok() {
                    files += 1;
                }
            }
        }
    }
    (files, bytes)
}

pub fn default_cache_dir() -> PathBuf {
    let xdg = std::env::var("XDG_CACHE_HOME").ok();
    let home = std::env::var("HOME").ok();
    cache_dir_for(xdg.as_deref(), home.as_deref())
}

/// The cache dir for the given XDG_CACHE_HOME and HOME values. As the XDG
/// spec says, an XDG_CACHE_HOME that is empty or relative is ignored: taken,
/// it would put the cache, and what --clear-cache deletes, under whatever
/// directory optique runs from.
fn cache_dir_for(xdg: Option<&str>, home: Option<&str>) -> PathBuf {
    if let Some(x) = xdg.filter(|x| Path::new(x).is_absolute()) {
        return PathBuf::from(x).join("optique");
    }
    PathBuf::from(home.unwrap_or("/tmp")).join(".cache/optique")
}

/// A hex id (a digest for the file name, a commit for a note) cut to 12
/// digits; anything else, an mtime key in a note, whole: cut, mtimes hours
/// apart would read the same.
fn short(s: &str) -> String {
    if s.bytes().all(|b| b.is_ascii_hexdigit()) {
        s.chars().take(12).collect()
    } else {
        s.to_string()
    }
}

/// Cache format version: bumped whenever the wrapper emits new data or the
/// entry layout changes, so a newer binary never reuses entries a previous
/// one wrote with missing fields. Part of every generation file name
/// (`v<SCHEMA>-<tree>-<conf>.jsonl`); files from other schemas are reaped
/// by the prune pass since nothing can read them again.
const SCHEMA: u32 = 24;

/// Keep the current generation plus the most recent other one.
fn prune_old_generations(dir: &Path, keep: &str) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let prefix = format!("v{SCHEMA}-");
    let mut others: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".jsonl") || name == keep {
            continue;
        }
        if !name.starts_with(&prefix) {
            let _ = fs::remove_file(e.path()); // other schema: unreadable now
            continue;
        }
        if let Ok(t) = e.metadata().and_then(|m| m.modified()) {
            others.push((t, e.path()));
        }
    }
    others.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    for (_, path) in others.into_iter().skip(1) {
        let _ = fs::remove_file(path);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests;
