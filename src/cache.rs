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
        // SCHEMA bumps whenever the wrapper emits new data, so entries
        // queried by an older binary don't linger with missing fields.
        const SCHEMA: u32 = 2;
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

/// Identity of the ports tree: git HEAD + dirty marker, falling back to the
/// mtime of Mk/bsd.port.mk for non-git trees.
pub fn tree_key(portsdir: &Path) -> String {
    let git = |args: &[&str]| -> Option<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(portsdir)
            .args(args)
            .output()
            .ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    // HEAD only: a `git status` dirty-check would stat the whole tree
    // (~10s cold on /usr/ports). Uncommitted local edits to the tree are
    // invisible to the cache — use --no-cache when hacking on ports.
    if let Some(head) = git(&["rev-parse", "HEAD"]) {
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
fn prune_old_generations(dir: &Path, keep: &str) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut others: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| {
            e.file_name().to_string_lossy().ends_with(".jsonl")
                && e.file_name().to_string_lossy() != keep
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
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
        for n in ["old1-a.jsonl", "old2-b.jsonl", "old3-c.jsonl"] {
            std::fs::write(cache_dir.join(n), "\n").unwrap();
        }
        let _c = Cache::open(&cache_dir, "current", "gen");
        let count = std::fs::read_dir(&cache_dir).unwrap().count();
        assert!(count <= 2, "expected current + 1 old generation, got {count}");
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
