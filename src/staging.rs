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
    /// files of every known OPTIONS_NAME, and their `options.local`, byte for
    /// byte: make reads both, and the cache the background re-queries share
    /// with the scan matches entries on both hashes. The bytes come from
    /// `files`, which the scan and the session have already read, so seeding
    /// reads nothing more. `options.local` is copied once, never rewritten:
    /// optique does not edit it.
    pub fn create<'a>(
        staging_dir: &Path,
        files: &OptionsFiles,
        names: impl Iterator<Item = &'a String>,
    ) -> Result<Self> {
        let root = staging_dir.join("db");
        fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
        let db = StagingDb { root };
        for name in names {
            db.seed(files, name)?;
        }
        Ok(db)
    }

    /// Seed one OPTIONS_NAME: copy what the real options dir holds for it
    /// into staging, byte for byte, from `files`. Every port staging learns
    /// of goes through here, at start or as a re-query brings it in. True
    /// when there was a file to copy.
    pub fn seed(&self, files: &OptionsFiles, options_name: &str) -> Result<bool> {
        self.seed_local(files, options_name)?;
        match files.bytes(options_name) {
            Some(content) => self.write(options_name, &content).map(|()| true),
            None => Ok(false),
        }
    }

    /// Copy the real options.local alone, which make reads after the file
    /// whatever the file is: a staged edit goes with it too.
    pub fn seed_local(&self, files: &OptionsFiles, options_name: &str) -> Result<()> {
        if let Some(content) = files.local_bytes(options_name) {
            let path = self.root.join(options_name).join("options.local");
            fs::create_dir_all(self.root.join(options_name))
                .and_then(|()| fs::write(&path, &content))
                .with_context(|| format!("writing {}", path.display()))?;
        }
        Ok(())
    }

    /// Does staging hold anything make reads for this OPTIONS_NAME?
    pub fn has(&self, options_name: &str) -> bool {
        let dir = self.root.join(options_name);
        dir.join("options").is_file() || dir.join("options.local").is_file()
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
        // An options.local beside a file, and one with no options file.
        fs::write(real.join("cat_odd/options.local"), "OPTIONS_FILE_SET+=B\n").unwrap();
        fs::create_dir_all(real.join("cat_local")).unwrap();
        fs::write(real.join("cat_local/options.local"), "OPTIONS_FILE_SET+=C\n").unwrap();
        let files = OptionsFiles::new(&real);
        let names: Vec<String> = ["cat_odd", "cat_plain", "cat_absent", "cat_local"]
            .iter()
            .map(ToString::to_string)
            .collect();
        // What the scan did: hash every file for the cache.
        let both = |f: &OptionsFiles, n: &String| (f.hash(n), f.local_hash(n));
        let hashes: Vec<(String, String)> = names.iter().map(|n| both(&files, n)).collect();
        let reads = (files.disk_reads(), files.local_reads());
        // An edit behind optique's back after the scan is not what the scan
        // judged, so it is not what staging gets either.
        fs::write(real.join("cat_plain/options"), "edited later\n").unwrap();

        let db = StagingDb::create(tmp.path(), &files, names.iter()).unwrap();
        assert_eq!((files.disk_reads(), files.local_reads()), reads, "seeding read nothing");
        assert_eq!(fs::read(db.path().join("cat_odd/options")).unwrap(), odd);
        assert_eq!(
            fs::read(db.path().join("cat_plain/options")).unwrap(),
            b"OPTIONS_FILE_SET+=A\n"
        );
        assert!(!db.path().join("cat_absent").exists());
        assert_eq!(
            fs::read(db.path().join("cat_odd/options.local")).unwrap(),
            b"OPTIONS_FILE_SET+=B\n"
        );
        assert!(!db.path().join("cat_local/options").exists());
        // The invariant the shared cache rests on.
        let staged = OptionsFiles::new(db.path());
        let staged_hashes: Vec<(String, String)> = names.iter().map(|n| both(&staged, n)).collect();
        assert_eq!(staged_hashes, hashes);
    }

    #[test]
    fn seeds_existing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir_all(real.join("cat_port")).unwrap();
        fs::write(real.join("cat_port/options"), "OPTIONS_FILE_SET+=A\n").unwrap();
        let names = ["cat_port".to_string(), "cat_other".to_string()];
        let db = StagingDb::create(tmp.path(), &OptionsFiles::new(&real), names.iter()).unwrap();
        assert!(db.path().join("cat_port/options").is_file());
        assert!(!db.path().join("cat_other/options").exists());
    }
}
