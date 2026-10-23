//! End-to-end tests against tests/ports, a handful of minimal ports laid
//! over the host's real framework (/usr/ports/Mk), each shaped after a real
//! port optique got wrong: the queries, the scan, sync and decide run as
//! they do on a full tree, with bsd.options.mk deciding what make sees.
//! Run with: cargo test --test fixture -- --ignored
//!
//! The ports: misc/cond (a SINGLE group named after an option, as
//! mail/faces), misc/slavey (a flavor turning an option on through
//! OPTIONS_SLAVE, as databases/sqlite3@icu), misc/flav (three flavors
//! sharing one file, as editors/emacs), misc/forced (a SINGLE group whose
//! make.conf forces a member on), and a ports-mgmt/pkg stub every port
//! depends on.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A ports tree in `tmp`: the host's Mk, Templates and Keywords linked in,
/// tests/ports copied over them.
fn tree(tmp: &Path) -> PathBuf {
    let tree = tmp.join("ports");
    fs::create_dir_all(&tree).unwrap();
    for dir in ["Mk", "Templates", "Keywords"] {
        std::os::unix::fs::symlink(Path::new("/usr/ports").join(dir), tree.join(dir)).unwrap();
    }
    copy_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ports"), &tree);
    tree
}

fn copy_dir(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&target).unwrap();
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// One fixture run: its tree, its options dir, its make.conf (empty unless
/// a test writes it), its cache.
struct Fixture {
    _tmp: tempfile::TempDir,
    tree: PathBuf,
    options: PathBuf,
    make_conf: PathBuf,
    cache: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let tree = tree(tmp.path());
        let options = tmp.path().join("options");
        fs::create_dir_all(&options).unwrap();
        let make_conf = tmp.path().join("make.conf");
        fs::write(&make_conf, "").unwrap();
        let cache = tmp.path().join("cache");
        Fixture { tree, options, make_conf, cache, _tmp: tmp }
    }

    /// `optique <action> -l -P <tree> -o <options>`, the host's make.conf
    /// replaced by the fixture's.
    fn optique(&self, action: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_optique"));
        c.env("XDG_CACHE_HOME", &self.cache).env("__MAKE_CONF", &self.make_conf);
        c.args([action, "-l", "-P"]).arg(&self.tree).arg("-o").arg(&self.options);
        c
    }

    fn run(&self, action: &str, args: &[&str]) -> std::process::Output {
        let out = self.optique(action).args(args).output().unwrap();
        let code = out.status.code().expect("optique must exit, not be signalled");
        assert!(code == 0 || code == 1, "{action} {args:?} exited {code}: {}", stderr(&out));
        out
    }

    fn file(&self, options_name: &str) -> PathBuf {
        self.options.join(options_name).join("options")
    }

    fn write_file(&self, options_name: &str, text: &str) {
        fs::create_dir_all(self.options.join(options_name)).unwrap();
        fs::write(self.file(options_name), text).unwrap();
    }

    fn decide(&self, args: &[&str], plan: &str) -> serde_json::Value {
        use std::io::Write as _;
        let mut child = self
            .optique("decide")
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.as_mut().unwrap().write_all(plan.as_bytes()).unwrap();
        json(&child.wait_with_output().unwrap())
    }

    /// `scan --json --options` for these ports, and the one named `port`.
    fn scan_port(&self, args: &[&str], port: &str) -> serde_json::Value {
        let mut all = vec!["--json", "--options"];
        all.extend_from_slice(args);
        let report = json(&self.run("scan", &all));
        report["ports"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["port"] == port)
            .unwrap_or_else(|| panic!("{port} missing from {report}"))
            .clone()
    }
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn json(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_str(&stdout(out))
        .unwrap_or_else(|e| panic!("not JSON ({e}):\n{}\n{}", stdout(out), stderr(out)))
}

fn option<'a>(port: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    port["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == name)
        .unwrap_or_else(|| panic!("{name} missing from {port}"))
}

/// A SINGLE group named after an option is checked only while that option
/// is on: misc/cond with AUDIO off is no conflict, and under -m a file that
/// only repeats its defaults goes.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_conditional_group_whose_option_is_off_is_no_conflict() {
    let f = Fixture::new();
    let port = f.scan_port(&["misc/cond"], "misc/cond");
    assert_ne!(port["status"], "conflict", "{port}");
    assert_eq!(port["violations"], serde_json::json!([]), "{port}");

    f.run("sync", &["misc/cond"]);
    assert!(f.file("misc_cond").exists(), "sync writes the defaults");
    f.run("sync", &["-m", "misc/cond"]);
    assert!(!f.file("misc_cond").exists(), "-m removes a file repeating the defaults");
}

/// An OPTIONS_SLAVE option is on with or without a file: misc/slavey@icu
/// needs no file under -m, and one repeating the defaults of both flavors
/// goes. With @icu alone it stays: its ICU line is the default flavor's,
/// which @icu, having ICU out of its list, cannot vouch for.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn an_options_slave_flavor_needs_no_file_under_minimal() {
    let f = Fixture::new();
    let out = f.run("sync", &["-m", "-n", "misc/slavey@icu"]);
    assert!(!stdout(&out).contains("new file"), "{}", stdout(&out));

    f.run("sync", &["misc/slavey"]);
    assert!(f.file("misc_slavey").exists());
    f.run("sync", &["-m", "misc/slavey@icu"]);
    assert!(f.file("misc_slavey").exists(), "@icu alone keeps the default flavor's ICU line");
    f.run("sync", &["-m", "misc/slavey", "misc/slavey@icu"]);
    assert!(!f.file("misc_slavey").exists(), "the file repeats both flavors' defaults");
}

/// A non-default flavor alone, or two of them, never lose the default
/// flavor's choices in the shared file, and an edit only one of them sees
/// is written.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn non_default_flavors_keep_the_default_flavors_choices() {
    let f = Fixture::new();
    // The default flavor's file, X11 turned off.
    let plan = f.decide(&["misc/flav"], r#"{"misc/flav": {"X11": false}}"#);
    assert_eq!(plan["applied"], true, "{plan}");
    let with_x11_off = fs::read_to_string(f.file("misc_flav")).unwrap();
    assert!(with_x11_off.contains("OPTIONS_FILE_UNSET+=X11"), "{with_x11_off}");

    // @nox alone: nothing it sees changed, nothing written, nothing removed.
    for args in [&["misc/flav@nox"][..], &["-m", "misc/flav@nox"][..]] {
        f.run("sync", args);
        assert_eq!(fs::read_to_string(f.file("misc_flav")).unwrap(), with_x11_off, "sync {args:?}");
    }

    // @nox and @pgtk: Y, which only @pgtk has, turned off and written; X11,
    // which neither has, kept as the default flavor left it.
    let plan = f.decide(&["misc/flav@nox", "misc/flav@pgtk"], r#"{"misc/flav@pgtk": {"Y": false}}"#);
    assert_eq!(plan["applied"], true, "{plan}");
    let text = fs::read_to_string(f.file("misc_flav")).unwrap();
    assert!(text.contains("OPTIONS_FILE_UNSET+=Y"), "{text}");
    assert!(text.contains("OPTIONS_FILE_UNSET+=X11"), "{text}");
}

/// A SINGLE group with a member forced on by make.conf and the other chosen
/// in the file is a conflict, and turning the chosen one off settles it.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_forced_member_conflict_is_fixed_from_the_file() {
    let f = Fixture::new();
    fs::write(&f.make_conf, "misc_forced_SET_FORCE=A\n").unwrap();
    f.write_file(
        "misc_forced",
        "_OPTIONS_READ=forced-1.0\n_FILE_COMPLETE_OPTIONS_LIST=A B\n\
         OPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_SET+=B\n",
    );
    let port = f.scan_port(&["misc/forced"], "misc/forced");
    assert_eq!(port["status"], "conflict", "{port}");

    let plan = f.decide(&["misc/forced"], r#"{"misc/forced": {"B": false}}"#);
    assert_eq!(plan["applied"], true, "{plan}");
    assert_eq!(plan["rejected"], serde_json::json!([]), "{plan}");
    let port = f.scan_port(&["misc/forced"], "misc/forced");
    assert_ne!(port["status"], "conflict", "{port}");
}

/// A make.conf with a Latin-1 comment is read as make reads it, not taken
/// for none.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_latin1_make_conf_is_read() {
    let f = Fixture::new();
    fs::write(&f.make_conf, b"# r\xe9glages\nmisc_cond_UNSET+=DOCS\n").unwrap();
    let port = f.scan_port(&["misc/cond"], "misc/cond");
    let docs = option(&port, "DOCS");
    assert_eq!(docs["makeconf"]["value"], false, "{docs}");
    assert_eq!(docs["makeconf"]["scope"], "port", "{docs}");
    assert_eq!(docs["staged"], false, "{docs}");
}
