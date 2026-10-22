use std::collections::HashMap;
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
/// long as the port's options file content (in the PORT_DBDIR the query saw)
/// is unchanged — that content is part of the entry.
pub struct Cache {
    map: HashMap<PortKey, CacheEntry>,
    file: Option<File>,
    enabled: bool,
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    options_name: String,
    options_hash: String,
    info: PortInfo,
}

impl Cache {
    /// A cache that never hits and never stores (--no-cache).
    pub fn disabled() -> Self {
        Cache { map: HashMap::new(), file: None, enabled: false }
    }

    pub fn open(cache_dir: &Path, tree_key: &str, conf_hash: &str) -> Self {
        let generation = format!("v{SCHEMA}-{}-{}.jsonl", &short(tree_key), &short(conf_hash));
        let path = cache_dir.join(&generation);
        let _ = fs::create_dir_all(cache_dir);
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
        Cache { map, file, enabled: true }
    }

    /// Return the cached PortInfo if the options file it was computed against
    /// is still byte-identical.
    pub fn lookup(&self, key: &PortKey, files: &OptionsFiles) -> Option<PortInfo> {
        if !self.enabled {
            return None;
        }
        let entry = self.map.get(key)?;
        if files.hash(&entry.options_name) == entry.options_hash {
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
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs())
                .to_string()
        })
        .collect();
    format!("mtime-{}", mtimes.join("-"))
}

/// Identity of the ports tree and the overlays laid over it, in order: the
/// main tree's `tree_key` alone without overlays (generation names stay as
/// they were), else a digest of it and each overlay's path and identity.
pub fn trees_key(portsdir: &Path, overlays: &[PathBuf]) -> String {
    if overlays.is_empty() {
        return tree_key(portsdir);
    }
    let mut text = tree_key(portsdir);
    for o in overlays {
        text.push_str(&format!("\n{} {}", o.display(), overlay_key(o)));
    }
    sha256_hex(text.as_bytes())
}

/// An overlay's identity: its HEAD when it is a checkout of its own (a
/// whole tree, too big to walk), else the count and newest mtime of what it
/// holds down to `category/port/file` (and `Mk/Uses/*.mk`, `MOVED`): an
/// overlay is a handful of ports, often edited before any commit, and an
/// edit to a port's Makefile touches the file, not its directory.
fn overlay_key(overlay: &Path) -> String {
    if let Some(head) = git_head(overlay) {
        return head;
    }
    fn walk(dir: &Path, depth: usize, count: &mut u64, newest: &mut u128) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            if entry.file_name() == ".git" {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            *count += 1;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            *newest = (*newest).max(mtime);
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

/// Keep the current generation plus the most recent other one.
/// Cache format version: bumped whenever the wrapper emits new data or the
/// entry layout changes, so a newer binary never reuses entries a previous
/// one wrote with missing fields. Part of every generation file name
/// (`v<SCHEMA>-<tree>-<conf>.jsonl`); files from other schemas are reaped
/// by the prune pass since nothing can read them again.
const SCHEMA: u32 = 9;

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
        if let (Ok(meta), path) = (e.metadata(), e.path()) {
            if let Ok(t) = meta.modified() {
                others.push((t, path));
            }
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
    use crate::model::options::PortOptions;

    fn info(origin: &str) -> PortInfo {
        let key = PortKey::parse(origin).unwrap();
        PortInfo {
            key: key.clone(),
            canonical: key,
            pkgname: "x-1.0".into(),
            flavors: vec![],
            options_name: origin.replace('/', "_"),
            options: PortOptions::default(),
            deps: vec![],
            broken: None,
            ignore: None,
            deprecated: None,
            pkg_help: None,
            about: Default::default(),
            default_versions: vec![],
            warnings: vec![],
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
}
