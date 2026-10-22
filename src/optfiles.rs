//! The options files of one options directory, each read from disk at most
//! once.
//!
//! Starting up reads every options file of the closure for three readers:
//! the cache checks each entry against the file's hash, the session parses
//! the saved choices, and the TUI's staging dir is seeded with a copy. On an
//! NFS-mounted poudriere.d each read is a round trip, so they share one
//! store instead of three serial passes over the directory.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::optionsfile::SavedOptionsFile;

/// Hash recorded for an options file that is not there.
pub const ABSENT: &str = "ABSENT";

/// Hash recorded for an options file that is there but cannot be read. It
/// differs from `ABSENT` so that a cache entry made while the file was
/// missing is never taken for one: make fails on such a file ("Cannot
/// open"), it does not read it as missing.
pub const UNREADABLE: &str = "UNREADABLE";

/// What one read of an options file found.
#[derive(Clone)]
enum Read {
    Present(Rc<[u8]>),
    /// Not there (NotFound).
    Absent,
    /// There, but any other read error: permissions, an NFS fault, a
    /// directory in its place.
    Unreadable,
}

pub struct OptionsFiles {
    dir: PathBuf,
    memo: RefCell<HashMap<String, Read>>,
    /// Reads that went to disk (for tests and the curious).
    disk_reads: Cell<usize>,
}

impl OptionsFiles {
    pub fn new(dir: &Path) -> Self {
        OptionsFiles {
            dir: dir.to_path_buf(),
            memo: RefCell::new(HashMap::new()),
            disk_reads: Cell::new(0),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `<dir>/<options_name>`, the entry holding the file.
    pub fn entry_dir(&self, options_name: &str) -> PathBuf {
        self.dir.join(options_name)
    }

    /// `<dir>/<options_name>/options`.
    pub fn path(&self, options_name: &str) -> PathBuf {
        self.entry_dir(options_name).join("options")
    }

    fn read(&self, options_name: &str) -> Read {
        if let Some(hit) = self.memo.borrow().get(options_name) {
            return hit.clone();
        }
        self.disk_reads.set(self.disk_reads.get() + 1);
        let read = match fs::read(self.path(options_name)) {
            Ok(bytes) => Read::Present(Rc::from(bytes)),
            Err(e) if e.kind() == ErrorKind::NotFound => Read::Absent,
            Err(_) => Read::Unreadable,
        };
        self.memo.borrow_mut().insert(options_name.to_string(), read.clone());
        read
    }

    /// The file's bytes, None when it is absent or cannot be read.
    pub fn bytes(&self, options_name: &str) -> Option<Rc<[u8]>> {
        match self.read(options_name) {
            Read::Present(bytes) => Some(bytes),
            Read::Absent | Read::Unreadable => None,
        }
    }

    /// SHA-256 of the file's bytes, `ABSENT` or `UNREADABLE`: what the cache
    /// records an entry against.
    pub fn hash(&self, options_name: &str) -> String {
        match self.read(options_name) {
            Read::Present(bytes) => crate::cache::sha256_hex(&bytes),
            Read::Absent => ABSENT.to_string(),
            Read::Unreadable => UNREADABLE.to_string(),
        }
    }

    /// The saved choices the file records. A stray byte that is not UTF-8
    /// (in a comment, say) does not void them: the lines that matter are
    /// plain ASCII, and make reads the file all the same.
    pub fn saved(&self, options_name: &str) -> Option<SavedOptionsFile> {
        let bytes = self.bytes(options_name)?;
        Some(SavedOptionsFile::parse(&String::from_utf8_lossy(&bytes)))
    }

    /// Drop what is known of these files, so the next read sees the disk:
    /// after optique itself wrote or removed them.
    pub fn forget<'a>(&self, names: impl IntoIterator<Item = &'a str>) {
        let mut memo = self.memo.borrow_mut();
        for name in names {
            memo.remove(name);
        }
    }

    #[cfg(test)]
    pub fn disk_reads(&self) -> usize {
        self.disk_reads.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(dir: &Path, name: &str, content: &[u8]) {
        fs::create_dir_all(dir.join(name)).unwrap();
        fs::write(dir.join(name).join("options"), content).unwrap();
    }

    #[test]
    fn each_file_is_read_once_whoever_asks() {
        let tmp = tempfile::tempdir().unwrap();
        put(tmp.path(), "cat_port", b"OPTIONS_FILE_SET+=A\n");
        let files = OptionsFiles::new(tmp.path());
        let bytes = files.bytes("cat_port").unwrap();
        let hash = files.hash("cat_port");
        let saved = files.saved("cat_port").unwrap();
        assert_eq!(files.bytes("cat_port").unwrap(), bytes);
        assert_eq!(files.disk_reads(), 1, "bytes, hash and saved share one read");
        assert_eq!(&*bytes, b"OPTIONS_FILE_SET+=A\n");
        assert_eq!(hash, crate::cache::sha256_hex(b"OPTIONS_FILE_SET+=A\n"));
        assert!(saved.set.contains("A"));
    }

    /// Absence is remembered like content: a closure with hundreds of
    /// unconfigured ports must not ask the NFS server about each one again.
    #[test]
    fn an_absent_file_is_remembered_too() {
        let tmp = tempfile::tempdir().unwrap();
        let files = OptionsFiles::new(tmp.path());
        assert!(files.bytes("cat_none").is_none());
        assert_eq!(files.hash("cat_none"), ABSENT);
        assert!(files.saved("cat_none").is_none());
        assert_eq!(files.disk_reads(), 1);
        // It stays absent for this store, as the scan saw it...
        put(tmp.path(), "cat_none", b"x\n");
        assert!(files.bytes("cat_none").is_none());
        // ...until forgotten.
        files.forget(["cat_none"]);
        assert_eq!(files.bytes("cat_none").as_deref(), Some(&b"x\n"[..]));
        assert_eq!(files.disk_reads(), 2);
    }

    #[test]
    fn forget_rereads_only_what_it_names() {
        let tmp = tempfile::tempdir().unwrap();
        put(tmp.path(), "cat_a", b"a1\n");
        put(tmp.path(), "cat_b", b"b1\n");
        let files = OptionsFiles::new(tmp.path());
        files.bytes("cat_a");
        files.bytes("cat_b");
        put(tmp.path(), "cat_a", b"a2\n");
        put(tmp.path(), "cat_b", b"b2\n");
        files.forget(["cat_a", "cat_unknown"]);
        assert_eq!(files.bytes("cat_a").as_deref(), Some(&b"a2\n"[..]));
        assert_eq!(files.bytes("cat_b").as_deref(), Some(&b"b1\n"[..]), "still as first read");
        assert_eq!(files.disk_reads(), 3);
    }

    /// The hash is the cache's: entries written before the store existed
    /// must still match.
    #[test]
    fn hash_is_sha256_hex_of_the_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        put(tmp.path(), "cat_port", b"");
        let h = OptionsFiles::new(tmp.path()).hash("cat_port");
        assert_eq!(h, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    /// A stray byte that is not UTF-8 does not void the saved choices: make
    /// reads the file all the same, so optique keeps its bytes and its sets.
    #[test]
    fn a_non_utf8_file_keeps_its_bytes_and_its_choices() {
        let tmp = tempfile::tempdir().unwrap();
        put(tmp.path(), "cat_port", b"# by hand \xff\xfe\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n");
        let files = OptionsFiles::new(tmp.path());
        let saved = files.saved("cat_port").expect("the choices survive");
        assert!(saved.set.contains("A") && saved.unset.contains("B"));
        assert_eq!(files.bytes("cat_port").unwrap().len(), 55);
        assert_ne!(files.hash("cat_port"), ABSENT);
    }

    /// A file that is there but cannot be read is not absent: make fails on
    /// it rather than reading it as missing, so a cache entry made while it
    /// was missing must not be reused. Its bytes and choices are unknown.
    #[test]
    fn an_unreadable_file_is_not_absent() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("cat_dir/options")).unwrap();
        let files = OptionsFiles::new(tmp.path());
        assert!(files.bytes("cat_dir").is_none() && files.saved("cat_dir").is_none());
        assert_eq!(files.hash("cat_dir"), UNREADABLE, "a directory in its place");
        assert_eq!(files.hash("cat_none"), ABSENT, "really missing");

        use std::os::unix::fs::PermissionsExt;
        put(tmp.path(), "cat_locked", b"x\n");
        let path = files.path("cat_locked");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        // root reads it anyway; the case only exists for everyone else.
        if fs::read(&path).is_err() {
            assert!(files.bytes("cat_locked").is_none());
            assert_eq!(files.hash("cat_locked"), UNREADABLE);
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn path_is_the_ports_framework_layout() {
        let files = OptionsFiles::new(Path::new("/opt/db"));
        assert_eq!(files.dir(), Path::new("/opt/db"));
        assert_eq!(files.path("www_nginx"), Path::new("/opt/db/www_nginx/options"));
    }
}
