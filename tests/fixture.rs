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

mod common;

use std::fs;

use common::{option, stderr, stdout, Fixture};

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

/// A dependency never follows MOVED: a port depending on misc/oldname is
/// refused as poudriere refuses it, misc/cond not queried on its account,
/// and scan counts the error.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_dependency_on_a_moved_origin_is_an_error() {
    let f = Fixture::new();
    fs::write(f.tree.join("MOVED"), "misc/oldname|misc/cond|2026-01-01|Renamed\n").unwrap();
    let dir = f.tree.join("misc/brokendep");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("Makefile"),
        "PORTNAME=\tbrokendep\nPORTVERSION=\t1.0\nCATEGORIES=\tmisc\n\
         MAINTAINER=\tnobody@example.org\nCOMMENT=\tDepends on a moved origin\n\
         WWW=\t\thttps://example.org/\nNO_BUILD=\tyes\n\
         RUN_DEPENDS=\tcond>0:misc/oldname\n\n.include <bsd.port.mk>\n",
    )
    .unwrap();

    let out = f.run("scan", &["--json", "misc/brokendep"]);
    assert_eq!(out.status.code(), Some(1), "{}", common::stderr(&out));
    let report = common::json(&out);
    assert_eq!(report["summary"]["errors"], 1, "{report}");
    assert_eq!(report["errors"][0]["subject"], "misc/brokendep", "{report}");
    assert_eq!(
        report["errors"][0]["message"],
        "depends on nonexistent origin misc/oldname (moved to misc/cond)",
        "{report}"
    );
    assert!(!report["ports"].as_array().unwrap().iter().any(|p| p["port"] == "misc/cond"), "{report}");
}

fn ports_of(report: &serde_json::Value) -> Vec<String> {
    report["ports"].as_array().unwrap().iter().map(|p| p["port"].as_str().unwrap().to_string()).collect()
}

/// A listed origin MOVED moves is flagged and not followed: misc/oldname
/// is pending (exit 1, a `moved` entry), misc/cond not configured on its
/// account. `--follow-moved` configures misc/cond, says the list still
/// names misc/oldname, and does not hold the gate for it. A local build
/// takes a listed origin in the tree as it is, whatever MOVED says
/// (misc/forced). Under -s it is flagged too, and never followed: synth
/// reads no MOVED.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_listed_moved_origin_is_flagged_unless_followed() {
    let f = Fixture::new();
    fs::write(
        f.tree.join("MOVED"),
        "misc/oldname|misc/cond|2026-01-01|Renamed\nmisc/forced|misc/condep|2026-02-01|Reused\n",
    )
    .unwrap();

    let out = f.run("scan", &["--json", "misc/oldname", "misc/forced"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let report = common::json(&out);
    assert_eq!(
        report["moved"],
        serde_json::json!([{"listed": "misc/oldname", "to": "misc/cond", "reason": "Renamed", "followed": false}]),
        "{report}"
    );
    assert_eq!(report["summary"]["moved"], 1, "{report}");
    assert_eq!(report["errors"], serde_json::json!([]), "{report}");
    let ports = ports_of(&report);
    assert!(!ports.contains(&"misc/cond".to_string()), "{ports:?}");
    assert!(ports.contains(&"misc/forced".to_string()), "a local build keeps misc/forced: {ports:?}");
    assert!(
        stderr(&out).contains("listed misc/oldname moved to misc/cond (Renamed): fix the list, or --follow-moved"),
        "{}",
        stderr(&out)
    );

    // Followed: misc/cond configured, the move noted, the gate free of it.
    f.run("sync", &["--follow-moved", "misc/oldname"]);
    assert!(f.file("misc_cond").exists(), "sync configures the target");
    let out = f.run("scan", &["--json", "--follow-moved", "misc/oldname"]);
    let report = common::json(&out);
    assert_eq!(report["moved"][0]["followed"], true, "{report}");
    assert_eq!(report["summary"]["moved"], 0, "{report}");
    assert_eq!(report["summary"]["pending"], 0, "{report}");
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(ports_of(&report).contains(&"misc/cond".to_string()), "{report}");
    assert!(stderr(&out).contains("followed, but the list still names misc/oldname"), "{}", stderr(&out));

    // clean --unused refuses to prune while the list names a moved origin.
    let out = f.optique("clean").args(["--unused", "misc/oldname"]).output().unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("refusing to prune"), "{}", stderr(&out));
    assert!(f.file("misc_cond").exists());

    // synth reads no MOVED: flagged all the same, --follow-moved ignored.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_optique"))
        .env("XDG_CACHE_HOME", &f.cache)
        .args(["scan", "--synth=optique-fixture-no-such-profile", "--follow-moved", "--json", "-P"])
        .arg(&f.tree)
        .arg("-o")
        .arg(&f.options)
        .arg("misc/oldname")
        .output()
        .unwrap();
    let report = common::json(&out);
    let err = stderr(&out);
    assert!(err.contains("--follow-moved has no effect under -s"), "{err}");
    assert!(err.contains("listed misc/oldname moved to misc/cond (Renamed): fix the list\n"), "{err}");
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert_eq!(
        report["moved"],
        serde_json::json!([{"listed": "misc/oldname", "to": "misc/cond", "reason": "Renamed", "followed": false}]),
        "{report}"
    );
    assert_eq!(report["summary"]["moved"], 1, "{report}");
    assert_eq!(report["errors"], serde_json::json!([]), "{report}");
    assert!(!ports_of(&report).contains(&"misc/cond".to_string()), "{report}");
}
