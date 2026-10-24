use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::origin::PortKey;
use crate::model::port::PortInfo;
use crate::optfiles::OptionsFiles;

/// Persistent per-generation query cache.
///
/// A generation is one (ports tree state, layered make.conf) pair; any change
/// to either starts a fresh file. Within a generation, an entry is valid as
/// long as the port's options file and `options.local` (in the PORT_DBDIR
/// the query saw) are unchanged — their content is part of the entry. A new
/// generation of a git tree starts with the entries of the one before it
/// that the commits between them leave alone ([`Invalidation`]).
pub struct Cache {
    map: HashMap<PortKey, CacheEntry>,
    file: Option<File>,
    enabled: bool,
    /// What opening carried over from an earlier generation, for the banner.
    note: Option<String>,
}

/// A generation file's first line: what it was made for, which is what a
/// later generation needs to know to carry its entries over.
#[derive(Serialize, Deserialize)]
struct Header {
    generation: Generation,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
struct Generation {
    /// The main tree's git commit; None for a tree without one.
    commit: Option<String>,
    /// The overlays' part of the tree key (see [`Trees`]).
    overlays: String,
    conf: String,
}

/// The trees a generation is opened for: its key, and what carrying entries
/// over from an earlier revision of the main tree needs.
pub struct Trees {
    /// The tree part of the generation's name ([`trees_key`]).
    pub key: String,
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
    info: PortInfo,
}

impl Cache {
    /// A cache that never hits and never stores (--no-cache).
    pub fn disabled() -> Self {
        Cache { map: HashMap::new(), file: None, enabled: false, note: None }
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
    pub fn open_trees(cache_dir: &Path, trees: &Trees, conf_hash: &str) -> Self {
        let generation = format!("v{SCHEMA}-{}-{}.jsonl", short(&trees.key), short(conf_hash));
        let path = cache_dir.join(&generation);
        let _ = fs::create_dir_all(cache_dir);
        let here = Generation {
            commit: trees.commit.clone(),
            overlays: trees.overlays.clone(),
            conf: conf_hash.to_string(),
        };
        let mut note = None;
        let fresh = fs::metadata(&path).map_or(true, |m| m.len() == 0);
        if fresh {
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
                let mut text = serde_json::to_string(&Header { generation: here.clone() })
                    .map(|h| h + "\n")
                    .unwrap_or_default();
                if let Some(carried) = carry_over(cache_dir, &generation, &here, &trees.portsdir) {
                    for line in &carried.lines {
                        text.push_str(line);
                        text.push('\n');
                    }
                    note = Some(carried.note);
                }
                let _ = f.write_all(text.as_bytes());
            }
        }
        prune_old_generations(cache_dir, &generation);

        let mut map = HashMap::new();
        if let Ok(text) = fs::read_to_string(&path) {
            for line in text.lines() {
                if let Ok(entry) = serde_json::from_str::<CacheEntry>(line) {
                    map.insert(entry.info.key.clone(), entry);
                }
            }
        }
        let file = OpenOptions::new().create(true).append(true).open(&path).ok();
        Cache { map, file, enabled: true, note }
    }

    /// What was carried over from an earlier generation, if anything was
    /// looked for: a line for the startup banner.
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Return the cached PortInfo if the options file and `options.local` it
    /// was computed against are still byte-identical.
    pub fn lookup(&self, key: &PortKey, files: &OptionsFiles) -> Option<PortInfo> {
        if !self.enabled {
            return None;
        }
        let entry = self.map.get(key)?;
        if files.hash(&entry.options_name) == entry.options_hash
            && files.local_hash(&entry.options_name) == entry.local_hash
        {
            Some(entry.info.clone())
        } else {
            None
        }
    }

    pub fn insert(&mut self, info: &PortInfo, files: &OptionsFiles) {
        if !self.enabled {
            return;
        }
        let entry = CacheEntry {
            options_name: info.options_name.clone(),
            options_hash: files.hash(&info.options_name),
            local_hash: files.local_hash(&info.options_name),
            info: info.clone(),
        };
        if let Some(f) = &mut self.file {
            if let Ok(line) = serde_json::to_string(&entry) {
                let _ = writeln!(f, "{line}");
            }
        }
        self.map.insert(entry.info.key.clone(), entry);
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
            fs::metadata(portsdir.join(file)).map_or(0, |m| mtime(&m).as_secs()).to_string()
        })
        .collect();
    format!("mtime-{}", mtimes.join("-"))
}

/// Identity of the ports tree and the overlays laid over it, in order: the
/// main tree's `tree_key` alone without overlays (generation names stay as
/// they were), else a digest of it and each overlay's path and identity.
#[cfg(test)]
pub fn trees_key(portsdir: &Path, overlays: &[PathBuf]) -> String {
    trees(portsdir, overlays).key
}

/// The trees' identity ([`trees_key`]) with its parts kept apart.
pub fn trees(portsdir: &Path, overlays: &[PathBuf]) -> Trees {
    let main = tree_key(portsdir);
    let overlays: String =
        overlays.iter().map(|o| format!("\n{} {}", o.display(), overlay_key(o))).collect();
    let key = if overlays.is_empty() {
        main.clone()
    } else {
        sha256_hex(format!("{main}{overlays}").as_bytes())
    };
    Trees { key, portsdir: portsdir.to_path_buf(), commit: object_id(&main), overlays }
}

/// What [`carry_over`] found for a new generation.
struct Carried {
    /// The kept entries' lines, as the earlier generation holds them.
    lines: Vec<String>,
    note: String,
}

/// The entries of the latest earlier generation that the commits since
/// leave alone, for a new generation `here` (`name`): one made for the same
/// make.conf and overlays, and another commit of the same git tree. None
/// when there is no such generation or git cannot list the changes.
fn carry_over(cache_dir: &Path, name: &str, here: &Generation, portsdir: &Path) -> Option<Carried> {
    let new = here.commit.as_deref()?;
    let prefix = format!("v{SCHEMA}-");
    let mut others: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(cache_dir)
        .ok()?
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(&prefix) && n.ends_with(".jsonl") && n != name
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    others.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    let (text, old) = others.into_iter().find_map(|(_, path)| {
        let text = fs::read_to_string(path).ok()?;
        let header: Header = serde_json::from_str(text.lines().next()?).ok()?;
        let g = header.generation;
        let same = g.conf == here.conf && g.overlays == here.overlays;
        let old = g.commit.filter(|old| same && old != new)?;
        Some((text, old))
    })?;
    let mut changes = git_changes(portsdir, &old, new)?;
    let changed = changes.len();
    changes.extend(parse_time_readers(portsdir, &changes).into_iter().map(|mk| ('M', mk)));
    let invalidation = Invalidation::of(&changes);
    let from = short(&old);
    if let Some(path) = &invalidation.all {
        return Some(Carried {
            lines: Vec::new(),
            note: format!("cache: nothing carried over from tree {from}: {path} changed since"),
        });
    }
    let (mut lines, mut dropped) = (Vec::new(), 0);
    for line in text.lines().skip(1) {
        match serde_json::from_str::<CacheEntry>(line) {
            Ok(entry) if invalidation.keeps(&entry.info) => lines.push(line.to_string()),
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
        .args(["diff-tree", "-r", "-z", "--no-renames", "--name-status", old, new])
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

/// Every `*.mk` under `dir`, relative to it, with its text.
fn mk_files(dir: &Path) -> Vec<(String, String)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else if path.extension().is_some_and(|e| e == "mk") {
                if let (Ok(rel), Ok(text)) = (path.strip_prefix(base), fs::read(&path)) {
                    let text = String::from_utf8_lossy(&text).into_owned();
                    out.push((rel.to_string_lossy().into_owned(), text));
                }
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
    if let Some(at) = line.find("!=") {
        if assignment(line).is_some_and(|(name, _)| line[..at].trim() == name) {
            out.push(line[at + 2..].to_string());
        }
    }
    let mut rest = line;
    while let Some(at) = rest.find(":!") {
        let tail = &rest[at + 2..];
        let end = tail.find("!}").or_else(|| tail.find("!:")).unwrap_or(tail.len());
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
/// `Makefile.inc`, probed too) and for a new `Mk/*.mk` (`bsd.local.mk` is
/// `.sinclude`d). A `Mk/` script run while make parses counts as its
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
                ["Mk", file] => *status == 'A' && file.ends_with(".mk"),
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
            let owned = file.strip_suffix("/Makefile").filter(|d| d.matches('/').count() == 1);
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
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            if entry.file_name() == ".git" {
                continue;
            }
            // Through a symlink, as the lookup goes: a symlinked port is
            // queried from its target, so its target's files are its key.
            // The depth limit stops a symlink loop.
            let Ok(meta) = fs::metadata(entry.path()) else { continue };
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
        if let Some(id) = fs::read_to_string(dir.join(refname)).ok().and_then(|t| object_id(t.trim()))
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
    let path = text.lines().find_map(|l| l.trim().strip_prefix("gitdir:"))?.trim();
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
        let Some((id, name)) = line.split_once(' ') else { continue };
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

/// A hex id (commit, hash) cut to 12 digits for the file name; anything
/// else, an mtime key, whole: cut, mtimes hours apart would share a name.
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
const SCHEMA: u32 = 17;

/// Keep the current generation plus the most recent other one.
fn prune_old_generations(dir: &Path, keep: &str) {
    let Ok(entries) = fs::read_dir(dir) else { return };
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
mod tests {
    use super::*;

    fn info(origin: &str) -> PortInfo {
        PortInfo {
            pkgname: "x-1.0".into(),
            ..PortInfo::stub(origin)
        }
    }

    /// A lookup and an insert for the same file, and two flavors sharing it,
    /// cost one read between them.
    #[test]
    fn lookups_and_inserts_share_one_read_per_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dbdir = tmp.path().join("db");
        std::fs::create_dir_all(dbdir.join("cat_port")).unwrap();
        std::fs::write(dbdir.join("cat_port/options"), "v1\n").unwrap();
        let files = OptionsFiles::new(&dbdir);

        let mut c = Cache::open(&tmp.path().join("cache"), "tree1", "conf1");
        let plain = info("cat/port");
        let mut flavored = info("cat/port");
        flavored.key = PortKey::parse("cat/port@f").unwrap();
        // Not in the cache yet: no file is read to find that out.
        assert!(c.lookup(&plain.key, &files).is_none());
        assert_eq!(files.disk_reads(), 0);
        c.insert(&plain, &files);
        c.insert(&flavored, &files);
        assert!(c.lookup(&plain.key, &files).is_some());
        assert!(c.lookup(&flavored.key, &files).is_some());
        assert_eq!(files.disk_reads(), 1);
    }

    #[test]
    fn roundtrip_and_options_file_invalidation() {
        let tmp = tempfile::tempdir().unwrap();
        let dbdir = tmp.path().join("db");
        std::fs::create_dir_all(dbdir.join("cat_port")).unwrap();
        std::fs::write(dbdir.join("cat_port/options"), "v1\n").unwrap();

        let mut c = Cache::open(&tmp.path().join("cache"), "tree1", "conf1");
        let i = info("cat/port");
        assert!(c.lookup(&i.key, &OptionsFiles::new(&dbdir)).is_none());
        c.insert(&i, &OptionsFiles::new(&dbdir));
        assert!(c.lookup(&i.key, &OptionsFiles::new(&dbdir)).is_some());

        // Changing the options file invalidates the entry.
        std::fs::write(dbdir.join("cat_port/options"), "v2\n").unwrap();
        assert!(c.lookup(&i.key, &OptionsFiles::new(&dbdir)).is_none());
        // Restoring the exact content revalidates it.
        std::fs::write(dbdir.join("cat_port/options"), "v1\n").unwrap();
        assert!(c.lookup(&i.key, &OptionsFiles::new(&dbdir)).is_some());
    }

    #[test]
    fn persists_across_reopen_same_generation_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dbdir = tmp.path().join("db");
        std::fs::create_dir_all(&dbdir).unwrap();
        let cache_dir = tmp.path().join("cache");

        let mut c = Cache::open(&cache_dir, "treeA", "confA");
        c.insert(&info("cat/port"), &OptionsFiles::new(&dbdir));
        drop(c);

        let c2 = Cache::open(&cache_dir, "treeA", "confA");
        assert!(c2.lookup(&PortKey::parse("cat/port").unwrap(), &OptionsFiles::new(&dbdir)).is_some());
        let c3 = Cache::open(&cache_dir, "treeB", "confA");
        assert!(c3.lookup(&PortKey::parse("cat/port").unwrap(), &OptionsFiles::new(&dbdir)).is_none());
    }

    #[test]
    fn disabled_never_stores() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = Cache::disabled();
        c.insert(&info("cat/port"), &OptionsFiles::new(tmp.path()));
        assert!(c.lookup(&PortKey::parse("cat/port").unwrap(), &OptionsFiles::new(tmp.path())).is_none());
    }

    #[test]
    fn prune_keeps_at_most_two_generations() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        for n in [
            format!("v{SCHEMA}-old1-a.jsonl"),
            format!("v{SCHEMA}-old2-b.jsonl"),
            format!("v{SCHEMA}-old3-c.jsonl"),
        ] {
            std::fs::write(cache_dir.join(n), "\n").unwrap();
        }
        let _c = Cache::open(&cache_dir, "current", "gen");
        let count = std::fs::read_dir(&cache_dir).unwrap().count();
        assert!(count <= 2, "expected current + 1 old generation, got {count}");
    }

    #[test]
    fn prune_reaps_other_schema_generations() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        // Not a cache generation at all (a draft, say): never touched.
        std::fs::write(cache_dir.join("draft.json"), "{}").unwrap();
        // Pre-versioning and other-schema files are unreadable: always removed.
        std::fs::write(cache_dir.join("aaaa-bbbb.jsonl"), "\n").unwrap();
        std::fs::write(cache_dir.join("v1-cccc-dddd.jsonl"), "\n").unwrap();
        std::fs::write(cache_dir.join("v999-eeee-ffff.jsonl"), "\n").unwrap();
        let _c = Cache::open(&cache_dir, "current", "gen");
        let names: Vec<String> = std::fs::read_dir(&cache_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        let mut names = names;
        names.sort();
        assert_eq!(names, vec!["draft.json".to_string(), format!("v{SCHEMA}-current-gen.jsonl")]);
    }

    /// An entry made while the options file was missing is not reused once
    /// the file is there but unreadable: make fails on such a file, it does
    /// not read it as missing.
    #[test]
    fn an_absent_entry_does_not_answer_for_an_unreadable_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dbdir = tmp.path().join("db");
        std::fs::create_dir_all(&dbdir).unwrap();
        let mut c = Cache::open(&tmp.path().join("cache"), "tree1", "conf1");
        let i = info("cat/port");
        c.insert(&i, &OptionsFiles::new(&dbdir));
        assert!(c.lookup(&i.key, &OptionsFiles::new(&dbdir)).is_some());
        // A directory where the file should be: there, and unreadable.
        std::fs::create_dir_all(dbdir.join("cat_port/options")).unwrap();
        assert!(c.lookup(&i.key, &OptionsFiles::new(&dbdir)).is_none());
    }

    /// `clear` removes every generation, counts files and bytes, and leaves
    /// whatever is not one.
    #[test]
    fn clear_counts_what_it_removes() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("v7-a-b.jsonl"), "12345").unwrap();
        std::fs::write(tmp.path().join("v6-c-d.jsonl"), "123").unwrap();
        std::fs::write(tmp.path().join("draft.json"), "{}").unwrap();
        assert_eq!(clear(tmp.path()), (2, 8));
        assert!(tmp.path().join("draft.json").is_file());
        assert_eq!(clear(tmp.path()), (0, 0));
    }

    /// Only a sha1 or sha256 hex id names a tree: anything else would make a
    /// stray file a cache generation.
    #[test]
    fn object_id_wants_hex_of_a_hash_length() {
        let sha1 = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(object_id(&format!("  {sha1}\n")).as_deref(), Some(sha1));
        assert!(object_id(&"a".repeat(64)).is_some());
        assert!(object_id(&"z".repeat(40)).is_none(), "not hex");
        assert!(object_id("abc123").is_none(), "hex, wrong length");
        assert!(object_id(&"a".repeat(41)).is_none());
    }

    /// A ports tree whose .git is a plain directory, with the given files
    /// written inside it. Returns (tempdir, portsdir).
    fn fake_tree(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        std::fs::create_dir_all(ports.join("Mk")).unwrap();
        std::fs::write(ports.join("Mk/bsd.port.mk"), "# fake\n").unwrap();
        for (rel, content) in files {
            let path = ports.join(".git").join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        (tmp, ports)
    }

    const ID: &str = "f377481e728bedfe7005118ec50f9f145ebfffac";
    const OTHER: &str = "0123456789abcdef0123456789abcdef01234567";

    /// Without overlays the key is the tree's own, generation names as they
    /// were; with one it follows the overlay, whose port edits — usually
    /// uncommitted — must reach it.
    #[test]
    fn trees_key_follows_every_overlay() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, over) = (tmp.path().join("main"), tmp.path().join("over"));
        std::fs::create_dir_all(main.join("Mk")).unwrap();
        std::fs::write(main.join("Mk/bsd.port.mk"), "").unwrap();
        std::fs::create_dir_all(over.join("cat/port")).unwrap();
        let makefile = over.join("cat/port/Makefile");
        std::fs::write(&makefile, "PORTVERSION=1\n").unwrap();
        assert_eq!(trees_key(&main, &[]), tree_key(&main));
        let before = trees_key(&main, std::slice::from_ref(&over));
        assert_ne!(before, tree_key(&main));
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        File::options().write(true).open(&makefile).unwrap().set_modified(later).unwrap();
        assert_ne!(trees_key(&main, std::slice::from_ref(&over)), before, "an edited port is a new generation");
    }

    /// An overlay that is a git checkout of its own is still edited in
    /// place before any commit: its key follows the files, not the HEAD.
    #[test]
    fn trees_key_follows_an_uncommitted_edit_in_a_git_overlay() {
        let (_tmp, over) = fake_tree(&[("HEAD", "ref: refs/heads/main\n"), ("refs/heads/main", &format!("{ID}\n"))]);
        let (_tmp2, main) = fake_tree(&[]);
        std::fs::create_dir_all(over.join("cat/port")).unwrap();
        let makefile = over.join("cat/port/Makefile");
        std::fs::write(&makefile, "PORTVERSION=1\n").unwrap();
        let before = trees_key(&main, std::slice::from_ref(&over));
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        File::options().write(true).open(&makefile).unwrap().set_modified(later).unwrap();
        assert_ne!(trees_key(&main, std::slice::from_ref(&over)), before, "same HEAD, edited port");
    }

    /// An overlay assembled from symlinked ports: the port is looked up
    /// through the link, so an edit behind it must reach the key too.
    #[test]
    fn trees_key_follows_an_edit_behind_a_symlinked_port() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, over, real) = (tmp.path().join("main"), tmp.path().join("over"), tmp.path().join("real"));
        std::fs::create_dir_all(&main).unwrap();
        std::fs::create_dir_all(over.join("cat")).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        let makefile = real.join("Makefile");
        std::fs::write(&makefile, "PORTVERSION=1\n").unwrap();
        std::os::unix::fs::symlink(&real, over.join("cat/port")).unwrap();
        let before = trees_key(&main, std::slice::from_ref(&over));
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        File::options().write(true).open(&makefile).unwrap().set_modified(later).unwrap();
        assert_ne!(trees_key(&main, std::slice::from_ref(&over)), before, "edited behind the link");
    }

    #[test]
    fn tree_key_reads_a_loose_ref() {
        let (_tmp, ports) = fake_tree(&[
            ("HEAD", "ref: refs/heads/main\n"),
            ("refs/heads/main", &format!("{ID}\n")),
        ]);
        assert_eq!(tree_key(&ports), ID);
    }

    #[test]
    fn tree_key_reads_packed_refs_when_the_loose_ref_is_missing() {
        // Comment headers and the '^' peel line of a packed tag must not be
        // mistaken for refs.
        let packed = format!(
            "# pack-refs with: peeled fully-peeled sorted \n\
             {OTHER} refs/heads/other\n\
             {ID} refs/heads/main\n\
             {OTHER} refs/tags/v1\n\
             ^{ID}\n"
        );
        let (_tmp, ports) =
            fake_tree(&[("HEAD", "ref: refs/heads/main\n"), ("packed-refs", &packed)]);
        assert_eq!(tree_key(&ports), ID);
    }

    #[test]
    fn tree_key_reads_a_detached_head() {
        let (_tmp, ports) = fake_tree(&[("HEAD", &format!("{ID}\n"))]);
        assert_eq!(tree_key(&ports), ID);
    }

    #[test]
    fn tree_key_follows_a_gitdir_file() {
        // Worktree/submodule layout: .git is a file naming the real gitdir,
        // here relative to the ports tree.
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        std::fs::create_dir_all(ports.join("Mk")).unwrap();
        std::fs::write(ports.join("Mk/bsd.port.mk"), "# fake\n").unwrap();
        let real = ports.join("../real-gitdir");
        std::fs::create_dir_all(real.join("refs/heads")).unwrap();
        std::fs::write(real.join("HEAD"), "ref: refs/heads/mine\n").unwrap();
        std::fs::write(real.join("refs/heads/mine"), format!("{ID}\n")).unwrap();
        std::fs::write(ports.join(".git"), "gitdir: ../real-gitdir\n").unwrap();
        assert_eq!(tree_key(&ports), ID);

        // Absolute form too.
        std::fs::write(
            ports.join(".git"),
            format!("gitdir: {}\n", real.canonicalize().unwrap().display()),
        )
        .unwrap();
        assert_eq!(tree_key(&ports), ID);
    }

    #[test]
    fn tree_key_falls_back_to_mtime_on_garbage() {
        // Unparsable HEAD, ref pointing nowhere, non-hex content, and a
        // gitdir file naming a missing directory: all must degrade quietly.
        for files in [
            vec![("HEAD", "not a ref at all\n")],
            vec![("HEAD", "ref: refs/heads/main\n")], // no such ref anywhere
            vec![("HEAD", "ref: ../../etc/passwd\n")],
            vec![("HEAD", "ref: refs/heads/main\n"), ("refs/heads/main", "zzz\n")],
        ] {
            let (_tmp, ports) = fake_tree(&files);
            let key = tree_key(&ports);
            assert!(key.starts_with("mtime-"), "{files:?} gave {key}");
        }

        let (_tmp, ports) = fake_tree(&[]);
        std::fs::write(ports.join(".git"), "gitdir: /nonexistent/gitdir\n").unwrap();
        assert!(tree_key(&ports).starts_with("mtime-"));
    }

    /// The reader must agree with git itself on the host's real ports tree.
    #[test]
    #[ignore]
    fn tree_key_matches_git_cli() {
        let ports = Path::new("/usr/ports");
        // /usr/ports is usually root's and the suite runs as someone else,
        // which git refuses as "dubious ownership"; tree_key reads .git
        // itself and has no such check, so lift it for this one read.
        let out = std::process::Command::new("git")
            .arg("-c")
            .arg(format!("safe.directory={}", ports.display()))
            .arg("-C")
            .arg(ports)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("git must be installed to run this test");
        assert!(out.status.success(), "/usr/ports is not a git checkout");
        let expected = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(tree_key(ports), expected);
    }

    #[test]
    fn tree_key_falls_back_to_mtime_without_git() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("Mk")).unwrap();
        std::fs::write(tmp.path().join("Mk/bsd.port.mk"), "# fake\n").unwrap();
        let key = tree_key(tmp.path());
        assert!(key.starts_with("mtime-"), "got {key}");
    }

    /// Without git, a tree key is mtimes: two of them an hour apart are two
    /// trees, never one generation.
    #[test]
    fn mtime_keys_one_hour_apart_are_distinct_generations() {
        let tmp = tempfile::tempdir().unwrap();
        let dbdir = tmp.path().join("db");
        std::fs::create_dir_all(&dbdir).unwrap();
        let cache_dir = tmp.path().join("cache");
        let mut c = Cache::open(&cache_dir, "mtime-1792652400", "conf");
        c.insert(&info("cat/port"), &OptionsFiles::new(&dbdir));
        drop(c);
        let later = Cache::open(&cache_dir, "mtime-1792656000", "conf");
        assert!(later.lookup(&PortKey::parse("cat/port").unwrap(), &OptionsFiles::new(&dbdir)).is_none());
    }

    /// Without git, an update that touches MOVED (or UPDATING) but not
    /// Mk/bsd.port.mk is still a new tree.
    #[test]
    fn a_moved_only_change_is_a_new_tree_key() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path();
        std::fs::create_dir_all(ports.join("Mk")).unwrap();
        std::fs::write(ports.join("Mk/bsd.port.mk"), "# fake\n").unwrap();
        std::fs::write(ports.join("MOVED"), "").unwrap();
        let set_mtime = |secs: u64| {
            let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
            File::options().write(true).open(ports.join("MOVED")).unwrap().set_modified(t).unwrap();
        };
        set_mtime(1_792_652_400);
        let before = tree_key(ports);
        set_mtime(1_792_656_000);
        assert_ne!(tree_key(ports), before);
    }

    /// The XDG spec: an empty or relative XDG_CACHE_HOME is ignored, so the
    /// cache (and --clear-cache) never lands under the current directory.
    #[test]
    fn default_cache_dir_ignores_empty_and_relative_xdg() {
        let home = Some("/home/u");
        assert_eq!(cache_dir_for(Some("/x/cache"), home), Path::new("/x/cache/optique"));
        for xdg in [None, Some(""), Some("cache"), Some("./cache")] {
            assert_eq!(cache_dir_for(xdg, home), Path::new("/home/u/.cache/optique"), "{xdg:?}");
        }
        assert_eq!(cache_dir_for(None, None), Path::new("/tmp/.cache/optique"));
    }

    #[test]
    fn options_hash_absent_vs_present() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(OptionsFiles::new(tmp.path()).hash("cat_port"), "ABSENT");
        std::fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
        std::fs::write(tmp.path().join("cat_port/options"), "x\n").unwrap();
        let h = OptionsFiles::new(tmp.path()).hash("cat_port");
        assert_ne!(h, "ABSENT");
        assert_eq!(h.len(), 64);
    }

    /// A port whose make read `makefiles`.
    fn read(origin: &str, makefiles: &[&str]) -> PortInfo {
        PortInfo { makefiles: makefiles.iter().map(|f| f.to_string()).collect(), ..info(origin) }
    }

    /// Each kind of change against a slave port that also reads another
    /// port's file: what it reads, the directories it owns and their
    /// category take it; what make never reads leaves it; a few changes
    /// take everything.
    #[test]
    fn invalidation_follows_what_make_read_and_what_it_probes() {
        let slave = read(
            "cat/slave",
            &[
                "Mk/bsd.port.mk",
                "Mk/Uses/python.mk",
                "cat/master/Makefile",
                "cat/slave/Makefile",
                "lang/python312/Makefile.version",
            ],
        );
        let keeps =
            |status: char, path: &str| Invalidation::of(&[(status, path.to_string())]).keeps(&slave);
        for (status, path) in [
            ('M', "MOVED"),
            ('M', "UIDs"),
            ('M', "Makefile"),
            ('M', ".mailmap"),
            ('A', ".github/workflows/x"),
            ('M', "Tools/scripts/x.sh"),
            ('M', "Keywords/sample.ucl"),
            ('M', "Mk/Scripts/qa.sh"),
            ('A', "Mk/Uses/new.mk"),
            ('M', "Mk/Uses/qt.mk"),
            ('M', "cat/Makefile"),
            ('M', "lang/python312/pkg-plist"),
            ('M', "other/port/Makefile"),
        ] {
            assert!(keeps(status, path), "{status} {path} must leave the entry");
        }
        for (status, path) in [
            ('M', "Mk/bsd.port.mk"),
            ('M', "Mk/Uses/python.mk"),
            ('M', "lang/python312/Makefile.version"),
            ('M', "cat/slave/Makefile"),
            ('A', "cat/master/pkg-help"),
            ('A', "cat/master/Makefile.local"),
            ('A', "cat/slave/files/patch-x"),
            ('A', "cat/Makefile.inc"),
        ] {
            assert!(!keeps(status, path), "{status} {path} must take the entry");
        }
        for (status, path) in [('A', "Mk/bsd.local.mk"), ('A', "Makefile.inc"), ('M', "NEWFILE")] {
            let inv = Invalidation::of(&[(status, path.to_string())]);
            assert_eq!(inv.all.as_deref(), Some(path), "{status} {path} takes everything");
        }
        // An entry that knows nothing of what make read cannot be judged.
        assert!(!Invalidation::of(&[]).keeps(&read("cat/port", &[])));
    }

    /// git, quietly, in `dir`; None when git cannot be run here.
    fn git(dir: &Path, args: &[&str]) -> Option<()> {
        let ok = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-C"])
            .arg(dir)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?
            .success();
        ok.then_some(())
    }

    /// Across a commit, a new generation starts with the entries the commit
    /// leaves alone and says so; a generation for another make.conf
    /// carries nothing over.
    #[test]
    fn a_new_commit_carries_over_what_its_changes_leave_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = tmp.path().join("ports");
        for dir in ["Mk", "cat/a", "cat/b"] {
            std::fs::create_dir_all(ports.join(dir)).unwrap();
        }
        std::fs::write(ports.join("Mk/bsd.port.mk"), "# v1\n").unwrap();
        std::fs::write(ports.join("cat/a/Makefile"), "# a1\n").unwrap();
        std::fs::write(ports.join("cat/b/Makefile"), "# b1\n").unwrap();
        if git(&ports, &["init", "-q"]).and_then(|()| git(&ports, &["add", "-A"])).is_none()
            || git(&ports, &["commit", "-q", "-m", "one"]).is_none()
        {
            eprintln!("git cannot run here; skipped");
            return;
        }
        let (dbdir, cache_dir) = (tmp.path().join("db"), tmp.path().join("cache"));
        let files = OptionsFiles::new(&dbdir);
        let mut c = Cache::open_trees(&cache_dir, &trees(&ports, &[]), "conf");
        assert_eq!(c.note(), None, "nothing to carry over from");
        c.insert(&read("cat/a", &["Mk/bsd.port.mk", "cat/a/Makefile"]), &files);
        c.insert(&read("cat/b", &["Mk/bsd.port.mk", "cat/b/Makefile"]), &files);
        drop(c);

        std::fs::write(ports.join("cat/a/pkg-help"), "help\n").unwrap();
        git(&ports, &["add", "-A"]).unwrap();
        git(&ports, &["commit", "-q", "-m", "two"]).unwrap();
        let c = Cache::open_trees(&cache_dir, &trees(&ports, &[]), "conf");
        let note = c.note().unwrap();
        assert!(note.contains("1 entries carried over"), "{note}");
        assert!(note.contains("1 dropped by 1 changed paths"), "{note}");
        assert!(c.lookup(&PortKey::parse("cat/a").unwrap(), &files).is_none());
        assert!(c.lookup(&PortKey::parse("cat/b").unwrap(), &files).is_some());
        drop(c);

        let c = Cache::open_trees(&cache_dir, &trees(&ports, &[]), "other conf");
        assert_eq!(c.note(), None);
        assert!(c.lookup(&PortKey::parse("cat/b").unwrap(), &files).is_none());
    }

    /// A script a parse-time command runs, directly, through a variable,
    /// or through `:sh`, counts as a change to the makefile running it; one
    /// a target's recipe runs, or a variable no such command uses, does not.
    #[test]
    fn a_script_run_while_parsing_counts_as_its_makefile() {
        let tmp = tempfile::tempdir().unwrap();
        let mk = tmp.path().join("Mk");
        std::fs::create_dir_all(mk.join("Uses")).unwrap();
        std::fs::write(
            mk.join("Uses/rusty.mk"),
            "_AWK=\t${AWK} \\\n\t\t-f${SCRIPTSDIR}/common.awk -f\n\
             .for _a _b in ${:!${_AWK} ${SCRIPTSDIR}/fetch.awk /dev/null!}\n.endfor\n",
        )
        .unwrap();
        let shy = "_V=\t${SH} ${SCRIPTSDIR}/version.sh\nV=\t${_V:sh}\n";
        std::fs::write(mk.join("Uses/shy.mk"), shy).unwrap();
        std::fs::write(mk.join("Uses/now.mk"), "X!=\t${SH} ${SCRIPTSDIR}/now.sh\n").unwrap();
        std::fs::write(
            mk.join("bsd.port.mk"),
            "DO_FETCH=\t${SH} ${SCRIPTSDIR}/do-fetch.sh\n\
             do-fetch:\n\t${SCRIPTSDIR}/qa.sh\n\t${DO_FETCH}\n",
        )
        .unwrap();
        let readers =
            |script: &str| parse_time_readers(tmp.path(), &[('M', format!("Mk/Scripts/{script}"))]);
        assert_eq!(readers("common.awk"), ["Mk/Uses/rusty.mk"]);
        assert_eq!(readers("fetch.awk"), ["Mk/Uses/rusty.mk"]);
        assert_eq!(readers("version.sh"), ["Mk/Uses/shy.mk"]);
        assert_eq!(readers("now.sh"), ["Mk/Uses/now.mk"]);
        assert!(readers("qa.sh").is_empty(), "a recipe line");
        assert!(readers("do-fetch.sh").is_empty(), "assigned, run only by a recipe");
        // A makefile change alone reads nothing.
        assert!(parse_time_readers(tmp.path(), &[('M', "Mk/Uses/rusty.mk".into())]).is_empty());
    }

    /// On the host's tree, cargo's awk scripts are the ones run while
    /// parsing (their crates add build dependencies), and the scripts the
    /// targets run are not.
    #[test]
    #[ignore = "needs /usr/ports on a FreeBSD host"]
    fn the_trees_parse_time_scripts_are_cargos() {
        let ports = Path::new("/usr/ports");
        let readers =
            |script: &str| parse_time_readers(ports, &[('M', format!("Mk/Scripts/{script}"))]);
        let cargo = ["cargo-crates-git-fetch.awk", "cargo-crates-git-common.awk", "split-url.awk"];
        for script in cargo {
            assert_eq!(readers(script), ["Mk/Uses/cargo.mk"], "{script}");
        }
        for script in ["qa.sh", "do-fetch.sh", "do-depends.sh", "checksum.sh", "makesum.sh"] {
            assert!(readers(script).is_empty(), "{script}");
        }
    }
}
