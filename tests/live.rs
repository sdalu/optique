//! Live end-to-end tests against the host's /usr/ports.
//! Run with: cargo test --test live -- --ignored
//!
//! They never touch real configuration: options dirs are tempdirs (-o) and
//! the query cache is redirected via XDG_CACHE_HOME.

use std::fs;
use std::process::Command;

fn optique(cache: &std::path::Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_optique"));
    c.env("XDG_CACHE_HOME", cache);
    c
}

#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn scan_smoke_and_cache_warmup() {
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    fs::create_dir_all(&optdir).unwrap();

    let run = || {
        let out = optique(tmp.path())
            .args(["scan", "-o"])
            .arg(&optdir)
            .arg("ports-mgmt/pkg")
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
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

    let out = optique(tmp.path())
        .args(["sync", "-o"])
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
    let out = optique(tmp.path())
        .args(["sync", "-o"])
        .arg(&optdir)
        .args(["ports-mgmt/pkg", "print/indexinfo"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
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
    fs::write(optdir.join("astro_no-such-port-xyz/options"), "x\n").unwrap();

    let out = optique(tmp.path()).args(["clean", "-o"]).arg(&optdir).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("astro_no-such-port-xyz"), "{stdout}");

    assert!(!optdir.join("astro_no-such-port-xyz").exists());
    assert!(optdir.join("ports-mgmt_pkg/options").exists(), "live entry must survive");
}

#[test]
#[ignore = "needs /usr/ports on a FreeBSD host"]
fn wrapper_metadata_matches_known_nginx_facts() {
    // Guards the *config* target-name requirement in bsd.port.mk: if a tree
    // update changes that guard, descriptions would silently vanish.
    let tmp = tempfile::tempdir().unwrap();
    let optdir = tmp.path().join("options");
    fs::create_dir_all(&optdir).unwrap();

    let out = optique(tmp.path())
        .args(["scan", "-v", "-o"])
        .arg(&optdir)
        .arg("www/nginx")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
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
