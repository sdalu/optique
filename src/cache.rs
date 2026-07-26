use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::origin::PortKey;
use crate::model::port::PortInfo;

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
    pub fn lookup(&self, key: &PortKey, port_dbdir: &Path) -> Option<PortInfo> {
        if !self.enabled {
            return None;
        }
        let entry = self.map.get(key)?;
        if options_hash(port_dbdir, &entry.options_name) == entry.options_hash {
            Some(entry.info.clone())
        } else {
            None
        }
    }

    pub fn insert(&mut self, info: &PortInfo, port_dbdir: &Path) {
        if !self.enabled {
            return;
        }
        let entry = CacheEntry {
            options_name: info.options_name.clone(),
            options_hash: options_hash(port_dbdir, &info.options_name),
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

/// Hash of the saved options file content, or "ABSENT".
pub fn options_hash(port_dbdir: &Path, options_name: &str) -> String {
    match fs::read(port_dbdir.join(options_name).join("options")) {
        Ok(bytes) => hex(&Sha256::digest(&bytes)),
        Err(_) => "ABSENT".to_string(),
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Identity of the ports tree: the git HEAD commit, falling back to the
/// mtime of Mk/bsd.port.mk for non-git trees.
pub fn tree_key(portsdir: &Path) -> String {
    // HEAD only: a `git status` dirty-check would stat the whole tree
    // (~10s cold on /usr/ports). Uncommitted local edits to the tree are
    // invisible to the cache — use --no-cache when hacking on ports.
    if let Some(head) = git_head(portsdir) {
        return head;
    }
    let mtime = fs::metadata(portsdir.join("Mk/bsd.port.mk"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("mtime-{mtime}")
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
    if let Ok(x) = std::env::var("XDG_CACHE_HOME") {
        return PathBuf::from(x).join("optique");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".cache/optique")
}

fn short(s: &str) -> String {
    s.chars().take(12).collect()
}

/// Keep the current generation plus the most recent other one.
/// Cache format version: bumped whenever the wrapper emits new data or the
/// entry layout changes, so a newer binary never reuses entries a previous
/// one wrote with missing fields. Part of every generation file name
/// (`v<SCHEMA>-<tree>-<conf>.jsonl`); files from other schemas are reaped
/// by the prune pass since nothing can read them again.
const SCHEMA: u32 = 2;

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
            default_versions: vec![],
            warnings: vec![],
        }
    }

    #[test]
    fn roundtrip_and_options_file_invalidation() {
        let tmp = tempfile::tempdir().unwrap();
        let dbdir = tmp.path().join("db");
        std::fs::create_dir_all(dbdir.join("cat_port")).unwrap();
        std::fs::write(dbdir.join("cat_port/options"), "v1\n").unwrap();

        let mut c = Cache::open(&tmp.path().join("cache"), "tree1", "conf1");
        let i = info("cat/port");
        assert!(c.lookup(&i.key, &dbdir).is_none());
        c.insert(&i, &dbdir);
        assert!(c.lookup(&i.key, &dbdir).is_some());

        // Changing the options file invalidates the entry.
        std::fs::write(dbdir.join("cat_port/options"), "v2\n").unwrap();
        assert!(c.lookup(&i.key, &dbdir).is_none());
        // Restoring the exact content revalidates it.
        std::fs::write(dbdir.join("cat_port/options"), "v1\n").unwrap();
        assert!(c.lookup(&i.key, &dbdir).is_some());
    }

    #[test]
    fn persists_across_reopen_same_generation_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dbdir = tmp.path().join("db");
        std::fs::create_dir_all(&dbdir).unwrap();
        let cache_dir = tmp.path().join("cache");

        let mut c = Cache::open(&cache_dir, "treeA", "confA");
        c.insert(&info("cat/port"), &dbdir);
        drop(c);

        let c2 = Cache::open(&cache_dir, "treeA", "confA");
        assert!(c2.lookup(&PortKey::parse("cat/port").unwrap(), &dbdir).is_some());
        let c3 = Cache::open(&cache_dir, "treeB", "confA");
        assert!(c3.lookup(&PortKey::parse("cat/port").unwrap(), &dbdir).is_none());
    }

    #[test]
    fn disabled_never_stores() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = Cache::disabled();
        c.insert(&info("cat/port"), tmp.path());
        assert!(c.lookup(&PortKey::parse("cat/port").unwrap(), tmp.path()).is_none());
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
        assert_eq!(names, vec![format!("v{SCHEMA}-current-gen.jsonl")], "{names:?}");
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
        let out = std::process::Command::new("git")
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

    #[test]
    fn options_hash_absent_vs_present() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(options_hash(tmp.path(), "cat_port"), "ABSENT");
        std::fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
        std::fs::write(tmp.path().join("cat_port/options"), "x\n").unwrap();
        let h = options_hash(tmp.path(), "cat_port");
        assert_ne!(h, "ABSENT");
        assert_eq!(h.len(), 64);
    }
}
