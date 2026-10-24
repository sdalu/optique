use super::*;

fn info(origin: &str) -> PortInfo {
    PortInfo {
        pkgname: "x-1.0".into(),
        ..PortInfo::stub(origin)
    }
}

/// A lookup and an insert for the same file, and two flavors sharing it,
/// cost one read between them.
#[test]
fn lookups_and_inserts_share_one_read_per_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dbdir = tmp.path().join("db");
    fs::create_dir_all(dbdir.join("cat_port")).unwrap();
    fs::write(dbdir.join("cat_port/options"), "v1\n").unwrap();
    let files = OptionsFiles::new(&dbdir);

    let mut c = Cache::open(&tmp.path().join("cache"), "tree1", "conf1");
    let plain = info("cat/port");
    let mut flavored = info("cat/port");
    flavored.key = PortKey::parse("cat/port@f").unwrap();
    // Not in the cache yet: no file is read to find that out.
    assert!(c.lookup(&plain.key, &files).is_none());
    assert_eq!(files.disk_reads(), 0);
    c.insert(&plain, &files, std::time::SystemTime::now());
    c.insert(&flavored, &files, std::time::SystemTime::now());
    assert!(c.lookup(&plain.key, &files).is_some());
    assert!(c.lookup(&flavored.key, &files).is_some());
    assert_eq!(files.disk_reads(), 1);
}

#[test]
fn roundtrip_and_options_file_invalidation() {
    let tmp = tempfile::tempdir().unwrap();
    let dbdir = tmp.path().join("db");
    fs::create_dir_all(dbdir.join("cat_port")).unwrap();
    fs::write(dbdir.join("cat_port/options"), "v1\n").unwrap();

    let mut c = Cache::open(&tmp.path().join("cache"), "tree1", "conf1");
    let i = info("cat/port");
    assert!(c.lookup(&i.key, &OptionsFiles::new(&dbdir)).is_none());
    c.insert(&i, &OptionsFiles::new(&dbdir), std::time::SystemTime::now());
    assert!(c.lookup(&i.key, &OptionsFiles::new(&dbdir)).is_some());

    // Changing the options file invalidates the entry.
    fs::write(dbdir.join("cat_port/options"), "v2\n").unwrap();
    assert!(c.lookup(&i.key, &OptionsFiles::new(&dbdir)).is_none());
    // Restoring the exact content revalidates it.
    fs::write(dbdir.join("cat_port/options"), "v1\n").unwrap();
    assert!(c.lookup(&i.key, &OptionsFiles::new(&dbdir)).is_some());
}

#[test]
fn persists_across_reopen_same_generation_only() {
    let tmp = tempfile::tempdir().unwrap();
    let dbdir = tmp.path().join("db");
    fs::create_dir_all(&dbdir).unwrap();
    let cache_dir = tmp.path().join("cache");

    let mut c = Cache::open(&cache_dir, "treeA", "confA");
    c.insert(
        &info("cat/port"),
        &OptionsFiles::new(&dbdir),
        std::time::SystemTime::now(),
    );
    drop(c);

    let c2 = Cache::open(&cache_dir, "treeA", "confA");
    assert!(
        c2.lookup(
            &PortKey::parse("cat/port").unwrap(),
            &OptionsFiles::new(&dbdir)
        )
        .is_some()
    );
    let c3 = Cache::open(&cache_dir, "treeB", "confA");
    assert!(
        c3.lookup(
            &PortKey::parse("cat/port").unwrap(),
            &OptionsFiles::new(&dbdir)
        )
        .is_none()
    );
}

#[test]
fn disabled_never_stores() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = Cache::disabled();
    c.insert(
        &info("cat/port"),
        &OptionsFiles::new(tmp.path()),
        std::time::SystemTime::now(),
    );
    assert!(
        c.lookup(
            &PortKey::parse("cat/port").unwrap(),
            &OptionsFiles::new(tmp.path())
        )
        .is_none()
    );
}

#[test]
fn prune_keeps_at_most_two_generations() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    for n in [
        format!("v{SCHEMA}-old1-a.jsonl"),
        format!("v{SCHEMA}-old2-b.jsonl"),
        format!("v{SCHEMA}-old3-c.jsonl"),
    ] {
        fs::write(cache_dir.join(n), "\n").unwrap();
    }
    let _c = Cache::open(&cache_dir, "current", "gen");
    let count = fs::read_dir(&cache_dir).unwrap().count();
    assert!(
        count <= 2,
        "expected current + 1 old generation, got {count}"
    );
}

#[test]
fn prune_reaps_other_schema_generations() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    // Not a cache generation at all (a draft, say): never touched.
    fs::write(cache_dir.join("draft.json"), "{}").unwrap();
    // Pre-versioning and other-schema files are unreadable: always removed.
    fs::write(cache_dir.join("aaaa-bbbb.jsonl"), "\n").unwrap();
    fs::write(cache_dir.join("v1-cccc-dddd.jsonl"), "\n").unwrap();
    fs::write(cache_dir.join("v999-eeee-ffff.jsonl"), "\n").unwrap();
    let _c = Cache::open(&cache_dir, "current", "gen");
    let names: Vec<String> = fs::read_dir(&cache_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let mut names = names;
    names.sort();
    assert_eq!(
        names,
        vec![
            "draft.json".to_string(),
            format!("v{SCHEMA}-current-gen.jsonl")
        ]
    );
}

/// An entry made while the options file was missing is not reused once
/// the file is there but unreadable: make fails on such a file, it does
/// not read it as missing.
#[test]
fn an_absent_entry_does_not_answer_for_an_unreadable_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dbdir = tmp.path().join("db");
    fs::create_dir_all(&dbdir).unwrap();
    let mut c = Cache::open(&tmp.path().join("cache"), "tree1", "conf1");
    let i = info("cat/port");
    c.insert(&i, &OptionsFiles::new(&dbdir), std::time::SystemTime::now());
    assert!(c.lookup(&i.key, &OptionsFiles::new(&dbdir)).is_some());
    // A directory where the file should be: there, and unreadable.
    fs::create_dir_all(dbdir.join("cat_port/options")).unwrap();
    assert!(c.lookup(&i.key, &OptionsFiles::new(&dbdir)).is_none());
}

/// `clear` removes every generation, counts files and bytes, and leaves
/// whatever is not one.
#[test]
fn clear_counts_what_it_removes() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("v7-a-b.jsonl"), "12345").unwrap();
    fs::write(tmp.path().join("v6-c-d.jsonl"), "123").unwrap();
    fs::write(tmp.path().join("draft.json"), "{}").unwrap();
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
    fs::create_dir_all(ports.join("Mk")).unwrap();
    fs::write(ports.join("Mk/bsd.port.mk"), "# fake\n").unwrap();
    for (rel, content) in files {
        let path = ports.join(".git").join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
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
    fs::create_dir_all(main.join("Mk")).unwrap();
    fs::write(main.join("Mk/bsd.port.mk"), "").unwrap();
    fs::create_dir_all(over.join("cat/port")).unwrap();
    let makefile = over.join("cat/port/Makefile");
    fs::write(&makefile, "PORTVERSION=1\n").unwrap();
    assert_eq!(trees_key(&main, &[]), trees_key(&main, &[]));
    let before = trees_key(&main, std::slice::from_ref(&over));
    assert_ne!(before, trees_key(&main, &[]));
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
    File::options()
        .write(true)
        .open(&makefile)
        .unwrap()
        .set_modified(later)
        .unwrap();
    assert_ne!(
        trees_key(&main, std::slice::from_ref(&over)),
        before,
        "an edited port is a new generation"
    );
}

/// An overlay that is a git checkout of its own is still edited in
/// place before any commit: its key follows the files, not the HEAD.
#[test]
fn trees_key_follows_an_uncommitted_edit_in_a_git_overlay() {
    let (_tmp, over) = fake_tree(&[
        ("HEAD", "ref: refs/heads/main\n"),
        ("refs/heads/main", &format!("{ID}\n")),
    ]);
    let (_tmp2, main) = fake_tree(&[]);
    fs::create_dir_all(over.join("cat/port")).unwrap();
    let makefile = over.join("cat/port/Makefile");
    fs::write(&makefile, "PORTVERSION=1\n").unwrap();
    let before = trees_key(&main, std::slice::from_ref(&over));
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
    File::options()
        .write(true)
        .open(&makefile)
        .unwrap()
        .set_modified(later)
        .unwrap();
    assert_ne!(
        trees_key(&main, std::slice::from_ref(&over)),
        before,
        "same HEAD, edited port"
    );
}

/// An overlay assembled from symlinked ports: the port is looked up
/// through the link, so an edit behind it must reach the key too.
#[test]
fn trees_key_follows_an_edit_behind_a_symlinked_port() {
    let tmp = tempfile::tempdir().unwrap();
    let (main, over, real) = (
        tmp.path().join("main"),
        tmp.path().join("over"),
        tmp.path().join("real"),
    );
    fs::create_dir_all(&main).unwrap();
    fs::create_dir_all(over.join("cat")).unwrap();
    fs::create_dir_all(&real).unwrap();
    let makefile = real.join("Makefile");
    fs::write(&makefile, "PORTVERSION=1\n").unwrap();
    std::os::unix::fs::symlink(&real, over.join("cat/port")).unwrap();
    let before = trees_key(&main, std::slice::from_ref(&over));
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
    File::options()
        .write(true)
        .open(&makefile)
        .unwrap()
        .set_modified(later)
        .unwrap();
    assert_ne!(
        trees_key(&main, std::slice::from_ref(&over)),
        before,
        "edited behind the link"
    );
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
    let (_tmp, ports) = fake_tree(&[("HEAD", "ref: refs/heads/main\n"), ("packed-refs", &packed)]);
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
    fs::create_dir_all(ports.join("Mk")).unwrap();
    fs::write(ports.join("Mk/bsd.port.mk"), "# fake\n").unwrap();
    let real = ports.join("../real-gitdir");
    fs::create_dir_all(real.join("refs/heads")).unwrap();
    fs::write(real.join("HEAD"), "ref: refs/heads/mine\n").unwrap();
    fs::write(real.join("refs/heads/mine"), format!("{ID}\n")).unwrap();
    fs::write(ports.join(".git"), "gitdir: ../real-gitdir\n").unwrap();
    assert_eq!(tree_key(&ports), ID);

    // Absolute form too.
    fs::write(
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
        vec![
            ("HEAD", "ref: refs/heads/main\n"),
            ("refs/heads/main", "zzz\n"),
        ],
    ] {
        let (_tmp, ports) = fake_tree(&files);
        let key = tree_key(&ports);
        assert!(key.starts_with("mtime-"), "{files:?} gave {key}");
    }

    let (_tmp, ports) = fake_tree(&[]);
    fs::write(ports.join(".git"), "gitdir: /nonexistent/gitdir\n").unwrap();
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
    fs::create_dir_all(tmp.path().join("Mk")).unwrap();
    fs::write(tmp.path().join("Mk/bsd.port.mk"), "# fake\n").unwrap();
    let key = tree_key(tmp.path());
    assert!(key.starts_with("mtime-"), "got {key}");
}

/// Without git, a tree key is mtimes: two of them an hour apart are two
/// trees, never one generation.
#[test]
fn mtime_keys_one_hour_apart_are_distinct_generations() {
    let tmp = tempfile::tempdir().unwrap();
    let dbdir = tmp.path().join("db");
    fs::create_dir_all(&dbdir).unwrap();
    let cache_dir = tmp.path().join("cache");
    let mut c = Cache::open(&cache_dir, "mtime-1792652400", "conf");
    c.insert(
        &info("cat/port"),
        &OptionsFiles::new(&dbdir),
        std::time::SystemTime::now(),
    );
    drop(c);
    let later = Cache::open(&cache_dir, "mtime-1792656000", "conf");
    assert!(
        later
            .lookup(
                &PortKey::parse("cat/port").unwrap(),
                &OptionsFiles::new(&dbdir)
            )
            .is_none()
    );
}

/// Without git, an update that touches MOVED (or UPDATING) but not
/// Mk/bsd.port.mk is still a new tree.
#[test]
fn a_moved_only_change_is_a_new_tree_key() {
    let tmp = tempfile::tempdir().unwrap();
    let ports = tmp.path();
    fs::create_dir_all(ports.join("Mk")).unwrap();
    fs::write(ports.join("Mk/bsd.port.mk"), "# fake\n").unwrap();
    fs::write(ports.join("MOVED"), "").unwrap();
    let set_mtime = |secs: u64| {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        File::options()
            .write(true)
            .open(ports.join("MOVED"))
            .unwrap()
            .set_modified(t)
            .unwrap();
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
    assert_eq!(
        cache_dir_for(Some("/x/cache"), home),
        Path::new("/x/cache/optique")
    );
    for xdg in [None, Some(""), Some("cache"), Some("./cache")] {
        assert_eq!(
            cache_dir_for(xdg, home),
            Path::new("/home/u/.cache/optique"),
            "{xdg:?}"
        );
    }
    assert_eq!(cache_dir_for(None, None), Path::new("/tmp/.cache/optique"));
}

#[test]
fn options_hash_absent_vs_present() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(OptionsFiles::new(tmp.path()).hash("cat_port"), "ABSENT");
    fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    fs::write(tmp.path().join("cat_port/options"), "x\n").unwrap();
    let h = OptionsFiles::new(tmp.path()).hash("cat_port");
    assert_ne!(h, "ABSENT");
    assert_eq!(h.len(), 64);
}

/// A port whose make read `makefiles`.
fn read(origin: &str, makefiles: &[&str]) -> PortInfo {
    PortInfo {
        makefiles: makefiles.iter().map(ToString::to_string).collect(),
        ..info(origin)
    }
}

/// Each kind of change against a slave port that also reads another
/// port's file: what it reads, the directories it owns and their
/// category take it; what make never reads leaves it; a few changes
/// take everything.
#[test]
fn invalidation_follows_what_make_read_and_what_it_probes() {
    let slave = read(
        "cat/slave",
        &[
            "Mk/bsd.port.mk",
            "Mk/Uses/python.mk",
            "cat/master/Makefile",
            "cat/slave/Makefile",
            "lang/python312/Makefile.version",
        ],
    );
    let keeps =
        |status: char, path: &str| Invalidation::of(&[(status, path.to_string())]).keeps(&slave);
    for (status, path) in [
        ('M', "MOVED"),
        ('M', "UIDs"),
        ('M', "Makefile"),
        ('M', ".mailmap"),
        ('A', ".github/workflows/x"),
        ('M', "Tools/scripts/x.sh"),
        ('M', "Keywords/sample.ucl"),
        ('M', "Mk/Scripts/qa.sh"),
        ('A', "Mk/Scripts/new.sh"),
        ('M', "Mk/Uses/qt.mk"),
        ('M', "cat/Makefile"),
        ('M', "lang/python312/pkg-plist"),
        ('M', "other/port/Makefile"),
    ] {
        assert!(keeps(status, path), "{status} {path} must leave the entry");
    }
    for (status, path) in [
        ('M', "Mk/bsd.port.mk"),
        ('M', "Mk/Uses/python.mk"),
        ('M', "lang/python312/Makefile.version"),
        ('M', "cat/slave/Makefile"),
        ('A', "cat/master/pkg-help"),
        ('A', "cat/master/Makefile.local"),
        ('A', "cat/slave/files/patch-x"),
        ('A', "cat/Makefile.inc"),
    ] {
        assert!(!keeps(status, path), "{status} {path} must take the entry");
    }
    for (status, path) in [
        ('A', "Mk/bsd.local.mk"),
        ('A', "Makefile.inc"),
        ('M', "NEWFILE"),
    ] {
        let inv = Invalidation::of(&[(status, path.to_string())]);
        assert_eq!(
            inv.all.as_deref(),
            Some(path),
            "{status} {path} takes everything"
        );
    }
    // An entry that knows nothing of what make read cannot be judged.
    assert!(!Invalidation::of(&[]).keeps(&read("cat/port", &[])));
}

/// A port whose USES named `newuses` before `Mk/Uses/newuses.mk`
/// existed: bsd.port.mk only probed for the file and set ERROR, so its
/// entry's makefiles do not name it. Its arrival takes everything, as
/// does any new `.mk` under `Mk/`; a modified one takes its readers.
#[test]
fn a_new_uses_file_takes_everything() {
    let port = read("cat/port", &["Mk/bsd.port.mk", "cat/port/Makefile"]);
    for path in ["Mk/Uses/newuses.mk", "Mk/Features/new.mk", "Mk/bsd.new.mk"] {
        let inv = Invalidation::of(&[('A', path.to_string())]);
        assert_eq!(inv.all.as_deref(), Some(path), "A {path} takes everything");
        assert!(!inv.keeps(&port), "A {path}");
    }
    assert!(Invalidation::of(&[('M', "Mk/Uses/newuses.mk".into())]).keeps(&port));
    assert!(Invalidation::of(&[('D', "Mk/Uses/old.mk".into())]).keeps(&port));
}

/// A git tree `ports` with a commit, and `clone`, a clone of it, in
/// `tmp`; None when git cannot run here.
fn two_trees(tmp: &Path) -> Option<(PathBuf, PathBuf)> {
    let (ports, clone) = (tmp.join("ports"), tmp.join("clone"));
    for dir in ["Mk", "cat/a", "cat/b"] {
        fs::create_dir_all(ports.join(dir)).unwrap();
    }
    fs::write(ports.join("Mk/bsd.port.mk"), "# v1\n").unwrap();
    fs::write(ports.join("cat/a/Makefile"), "# a1\n").unwrap();
    fs::write(ports.join("cat/b/Makefile"), "# b1\n").unwrap();
    git(&ports, &["init", "-q"])?;
    git(&ports, &["add", "-A"])?;
    git(&ports, &["commit", "-q", "-m", "one"])?;
    git(tmp, &["clone", "-q", "ports", "clone"])?;
    assert_eq!(tree_key(&ports), tree_key(&clone), "one commit");
    Some((ports, clone))
}

/// Two trees at one commit under one make.conf (poudriere's `-p
/// default` and `-p dev`, a clone) are two generations.
#[test]
fn two_trees_at_one_commit_share_no_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((ports, clone)) = two_trees(tmp.path()) else {
        eprintln!("git cannot run here; skipped");
        return;
    };
    let (dbdir, cache_dir) = (tmp.path().join("db"), tmp.path().join("cache"));
    let files = OptionsFiles::new(&dbdir);
    let b = PortKey::parse("cat/b").unwrap();
    let mut c = Cache::open_trees(&cache_dir, &trees(&ports, &[]), "conf");
    c.insert(
        &read("cat/b", &["Mk/bsd.port.mk", "cat/b/Makefile"]),
        &files,
        std::time::SystemTime::now(),
    );
    drop(c);
    let c = Cache::open_trees(&cache_dir, &trees(&clone, &[]), "conf");
    assert!(
        c.lookup(&b, &files).is_none(),
        "the clone does not share the generation"
    );
    drop(c);
    let c = Cache::open_trees(&cache_dir, &trees(&ports, &[]), "conf");
    assert!(
        c.lookup(&b, &files).is_some(),
        "the first tree keeps its own"
    );
}

/// The clone's next commit carries nothing over from the first tree's
/// generation, made at the commit before it under the same make.conf.
#[test]
fn carry_over_needs_the_same_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let Some((ports, clone)) = two_trees(tmp.path()) else {
        eprintln!("git cannot run here; skipped");
        return;
    };
    let (dbdir, cache_dir) = (tmp.path().join("db"), tmp.path().join("cache"));
    let files = OptionsFiles::new(&dbdir);
    let b = PortKey::parse("cat/b").unwrap();
    let mut c = Cache::open_trees(&cache_dir, &trees(&ports, &[]), "conf");
    c.insert(
        &read("cat/b", &["Mk/bsd.port.mk", "cat/b/Makefile"]),
        &files,
        std::time::SystemTime::now(),
    );
    drop(c);

    fs::write(clone.join("cat/a/pkg-help"), "help\n").unwrap();
    git(&clone, &["add", "-A"]).unwrap();
    git(&clone, &["commit", "-q", "-m", "two"]).unwrap();
    let c = Cache::open_trees(&cache_dir, &trees(&clone, &[]), "conf");
    assert_eq!(c.note(), None, "nothing carried over from another tree");
    assert!(c.lookup(&b, &files).is_none());
}

/// git, quietly, in `dir`; None when git cannot be run here.
fn git(dir: &Path, args: &[&str]) -> Option<()> {
    let ok = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "-C",
        ])
        .arg(dir)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?
        .success();
    ok.then_some(())
}

/// Across a commit, a new generation starts with the entries the commit
/// leaves alone and says so; a generation for another make.conf
/// carries nothing over.
#[test]
fn a_new_commit_carries_over_what_its_changes_leave_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let ports = tmp.path().join("ports");
    for dir in ["Mk", "cat/a", "cat/b"] {
        fs::create_dir_all(ports.join(dir)).unwrap();
    }
    fs::write(ports.join("Mk/bsd.port.mk"), "# v1\n").unwrap();
    fs::write(ports.join("cat/a/Makefile"), "# a1\n").unwrap();
    fs::write(ports.join("cat/b/Makefile"), "# b1\n").unwrap();
    if git(&ports, &["init", "-q"])
        .and_then(|()| git(&ports, &["add", "-A"]))
        .is_none()
        || git(&ports, &["commit", "-q", "-m", "one"]).is_none()
    {
        eprintln!("git cannot run here; skipped");
        return;
    }
    let (dbdir, cache_dir) = (tmp.path().join("db"), tmp.path().join("cache"));
    let files = OptionsFiles::new(&dbdir);
    let mut c = Cache::open_trees(&cache_dir, &trees(&ports, &[]), "conf");
    assert_eq!(c.note(), None, "nothing to carry over from");
    c.insert(
        &read("cat/a", &["Mk/bsd.port.mk", "cat/a/Makefile"]),
        &files,
        std::time::SystemTime::now(),
    );
    c.insert(
        &read("cat/b", &["Mk/bsd.port.mk", "cat/b/Makefile"]),
        &files,
        std::time::SystemTime::now(),
    );
    drop(c);

    fs::write(ports.join("cat/a/pkg-help"), "help\n").unwrap();
    git(&ports, &["add", "-A"]).unwrap();
    git(&ports, &["commit", "-q", "-m", "two"]).unwrap();
    let c = Cache::open_trees(&cache_dir, &trees(&ports, &[]), "conf");
    let note = c.note().unwrap();
    assert!(note.contains("1 entries carried over"), "{note}");
    assert!(note.contains("1 dropped by 1 changed paths"), "{note}");
    assert!(
        c.lookup(&PortKey::parse("cat/a").unwrap(), &files)
            .is_none()
    );
    assert!(
        c.lookup(&PortKey::parse("cat/b").unwrap(), &files)
            .is_some()
    );
    drop(c);

    let c = Cache::open_trees(&cache_dir, &trees(&ports, &[]), "other conf");
    assert_eq!(c.note(), None);
    assert!(
        c.lookup(&PortKey::parse("cat/b").unwrap(), &files)
            .is_none()
    );
}

/// A script a parse-time command runs, directly, through a variable,
/// or through `:sh`, counts as a change to the makefile running it; one
/// a target's recipe runs, or a variable no such command uses, does not.
#[test]
fn a_script_run_while_parsing_counts_as_its_makefile() {
    let tmp = tempfile::tempdir().unwrap();
    let mk = tmp.path().join("Mk");
    fs::create_dir_all(mk.join("Uses")).unwrap();
    fs::write(
        mk.join("Uses/rusty.mk"),
        "_AWK=\t${AWK} \\\n\t\t-f${SCRIPTSDIR}/common.awk -f\n\
         .for _a _b in ${:!${_AWK} ${SCRIPTSDIR}/fetch.awk /dev/null!}\n.endfor\n",
    )
    .unwrap();
    let shy = "_V=\t${SH} ${SCRIPTSDIR}/version.sh\nV=\t${_V:sh}\n";
    fs::write(mk.join("Uses/shy.mk"), shy).unwrap();
    fs::write(mk.join("Uses/now.mk"), "X!=\t${SH} ${SCRIPTSDIR}/now.sh\n").unwrap();
    fs::write(
        mk.join("bsd.port.mk"),
        "DO_FETCH=\t${SH} ${SCRIPTSDIR}/do-fetch.sh\n\
         do-fetch:\n\t${SCRIPTSDIR}/qa.sh\n\t${DO_FETCH}\n",
    )
    .unwrap();
    let readers =
        |script: &str| parse_time_readers(tmp.path(), &[('M', format!("Mk/Scripts/{script}"))]);
    assert_eq!(readers("common.awk"), ["Mk/Uses/rusty.mk"]);
    assert_eq!(readers("fetch.awk"), ["Mk/Uses/rusty.mk"]);
    assert_eq!(readers("version.sh"), ["Mk/Uses/shy.mk"]);
    assert_eq!(readers("now.sh"), ["Mk/Uses/now.mk"]);
    assert!(readers("qa.sh").is_empty(), "a recipe line");
    assert!(
        readers("do-fetch.sh").is_empty(),
        "assigned, run only by a recipe"
    );
    // A makefile change alone reads nothing.
    assert!(parse_time_readers(tmp.path(), &[('M', "Mk/Uses/rusty.mk".into())]).is_empty());
}

/// On the host's tree, cargo's awk scripts are the ones run while
/// parsing (their crates add build dependencies), and the scripts the
/// targets run are not.
#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn the_trees_parse_time_scripts_are_cargos() {
    let ports = Path::new("/usr/ports");
    let readers =
        |script: &str| parse_time_readers(ports, &[('M', format!("Mk/Scripts/{script}"))]);
    let cargo = [
        "cargo-crates-git-fetch.awk",
        "cargo-crates-git-common.awk",
        "split-url.awk",
    ];
    for script in cargo {
        assert_eq!(readers(script), ["Mk/Uses/cargo.mk"], "{script}");
    }
    for script in [
        "qa.sh",
        "do-fetch.sh",
        "do-depends.sh",
        "checksum.sh",
        "makesum.sh",
    ] {
        assert!(readers(script).is_empty(), "{script}");
    }
}

/// A file make.conf includes is part of what an entry was made from: the
/// generation it is in keeps it only while that file holds what it held,
/// while entries that never read the file stay.
#[test]
fn an_included_make_conf_change_drops_its_readers() {
    let tmp = tempfile::tempdir().unwrap();
    let dbdir = tmp.path().join("db");
    fs::create_dir_all(&dbdir).unwrap();
    let cache_dir = tmp.path().join("cache");
    let included = tmp.path().join("opts.conf");
    fs::write(&included, "OPTIONS_UNSET+=DOCS\n").unwrap();
    let reader = PortInfo {
        outside_files: vec![included.display().to_string()],
        ..info("cat/reader")
    };
    let other = info("cat/other");
    let files = OptionsFiles::new(&dbdir);
    let mut c = Cache::open(&cache_dir, "tree1", "conf1");
    c.insert(&reader, &files, std::time::SystemTime::now());
    c.insert(&other, &files, std::time::SystemTime::now());

    let c = Cache::open(&cache_dir, "tree1", "conf1");
    assert!(c.lookup(&reader.key, &files).is_some());
    assert_eq!(c.note(), None);

    fs::write(&included, "OPTIONS_UNSET+=NLS\n").unwrap();
    let c = Cache::open(&cache_dir, "tree1", "conf1");
    assert!(
        c.lookup(&reader.key, &files).is_none(),
        "the include changed"
    );
    assert!(
        c.lookup(&other.key, &files).is_some(),
        "it never read the include"
    );
    assert_eq!(
        c.note(),
        Some("cache: 1 entries dropped: a file make.conf includes changed")
    );

    fs::remove_file(&included).unwrap();
    let c = Cache::open(&cache_dir, "tree1", "conf1");
    assert!(
        c.lookup(&reader.key, &files).is_none(),
        "the include is gone"
    );
}

/// A make.conf edit to option policy alone starts the new generation with
/// the entries no changed word reaches, their dumps patched to the new
/// value; one under other host values, or with carrying off, starts empty.
#[test]
fn an_option_edit_carries_over_what_it_cannot_reach() {
    let tmp = tempfile::tempdir().unwrap();
    let dbdir = tmp.path().join("db");
    fs::create_dir_all(&dbdir).unwrap();
    let cache_dir = tmp.path().join("cache");
    let trees = Trees {
        key: "tree".to_string(),
        portsdir: PathBuf::new(),
        commit: None,
        overlays: String::new(),
    };
    // OPTIONS_UNSET as make would expand these plain texts.
    let probe = |text: &str, vars: &[String]| {
        let unset: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("OPTIONS_UNSET+="))
            .collect();
        Some(
            vars.iter()
                .map(|v| {
                    if v == "OPTIONS_UNSET" {
                        unset.join(" ")
                    } else {
                        String::new()
                    }
                })
                .collect(),
        )
    };
    let conf = |hash: &'static str, text: &'static str, rest: &'static str, carry: bool| Conf {
        hash,
        text,
        rest,
        probe: Some(&probe),
        carry,
    };
    let port = |origin: &str, option: &str| {
        let mut i = info(origin);
        i.options.complete = vec![option.to_string()];
        i.options.mc_unset = ["DOCS".to_string()].into();
        i
    };
    let (x11, nls) = (port("x11/port", "X11"), port("misc/nls", "NLS"));
    let files = OptionsFiles::new(&dbdir);
    let mut c = Cache::open_trees(
        &cache_dir,
        &trees,
        conf("a", "OPTIONS_UNSET+=DOCS\n", "r", true),
    );
    c.insert(&x11, &files, std::time::SystemTime::now());
    c.insert(&nls, &files, std::time::SystemTime::now());

    let text = "OPTIONS_UNSET+=DOCS\nOPTIONS_UNSET+=NLS\n";
    let c = Cache::open_trees(&cache_dir, &trees, conf("b", text, "r", true));
    let kept = c
        .lookup(&x11.key, &files)
        .expect("NLS names none of its options");
    assert_eq!(
        kept.options.mc_unset,
        ["DOCS".to_string(), "NLS".to_string()].into()
    );
    assert!(c.lookup(&nls.key, &files).is_none(), "NLS is its option");
    assert_eq!(
        c.note(),
        Some(
            "cache: 1 entries carried over from the make.conf before, 1 dropped by its edit to OPTIONS_UNSET"
        )
    );

    let other_host = Cache::open_trees(
        &cache_dir,
        &trees,
        conf("c", "OPTIONS_UNSET+=X\n", "other", true),
    );
    assert!(other_host.lookup(&x11.key, &files).is_none());
    let off = Cache::open_trees(
        &cache_dir,
        &trees,
        conf("d", "OPTIONS_UNSET+=Y\n", "r", false),
    );
    assert!(off.lookup(&x11.key, &files).is_none());
}

/// A port whose own Makefile runs make at parse time on another port reads
/// that port in a child make `.MAKE.MAKEFILES` never names: carried over no
/// tree change. A `!=` running anything else reads no other port.
#[test]
fn a_port_running_make_at_parse_time_is_not_carried() {
    let tmp = tempfile::tempdir().unwrap();
    let write = |rel: &str, text: &str| {
        let path = tmp.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    write(
        "www/caddy-custom/Makefile",
        "CADDY_VERSION!=\t${MAKE} -C ${.CURDIR}/../caddy  -V '$${DISTVERSION}'\n",
    );
    write(
        "www/plain/Makefile",
        "ARCH!= uname -p\nall:\n\t${MAKE} -C sub -V X\n",
    );
    let info = |dir: &str| PortInfo {
        makefiles: vec![format!("{dir}/Makefile"), "Mk/bsd.port.mk".to_string()],
        ..PortInfo::stub(dir)
    };
    let mut seen = HashMap::new();
    assert!(runs_make_at_parse(
        tmp.path(),
        &info("www/caddy-custom"),
        &mut seen
    ));
    assert!(!runs_make_at_parse(
        tmp.path(),
        &info("www/plain"),
        &mut seen
    ));
    assert!(runs_make(
        "${MAKE_CMD} -C ../claws-mail-${_addon} -V IGNORE"
    ));
    assert!(!runs_make("${SED} -n 1p ${.CURDIR}/version"));
}

/// A make.conf a local build reads in place, edited during the run: the
/// answers made after it are not stored under the generation of the text
/// it had at startup.
#[test]
fn a_watched_make_conf_edited_mid_run_stops_storing() {
    let tmp = tempfile::tempdir().unwrap();
    let dbdir = tmp.path().join("db");
    fs::create_dir_all(&dbdir).unwrap();
    let conf = tmp.path().join("make.conf");
    fs::write(&conf, "OPTIONS_UNSET+=DOCS\n").unwrap();
    let cache_dir = tmp.path().join("cache");
    let files = OptionsFiles::new(&dbdir);

    let mut c = Cache::open(&cache_dir, "tree1", "conf1");
    c.watch(std::slice::from_ref(&conf));
    c.insert(&info("cat/before"), &files, std::time::SystemTime::now());
    fs::write(&conf, "OPTIONS_UNSET+=DOCS NLS\n").unwrap();
    c.insert(&info("cat/after"), &files, std::time::SystemTime::now());
    drop(c);

    let c = Cache::open(&cache_dir, "tree1", "conf1");
    let key = |o: &str| PortKey::parse(o).unwrap();
    assert!(c.lookup(&key("cat/before"), &files).is_some());
    assert!(c.lookup(&key("cat/after"), &files).is_none());
}

/// An options file written while its port's query ran may hold other bytes
/// than make read: the answer is not stored under them.
#[test]
fn an_options_file_written_during_the_query_is_not_cached() {
    let tmp = tempfile::tempdir().unwrap();
    let dbdir = tmp.path().join("db");
    fs::create_dir_all(dbdir.join("cat_port")).unwrap();
    let since = std::time::SystemTime::now();
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(dbdir.join("cat_port/options"), "rewritten\n").unwrap();
    let files = OptionsFiles::new(&dbdir);
    let mut c = Cache::open(&tmp.path().join("cache"), "tree1", "conf1");
    c.insert(&info("cat/port"), &files, since);
    assert!(
        c.lookup(&PortKey::parse("cat/port").unwrap(), &files)
            .is_none()
    );
}

/// A generation whose last line was cut short gets it ended before the
/// next entry is appended, which then parses.
#[test]
fn an_unterminated_line_does_not_swallow_the_next() {
    let tmp = tempfile::tempdir().unwrap();
    let dbdir = tmp.path().join("db");
    fs::create_dir_all(&dbdir).unwrap();
    let cache_dir = tmp.path().join("cache");
    let files = OptionsFiles::new(&dbdir);
    drop(Cache::open(&cache_dir, "tree1", "conf1"));
    let generation = fs::read_dir(&cache_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .unwrap();
    let mut f = OpenOptions::new().append(true).open(&generation).unwrap();
    f.write_all(b"{\"cut\": ").unwrap();
    drop(f);
    let mut c = Cache::open(&cache_dir, "tree1", "conf1");
    c.insert(&info("cat/port"), &files, std::time::SystemTime::now());
    drop(c);
    let c = Cache::open(&cache_dir, "tree1", "conf1");
    assert!(
        c.lookup(&PortKey::parse("cat/port").unwrap(), &files)
            .is_some()
    );
}
