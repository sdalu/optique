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
        let generation = format!("{}-{}.jsonl", &short(tree_key), &short(conf_hash));
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
    if let Some(head) = git(&["rev-parse", "HEAD"]) {
        let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        return if dirty { format!("{head}-dirty") } else { head };
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
