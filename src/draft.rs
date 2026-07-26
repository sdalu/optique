use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cache;
use crate::session::Session;

/// A saved set of staged edits, keyed by options dir.
///
/// Written when quitting the TUI with unapplied edits, read back on the next
/// launch against the same options dir. Only the *intent* (which options are
/// enabled) is stored — never the port list or the baseline, so a draft stays
/// usable after the ports tree moves on; entries that no longer exist are
/// dropped at restore time.
#[derive(Debug, Serialize, Deserialize)]
pub struct Draft {
    pub options_dir: PathBuf,
    pub saved_at_secs: u64,
    /// OPTIONS_NAME -> enabled option set.
    pub staged: BTreeMap<String, BTreeSet<String>>,
}

impl Draft {
    /// Seconds since the draft was written (0 if the clock went backwards).
    pub fn age_secs(&self) -> u64 {
        now_secs().saturating_sub(self.saved_at_secs)
    }
}

/// cache_dir/drafts/<sha256(options_dir)[..16]>.json
pub fn draft_path_in(cache_dir: &Path, options_dir: &Path) -> PathBuf {
    // The dir string is hashed as given: the TUI always gets the same
    // configured path, and hashing avoids escaping separators in a filename.
    let digest = cache::sha256_hex(options_dir.to_string_lossy().as_bytes());
    cache_dir.join("drafts").join(format!("{}.json", &digest[..16]))
}

/// Write the session's dirty staged sets as the draft for `options_dir`.
/// A draft with zero dirty states is still written; callers guard on
/// `Session::dirty()`.
pub fn save_in(cache_dir: &Path, session: &Session, options_dir: &Path) -> Result<PathBuf> {
    let staged: BTreeMap<String, BTreeSet<String>> = session
        .states
        .iter()
        .filter(|(_, state)| state.staged != state.baseline)
        .map(|(name, state)| (name.clone(), state.staged.clone()))
        .collect();
    let draft = Draft {
        options_dir: options_dir.to_path_buf(),
        saved_at_secs: now_secs(),
        staged,
    };
    let path = draft_path_in(cache_dir, options_dir);
    let dir = path.parent().expect("draft path always has a parent");
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating draft dir {}", dir.display()))?;
    let json = serde_json::to_string_pretty(&draft)?;
    std::fs::write(&path, json).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// The draft for `options_dir`, or None when there is none (or it is
/// unreadable/corrupt — a bad draft is never worth an error at startup).
pub fn load_in(cache_dir: &Path, options_dir: &Path) -> Option<Draft> {
    let text = std::fs::read_to_string(draft_path_in(cache_dir, options_dir)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn discard_in(cache_dir: &Path, options_dir: &Path) {
    let _ = std::fs::remove_file(draft_path_in(cache_dir, options_dir));
}

pub fn save(session: &Session, options_dir: &Path) -> Result<PathBuf> {
    save_in(&cache::default_cache_dir(), session, options_dir)
}

pub fn load(options_dir: &Path) -> Option<Draft> {
    load_in(&cache::default_cache_dir(), options_dir)
}

pub fn discard(options_dir: &Path) {
    discard_in(&cache::default_cache_dir(), options_dir)
}

/// Coarse "how old is this draft" label: minutes under an hour, hours under
/// two days, days beyond.
pub fn age_label(secs: u64) -> String {
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = secs / 3600;
    if hours < 48 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", secs / 86_400)
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::OptState;
    use std::collections::HashMap;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// A session with only `states` filled in: that is all save() reads.
    fn session_with(states: Vec<(&str, OptState)>) -> Session {
        Session {
            ports: BTreeMap::new(),
            states: states.into_iter().map(|(n, s)| (n.to_string(), s)).collect(),
            aliases: HashMap::new(),
            roots: Vec::new(),
            owners: HashMap::new(),
        }
    }

    #[test]
    fn saves_only_dirty_states_and_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("cache");
        let options_dir = tmp.path().join("options");

        let session = session_with(vec![
            (
                "cat_dirty",
                OptState { saved: None, baseline: set(&["A"]), staged: set(&["A", "B"]) },
            ),
            (
                "cat_clean",
                OptState { saved: None, baseline: set(&["X"]), staged: set(&["X"]) },
            ),
        ]);

        let path = save_in(&cache_dir, &session, &options_dir).unwrap();
        assert!(path.exists(), "draft file written");
        assert_eq!(path, draft_path_in(&cache_dir, &options_dir));

        let draft = load_in(&cache_dir, &options_dir).unwrap();
        assert_eq!(draft.options_dir, options_dir);
        assert_eq!(draft.staged.keys().collect::<Vec<_>>(), vec!["cat_dirty"]);
        assert_eq!(draft.staged["cat_dirty"], set(&["A", "B"]));
        assert!(draft.saved_at_secs > 0);

        discard_in(&cache_dir, &options_dir);
        assert!(!path.exists(), "discard removes the file");
        assert!(load_in(&cache_dir, &options_dir).is_none());
    }

    #[test]
    fn drafts_are_keyed_by_options_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("cache");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        let session = session_with(vec![(
            "cat_port",
            OptState { saved: None, baseline: set(&[]), staged: set(&["A"]) },
        )]);

        save_in(&cache_dir, &session, &a).unwrap();
        assert!(load_in(&cache_dir, &a).is_some());
        assert!(load_in(&cache_dir, &b).is_none(), "another dir has its own draft");
    }

    #[test]
    fn load_ignores_corrupt_draft() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join("cache");
        let options_dir = tmp.path().join("options");
        let path = draft_path_in(&cache_dir, &options_dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load_in(&cache_dir, &options_dir).is_none());
    }

    #[test]
    fn age_labels() {
        assert_eq!(age_label(120), "2m ago");
        assert_eq!(age_label(7200), "2h ago");
        assert_eq!(age_label(259_200), "3d ago");
        assert_eq!(age_label(0), "0m ago");
        assert_eq!(age_label(3600 * 47), "47h ago");
    }
}
