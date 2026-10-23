//! Shared by the fixture tests: tests/ports laid over the host's
//! /usr/ports/Mk in a temp dir, and the binary run against it.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A ports tree in `tmp`: the host's Mk, Templates and Keywords linked in,
/// tests/ports copied over them.
pub fn tree(tmp: &Path) -> PathBuf {
    let tree = tmp.join("ports");
    fs::create_dir_all(&tree).unwrap();
    for dir in ["Mk", "Templates", "Keywords"] {
        std::os::unix::fs::symlink(Path::new("/usr/ports").join(dir), tree.join(dir)).unwrap();
    }
    copy_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ports"), &tree);
    tree
}

pub fn copy_dir(from: &Path, to: &Path) {
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
pub struct Fixture {
    _tmp: tempfile::TempDir,
    pub tree: PathBuf,
    pub options: PathBuf,
    pub make_conf: PathBuf,
    pub cache: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
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
    pub fn optique(&self, action: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_optique"));
        c.env("XDG_CACHE_HOME", &self.cache).env("__MAKE_CONF", &self.make_conf);
        c.args([action, "-l", "-P"]).arg(&self.tree).arg("-o").arg(&self.options);
        c
    }

    pub fn run(&self, action: &str, args: &[&str]) -> std::process::Output {
        let out = self.optique(action).args(args).output().unwrap();
        let code = out.status.code().expect("optique must exit, not be signalled");
        assert!(code == 0 || code == 1, "{action} {args:?} exited {code}: {}", stderr(&out));
        out
    }

    pub fn file(&self, options_name: &str) -> PathBuf {
        self.options.join(options_name).join("options")
    }

    pub fn write_file(&self, options_name: &str, text: &str) {
        fs::create_dir_all(self.options.join(options_name)).unwrap();
        fs::write(self.file(options_name), text).unwrap();
    }

    pub fn decide(&self, args: &[&str], plan: &str) -> serde_json::Value {
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
    pub fn scan_port(&self, args: &[&str], port: &str) -> serde_json::Value {
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

pub fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

pub fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn json(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_str(&stdout(out))
        .unwrap_or_else(|e| panic!("not JSON ({e}):\n{}\n{}", stdout(out), stderr(out)))
}

pub fn option<'a>(port: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    port["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == name)
        .unwrap_or_else(|| panic!("{name} missing from {port}"))
}
