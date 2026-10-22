//! Hermetic binary-level tests: no ports tree, no make, no network.

use std::process::Command;

fn optique() -> Command {
    Command::new(env!("CARGO_BIN_EXE_optique"))
}

#[test]
fn help_mentions_every_command_and_global_flag() {
    let out = optique().arg("--help").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for needle in [
        "tui", "scan", "sync", "clean", "decide", "--dry-run", "--verbose", "--file",
        "--no-cache", "--quiet", "--color",
    ] {
        assert!(text.contains(needle), "--help must mention {needle}\n{text}");
    }
}

#[test]
fn subcommand_help_shows_specific_flags() {
    let out = optique().args(["clean", "--help"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("--redundant"));
    assert!(text.contains("--unused"));
    assert!(text.contains("--dry-run"));
}

/// `tui --drive` is the headless debugging driver. Its help text is all that
/// can be checked hermetically: driving it needs a ports tree to scan first.
#[test]
fn tui_help_documents_the_drive_flag() {
    let out = optique().args(["tui", "--help"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("--drive"), "tui --help must mention --drive\n{text}");
}

/// `decide` parses its plan before touching a ports tree, so every bad-plan
/// message is reachable hermetically.
#[test]
fn decide_rejects_a_bad_plan_before_scanning() {
    use std::io::Write as _;
    for (plan, needle) in [
        ("", "empty plan"),
        ("not json", "valid JSON"),
        ("[]", "must be a JSON object"),
        (r#"{"nginx": {"A": true}}"#, "not a port origin"),
        (r#"{"www/nginx": {"A": "yes"}}"#, "expected true or false"),
        (r#"{"www/nginx": {}}"#, "no options given"),
    ] {
        let mut child = optique()
            .args(["decide", "-n"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.as_mut().unwrap().write_all(plan.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(!out.status.success(), "{plan:?} must be refused");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(needle), "{plan:?} -> {err:?} must mention {needle:?}");
    }
}

#[test]
fn decide_help_describes_the_plan() {
    let out = optique().args(["decide", "--help"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for needle in ["stdin", "JSON"] {
        assert!(text.contains(needle), "decide --help must mention {needle}\n{text}");
    }
}

/// `--options` only shapes the JSON, so asking for it without --json is a
/// usage error rather than a silent no-op.
#[test]
fn scan_options_requires_json() {
    let out = optique().args(["scan", "--options", "www/nginx"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--json"), "{err}");
}

#[test]
fn sync_and_clean_advertise_json() {
    for cmd in ["sync", "clean"] {
        let out = optique().args([cmd, "--help"]).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("--json"), "{cmd} --help must mention --json\n{text}");
    }
}

#[test]
fn clean_unused_requires_a_list() {
    // No origins, no -f: there is nothing to compute a closure from, and the
    // check must fire before any ports tree is touched.
    let out = optique().args(["clean", "--unused"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("package list"), "{err}");
}

#[test]
fn clean_origins_require_unused() {
    // Plain clean walks the whole options dir; a list would be silently
    // ignored, so it is an error instead.
    let out = optique().args(["clean", "ports-mgmt/pkg"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--unused"), "{err}");
}

#[test]
fn scan_help_advertises_json() {
    let out = optique().args(["scan", "--help"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("--json"), "scan --help must mention --json\n{text}");
    // The marker column is the one thing --color affects today.
    assert!(text.contains("--color"), "scan --help must mention --color\n{text}");
}

/// -s selects the synth(1) layout, -z/-j the poudriere one: asking for both is
/// a usage error, caught by clap before any tree is touched.
#[test]
fn synth_conflicts_with_poudriere_flags() {
    for args in [
        vec!["-s", "-z", "ws", "scan", "foo/bar"],
        vec!["--synth", "LiveSystem", "-j", "141amd64", "scan", "foo/bar"],
        vec!["-s", "-p", "default", "scan", "foo/bar"],
        // Straddling the subcommand, which clap's own checks let through.
        vec!["-s", "scan", "-z", "ws", "foo/bar"],
        vec!["scan", "-j", "141amd64", "--synth=LiveSystem", "foo/bar"],
    ] {
        let out = optique().args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?} is a usage error");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("--synth"), "{args:?}: {err}");
        assert!(err.contains("cannot be used with"), "{args:?}: {err}");
    }
}

/// -l is a local build: no poudriere or synth flag goes with it, and -p (a
/// poudriere tree name) does not go with -P (a directory) either.
#[test]
fn local_and_portsdir_conflicts() {
    for (args, flag) in [
        (vec!["-l", "-z", "ws", "scan", "foo/bar"], "--local"),
        (vec!["--local", "-j", "141amd64", "scan", "foo/bar"], "--local"),
        (vec!["-l", "-p", "default", "scan", "foo/bar"], "--local"),
        (vec!["-l", "-s", "scan", "foo/bar"], "--local"),
        (vec!["-p", "default", "-P", "/usr/ports", "scan", "foo/bar"], "--portsdir"),
        // Straddling the subcommand, which clap's own checks let through.
        (vec!["-l", "scan", "-s", "foo/bar"], "--local"),
        (vec!["-l", "scan", "-z", "ws", "foo/bar"], "--local"),
        (vec!["scan", "-p", "default", "--local", "foo/bar"], "--local"),
        (vec!["-p", "default", "scan", "-P", "/usr/ports", "foo/bar"], "--portsdir"),
        (vec!["-v", "scan", "-q", "foo/bar"], "--quiet"),
    ] {
        let out = optique().args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?} is a usage error");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(flag), "{args:?}: {err}");
        assert!(err.contains("cannot be used with"), "{args:?}: {err}");
    }
}

/// --repo needs -s wherever each one sits, and is refused without it.
#[test]
fn repo_requires_synth_on_either_side_of_the_subcommand() {
    // Accepted: the run goes on to refuse --repo with ports given, a
    // check made after the options were parsed, and nothing is scanned.
    for args in [
        vec!["-s", "scan", "--repo", "Synth", "foo/bar"],
        vec!["--repo", "Synth", "scan", "-s", "foo/bar"],
    ] {
        let out = optique().args(&args).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("only used when no ports are given"), "{args:?}: {err}");
    }
    let out = optique().args(["scan", "--repo", "Synth", "foo/bar"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("'--repo' requires '--synth'"), "{err}");
}

/// --repo only filters the implicit installed-package list, so every path
/// that cannot use that list refuses it before doing anything: bare
/// origins, -f, decide (its plan names the ports) and clean without
/// --unused (which consults no list). -n and the scratch directories keep
/// a run that gets past the check from touching the host.
#[test]
fn repo_with_explicit_ports_is_refused_everywhere() {
    let tmp = tempfile::tempdir().unwrap();
    let options = tmp.path().join("options");
    let ports = tmp.path().join("ports");
    for sub in [
        vec!["foo/bar"],
        vec!["-f", "/nonexistent/optique-list"],
        vec!["scan", "foo/bar"],
        vec!["sync", "-f", "/nonexistent/optique-list"],
        vec!["decide"],
        vec!["clean"],
        vec!["clean", "--redundant"],
        vec!["clean", "--unused", "foo/bar"],
    ] {
        let out = optique()
            .env("XDG_CACHE_HOME", tmp.path())
            .args(["-s", "--repo", "Synth", "-n", "-o"])
            .arg(&options)
            .arg("-P")
            .arg(&ports)
            .args(&sub)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(3), "{sub:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("only used when no ports are given"), "{sub:?}: {err}");
    }
}

/// `-s` alone must not swallow the subcommand as its profile name: this has to
/// reach `scan` and fail on the origin, not open the TUI.
#[test]
fn synth_without_a_profile_keeps_the_subcommand() {
    let out = optique().args(["-s", "scan", "not-an-origin"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not-an-origin"), "{err}");
}

#[test]
fn synth_is_a_global_flag() {
    let out = optique().args(["scan", "--help"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("--synth"), "scan --help must mention --synth\n{text}");
}

#[test]
fn color_is_rejected_unless_it_names_a_mode() {
    let out = optique().args(["--color", "sometimes", "scan", "www/nginx"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("sometimes"), "{err}");
}

#[test]
fn quiet_is_a_global_flag() {
    // -Q must be accepted before the subcommand and must not swallow errors:
    // the malformed origin is still reported on stderr.
    let out = optique().args(["-q", "scan", "not-an-origin"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not-an-origin"), "{err}");
}

#[test]
fn clear_cache_alone_clears_and_exits() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("optique");
    std::fs::create_dir_all(cache.join("drafts")).unwrap();
    std::fs::write(cache.join("v3-x-y.jsonl"), "{}\n").unwrap();
    std::fs::write(cache.join("drafts/keep.json"), "{}\n").unwrap();
    let out = optique()
        .env("XDG_CACHE_HOME", tmp.path())
        .arg("--clear-cache")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!cache.join("v3-x-y.jsonl").exists(), "generation file must be removed");
    assert!(cache.join("drafts/keep.json").exists(), "drafts must survive");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cache cleared"), "{err}");
}

#[test]
fn malformed_origin_is_a_clean_error() {
    let out = optique().args(["scan", "not-an-origin"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not-an-origin"), "{err}");
}

#[test]
fn no_arguments_is_a_helpful_error() {
    // The hint names no build-specific flag: -z would be refused with -l.
    for args in [vec![], vec!["-l"]] {
        let out = optique().args(&args).output().unwrap();
        assert!(!out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("no ports given"), "{args:?}: {err}");
        assert!(!err.contains("-z"), "{args:?}: {err}");
    }
}

#[test]
fn missing_list_file_is_reported_with_its_path() {
    let out = optique().args(["scan", "-f", "/nonexistent/pkglist"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("/nonexistent/pkglist"), "{err}");
}

#[test]
fn man_page_is_valid_mdoc() {
    // mandoc exits nonzero for mere STYLE/WARNING output too (e.g. "referenced
    // manual not found" for poudriere(8) on hosts without it installed), so the
    // assertion is on the absence of ERROR/UNSUPP diagnostics, not on the exit
    // status. Skipped where mandoc is not available.
    let man = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("man").join("optique.8");
    assert!(man.is_file(), "{} must exist", man.display());

    let out = match Command::new("mandoc").args(["-T", "lint"]).arg(&man).output() {
        Ok(out) => out,
        Err(_) => return, // no mandoc on this host
    };
    let text =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    for bad in ["ERROR:", "UNSUPP:", "SYSERR:"] {
        assert!(!text.contains(bad), "mandoc reported {bad} on man/optique.8:\n{text}");
    }
}

/// A tagged list whose tag column is wider than its content leaves a few
/// columns for the text and renders as a ragged word-per-line mess. mandoc
/// says nothing about it, so the widths are checked here: the FILES section
/// once asked for 59 columns and became unreadable.
#[test]
fn man_page_tag_lists_leave_room_for_their_text() {
    // An 80-column page indents the body by width + 5, so anything past this
    // leaves less than half the line for prose.
    const MAX: usize = 24;
    let man = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("man").join("optique.8");
    let text = std::fs::read_to_string(&man).unwrap();
    for (n, line) in text.lines().enumerate() {
        if !line.starts_with(".Bl ") {
            continue;
        }
        let Some(rest) = line.split_once("-width ").map(|(_, r)| r) else { continue };
        // Either a quoted sample string, or a scaling unit like Ds / 16n.
        let width = match rest.strip_prefix('"') {
            Some(quoted) => quoted.split('"').next().unwrap_or("").chars().count(),
            None => {
                let word = rest.split_whitespace().next().unwrap_or("");
                match word.strip_suffix('n').and_then(|d| d.parse::<usize>().ok()) {
                    Some(cols) => cols,
                    // Ds and the other symbolic widths are small by definition.
                    None => continue,
                }
            }
        };
        assert!(
            width <= MAX,
            "man/optique.8:{}: -width of {width} columns leaves too little room:\n{line}",
            n + 1
        );
    }
}

#[test]
fn man_page_documents_every_subcommand_and_flag() {
    // Cheap guard against the man page drifting from the CLI surface.
    let man = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("man").join("optique.8");
    let text = std::fs::read_to_string(&man).unwrap();
    for needle in [
        "tui", "scan", "sync", "clean", "-json", "-redundant", "-unused", "-no-cache",
        "-dry-run", "-options-dir", "-color", "NO_COLOR", "EXIT STATUS",
        "POUDRIERE INTEGRATION", "-synth", "SYNTH", "Dependency loops",
        "-local", "LOCAL BUILDS", "-portsdir", "PORTSDIR", "PORT_DBDIR",
        "loops_blocking", "decide", "-options", "MACHINE INTERFACE",
    ] {
        assert!(text.contains(needle), "man/optique.8 must document {needle}");
    }
}

#[test]
fn tui_refuses_without_terminal_before_scanning() {
    // Origins are given, stdin/stdout are pipes -> must fail fast with the
    // terminal message, not attempt a scan.
    let out = optique().args(["tui", "ports-mgmt/pkg"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("real terminal"), "{err}");
}

/// A run that fails outright exits 3, never 1: scan and decide answer
/// "something is left to do" with 1, and a gate must tell the two apart.
/// Bad usage stays clap's 2.
#[test]
fn errors_exit_3_not_the_gates_1() {
    let tmp = tempfile::tempdir().unwrap();
    for sub in ["scan", "sync"] {
        let out = optique()
            .env("XDG_CACHE_HOME", tmp.path())
            .args([sub, "-o"])
            .arg(tmp.path().join("options"))
            .args(["-f", "/nonexistent/optique-list"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(3), "{sub}: {}", String::from_utf8_lossy(&out.stderr));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.starts_with("Error: cannot read list file"), "{stderr}");
    }
    let out = optique().args(["scan", "--no-such-flag"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

/// A reader that goes away (`optique clean -n | head -0`) ends the run with
/// the documented error status, quietly, never with a panic and Rust's 101.
/// The pipe's read end is closed before the child starts, so the first
/// write to stdout fails every time. clean -n over a scratch options dir
/// holding one vanished port is a hermetic path that prints: the listing,
/// and the JSON report.
#[test]
fn closed_stdout_exits_documented_status() {
    let tmp = tempfile::tempdir().unwrap();
    let options = tmp.path().join("options");
    let ports = tmp.path().join("ports");
    std::fs::create_dir_all(options.join("www_gone")).unwrap();
    std::fs::write(options.join("www_gone/options"), "").unwrap();
    std::fs::create_dir_all(ports.join("Mk")).unwrap();
    std::fs::write(ports.join("Mk/bsd.port.mk"), "").unwrap();
    for json in [false, true] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let mut cmd = optique();
        cmd.env("XDG_CACHE_HOME", tmp.path())
            .args(["-l", "-n", "-o"])
            .arg(&options)
            .arg("-P")
            .arg(&ports)
            .arg("clean");
        if json {
            cmd.arg("--json");
        }
        let out = cmd.stdout(writer).stderr(std::process::Stdio::piped()).output().unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(3), "json={json}: {err}");
        assert!(!err.contains("panicked"), "json={json}: {err}");
        assert!(!err.contains("Error:"), "a closed stdout is not worth a message: {err}");
    }
}
