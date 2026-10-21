use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::optfiles::OptionsFiles;

/// Staging PORT_DBDIR: a private copy of the options files that background
/// re-queries read, so staged edits take effect without touching the real
/// options dir until Apply.
pub struct StagingDb {
    root: PathBuf,
}

impl StagingDb {
    /// Create under the given staging dir and seed it with the saved options
    /// files of every known OPTIONS_NAME, byte for byte: the cache the
    /// background re-queries share with the scan matches entries on the
    /// file's hash. The bytes come from `files`, which the scan and the
    /// session have already read, so seeding reads nothing more.
    pub fn create<'a>(
        staging_dir: &Path,
        files: &OptionsFiles,
        names: impl Iterator<Item = &'a String>,
    ) -> Result<Self> {
        let root = staging_dir.join("db");
        fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
        let db = StagingDb { root };
        for name in names {
            if let Some(content) = files.bytes(name) {
                db.write(name, &content)?;
            }
        }
        Ok(db)
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// (Over)write one staged options file.
    pub fn write(&self, options_name: &str, content: &[u8]) -> Result<()> {
        let dir = self.root.join(options_name);
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        fs::write(dir.join("options"), content)
            .with_context(|| format!("writing staged options for {options_name}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Staging is seeded from what the store holds, byte for byte, without a
    /// second read: the scan's cache entries were checked against those very
    /// bytes, so the background re-queries must find them unchanged.
    #[test]
    fn seeds_byte_for_byte_from_the_store_without_reading_again() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        let odd: &[u8] = b"# hand-edited\n_FILE_COMPLETE_OPTIONS_LIST= A  B\n\xff\n";
        for (name, content) in [("cat_odd", odd), ("cat_plain", &b"OPTIONS_FILE_SET+=A\n"[..])] {
            fs::create_dir_all(real.join(name)).unwrap();
            fs::write(real.join(name).join("options"), content).unwrap();
        }
        let files = OptionsFiles::new(&real);
        let names: Vec<String> =
            ["cat_odd", "cat_plain", "cat_absent"].iter().map(|s| s.to_string()).collect();
        // What the scan did: hash every file for the cache.
        let hashes: Vec<String> = names.iter().map(|n| files.hash(n)).collect();
        let reads = files.disk_reads();
        // An edit behind optique's back after the scan is not what the scan
        // judged, so it is not what staging gets either.
        fs::write(real.join("cat_plain/options"), "edited later\n").unwrap();

        let db = StagingDb::create(tmp.path(), &files, names.iter()).unwrap();
        assert_eq!(files.disk_reads(), reads, "seeding read nothing");
        assert_eq!(fs::read(db.path().join("cat_odd/options")).unwrap(), odd);
        assert_eq!(
            fs::read(db.path().join("cat_plain/options")).unwrap(),
            b"OPTIONS_FILE_SET+=A\n"
        );
        assert!(!db.path().join("cat_absent").exists());
        // The invariant the shared cache rests on.
        let staged = OptionsFiles::new(db.path());
        let staged_hashes: Vec<String> = names.iter().map(|n| staged.hash(n)).collect();
        assert_eq!(staged_hashes, hashes);
    }

    #[test]
    fn seeds_existing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir_all(real.join("cat_port")).unwrap();
        fs::write(real.join("cat_port/options"), "OPTIONS_FILE_SET+=A\n").unwrap();
        let names = vec!["cat_port".to_string(), "cat_other".to_string()];
        let db = StagingDb::create(tmp.path(), &OptionsFiles::new(&real), names.iter()).unwrap();
        assert!(db.path().join("cat_port/options").is_file());
        assert!(!db.path().join("cat_other/options").exists());
    }
}
