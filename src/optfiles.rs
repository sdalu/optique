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

/// The options file a port reads, named by its place in the options dir:
/// the entry's name alone (`devel_git`) for the framework's own
/// `<entry>/options`, `<entry>/<file>` for another file in the entry, which
/// a port's Makefile (devel/git's flavors read `devel_git/lite-options`) or
/// make.conf names as `OPTIONS_FILE`. Every map keyed on a port's file uses
/// this key, `PortInfo::options_name`.
pub fn key_file(key: &str) -> PathBuf {
    if key.contains('/') {
        PathBuf::from(key)
    } else {
        Path::new(key).join("options")
    }
}

/// The entry directory `key`'s file is in.
pub fn key_entry(key: &str) -> &str {
    key.split_once('/').map_or(key, |(entry, _)| entry)
}

/// How a message or the JSON names `key`'s file: `devel_git/options`,
/// `devel_git/lite-options`.
pub fn key_label(key: &str) -> String {
    key_file(key).display().to_string()
}

/// The `.local` make reads after `key`'s file, relative to the options dir.
pub fn key_local_file(key: &str) -> PathBuf {
    local_of(&key_file(key))
}

/// `path` with `.local` appended, as bsd.options.mk names the file it reads
/// after `path`.
fn local_of(path: &Path) -> PathBuf {
    let mut local = path.as_os_str().to_owned();
    local.push(".local");
    PathBuf::from(local)
}

/// The key ([`key_file`]) of `options_file`, an `OPTIONS_FILE` as make
/// expanded it, when it lies one entry deep in `port_dbdir`, the
/// `PORT_DBDIR` the query was given: `<entry>/options` is the entry's own
/// key, `<entry>/<file>` that path. None for a file anywhere else, which
/// optique does not manage, or for a `.local` name, which make would read
/// as the file's own `.local` too.
pub fn options_file_key(port_dbdir: &Path, options_file: &str) -> Option<String> {
    use std::path::Component;
    let rel = Path::new(options_file).strip_prefix(port_dbdir).ok()?;
    let parts: Vec<&str> = rel
        .components()
        .map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let [entry, file] = parts.as_slice() else {
        return None;
    };
    if file.ends_with(".local") {
        return None;
    }
    Some(if *file == "options" {
        (*entry).to_string()
    } else {
        format!("{entry}/{file}")
    })
}

pub struct OptionsFiles {
    dir: PathBuf,
    memo: RefCell<HashMap<String, Read>>,
    /// The `options.local` files, kept as `memo` keeps the options files:
    /// make reads one after the options file (bsd.options.mk's
    /// `.sinclude`), so the cache keys on it too. optique never writes one.
    local_memo: RefCell<HashMap<String, Read>>,
    /// Options files read from disk (for tests and the curious).
    disk_reads: Cell<usize>,
    /// `options.local` files read from disk.
    local_reads: Cell<usize>,
}

impl OptionsFiles {
    pub fn new(dir: &Path) -> Self {
        OptionsFiles {
            dir: dir.to_path_buf(),
            memo: RefCell::new(HashMap::new()),
            local_memo: RefCell::new(HashMap::new()),
            disk_reads: Cell::new(0),
            local_reads: Cell::new(0),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The entry holding the file `options_name` names ([`key_entry`]).
    pub fn entry_dir(&self, options_name: &str) -> PathBuf {
        self.dir.join(key_entry(options_name))
    }

    /// The file `options_name` names ([`key_file`]).
    pub fn path(&self, options_name: &str) -> PathBuf {
        self.dir.join(key_file(options_name))
    }

    /// The `.local` make reads after that file (bsd.options.mk's
    /// `.sinclude "${OPTIONS_FILE}.local"`): `<entry>/options.local` for
    /// the framework's own file.
    pub fn local_path(&self, options_name: &str) -> PathBuf {
        local_of(&self.path(options_name))
    }

    fn read(&self, options_name: &str) -> Read {
        read_once(&self.memo, &self.disk_reads, options_name, || {
            self.path(options_name)
        })
    }

    fn read_local(&self, options_name: &str) -> Read {
        read_once(&self.local_memo, &self.local_reads, options_name, || {
            self.local_path(options_name)
        })
    }

    /// The file's bytes, None when it is absent or cannot be read.
    pub fn bytes(&self, options_name: &str) -> Option<Rc<[u8]>> {
        self.read(options_name).bytes()
    }

    /// SHA-256 of the file's bytes, `ABSENT` or `UNREADABLE`: what the cache
    /// records an entry against.
    pub fn hash(&self, options_name: &str) -> String {
        self.read(options_name).hash()
    }

    /// `bytes` for the entry's `options.local`.
    pub fn local_bytes(&self, options_name: &str) -> Option<Rc<[u8]>> {
        self.read_local(options_name).bytes()
    }

    /// `hash` for the entry's `options.local`.
    pub fn local_hash(&self, options_name: &str) -> String {
        self.read_local(options_name).hash()
    }

    /// The saved choices as make reads them: the options file, with the
    /// SET and UNSET lines of the `options.local` beside it read after it
    /// (`SavedOptionsFile::with_local`). make `.sinclude`s options.local
    /// whether or not the options file is there (bsd.options.mk:343), so
    /// an options.local without an options file is read after an empty
    /// one. None when neither is there, or the options file cannot be
    /// read. A stray byte that is not UTF-8 (in a comment, say) does not
    /// void them: the lines that matter are plain ASCII, and make reads the
    /// files all the same.
    pub fn saved(&self, options_name: &str) -> Option<SavedOptionsFile> {
        let own = match self.read(options_name) {
            Read::Present(bytes) => SavedOptionsFile::parse(&String::from_utf8_lossy(&bytes)),
            Read::Absent if self.local_bytes(options_name).is_some() => SavedOptionsFile::default(),
            Read::Absent | Read::Unreadable => return None,
        };
        Some(self.under_local(options_name, own))
    }

    /// `saved`, an absent options file read as an empty one: what make
    /// reads for the entry, options.local included, whether or not the
    /// file is there.
    pub fn combined(&self, options_name: &str) -> SavedOptionsFile {
        self.under_local(options_name, self.own(options_name).unwrap_or_default())
    }

    /// `file` with this entry's options.local read after it, as make would
    /// read it in place of the options file.
    pub fn under_local(&self, options_name: &str, file: SavedOptionsFile) -> SavedOptionsFile {
        match self.local_bytes(options_name) {
            Some(local) => file.with_local(&String::from_utf8_lossy(&local)),
            None => file,
        }
    }

    /// The options file alone, as written: what a write may carry over.
    pub fn own(&self, options_name: &str) -> Option<SavedOptionsFile> {
        let bytes = self.bytes(options_name)?;
        Some(SavedOptionsFile::parse(&String::from_utf8_lossy(&bytes)))
    }

    /// Drop what is known of these files, so the next read sees the disk:
    /// after optique itself wrote or removed them.
    pub fn forget<'a>(&self, names: impl IntoIterator<Item = &'a str>) {
        // Both memos, so the next reading sees the entry as it now is,
        // options.local included.
        let mut memo = self.memo.borrow_mut();
        let mut local = self.local_memo.borrow_mut();
        for name in names {
            memo.remove(name);
            local.remove(name);
        }
    }

    #[cfg(test)]
    pub fn disk_reads(&self) -> usize {
        self.disk_reads.get()
    }

    #[cfg(test)]
    pub fn local_reads(&self) -> usize {
        self.local_reads.get()
    }
}

/// One file of an entry: what `memo` holds, else a read from disk counted
/// in `reads` and kept.
fn read_once(
    memo: &RefCell<HashMap<String, Read>>,
    reads: &Cell<usize>,
    options_name: &str,
    path: impl FnOnce() -> PathBuf,
) -> Read {
    if let Some(hit) = memo.borrow().get(options_name) {
        return hit.clone();
    }
    reads.set(reads.get() + 1);
    let read = match fs::read(path()) {
        Ok(bytes) => Read::Present(Rc::from(bytes)),
        Err(e) if e.kind() == ErrorKind::NotFound => Read::Absent,
        Err(_) => Read::Unreadable,
    };
    memo.borrow_mut()
        .insert(options_name.to_string(), read.clone());
    read
}

impl Read {
    fn bytes(self) -> Option<Rc<[u8]>> {
        match self {
            Read::Present(bytes) => Some(bytes),
            Read::Absent | Read::Unreadable => None,
        }
    }

    fn hash(&self) -> String {
        match self {
            Read::Present(bytes) => crate::cache::sha256_hex(bytes),
            Read::Absent => ABSENT.to_string(),
            Read::Unreadable => UNREADABLE.to_string(),
        }
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
        assert_eq!(
            files.disk_reads(),
            1,
            "bytes, hash and saved share one read"
        );
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
        assert_eq!(
            files.bytes("cat_b").as_deref(),
            Some(&b"b1\n"[..]),
            "still as first read"
        );
        assert_eq!(files.disk_reads(), 3);
    }

    /// The hash is the cache's: entries written before the store existed
    /// must still match.
    #[test]
    fn hash_is_sha256_hex_of_the_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        put(tmp.path(), "cat_port", b"");
        let h = OptionsFiles::new(tmp.path()).hash("cat_port");
        assert_eq!(
            h,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// A stray byte that is not UTF-8 does not void the saved choices: make
    /// reads the file all the same, so optique keeps its bytes and its sets.
    #[test]
    fn a_non_utf8_file_keeps_its_bytes_and_its_choices() {
        let tmp = tempfile::tempdir().unwrap();
        put(
            tmp.path(),
            "cat_port",
            b"# by hand \xff\xfe\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n",
        );
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
        assert_eq!(
            files.hash("cat_dir"),
            UNREADABLE,
            "a directory in its place"
        );
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

    /// The saved choices are the file with options.local read after it, as
    /// make reads them (bsd.options.mk:341-363); the file alone stays at
    /// hand for what a write carries over.
    #[test]
    fn saved_reads_options_local_after_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        put(
            tmp.path(),
            "cat_port",
            b"OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B C\n",
        );
        fs::write(
            tmp.path().join("cat_port/options.local"),
            "OPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_SET+=B\n",
        )
        .unwrap();
        let files = OptionsFiles::new(tmp.path());
        let saved = files.saved("cat_port").unwrap();
        let v: Vec<_> = ["A", "B", "C"].iter().map(|o| saved.value(o)).collect();
        assert_eq!(v, [Some(false), Some(false), Some(false)]);
        assert_eq!(files.own("cat_port").unwrap().value("A"), Some(true));
        let both = files.combined("cat_port");
        assert_eq!(
            both.local_set
                .into_iter()
                .chain(both.local_unset)
                .collect::<Vec<_>>(),
            ["B", "A"]
        );
        assert_eq!(files.combined("cat_none"), SavedOptionsFile::default());
    }

    /// bsd.options.mk:343 `.sinclude`s options.local whether or not the
    /// options file is there: the saved choices are then options.local read
    /// after an empty file, while the file alone stays absent.
    #[test]
    fn saved_reads_options_local_without_an_options_file() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
        fs::write(
            tmp.path().join("cat_port/options.local"),
            "OPTIONS_FILE_SET+=A
OPTIONS_FILE_UNSET+=B
",
        )
        .unwrap();
        let files = OptionsFiles::new(tmp.path());
        let saved = files
            .saved("cat_port")
            .expect("options.local alone is read");
        let v: Vec<_> = ["A", "B", "C"].iter().map(|o| saved.value(o)).collect();
        assert_eq!(v, [Some(true), Some(false), None]);
        assert!(saved.complete.is_empty());
        assert_eq!(saved, files.combined("cat_port"));
        assert!(files.own("cat_port").is_none() && files.bytes("cat_port").is_none());
        assert!(files.saved("cat_none").is_none(), "neither file");
    }

    #[test]
    fn path_is_the_ports_framework_layout() {
        let files = OptionsFiles::new(Path::new("/opt/db"));
        assert_eq!(files.dir(), Path::new("/opt/db"));
        assert_eq!(
            files.path("www_nginx"),
            Path::new("/opt/db/www_nginx/options")
        );
    }

    /// After optique writes or removes an entry, `forget` makes the next
    /// reading go to disk again, options.local included.
    #[test]
    fn forget_clears_the_options_local_reading_too() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
        fs::write(
            tmp.path().join("cat_port/options.local"),
            "OPTIONS_FILE_SET+=A\n",
        )
        .unwrap();
        let files = OptionsFiles::new(tmp.path());
        assert!(files.local_bytes("cat_port").is_some());
        fs::remove_file(tmp.path().join("cat_port/options.local")).unwrap();
        files.forget(["cat_port"]);
        assert!(
            files.local_bytes("cat_port").is_none(),
            "read again after forget"
        );
        assert_eq!(files.local_reads(), 2);
    }

    /// A port's file is named by its place in the options dir: the entry
    /// for the framework's own `options`, `<entry>/<file>` for another file
    /// of the entry; an OPTIONS_FILE anywhere else has no key.
    #[test]
    fn keys_name_files_one_entry_deep() {
        assert_eq!(key_file("devel_git"), Path::new("devel_git/options"));
        assert_eq!(
            key_file("devel_git/lite-options"),
            Path::new("devel_git/lite-options")
        );
        assert_eq!(
            key_local_file("devel_git"),
            Path::new("devel_git/options.local")
        );
        assert_eq!(
            key_local_file("devel_git/lite-options"),
            Path::new("devel_git/lite-options.local")
        );
        assert_eq!(key_entry("devel_git/lite-options"), "devel_git");
        assert_eq!(key_label("devel_git"), "devel_git/options");

        let db = Path::new("/db");
        let key = |file: &str| options_file_key(db, file);
        assert_eq!(
            key("/db/devel_git/lite-options").as_deref(),
            Some("devel_git/lite-options")
        );
        assert_eq!(
            key("/db/mail_dovecot24-x/options").as_deref(),
            Some("mail_dovecot24-x")
        );
        for elsewhere in [
            "/db/options",
            "/db/a/b/options",
            "/db/../a/options",
            "/db/a/options.local",
            "/dbx/a/options",
            "/elsewhere/a/options",
        ] {
            assert_eq!(key(elsewhere), None, "{elsewhere}");
        }
    }

    /// A key one entry deep reads its file and the `.local` after it.
    #[test]
    fn a_key_one_entry_deep_reads_its_file_and_local() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("devel_git")).unwrap();
        fs::write(tmp.path().join("devel_git/options"), "default\n").unwrap();
        fs::write(tmp.path().join("devel_git/lite-options"), "lite\n").unwrap();
        fs::write(tmp.path().join("devel_git/lite-options.local"), "local\n").unwrap();
        let files = OptionsFiles::new(tmp.path());
        let key = "devel_git/lite-options";
        assert_eq!(files.bytes(key).as_deref(), Some(&b"lite\n"[..]));
        assert_eq!(files.local_bytes(key).as_deref(), Some(&b"local\n"[..]));
        assert_eq!(files.bytes("devel_git").as_deref(), Some(&b"default\n"[..]));
        assert_eq!(files.local_bytes("devel_git"), None);
        assert_eq!(files.entry_dir(key), tmp.path().join("devel_git"));
    }
}
