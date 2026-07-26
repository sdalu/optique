use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Staging PORT_DBDIR: a private copy of the options files that background
/// re-queries read, so staged edits take effect without touching the real
/// options dir until Apply.
pub struct StagingDb {
    root: PathBuf,
}

impl StagingDb {
    /// Create under the given staging dir and seed it with the saved options
    /// files of every known OPTIONS_NAME.
    pub fn create<'a>(
        staging_dir: &Path,
        options_dir: &Path,
        names: impl Iterator<Item = &'a String>,
    ) -> Result<Self> {
        let root = staging_dir.join("db");
        fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
        let db = StagingDb { root };
        for name in names {
            let src = options_dir.join(name).join("options");
            if let Ok(content) = fs::read(&src) {
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

    #[test]
    fn seeds_existing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir_all(real.join("cat_port")).unwrap();
        fs::write(real.join("cat_port/options"), "OPTIONS_FILE_SET+=A\n").unwrap();
        let names = vec!["cat_port".to_string(), "cat_other".to_string()];
        let db = StagingDb::create(tmp.path(), &real, names.iter()).unwrap();
        assert!(db.path().join("cat_port/options").is_file());
        assert!(!db.path().join("cat_other/options").exists());
    }
}
