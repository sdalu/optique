//! End-to-end tests against tests/ports, a handful of minimal ports laid
//! over the host's real framework (/usr/ports/Mk), each shaped after a real
//! port optique got wrong: the queries, the scan, sync and decide run as
//! they do on a full tree, with bsd.options.mk deciding what make sees.
//! Run with: cargo test --test fixture -- --ignored
//!
//! The ports: misc/cond (a SINGLE group named after an option, as
//! mail/faces), misc/slavey (a flavor turning an option on through
//! OPTIONS_SLAVE, as databases/sqlite3@icu), misc/flav (three flavors
//! sharing one file, as editors/emacs), misc/flavfile (a flavor whose own
//! Makefile names it an options file of its own, as devel/git@lite),
//! misc/forced (a SINGLE group whose make.conf forces a member on), and a
//! ports-mgmt/pkg stub every port depends on.

mod common;

use std::fs;

use common::{Fixture, option, stderr, stdout};

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
    assert!(
        !f.file("misc_cond").exists(),
        "-m removes a file repeating the defaults"
    );
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
    assert!(
        f.file("misc_slavey").exists(),
        "@icu alone keeps the default flavor's ICU line"
    );
    f.run("sync", &["-m", "misc/slavey", "misc/slavey@icu"]);
    assert!(
        !f.file("misc_slavey").exists(),
        "the file repeats both flavors' defaults"
    );
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
    assert!(
        with_x11_off.contains("OPTIONS_FILE_UNSET+=X11"),
        "{with_x11_off}"
    );

    // @nox alone: nothing it sees changed, nothing written, nothing removed.
    for args in [&["misc/flav@nox"][..], &["-m", "misc/flav@nox"][..]] {
        f.run("sync", args);
        assert_eq!(
            fs::read_to_string(f.file("misc_flav")).unwrap(),
            with_x11_off,
            "sync {args:?}"
        );
    }

    // @nox and @pgtk: Y, which only @pgtk has, turned off and written; X11,
    // which neither has, kept as the default flavor left it.
    let plan = f.decide(
        &["misc/flav@nox", "misc/flav@pgtk"],
        r#"{"misc/flav@pgtk": {"Y": false}}"#,
    );
    assert_eq!(plan["applied"], true, "{plan}");
    let text = fs::read_to_string(f.file("misc_flav")).unwrap();
    assert!(text.contains("OPTIONS_FILE_UNSET+=Y"), "{text}");
    assert!(text.contains("OPTIONS_FILE_UNSET+=X11"), "{text}");
}

/// A plan replayed from scan's report is honoured for a flavor that excludes
/// some of the shared file's options: scan lists the file's options for it
/// (`X11` for misc/flav@nox), and the file is the default flavor's.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_plan_naming_an_option_the_flavor_excludes_is_honoured() {
    let f = Fixture::new();
    let order = ["misc/flav", "misc/flav@nox"];
    let nox = f.scan_port(&order, "misc/flav@nox");
    assert_eq!(option(&nox, "X11")["staged"], true, "{nox}");
    let plan = f.decide(&order, r#"{"misc/flav@nox": {"X11": true, "A": true}}"#);
    assert_eq!(plan["applied"], true, "{plan}");
    assert_eq!(plan["rejected"], serde_json::json!([]), "{plan}");
}

/// A plan confirming an unconfigured port's defaults records them: the
/// file is written and scan stops reporting the port; the same plan again
/// writes nothing.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_plan_confirming_the_defaults_writes_the_file() {
    let f = Fixture::new();
    let origin = "misc/flav";
    let port = f.scan_port(&[origin], "misc/flav@full");
    assert_eq!(port["status"], "unconfigured", "{port}");
    let wanted = r#"{"misc/flav": {"A": true, "X11": true, "Y": true}}"#;
    let plan = f.decide(&[origin], wanted);
    assert_eq!(plan["applied"], true, "{plan}");
    assert_eq!(plan["summary"]["written"], 1, "{plan}");
    assert!(f.file("misc_flav").exists());
    assert_eq!(f.scan_port(&[origin], "misc/flav@full")["status"], "ok");
    let again = f.decide(&[origin], wanted);
    assert_eq!(again["summary"]["written"], 0, "{again}");
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
    fs::write(
        f.tree.join("MOVED"),
        "misc/oldname|misc/cond|2026-01-01|Renamed\n",
    )
    .unwrap();
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
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let report = common::json(&out);
    assert_eq!(report["summary"]["errors"], 1, "{report}");
    assert_eq!(report["errors"][0]["subject"], "misc/brokendep", "{report}");
    assert_eq!(
        report["errors"][0]["message"],
        "depends on nonexistent origin misc/oldname (moved to misc/cond)",
        "{report}"
    );
    assert!(
        !report["ports"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["port"] == "misc/cond"),
        "{report}"
    );
}

/// A port of the fixture's shape at `origin`, `extra` added to its
/// Makefile.
fn add_port(f: &Fixture, origin: &str, extra: &str) {
    let dir = f.tree.join(origin);
    fs::create_dir_all(&dir).unwrap();
    let name = origin.rsplit('/').next().unwrap();
    fs::write(
        dir.join("Makefile"),
        format!(
            "PORTNAME=\t{name}\nPORTVERSION=\t1.0\nCATEGORIES=\tmisc\n\
             MAINTAINER=\tnobody@example.org\nCOMMENT=\tFixture port\n\
             WWW=\t\thttps://example.org/\nNO_BUILD=\tyes\n{extra}\n.include <bsd.port.mk>\n"
        ),
    )
    .unwrap();
}

/// poudriere's `check_moved` gets a listed originspec whole, `~sub`
/// included, and MOVED names no subpackage: misc/oldname~sub is not moved
/// (misc/oldname, in no tree, is an error), misc/oldflav@nox~sub not by
/// misc/oldflav@nox's own entry, misc/oldflav2@nox~sub by its bare
/// origin's, as misc/flav@nox. A dependency on misc/oldname~sub gets no
/// "moved to" note, deps_sanity's `check_moved` finding none.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_listed_subpackage_is_not_moved_by_check_moved() {
    let f = Fixture::new();
    fs::write(
        f.tree.join("MOVED"),
        "misc/oldname|misc/cond|2026-01-01|Renamed\n\
         misc/oldflav@nox|misc/flav@pgtk|2026-01-01|Flavor moved\n\
         misc/oldflav2|misc/flav|2026-01-01|Renamed too\n",
    )
    .unwrap();
    let no_moves =
        |report: &serde_json::Value| report["moved"].as_array().is_none_or(Vec::is_empty);

    for listed in ["misc/oldname~sub", "misc/oldflav@nox~sub"] {
        let out = f.run("scan", &["--json", "--follow-moved", listed]);
        let report = common::json(&out);
        assert!(no_moves(&report), "{listed}: {report}");
        assert_eq!(report["summary"]["errors"], 1, "{listed}: {report}");
        assert!(
            !ports_of(&report)
                .iter()
                .any(|p| p.starts_with("misc/cond") || p.starts_with("misc/flav")),
            "{report}"
        );
    }

    let out = f.run("scan", &["--json", "misc/oldflav2@nox~sub"]);
    let report = common::json(&out);
    assert_eq!(
        report["moved"],
        serde_json::json!([{"listed": "misc/oldflav2@nox", "to": "misc/flav@nox", "reason": "Renamed too", "followed": false}]),
        "{report}"
    );
    let out = f.run(
        "scan",
        &["--json", "--follow-moved", "misc/oldflav2@nox~sub"],
    );
    assert!(
        ports_of(&common::json(&out)).contains(&"misc/flav@nox".to_string()),
        "{}",
        stderr(&out)
    );

    add_port(&f, "misc/subdep", "RUN_DEPENDS=\tcond>0:misc/oldname~sub\n");
    let out = f.run("scan", &["--json", "misc/subdep"]);
    let report = common::json(&out);
    assert_eq!(report["errors"][0]["subject"], "misc/subdep", "{report}");
    assert_eq!(
        report["errors"][0]["message"], "depends on nonexistent origin misc/oldname",
        "{report}"
    );
}

/// A followed move is no alias for the dependency graph: misc/mid's
/// dependency on misc/old, in no tree, is an error, and does not lead to
/// misc/new, which depends on misc/mid, to close a loop.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_followed_move_does_not_close_a_loop_through_a_nonexistent_dependency() {
    let f = Fixture::new();
    fs::write(
        f.tree.join("MOVED"),
        "misc/old|misc/new|2026-01-01|Renamed\n",
    )
    .unwrap();
    add_port(
        &f,
        "misc/new",
        "OPTIONS_DEFINE=\tX\nRUN_DEPENDS=\tmid>0:misc/mid\n",
    );
    add_port(
        &f,
        "misc/mid",
        "OPTIONS_DEFINE=\tX\nRUN_DEPENDS=\told>0:misc/old\n",
    );

    let out = f.run("scan", &["--json", "--follow-moved", "misc/old"]);
    let report = common::json(&out);
    let err = stderr(&out);
    assert!(!err.contains("loop:"), "{err}");
    assert_eq!(report["loops"], serde_json::json!([]), "{report}");
    assert_eq!(report["errors"][0]["subject"], "misc/mid", "{report}");
    assert_eq!(
        report["errors"][0]["message"],
        "depends on nonexistent origin misc/old (moved to misc/new)",
        "{report}"
    );
    let ports = ports_of(&report);
    assert!(
        ports.contains(&"misc/new".to_string()) && ports.contains(&"misc/mid".to_string()),
        "{ports:?}"
    );

    // A plan naming the old origin still reaches the port it moved to.
    let plan = f.decide(
        &["--follow-moved", "misc/old"],
        r#"{"misc/old": {"X": true}}"#,
    );
    assert_eq!(plan["rejected"], serde_json::json!([]), "{plan}");
    let text = fs::read_to_string(f.file("misc_new")).unwrap_or_else(|e| panic!("{e}: {plan}"));
    assert!(text.contains("OPTIONS_FILE_SET+=X"), "{text}");
}

fn ports_of(report: &serde_json::Value) -> Vec<String> {
    report["ports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["port"].as_str().unwrap().to_string())
        .collect()
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
    assert!(
        ports.contains(&"misc/forced".to_string()),
        "a local build keeps misc/forced: {ports:?}"
    );
    assert!(
        stderr(&out).contains(
            "listed misc/oldname moved to misc/cond (Renamed): fix the list, or --follow-moved"
        ),
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
    assert!(
        ports_of(&report).contains(&"misc/cond".to_string()),
        "{report}"
    );
    assert!(
        stderr(&out).contains("followed, but the list still names misc/oldname"),
        "{}",
        stderr(&out)
    );

    // clean --unused refuses to prune while the list names a moved origin.
    let out = f
        .optique("clean")
        .args(["--unused", "misc/oldname"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("refusing to prune"),
        "{}",
        stderr(&out)
    );
    assert!(f.file("misc_cond").exists());

    // synth reads no MOVED: flagged all the same, --follow-moved ignored.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_optique"))
        .env("XDG_CACHE_HOME", &f.cache)
        .args([
            "scan",
            "--synth=optique-fixture-no-such-profile",
            "--follow-moved",
            "--json",
            "-P",
        ])
        .arg(&f.tree)
        .arg("-o")
        .arg(&f.options)
        .arg("misc/oldname")
        .output()
        .unwrap();
    let report = common::json(&out);
    let err = stderr(&out);
    assert!(
        err.contains("--follow-moved has no effect under -s"),
        "{err}"
    );
    assert!(
        err.contains("listed misc/oldname moved to misc/cond (Renamed): fix the list\n"),
        "{err}"
    );
    assert_eq!(out.status.code(), Some(1), "{err}");
    assert_eq!(
        report["moved"],
        serde_json::json!([{"listed": "misc/oldname", "to": "misc/cond", "reason": "Renamed", "followed": false}]),
        "{report}"
    );
    assert_eq!(report["summary"]["moved"], 1, "{report}");
    assert_eq!(report["errors"], serde_json::json!([]), "{report}");
    assert!(
        !ports_of(&report).contains(&"misc/cond".to_string()),
        "{report}"
    );
}

/// A make.conf `PORT_DBDIR` sends the build to another directory, which
/// optique's own command line hides from its queries: scan reports it as
/// an error naming make.conf and exits 1, the rest of its report intact.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_make_conf_port_dbdir_elsewhere_is_an_error() {
    let f = Fixture::new();
    fs::write(&f.make_conf, "PORT_DBDIR=/nonexistent/elsewhere\n").unwrap();
    let out = f.run("scan", &["--json", "misc/cond"]);
    let err = stderr(&out);
    let want = format!(
        "make.conf or the environment sets PORT_DBDIR=/nonexistent/elsewhere: the build reads \
         that directory, not {}",
        f.options.display()
    );
    assert!(err.contains(&format!("error: {want}")), "{err}");
    assert_eq!(out.status.code(), Some(1), "{err}");
    let report = common::json(&out);
    assert_eq!(
        report["errors"],
        serde_json::json!([{"subject": "make.conf", "message": want}]),
        "{report}"
    );
    assert_eq!(report["summary"]["errors"], 1, "{report}");
    assert!(
        report["ports"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["port"] == "misc/cond"),
        "{report}"
    );

    // sync says so too, and writes all the same.
    let out = f.run("sync", &["misc/cond"]);
    assert!(stderr(&out).contains(&want), "{}", stderr(&out));
    assert!(f.file("misc_cond").exists());

    // The same directory, spelled as the options dir: nothing to report.
    fs::write(
        &f.make_conf,
        format!("PORT_DBDIR={}/\n", f.options.display()),
    )
    .unwrap();
    let out = f.run("scan", &["--json", "misc/cond"]);
    assert_eq!(
        common::json(&out)["errors"],
        serde_json::json!([]),
        "{}",
        stderr(&out)
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

/// A make.conf moving OPTIONS_FILE per flavor: `misc_flav-nox` is
/// misc/flav@nox's file, not a vanished misc/flav-nox's. optique cannot
/// tell which entries make reads, so neither plain clean nor --unused
/// removes one as vanished or unused, the stale `misc_gone` included.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_relocated_options_file_keeps_clean_from_removing_entries() {
    let f = Fixture::new();
    let conf = "OPTIONS_FILE=${PORT_DBDIR}/${OPTIONS_NAME}${FLAVOR:D-${FLAVOR}}/options\n";
    fs::write(&f.make_conf, conf).unwrap();
    let nox = f
        .options
        .join("misc_flav-nox/options")
        .display()
        .to_string();
    let out = f.make("misc/flav", &["FLAVOR=nox", "-V", "OPTIONS_FILE"]);
    assert_eq!(stdout(&out).trim(), nox);
    f.write_file(
        "misc_flav-nox",
        "_OPTIONS_READ=flav-1.0\n_FILE_COMPLETE_OPTIONS_LIST=A\nOPTIONS_FILE_UNSET+=A\n",
    );
    f.write_file(
        "misc_gone",
        "_OPTIONS_READ=gone-1.0\n_FILE_COMPLETE_OPTIONS_LIST=A\nOPTIONS_FILE_SET+=A\n",
    );
    let warning = "warning: make.conf sets OPTIONS_FILE (${PORT_DBDIR}/${OPTIONS_NAME}${FLAVOR:D-${FLAVOR}}/options): \
                   optique cannot tell which options-dir entries make reads, so none is removed as vanished or unused";

    for args in [&[][..], &["--unused", "misc/flav@nox"][..]] {
        let out = f.run("clean", args);
        let err = stderr(&out);
        assert_eq!(out.status.code(), Some(0), "clean {args:?}: {err}");
        assert!(err.contains(warning), "clean {args:?}: {err}");
        assert!(
            f.file("misc_flav-nox").exists(),
            "clean {args:?} removed the file make reads"
        );
        assert!(
            f.file("misc_gone").exists(),
            "clean {args:?} removed a vanished entry"
        );
    }

    // Moved for this port only, where no probe outside a port sees it:
    // the entry it moves to is @nox's own (`misc_flav-nox`), so --unused
    // keeps it as the closure's, with no note, while the vanished one goes.
    let conf = format!(".if ${{.CURDIR:M*/misc/flav}}\n{conf}.endif\n");
    fs::write(&f.make_conf, conf).unwrap();
    let out = f.run("clean", &["--unused", "misc/flav@nox"]);
    let err = stderr(&out);
    assert!(!err.contains("make.conf sets OPTIONS_FILE ("), "{err}");
    assert!(!err.contains("(OPTIONS_FILE); kept"), "{err}");
    assert!(f.file("misc_flav-nox").exists(), "{err}");
    assert!(!f.file("misc_gone").exists(), "{err}");
}

/// A make.conf renaming the entries through OPTIONS_NAME rather than
/// OPTIONS_FILE: `misc_flav-nox` is misc/flav@nox's file all the same, so
/// plain clean removes no entry as vanished, the stale `misc_gone` included.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_renamed_options_name_keeps_clean_from_removing_entries() {
    let f = Fixture::new();
    let conf = "OPTIONS_NAME=${PKGORIGIN:S/\\//_/}${FLAVOR:D-${FLAVOR}}\n";
    fs::write(&f.make_conf, conf).unwrap();
    let nox = f
        .options
        .join("misc_flav-nox/options")
        .display()
        .to_string();
    let out = f.make("misc/flav", &["FLAVOR=nox", "-V", "OPTIONS_FILE"]);
    assert_eq!(stdout(&out).trim(), nox);
    f.write_file(
        "misc_flav-nox",
        "_OPTIONS_READ=flav-1.0\n_FILE_COMPLETE_OPTIONS_LIST=A\nOPTIONS_FILE_UNSET+=A\n",
    );
    f.write_file(
        "misc_gone",
        "_OPTIONS_READ=gone-1.0\n_FILE_COMPLETE_OPTIONS_LIST=A\nOPTIONS_FILE_SET+=A\n",
    );
    let out = f.run("clean", &[]);
    let err = stderr(&out);
    assert!(
        err.contains(
            "warning: make.conf sets OPTIONS_NAME (${PKGORIGIN:S/\\//_/}${FLAVOR:D-${FLAVOR}}): "
        ),
        "{err}"
    );
    assert!(
        f.file("misc_flav-nox").exists(),
        "removed the file make reads: {err}"
    );
    assert!(
        f.file("misc_gone").exists(),
        "removed a vanished entry: {err}"
    );
}

/// clean -q drops its banner, as every other command does, and keeps its
/// warnings.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn clean_quiet_drops_the_banner() {
    let f = Fixture::new();
    let out = f.run("clean", &["-n"]);
    assert!(
        stderr(&out).contains("optique clean: options dir"),
        "{}",
        stderr(&out)
    );
    let out = f.run("clean", &["-n", "-q"]);
    assert!(!stderr(&out).contains("optique clean:"), "{}", stderr(&out));
}

/// devel/git's Makefile gives every flavor but the default an options file
/// of its own, `${PORT_DBDIR}/${OPTIONS_NAME}/${FLAVOR}-options`: misc/
/// flavfile@lite reads misc_flavfile/lite-options. That file is managed like
/// any other: no warning, both flavors unconfigured and editable, sync
/// writes each flavor's own file, and an edit to one flavor reaches that
/// flavor's file alone.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_flavor_the_port_gives_its_own_file_is_managed() {
    let f = Fixture::new();
    let lite = f.options.join("misc_flavfile/lite-options");
    let out = f.make("misc/flavfile", &["FLAVOR=lite", "-V", "OPTIONS_FILE"]);
    assert_eq!(stdout(&out).trim(), lite.display().to_string());
    let order = ["misc/flavfile", "misc/flavfile@lite"];
    let default = f.scan_port(&order, "misc/flavfile@default");
    assert_eq!(default["status"], "unconfigured", "{default}");
    assert_eq!(
        default["options_file"], "misc_flavfile/options",
        "{default}"
    );
    let port = f.scan_port(&order, "misc/flavfile@lite");
    assert_eq!(port["status"], "unconfigured", "{port}");
    assert_eq!(port["options_file"], "misc_flavfile/lite-options", "{port}");
    assert_eq!(option(&port, "A")["locked"], false, "{port}");

    let out = f.run("sync", &order);
    let err = stderr(&out);
    assert!(!err.contains("OPTIONS_FILE"), "{err}");
    assert!(f.file("misc_flavfile").is_file(), "{err}");
    assert!(lite.is_file(), "sync wrote @lite's own file: {err}");

    let plan = f.decide(&order, r#"{"misc/flavfile@lite": {"A": false}}"#);
    assert_eq!(plan["applied"], true, "{plan}");
    let out = f.make("misc/flavfile", &["FLAVOR=lite", "-V", "PORT_OPTIONS"]);
    assert_eq!(stdout(&out).trim(), "", "@lite reads its own file");
    assert_eq!(f.make_v("misc/flavfile", "PORT_OPTIONS"), ["A", "X11"]);
}

/// An edit to make.conf's option policy alone keeps the cached answers it
/// cannot reach: the next scan says how many it carried over and how many
/// the edit dropped, and answers byte for byte as a scan with no cache.
/// Any other edit carries nothing over, and neither does --no-carry-over.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn an_option_edit_to_make_conf_keeps_what_it_cannot_reach() {
    let f = Fixture::new();
    let ports = [
        "misc/cond",
        "misc/flav",
        "misc/flav@nox",
        "misc/forced",
        "misc/slavey",
        "misc/flavfile",
        "misc/flavfile@lite",
        "misc/model-globs",
        "misc/model-radio",
    ];
    let scan = |extra: &[&str]| {
        let mut args = vec!["--json", "--options"];
        args.extend_from_slice(extra);
        args.extend_from_slice(&ports);
        f.run("scan", &args)
    };
    fs::write(&f.make_conf, "OPTIONS_UNSET+=DOCS\n").unwrap();
    scan(&[]);

    fs::write(
        &f.make_conf,
        "OPTIONS_UNSET+=DOCS\n# X11 for the flavors\nOPTIONS_SET+=X11\n",
    )
    .unwrap();
    let carried = scan(&[]);
    let err = stderr(&carried);
    assert!(
        err.contains("entries carried over from the make.conf before")
            && err.contains("dropped by its edit to OPTIONS_SET"),
        "{err}"
    );
    let fresh = scan(&["--no-cache"]);
    assert_eq!(stdout(&carried), stdout(&fresh), "{err}");

    // Anything but option policy: nothing carried over.
    fs::write(
        &f.make_conf,
        "OPTIONS_UNSET+=DOCS\nOPTIONS_SET+=X11\nCPUTYPE?=native\n",
    )
    .unwrap();
    let err = stderr(&scan(&[]));
    assert!(!err.contains("carried over"), "{err}");
    // Nor under --no-carry-over.
    fs::write(
        &f.make_conf,
        "OPTIONS_UNSET+=DOCS NLS\nOPTIONS_SET+=X11\nCPUTYPE?=native\n",
    )
    .unwrap();
    let err = stderr(&scan(&["--no-carry-over"]));
    assert!(!err.contains("carried over"), "{err}");
}

/// --no-framework-probe gives the queries no host values probed once:
/// each works them out itself, the banner says so, and the answers are
/// those of a probed run, byte for byte.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn the_framework_probe_changes_no_answer() {
    let f = Fixture::new();
    let ports = ["misc/cond", "misc/flav", "misc/slavey", "misc/forced"];
    let scan = |extra: &[&str]| {
        let mut args = vec!["--json", "--options", "--no-cache"];
        args.extend_from_slice(extra);
        args.extend_from_slice(&ports);
        f.run("scan", &args)
    };
    let probed = scan(&[]);
    let unprobed = scan(&["--no-framework-probe"]);
    let err = stderr(&unprobed);
    assert!(err.contains("framework probe off"), "{err}");
    assert!(!stderr(&probed).contains("framework probe off"));
    assert_eq!(stdout(&probed), stdout(&unprobed), "{err}");
}

/// A make.conf `.include` is part of what the cache keys on: an edit to the
/// included file alone is a new answer for the ports that read it, without
/// --clear-cache.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn an_included_make_conf_edit_is_not_served_from_cache() {
    let f = Fixture::new();
    let included = f.make_conf.with_file_name("opts.conf");
    fs::write(&included, "OPTIONS_UNSET+=DOCS\n").unwrap();
    fs::write(
        &f.make_conf,
        format!(".include \"{}\"\n", included.display()),
    )
    .unwrap();
    let port = f.scan_port(&["misc/cond"], "misc/cond");
    assert_eq!(option(&port, "DOCS")["staged"], false, "{port}");

    fs::write(&included, "\n").unwrap();
    let out = f.run("scan", &["--json", "--options", "misc/cond"]);
    let err = stderr(&out);
    assert!(
        err.contains("dropped: a file make.conf includes changed"),
        "{err}"
    );
    let report = common::json(&out);
    let port = report["ports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["port"] == "misc/cond")
        .unwrap();
    assert_eq!(option(port, "DOCS")["staged"], true, "{port}");
}

/// A make.conf moving OPTIONS_FILE: make reads the moved file, so an edit
/// to it is a new answer, never the cached one made before it.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_moved_options_file_edit_is_not_served_from_cache() {
    let f = Fixture::new();
    fs::write(
        &f.make_conf,
        "OPTIONS_FILE=${PORT_DBDIR}/${OPTIONS_NAME}/alt\n",
    )
    .unwrap();
    let report = common::json(&f.run("scan", &["--json", "--options", "misc/cond"]));
    assert!(
        !ports_of(&report).contains(&"misc/condep".to_string()),
        "{report}"
    );

    fs::create_dir_all(f.options.join("misc_cond")).unwrap();
    fs::write(
        f.options.join("misc_cond/alt"),
        "OPTIONS_FILE_UNSET+=DOCS\nOPTIONS_FILE_SET+=SOX\n",
    )
    .unwrap();
    let report = common::json(&f.run("scan", &["--json", "--options", "misc/cond"]));
    assert!(
        ports_of(&report).contains(&"misc/condep".to_string()),
        "{report}"
    );
    let port = report["ports"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["port"] == "misc/cond")
        .unwrap();
    assert_eq!(option(port, "DOCS")["staged"], false, "{port}");
}

/// A make.conf moving OPTIONS_FILE for misc/flav@nox alone: @nox reads
/// a file of its own, @full still reads misc_flav/options. So @nox is left
/// to make, its edit refused, while the shared file stays @full's to
/// manage: unconfigured in scan, written by sync and by decide, whichever
/// order the closure meets the flavors in, and judged redundant for @full
/// alone. The warning names @nox, and does not say make reads the shared
/// file for nobody.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_flavor_moving_options_file_leaves_the_owners_file_managed() {
    let f = Fixture::new();
    fs::write(
        &f.make_conf,
        ".if ${FLAVOR:U} == nox\nOPTIONS_FILE=/nonexistent/nox/options\n.endif\n",
    )
    .unwrap();
    let out = f.make("misc/flav", &["FLAVOR=nox", "-V", "OPTIONS_FILE"]);
    assert_eq!(stdout(&out).trim(), "/nonexistent/nox/options");
    for order in [
        ["misc/flav", "misc/flav@nox"],
        ["misc/flav@nox", "misc/flav"],
    ] {
        let full = f.scan_port(&order, "misc/flav@full");
        assert_eq!(full["status"], "unconfigured", "{order:?}: {full}");
        assert_eq!(option(&full, "A")["locked"], false, "{order:?}: {full}");
        let nox = f.scan_port(&order, "misc/flav@nox");
        assert_eq!(nox["status"], "ok", "{order:?}: {nox}");
        assert_eq!(
            option(&nox, "A")["locked_by"],
            "OPTIONS_FILE",
            "{order:?}: {nox}"
        );

        let out = f.run("sync", &order);
        let err = stderr(&out);
        assert!(
            err.contains(
                "warning: OPTIONS_FILE is /nonexistent/nox/options for misc/flav@nox, outside the \
                 options dir: make reads that file, not misc_flav/options, for it"
            ),
            "{order:?}: {err}"
        );
        assert!(
            !err.contains("make does not read misc_flav/options"),
            "{order:?}: {err}"
        );
        assert!(
            f.file("misc_flav").exists(),
            "{order:?}: sync writes @full's file: {err}"
        );
        fs::remove_file(f.file("misc_flav")).unwrap();

        let plan = f.decide(&order, r#"{"misc/flav": {"A": false}}"#);
        assert_eq!(plan["applied"], true, "{order:?}: {plan}");
        assert_eq!(
            f.make_v("misc/flav", "PORT_OPTIONS"),
            ["X11", "Y"],
            "{order:?}"
        );
        let out = f.make("misc/flav", &["FLAVOR=nox", "-V", "PORT_OPTIONS"]);
        assert_eq!(
            stdout(&out).trim(),
            "A",
            "{order:?}: @nox reads its own file"
        );
        let plan = f.decide(&order, r#"{"misc/flav@nox": {"A": false}}"#);
        let rejected = &plan["rejected"];
        assert_eq!(
            rejected.as_array().map(Vec::len),
            Some(1),
            "{order:?}: {plan}"
        );
        assert_eq!(
            rejected[0]["reason"],
            "OPTIONS_FILE is /nonexistent/nox/options, outside the options dir optique manages; \
             edit it there",
            "{order:?}: {plan}"
        );
        // @full's view alone judges the file: back at its defaults, -m
        // removes it.
        let plan = f.decide(&["-m", order[0], order[1]], r#"{"misc/flav": {"A": true}}"#);
        assert_eq!(plan["applied"], true, "{order:?}: {plan}");
        assert!(!f.file("misc_flav").exists(), "{order:?}: {plan}");
    }
}

/// bsd.options.mk:341-363: a `?=` in options.local assigns only while the
/// options file leaves the list unassigned. With misc/model-globs's file
/// unsetting C, `OPTIONS_FILE_UNSET?=C` there is inert; once a write turns
/// every option on, the file has no UNSET line and the same `?=` turns C
/// off. So such a line leaves the file unmanaged, whatever it holds now:
/// scan reports make's own outcome, decide refuses, nothing is written.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn an_options_local_ifunset_leaves_the_file_unmanaged() {
    let f = Fixture::new();
    let origin = "misc/model-globs";
    let file = "_FILE_COMPLETE_OPTIONS_LIST=A A2 B C\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_SET+=A2\n\
                OPTIONS_FILE_SET+=B\nOPTIONS_FILE_UNSET+=C\n";
    f.write_file("misc_model-globs", file);
    fs::write(
        f.options.join("misc_model-globs/options.local"),
        "OPTIONS_FILE_UNSET?=C\n",
    )
    .unwrap();
    let make = f.make_v(origin, "PORT_OPTIONS");
    assert_eq!(make, ["A", "A2", "B"]);

    let port = f.scan_port(&[origin], origin);
    assert_eq!(port["status"], "ok", "{port}");
    let c = option(&port, "C");
    assert_eq!(
        (&c["staged"], &c["locked_by"]),
        (
            &serde_json::json!(false),
            &serde_json::json!("options.local")
        ),
        "{c}"
    );

    let out = f.run("sync", &[origin]);
    let err = stderr(&out);
    assert!(
        err.contains("misc_model-globs/options.local line 1 assigns OPTIONS_FILE_UNSET with ?="),
        "{err}"
    );
    let plan = f.decide(&[origin], r#"{"misc/model-globs": {"C": true}}"#);
    assert_eq!(plan["rejected"].as_array().map(Vec::len), Some(1), "{plan}");
    assert_eq!(
        fs::read_to_string(f.file("misc_model-globs")).unwrap(),
        file
    );
    assert_eq!(f.make_v(origin, "PORT_OPTIONS"), make);
}

/// In the TUI, misc/condep comes in once misc/cond's SOX is on, first
/// queried against the staging options dir, which does not hold its file
/// yet. Its file is one optique cannot manage (a FOO line), so the pane
/// shows make's own outcome, X on as scan has it, not the first query's.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_newcomer_with_an_unmanaged_file_shows_makes_outcome_with_it() {
    use std::io::Write as _;
    let f = Fixture::new();
    f.write_file("misc_condep", "OPTIONS_FILE_SET+=X\nFOO=bar\n");
    assert_eq!(f.make_v("misc/condep", "PORT_OPTIONS"), ["X"]);
    let script = "key tab\nkey space\nkey down\nkey down\nkey down\nkey space\nsettle\nsettle\n\
                  key tab\nkey /\nkeys condep\nkey enter\nsettle\ndump\nquit\n";
    let mut child = f
        .optique("tui")
        .arg("--drive")
        .arg("misc/cond")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let screen = stdout(&out);
    assert!(
        screen.contains("condep-1.0") && screen.contains("UNMANAGED: "),
        "{screen}\n{}",
        stderr(&out)
    );
    assert!(screen.contains("[x] X"), "{screen}");
}

/// An options.local SET glob (`OPTIONS_FILE_SET+=C*`) turns nothing on and
/// joins the file's list as make reads it: the file sync writes is up to
/// date on the next run, which writes nothing.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn an_options_local_set_glob_leaves_a_written_file_up_to_date() {
    let f = Fixture::new();
    let origin = "misc/model-globs";
    fs::create_dir_all(f.options.join("misc_model-globs")).unwrap();
    fs::write(
        f.options.join("misc_model-globs/options.local"),
        "OPTIONS_FILE_SET+=C*\n",
    )
    .unwrap();
    let out = f.run("sync", &[origin]);
    assert!(stdout(&out).contains("new file"), "{}", stdout(&out));
    let written = fs::read_to_string(f.file("misc_model-globs")).unwrap();
    for _ in 0..2 {
        let out = f.run("sync", &[origin]);
        assert!(
            stderr(&out).contains("nothing to write"),
            "{}\n{}",
            stdout(&out),
            stderr(&out)
        );
        assert_eq!(
            fs::read_to_string(f.file("misc_model-globs")).unwrap(),
            written
        );
    }
}
