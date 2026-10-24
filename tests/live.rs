//! Live end-to-end tests against the host's /usr/ports.
//! Run with: cargo test --test live -- --ignored
//!
//! They never touch real configuration: options dirs are tempdirs (-o) and
//! the query cache is redirected via XDG_CACHE_HOME. Nor do they read the
//! host's poudriere trees: -P names /usr/ports whatever poudriere's
//! `default` points at.

use std::fs;
use std::process::Command;

fn optique(cache: &std::path::Path, action: &str) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_optique"));
    c.env("XDG_CACHE_HOME", cache);
    // The action first, its options after it.
    c.args([action, "-P", "/usr/ports"]);
    c
}

/// `scan`'s exit status is a gate, not a verdict on the run: 0 = nothing
/// pending, 1 = a human decision is pending (the usual outcome against a
/// fresh options dir). Any other code means the scan itself broke.
fn scan_exit_code(out: &std::process::Output) -> i32 {
    let code = out.status.code().expect("scan must exit, not be signalled");
    assert!(
        code == 0 || code == 1,
        "scan errored with exit {code}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    code
}

#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn scan_smoke_and_cache_warmup() {
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    fs::create_dir_all(&optdir).unwrap();

    let run = || {
        let out = optique(tmp.path(), "scan")
            .arg("-o")
            .arg(&optdir)
            .arg("ports-mgmt/pkg")
            .output()
            .unwrap();
        scan_exit_code(&out);
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };

    let (stdout, _) = run();
    assert!(stdout.contains("ports-mgmt/pkg"), "{stdout}");
    // Second run must be served entirely from the cache.
    let (_, stderr) = run();
    assert!(stderr.contains(" 0 queried"), "warm scan should not invoke make: {stderr}");
}

#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn scan_json_and_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    fs::create_dir_all(&optdir).unwrap();

    let out = optique(tmp.path(), "scan")
        .args(["--json", "-o"])
        .arg(&optdir)
        .arg("ports-mgmt/pkg")
        .output()
        .unwrap();
    let code = scan_exit_code(&out);

    // stdout must be nothing but the report object.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let report: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("stdout is not JSON ({e}):\n{stdout}"));
    let ports = report["ports"].as_array().expect("ports must be an array");
    let pkg = ports
        .iter()
        .find(|p| p["port"] == "ports-mgmt/pkg")
        .unwrap_or_else(|| panic!("ports-mgmt/pkg missing from {stdout}"));
    assert!(pkg["pkgname"].as_str().unwrap().starts_with("pkg-"), "{pkg}");
    assert!(pkg["undecided"].is_array() && pkg["added"].is_array() && pkg["removed"].is_array());
    assert!(report["options_dir"].as_str().unwrap().contains("options"));

    // The gate: attention > 0 <=> exit 1. pkg's DOCS is normally undecided
    // (no make.conf stance) so this is usually the exit-1 side, but the
    // assertion holds either way.
    assert!(report["summary"]["attention"].is_u64(), "summary.attention must be a number");
    let pending =
        report["summary"]["pending"].as_u64().expect("summary.pending must be a number");
    assert_eq!(
        pending > 0,
        code == 1,
        "exit {code} contradicts summary.pending {pending}"
    );
}

/// The gate itself: each status counts as it should, the exit status
/// follows, and the table says what the JSON says. Built from states that
/// are definite whatever this host's make.conf decides: a conflict, a stale
/// file, and the same port once synced.
#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn scan_gate_counts_each_status() {
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    // nginx: both members of its GSSAPI radio group on.
    fs::create_dir_all(optdir.join("www_nginx")).unwrap();
    fs::write(
        optdir.join("www_nginx/options"),
        "OPTIONS_FILE_SET+=GSSAPI_HEIMDAL\nOPTIONS_FILE_SET+=GSSAPI_MIT\n",
    )
    .unwrap();
    // pkg: the file knows an option the port lost and misses one it has.
    fs::create_dir_all(optdir.join("ports-mgmt_pkg")).unwrap();
    fs::write(
        optdir.join("ports-mgmt_pkg/options"),
        "_OPTIONS_READ=pkg-1.0\n_FILE_COMPLETE_OPTIONS_LIST=OBSOLETE\nOPTIONS_FILE_SET+=OBSOLETE\n",
    )
    .unwrap();
    let scan = |json: bool| {
        let mut c = optique(tmp.path(), "scan");
        if json {
            c.arg("--json");
        }
        c.arg("-o").arg(&optdir).args(["www/nginx", "ports-mgmt/pkg"]).output().unwrap()
    };
    let row = |report: &serde_json::Value, port: &str| {
        report["ports"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["port"] == port)
            .unwrap_or_else(|| panic!("{port} missing from {report}"))
            .clone()
    };
    // The gate recomputed from the rows: a conflict always counts, missing
    // or outdated configuration only when something is undecided.
    let attention = |report: &serde_json::Value| {
        report["ports"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| !p["blacklisted"].as_bool().unwrap())
            .filter(|p| match p["status"].as_str().unwrap() {
                "conflict" => true,
                "unconfigured" | "stale" => !p["undecided"].as_array().unwrap().is_empty(),
                _ => false,
            })
            .count() as u64
    };

    let out = scan(true);
    let report = parse_json(&out);
    assert_eq!(row(&report, "www/nginx")["status"], "conflict");
    let pkg = row(&report, "ports-mgmt/pkg");
    assert_eq!(pkg["status"], "stale", "{pkg}");
    assert_eq!(pkg["added"], serde_json::json!(["DOCS"]), "{pkg}");
    assert_eq!(pkg["removed"], serde_json::json!(["OBSOLETE"]), "{pkg}");
    let summary = &report["summary"];
    assert_eq!(summary["attention"].as_u64().unwrap(), attention(&report), "{summary}");
    assert!(summary["conflict"].as_u64().unwrap() >= 1, "{summary}");
    assert!(summary["stale"].as_u64().unwrap() >= 1, "{summary}");
    assert_eq!(out.status.code(), Some(1), "a conflict always holds the gate");

    // The table carries the same verdicts, markers included.
    let out = scan(false);
    assert_eq!(out.status.code(), Some(1));
    let table = String::from_utf8_lossy(&out.stdout);
    let line = |port: &str| {
        table.lines().find(|l| l.contains(port)).unwrap_or_else(|| panic!("{port}:\n{table}")).to_string()
    };
    assert!(line("www/nginx ").contains("CONFLICT"), "{table}");
    assert!(line("ports-mgmt/pkg ").contains("STALE +DOCS -OBSOLETE"), "{table}");

    // Synced, pkg is ok and counts for nothing; nginx's conflict remains.
    let synced = optique(tmp.path(), "sync")
        .arg("-o")
        .arg(&optdir)
        .arg("ports-mgmt/pkg")
        .output()
        .unwrap();
    assert!(synced.status.success(), "{}", String::from_utf8_lossy(&synced.stderr));
    let report = parse_json(&scan(true));
    assert_eq!(row(&report, "ports-mgmt/pkg")["status"], "ok");
    assert_eq!(report["summary"]["attention"].as_u64().unwrap(), attention(&report));

    // With nginx's conflict resolved too, the gate opens if make.conf leaves
    // nothing undecided, and says why otherwise.
    fs::remove_dir_all(optdir.join("www_nginx")).unwrap();
    let synced = optique(tmp.path(), "sync")
        .arg("-o")
        .arg(&optdir)
        .args(["www/nginx", "ports-mgmt/pkg"])
        .output()
        .unwrap();
    assert!(synced.status.success());
    let out = scan(true);
    let report = parse_json(&out);
    let pending = report["summary"]["pending"].as_u64().unwrap();
    assert_eq!(out.status.code(), Some(i32::from(pending > 0)), "{}", report["summary"]);
}

/// An options file optique cannot read is never taken for a missing one:
/// make reports it and carries on as if it were missing, and sync would
/// then replace the saved choices with defaults. The port fails instead,
/// the file is left as it was, and scan says what to fix.
#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn an_unreadable_options_file_is_reported_not_overwritten() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    let file = optdir.join("ports-mgmt_pkg/options");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let saved = "_OPTIONS_READ=pkg-1.0\nOPTIONS_FILE_UNSET+=DOCS\n";
    fs::write(&file, saved).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&file).is_ok() {
        return; // root reads it anyway: the case only exists for others
    }
    let sync = optique(tmp.path(), "sync").arg("-o").arg(&optdir).arg("ports-mgmt/pkg").output().unwrap();
    let scan = optique(tmp.path(), "scan").args(["--json", "-o"]).arg(&optdir).arg("ports-mgmt/pkg").output().unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();

    assert_eq!(fs::read_to_string(&file).unwrap(), saved, "sync must not rewrite it");
    let stderr = String::from_utf8_lossy(&sync.stderr);
    assert!(stderr.contains("cannot read options file"), "{stderr}");
    assert_eq!(scan.status.code(), Some(1));
    let report = parse_json(&scan);
    assert_eq!(report["summary"]["errors"], 1, "{report}");
    let message = report["errors"][0]["message"].as_str().unwrap();
    assert!(message.starts_with("cannot read options file") && message.contains("Permission denied"), "{message}");
}

/// A port whose query fails was not checked, and neither was what it
/// depends on: scan does not call that done.
#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn scan_gate_counts_ports_it_could_not_check() {
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    fs::create_dir_all(&optdir).unwrap();
    let out = optique(tmp.path(), "scan")
        .args(["--json", "-o"])
        .arg(&optdir)
        .arg("misc/optique-no-such-port")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
    let report = parse_json(&out);
    assert_eq!(report["summary"]["errors"], 1, "{report}");
    assert!(report["summary"]["pending"].as_u64().unwrap() >= 1);
    assert_eq!(report["errors"][0]["subject"], "misc/optique-no-such-port", "{report}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(" · 1 error (see above)"), "{stderr}");
}

/// `clean --redundant` removes the files that only repeat defaults and
/// make.conf, keeps a file that decides something, and counts what it did.
#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn clean_redundant_keeps_only_files_that_decide_something() {
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    let out = optique(tmp.path(), "sync")
        .arg("-o")
        .arg(&optdir)
        .args(["ports-mgmt/pkg", "www/nginx"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    // nginx now deviates: IPV6 is on by default, the file turns it off.
    let nginx = optdir.join("www_nginx/options");
    let text = fs::read_to_string(&nginx).unwrap();
    assert!(text.contains("OPTIONS_FILE_SET+=IPV6\n"), "{text}");
    fs::write(&nginx, text.replace("OPTIONS_FILE_SET+=IPV6\n", "OPTIONS_FILE_UNSET+=IPV6\n")).unwrap();

    let out = optique(tmp.path(), "clean")
        .args(["--redundant", "--json", "-o"])
        .arg(&optdir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let report = parse_json(&out);
    let removed: Vec<&str> = report["removals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["options_name"].as_str().unwrap())
        .collect();
    assert!(removed.contains(&"ports-mgmt_pkg"), "{report}");
    assert!(!removed.contains(&"www_nginx"), "{report}");
    assert_eq!(report["summary"]["removed"].as_u64().unwrap(), removed.len() as u64, "{report}");
    assert!(!optdir.join("ports-mgmt_pkg").exists());
    assert!(nginx.is_file());
}

/// Feed a plan to a command on stdin and collect everything it said.
fn with_stdin(mut cmd: Command, input: &str) -> std::process::Output {
    use std::io::Write as _;
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawning optique");
    child.stdin.as_mut().expect("stdin is piped").write_all(input.as_bytes()).unwrap();
    child.wait_with_output().expect("collecting output")
}

fn parse_json(out: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}):\n{stdout}"))
}

/// The machine path end to end: read the option detail, plan one change from
/// it, preview it, carry it out, then replay it.
#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn decide_writes_a_plan_and_is_replayable() {
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    fs::create_dir_all(&optdir).unwrap();
    let file = optdir.join("ports-mgmt_pkg").join("options");
    let decide = || {
        let mut c = optique(tmp.path(), "decide");
        c.arg("-o").arg(&optdir).arg("ports-mgmt/pkg");
        c
    };

    // Which option to flip comes from the port itself: the tree is free to
    // rename them, and only an unlocked one outside a group is certain to be
    // the file's to decide.
    let out = optique(tmp.path(), "scan")
        .args(["--json", "--options", "-o"])
        .arg(&optdir)
        .arg("ports-mgmt/pkg")
        .output()
        .unwrap();
    scan_exit_code(&out);
    let report = parse_json(&out);
    let pkg = report["ports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["port"] == "ports-mgmt/pkg")
        .unwrap_or_else(|| panic!("ports-mgmt/pkg missing from {report}"));
    let options = pkg["options"].as_array().expect("--options must fill in options");
    assert!(!options.is_empty(), "ports-mgmt/pkg has options");
    for o in options {
        for key in ["name", "staged", "default", "saved", "new", "locked", "desc"] {
            assert!(!o[key].is_null() || key == "saved", "{key} missing from {o}");
        }
    }
    let target = options
        .iter()
        .find(|o| o["locked"] == false && o["group"].is_null())
        .unwrap_or_else(|| panic!("no unlocked ungrouped option in {pkg}"));
    let name = target["name"].as_str().unwrap().to_string();
    let want = !target["staged"].as_bool().unwrap();
    let plan = format!("{{\"ports-mgmt/pkg\": {{\"{name}\": {want}}}}}");

    // -n builds the whole report and touches nothing.
    let mut cmd = decide();
    cmd.arg("-n");
    let out = with_stdin(cmd, &plan);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let preview = parse_json(&out);
    assert_eq!(preview["dry_run"], true);
    assert_eq!(preview["applied"], false);
    assert_eq!(preview["writes"].as_array().unwrap().len(), 1, "{preview}");
    assert!(!file.exists(), "a dry run must write nothing");

    // For real.
    let out = with_stdin(decide(), &plan);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let done = parse_json(&out);
    assert_eq!(done["applied"], true, "{done}");
    assert_eq!(done["summary"]["written"], 1, "{done}");
    assert!(done["rejected"].as_array().unwrap().is_empty(), "{done}");
    let text = fs::read_to_string(&file).expect("the options file was written");
    let expected = if want {
        format!("OPTIONS_FILE_SET+={name}")
    } else {
        format!("OPTIONS_FILE_UNSET+={name}")
    };
    assert!(text.contains(&expected), "{file:?} must record {expected}:\n{text}");

    // Replaying the same plan asks for values the port already has.
    let out = with_stdin(decide(), &plan);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let again = parse_json(&out);
    assert_eq!(again["applied"], false, "a replay changes nothing: {again}");
    assert!(again["writes"].as_array().unwrap().is_empty(), "{again}");
}

/// A plan the port's rules cannot honour writes nothing at all and says why.
#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn decide_refuses_a_plan_it_cannot_honour() {
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    fs::create_dir_all(&optdir).unwrap();

    let mut cmd = optique(tmp.path(), "decide");
    cmd.arg("-o").arg(&optdir).arg("ports-mgmt/pkg");
    let out = with_stdin(
        cmd,
        r#"{"ports-mgmt/pkg": {"NO_SUCH_OPTION_HERE": true}}"#,
    );
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
    let report = parse_json(&out);
    assert_eq!(report["applied"], false);
    assert!(report["writes"].as_array().unwrap().is_empty(), "{report}");
    let rejected = report["rejected"].as_array().unwrap();
    assert_eq!(rejected.len(), 1, "{report}");
    assert_eq!(rejected[0]["option"], "NO_SUCH_OPTION_HERE");
    assert!(
        rejected[0]["reason"].as_str().unwrap().contains("no option"),
        "{}",
        rejected[0]
    );
    assert!(!optdir.join("ports-mgmt_pkg").exists(), "nothing may be written");
}

#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn sync_drops_obsolete_options_and_optionless_files() {
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");

    // ports-mgmt/pkg: file with an option that no longer exists.
    fs::create_dir_all(optdir.join("ports-mgmt_pkg")).unwrap();
    fs::write(
        optdir.join("ports-mgmt_pkg/options"),
        "_OPTIONS_READ=pkg-1.0\n_FILE_COMPLETE_OPTIONS_LIST=DOCS OBSOLETE\n\
         OPTIONS_FILE_SET+=OBSOLETE\nOPTIONS_FILE_UNSET+=DOCS\n",
    )
    .unwrap();
    // print/indexinfo: file although the port has no options at all.
    fs::create_dir_all(optdir.join("print_indexinfo")).unwrap();
    fs::write(
        optdir.join("print_indexinfo/options"),
        "_OPTIONS_READ=indexinfo-0.3\n_FILE_COMPLETE_OPTIONS_LIST=DOCS\nOPTIONS_FILE_SET+=DOCS\n",
    )
    .unwrap();

    let out = optique(tmp.path(), "sync")
        .arg("-o")
        .arg(&optdir)
        .args(["ports-mgmt/pkg", "print/indexinfo"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    let rewritten = fs::read_to_string(optdir.join("ports-mgmt_pkg/options")).unwrap();
    assert!(!rewritten.contains("OBSOLETE"), "obsolete option must be dropped:\n{rewritten}");
    assert!(rewritten.contains("DOCS"));
    assert!(
        !optdir.join("print_indexinfo").exists(),
        "optionless port's leftover file must be removed"
    );

    // Second sync over the same state is a no-op.
    let out = optique(tmp.path(), "sync")
        .arg("-o")
        .arg(&optdir)
        .args(["ports-mgmt/pkg", "print/indexinfo"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("everything up to date"), "{stderr}");
}

/// A file listing options it gives no SET or UNSET line is warned about,
/// and sync completes it as `make config` would write it; once complete,
/// neither the warning nor a write comes back.
#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn sync_completes_a_file_missing_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    fs::create_dir_all(optdir.join("textproc_expat2")).unwrap();
    let list = String::from_utf8(
        Command::new("make")
            .args(["-C", "/usr/ports/textproc/expat2", "-V", "COMPLETE_OPTIONS_LIST"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let opts: Vec<&str> = list.split_whitespace().collect();
    assert!(opts.len() >= 2, "expat2 needs two options for this test: {list}");
    // Only the last option has a line.
    fs::write(
        optdir.join("textproc_expat2/options"),
        format!(
            "_FILE_COMPLETE_OPTIONS_LIST={}\nOPTIONS_FILE_UNSET+={}\n",
            opts.join(" "),
            opts[opts.len() - 1]
        ),
    )
    .unwrap();

    let sync = || {
        optique(tmp.path(), "sync")
            .arg("-o")
            .arg(&optdir)
            .arg("textproc/expat2")
            .output()
            .unwrap()
    };
    let out = sync();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("textproc_expat2/options has no SET or UNSET line for"),
        "{stderr}"
    );
    let rewritten = fs::read_to_string(optdir.join("textproc_expat2/options")).unwrap();
    for opt in &opts {
        assert!(
            rewritten.contains(&format!("OPTIONS_FILE_SET+={opt}\n"))
                || rewritten.contains(&format!("OPTIONS_FILE_UNSET+={opt}\n")),
            "{opt} has no line after sync:\n{rewritten}"
        );
    }

    let out = sync();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("has no SET or UNSET line"), "{stderr}");
    assert!(stderr.contains("everything up to date"), "{stderr}");
}

#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn clean_removes_gone_ports_only() {
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");

    // A live entry (kept) and an entry whose port never existed (removed).
    fs::create_dir_all(optdir.join("ports-mgmt_pkg")).unwrap();
    fs::write(
        optdir.join("ports-mgmt_pkg/options"),
        "_OPTIONS_READ=pkg-1.0\n_FILE_COMPLETE_OPTIONS_LIST=DOCS\nOPTIONS_FILE_SET+=DOCS\n",
    )
    .unwrap();
    fs::create_dir_all(optdir.join("astro_no-such-port-xyz")).unwrap();
    fs::write(optdir.join("astro_no-such-port-xyz/options"), "# x\n").unwrap();

    let out = optique(tmp.path(), "clean").arg("-o").arg(&optdir).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("astro_no-such-port-xyz"), "{stdout}");

    assert!(!optdir.join("astro_no-such-port-xyz").exists());
    assert!(optdir.join("ports-mgmt_pkg/options").exists(), "live entry must survive");
}

#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn clean_unused_prunes_outside_closure() {
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");

    // Both ports are alive in the tree, so only closure membership decides:
    // ports-mgmt/pkg is the root, www/nginx is nowhere near it.
    fs::create_dir_all(optdir.join("ports-mgmt_pkg")).unwrap();
    fs::write(
        optdir.join("ports-mgmt_pkg/options"),
        "_OPTIONS_READ=pkg-1.0\n_FILE_COMPLETE_OPTIONS_LIST=DOCS\nOPTIONS_FILE_SET+=DOCS\n",
    )
    .unwrap();
    fs::create_dir_all(optdir.join("www_nginx")).unwrap();
    fs::write(
        optdir.join("www_nginx/options"),
        "_OPTIONS_READ=nginx-1.0\n_FILE_COMPLETE_OPTIONS_LIST=IPV6\nOPTIONS_FILE_SET+=IPV6\n",
    )
    .unwrap();

    let out = optique(tmp.path(), "clean")
        .args(["--unused", "-o"])
        .arg(&optdir)
        .arg("ports-mgmt/pkg")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("www_nginx"), "{stdout}");
    assert!(stdout.contains("not needed by the given list"), "{stdout}");

    assert!(!optdir.join("www_nginx").exists(), "entry outside the closure must go");
    assert!(optdir.join("ports-mgmt_pkg/options").exists(), "closure entry must survive");
}

/// Drive the real TUI headlessly: the keymap, the App and the drawing code
/// are the production ones, only the backend is in memory.
#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn tui_drive_end_to_end() {
    use std::io::Write as _;
    use std::process::Stdio;

    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    fs::create_dir_all(&optdir).unwrap();

    let script = "\
state
dump
key ?
dump
key 2
dump
key esc
key /
keys pkg
state
key esc
quit
";

    let mut child = optique(tmp.path(), "tui")
        .args(["--drive", "-o"])
        .arg(&optdir)
        .arg("ports-mgmt/pkg")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The scan runs before the driver reads a byte, so the whole script can
    // go in upfront; closing stdin also guarantees an exit.
    child.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "driver exited with {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    // One `screen WxH` ... `end` block per dump, in script order.
    let dumps: Vec<String> = stdout
        .split("screen ")
        .skip(1)
        .map(|chunk| chunk.split("\nend\n").next().unwrap_or_default().to_string())
        .collect();
    assert_eq!(dumps.len(), 3, "expected 3 dumps:\n{stdout}");

    let states: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|l| l.starts_with('{'))
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad state line ({e}): {l}")))
        .collect();
    assert_eq!(states.len(), 2, "expected 2 state lines:\n{stdout}");
    assert_eq!(states[0]["focus"], "list");
    assert_eq!(states[0]["overlay"], "none");

    // Initial screen: the port list with its title and the root port.
    assert!(dumps[0].contains("ports-mgmt/pkg"), "{}", dumps[0]);
    assert!(dumps[0].contains("Ports ("), "{}", dumps[0]);
    // '?' opens the help overlay on its first tab...
    assert!(dumps[1].contains("1:Markers"), "help tab bar missing:\n{}", dumps[1]);
    // ...and '2' switches to the option-row legend.
    assert!(
        dumps[2].contains("on | off = 1st column: the port's default value"),
        "option row tab missing:\n{}",
        dumps[2]
    );

    // Typing into the filter prompt lands in the filter, not in the keymap.
    assert_eq!(states[1]["filter"], "pkg");
    assert_eq!(states[1]["focus"], "filter");
    assert!(stdout.contains("\"filter\":\"pkg\""), "{stdout}");
    assert!(stdout.trim_end().ends_with("ok quit"), "{stdout}");
}

#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn wrapper_metadata_matches_known_nginx_facts() {
    // Guards the *config* target-name requirement in bsd.port.mk: if a tree
    // update changes that guard, descriptions would silently vanish.
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    fs::create_dir_all(&optdir).unwrap();

    let out = optique(tmp.path(), "scan")
        .args(["-v", "-o"])
        .arg(&optdir)
        .arg("www/nginx")
        .output()
        .unwrap();
    scan_exit_code(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let nginx_state = stdout
        .lines()
        .skip_while(|l| !l.contains("www/nginx "))
        .nth(1)
        .unwrap_or_default();
    // Radio-group members and a well-known option must be present.
    for opt in ["GSSAPI_MIT", "HTTP_PERL", "IPV6"] {
        assert!(nginx_state.contains(opt), "expected {opt} in: {nginx_state}");
    }
}

/// A local build reads the make.conf `make` would, `__MAKE_CONF` from the
/// environment included, and reads it in place, so that a relative
/// `.include` in it still finds its file.
#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn local_make_conf_is_the_environments_and_read_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    fs::create_dir_all(&optdir).unwrap();
    let conf_dir = tmp.path().join("conf");
    fs::create_dir_all(&conf_dir).unwrap();
    fs::write(conf_dir.join("make.conf"), ".include \"make.conf.local\"\n").unwrap();
    fs::write(conf_dir.join("make.conf.local"), "ports-mgmt_pkg_SET+=DOCS\n").unwrap();

    let out = optique(tmp.path(), "scan")
        .env("__MAKE_CONF", conf_dir.join("make.conf"))
        .args(["-l", "--json", "--options", "-o"])
        .arg(&optdir)
        .arg("ports-mgmt/pkg")
        .output()
        .unwrap();
    scan_exit_code(&out);
    let report = parse_json(&out);
    assert_eq!(report["errors"], serde_json::json!([]), "{report}");
    let pkg = report["ports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["port"] == "ports-mgmt/pkg")
        .unwrap_or_else(|| panic!("ports-mgmt/pkg missing from {report}"))
        .clone();
    let docs = pkg["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "DOCS")
        .unwrap_or_else(|| panic!("DOCS missing from {pkg}"))
        .clone();
    assert_eq!(docs["makeconf"]["value"], true, "the included file's stance: {docs}");
    assert_ne!(docs["makeconf"]["scope"], "global", "{docs}");
}

/// A synth profile without a make.conf of its own reads none: not the
/// host's /etc/make.conf, nor the environment's __MAKE_CONF, neither of
/// which a synth builder sees.
#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn synth_without_a_profile_make_conf_reads_none() {
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    fs::create_dir_all(&optdir).unwrap();
    let trap = tmp.path().join("host-make.conf");
    fs::write(&trap, ".error the host's make.conf was read\n").unwrap();

    // No profile of that name: synth.ini's defaults, and no make.conf.
    let out = Command::new(env!("CARGO_BIN_EXE_optique"))
        .env("XDG_CACHE_HOME", tmp.path())
        .env("__MAKE_CONF", &trap)
        .args(["scan", "--synth=optique-live-tests-no-such-profile", "-P", "/usr/ports"])
        .args(["--json", "-o"])
        .arg(&optdir)
        .arg("ports-mgmt/pkg")
        .output()
        .unwrap();
    scan_exit_code(&out);
    let report = parse_json(&out);
    assert_eq!(report["errors"], serde_json::json!([]), "{report}");
}

/// A relative -o is the same directory for make as for optique: make runs
/// in the port's directory, and a relative PORT_DBDIR read from there hid
/// every saved choice, so `clean --redundant` took a file turning an
/// option off for one repeating the defaults.
#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn a_relative_options_dir_is_the_one_make_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("opts/devel_gmake/options");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    // NLS is on by default: turning it off is a real choice.
    fs::write(&file, "_FILE_COMPLETE_OPTIONS_LIST=NLS\nOPTIONS_FILE_UNSET+=NLS\n").unwrap();

    let out = optique(tmp.path(), "clean")
        .current_dir(tmp.path())
        .args(["--no-cache", "-n", "--redundant", "-o", "opts"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{err}");
    assert!(!stdout.contains("redundant"), "a real choice taken for a redundant one:\n{stdout}{err}");
    assert!(file.exists());
}

/// An overlay's port answers for its origin over the ports tree's, and the
/// scan says so: a copy of sysutils/tmux with PORTREVISION 99, laid over
/// /usr/ports with --overlay-dir, is the one queried.
#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn an_overlay_port_shadows_the_trees() {
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    fs::create_dir_all(&optdir).unwrap();
    let port = tmp.path().join("overlay/sysutils/tmux");
    fs::create_dir_all(&port).unwrap();
    for entry in fs::read_dir("/usr/ports/sysutils/tmux").unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            fs::copy(entry.path(), port.join(entry.file_name())).unwrap();
        }
    }
    let makefile = fs::read_to_string(port.join("Makefile")).unwrap();
    let makefile: String = makefile
        .lines()
        .filter(|l| !l.starts_with("PORTREVISION="))
        .map(|l| if l.starts_with("PORTVERSION=") || l.starts_with("DISTVERSION=") {
            format!("{l}\nPORTREVISION=\t99\n")
        } else {
            format!("{l}\n")
        })
        .collect();
    fs::write(port.join("Makefile"), makefile).unwrap();
    let overlay = tmp.path().join("overlay");

    let out = optique(tmp.path(), "scan")
        .args(["-l", "--json", "--overlay-dir"])
        .arg(&overlay)
        .arg("-o")
        .arg(&optdir)
        .arg("sysutils/tmux")
        .output()
        .unwrap();
    scan_exit_code(&out);
    let report = parse_json(&out);
    let tmux = report["ports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["port"] == "sysutils/tmux")
        .unwrap_or_else(|| panic!("sysutils/tmux missing from {report}"))
        .clone();
    assert!(tmux["pkgname"].as_str().unwrap().ends_with("_99"), "{tmux}");
    let overlay = overlay.canonicalize().unwrap().display().to_string();
    assert_eq!(tmux["overlay"], overlay.as_str(), "{tmux}");
    assert_eq!(report["overlays"], serde_json::json!([overlay]), "{report}");

    let table = optique(tmp.path(), "scan")
        .args(["-l", "--overlay-dir", &overlay, "-o"])
        .arg(&optdir)
        .arg("sysutils/tmux")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&table.stdout);
    let row = text.lines().find(|l| l.contains("sysutils/tmux")).unwrap_or_default();
    assert!(row.ends_with(&format!("[overlay {overlay}]")), "{text}");
}

/// Under -m the defaults are the decision: on an empty options dir every
/// port they settle is done, not UNCONFIGURED, and the gate passes; the
/// JSON keeps the factual status.
#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn minimal_scan_counts_bare_defaults_as_done() {
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    fs::create_dir_all(&optdir).unwrap();
    let run = |json: bool| {
        let mut c = optique(tmp.path(), "scan");
        c.args(["-m", "-l", "-o"]).arg(&optdir);
        if json {
            c.arg("--json");
        }
        c.arg("ports-mgmt/pkg").output().unwrap()
    };
    let out = run(false);
    let text = String::from_utf8_lossy(&out.stdout);
    let row = text.lines().find(|l| l.contains("ports-mgmt/pkg")).unwrap_or_default();
    assert!(row.ends_with("ok (no file: defaults + make.conf)"), "{text}");
    assert!(!text.contains("UNCONFIGURED"), "{text}");

    let report = parse_json(&run(true));
    let pkg = report["ports"].as_array().unwrap().iter().find(|p| p["port"] == "ports-mgmt/pkg").unwrap().clone();
    assert_eq!((pkg["status"].as_str(), pkg["mc_covered"].as_bool()), (Some("unconfigured"), Some(true)), "{pkg}");
}

/// Under -m the rows shown as ok (no file, defaults decide) sort with the
/// ok ones: a conflict comes first, not among them. textproc/rxp's
/// single-choice CHAR_SIZE group with both members set is the conflict.
#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn minimal_scan_lists_problems_first() {
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    fs::create_dir_all(optdir.join("textproc_rxp")).unwrap();
    fs::write(
        optdir.join("textproc_rxp/options"),
        "_OPTIONS_READ=rxp-1\n_FILE_COMPLETE_OPTIONS_LIST=8 16\n\
         OPTIONS_FILE_SET+=8\nOPTIONS_FILE_SET+=16\n",
    )
    .unwrap();
    let out = optique(tmp.path(), "scan")
        .args(["-m", "-l", "-o"])
        .arg(&optdir)
        .args(["devel/gmake", "textproc/rxp"])
        .output()
        .unwrap();
    assert_eq!(scan_exit_code(&out), 1, "a conflict is pending");
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next().unwrap_or_default();
    assert!(first.contains("textproc/rxp") && first.contains("CONFLICT"), "{text}");
    assert!(text.contains("ok (no file"), "{text}");
}
