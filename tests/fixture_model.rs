//! End-to-end tests of optique's model of a port's options against the
//! framework's own answer: each sets up a make.conf and an options dir,
//! asks make (`make -V PORT_OPTIONS`, `NEW_OPTIONS`, `check-config`) and
//! checks that optique says the same. Like tests/fixture.rs they lay
//! tests/ports over the host's /usr/ports/Mk.
//! Run with: cargo test --test fixture_model -- --ignored

mod common;

use common::Fixture;

/// The names of `port`'s options (`scan --options`) whose `key` is true,
/// sorted.
fn flagged(port: &serde_json::Value, key: &str) -> Vec<String> {
    let mut out: Vec<String> = port["options"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o[key] == true)
        .map(|o| o["name"].as_str().unwrap().to_string())
        .collect();
    out.sort();
    out
}

/// bsd.options.mk:306-408: an option is new to make when no SET or UNSET
/// line names it, whatever `_FILE_COMPLETE_OPTIONS_LIST` says. SOX has a
/// line but is not listed: make counts nothing new, nor does optique.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn new_is_what_no_line_names() {
    let f = Fixture::new();
    f.write_file(
        "misc_cond",
        "_OPTIONS_READ=cond-1.0\n_FILE_COMPLETE_OPTIONS_LIST=AUDIO DOCS NAS\n\
         OPTIONS_FILE_SET+=DOCS\nOPTIONS_FILE_UNSET+=AUDIO\n\
         OPTIONS_FILE_UNSET+=NAS\nOPTIONS_FILE_UNSET+=SOX\n",
    );
    assert_eq!(f.make_v("misc/cond", "NEW_OPTIONS"), Vec::<String>::new());
    let port = f.scan_port(&["misc/cond"], "misc/cond");
    assert_eq!(flagged(&port, "new"), Vec::<String>::new(), "{port}");
    assert_eq!(port["status"], "ok", "{port}");
}

/// The options file is a makefile (bsd.options.mk:341): several words to a
/// line, spaces around `+=`, `=` resetting the list. make ends up with
/// AUDIO and SOX, and so does optique.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_hand_written_file_is_read_as_make_reads_it() {
    let f = Fixture::new();
    f.write_file(
        "misc_cond",
        "OPTIONS_FILE_SET+=NAS\nOPTIONS_FILE_UNSET += DOCS SOX\n\
         OPTIONS_FILE_SET=AUDIO SOX\nOPTIONS_FILE_UNSET=DOCS NAS\n",
    );
    let make = f.make_v("misc/cond", "PORT_OPTIONS");
    assert_eq!(make, ["AUDIO", "SOX"]);
    let port = f.scan_port(&["misc/cond"], "misc/cond");
    assert_eq!(flagged(&port, "staged"), make, "{port}");
    assert_eq!(flagged(&port, "new"), Vec::<String>::new(), "{port}");
}

/// bsd.options.mk:341-363: options.local is read after the options file and
/// joins its lists, UNSET winning. The file sets DOCS, options.local unsets
/// it and sets AUDIO, which the file unsets: make has neither on, nor does
/// optique, the port is configured, and sync leaves both files alone.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn options_local_is_read_after_the_file() {
    let f = Fixture::new();
    let file = "_OPTIONS_READ=cond-1.0\n_FILE_COMPLETE_OPTIONS_LIST=AUDIO DOCS NAS SOX\n\
                OPTIONS_FILE_UNSET+=AUDIO\nOPTIONS_FILE_SET+=DOCS\n\
                OPTIONS_FILE_UNSET+=NAS\nOPTIONS_FILE_UNSET+=SOX\n";
    let local = "OPTIONS_FILE_UNSET+=DOCS\nOPTIONS_FILE_SET+=AUDIO\n";
    f.write_file("misc_cond", file);
    let local_path = f.options.join("misc_cond/options.local");
    std::fs::write(&local_path, local).unwrap();
    let make = f.make_v("misc/cond", "PORT_OPTIONS");
    assert_eq!(make, Vec::<String>::new());
    let port = f.scan_port(&["misc/cond"], "misc/cond");
    assert_eq!(flagged(&port, "staged"), make, "{port}");
    assert_eq!(port["status"], "ok", "{port}");

    let out = f.run("sync", &["misc/cond"]);
    assert!(
        common::stderr(&out).contains("misc_cond/options.local also sets AUDIO and unsets DOCS"),
        "{}",
        common::stderr(&out)
    );
    assert!(common::stderr(&out).contains("up to date"), "{}", common::stderr(&out));
    assert_eq!(std::fs::read_to_string(f.file("misc_cond")).unwrap(), file);
    assert_eq!(std::fs::read_to_string(&local_path).unwrap(), local);
}

/// bsd.options.mk:342 reads options.local whether or not the options file
/// is there. The file sync writes for the defaults (DOCS) unsets AUDIO and
/// NAS, which options.local sets: without the file make would turn them
/// on. `sync -m` keeps the file, and make's PORT_OPTIONS stays as it was.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn minimal_sync_keeps_a_file_options_local_needs() {
    let f = Fixture::new();
    let origin = "misc/cond";
    f.run("sync", &[origin]);
    let file = std::fs::read_to_string(f.file("misc_cond")).expect("sync writes the defaults");
    std::fs::write(f.options.join("misc_cond/options.local"), "OPTIONS_FILE_SET+=AUDIO NAS\n").unwrap();
    let before = f.make_v(origin, "PORT_OPTIONS");
    assert_eq!(before, ["DOCS"]);
    let out = f.run("sync", &["-m", origin]);
    assert_eq!(std::fs::read_to_string(f.file("misc_cond")).ok().as_deref(), Some(&*file), "{}", common::stderr(&out));
    assert_eq!(f.make_v(origin, "PORT_OPTIONS"), before);
}

/// bsd.options.mk:342-363: options.local's entries join the file's lists,
/// UNSET winning. The file sets DOCS, options.local unsets it: make has
/// DOCS off whatever the file says, so `decide` refuses to turn it on,
/// naming options.local, writes nothing, and scan reports DOCS locked by it.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn decide_refuses_what_options_local_decides() {
    let f = Fixture::new();
    let origin = "misc/cond";
    f.run("sync", &[origin]);
    let file = std::fs::read_to_string(f.file("misc_cond")).expect("sync writes the defaults");
    assert!(file.contains("OPTIONS_FILE_SET+=DOCS"), "{file}");
    std::fs::write(f.options.join("misc_cond/options.local"), "OPTIONS_FILE_UNSET+=DOCS\n").unwrap();
    assert_eq!(f.make_v(origin, "PORT_OPTIONS"), Vec::<String>::new());

    let plan = f.decide(&[origin], r#"{"misc/cond": {"DOCS": true}}"#);
    let rejected = &plan["rejected"];
    assert_eq!(rejected.as_array().map(Vec::len), Some(1), "{plan}");
    assert_eq!(rejected[0]["option"], "DOCS", "{plan}");
    assert_eq!(rejected[0]["reason"], "options.local decides DOCS; edit it there", "{plan}");
    assert_eq!(std::fs::read_to_string(f.file("misc_cond")).unwrap(), file);

    let port = f.scan_port(&[origin], origin);
    let docs = common::option(&port, "DOCS");
    assert_eq!((&docs["locked"], &docs["locked_by"]), (&serde_json::json!(true), &serde_json::json!("options.local")), "{docs}");
}

/// bsd.options.mk:321: an UNSET entry is a make pattern. make.conf's
/// `OPTIONS_UNSET=A*` takes A and A2 out of PORT_OPTIONS and NEW_OPTIONS:
/// optique reports both off by make.conf and leaves undecided what make
/// still counts new.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn make_conf_unset_entries_are_patterns() {
    let f = Fixture::new();
    std::fs::write(&f.make_conf, "OPTIONS_UNSET=A*\n").unwrap();
    assert_eq!(f.make_v("misc/model-globs", "PORT_OPTIONS"), ["B"]);
    let new = f.make_v("misc/model-globs", "NEW_OPTIONS");
    assert_eq!(new, ["B", "C"]);
    let port = f.scan_port(&["misc/model-globs"], "misc/model-globs");
    assert_eq!(port["undecided"], serde_json::json!(new), "{port}");
    for opt in ["A", "A2"] {
        let o = common::option(&port, opt);
        assert_eq!(o["makeconf"], serde_json::json!({"value": false, "scope": "global"}), "{o}");
    }
}

/// bsd.port.mk ~4987: pre-check-config checks the PREVENTS of every option
/// in PORT_OPTIONS, OPTIONS_SLAVE ones included. X, on by OPTIONS_SLAVE,
/// prevents the default Y: check-config fails, and optique reports the
/// conflict. X's IMPLIES turns no Z on (bsd.options.mk:414 reads only the
/// list's), for make as for optique.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn an_options_slave_options_prevents_are_checked() {
    let f = Fixture::new();
    let origin = "misc/model-slave-prevents";
    assert_eq!(f.make_v(origin, "PORT_OPTIONS"), ["X", "Y"]);
    let check = f.make(origin, &["check-config"]);
    assert!(!check.status.success(), "check-config passes: {}", common::stdout(&check));
    let port = f.scan_port(&[origin], origin);
    assert_eq!(port["status"], "conflict", "{port}");
    assert_eq!(port["violations"], serde_json::json!(["X conflicts with Y: mutually exclusive"]), "{port}");
    assert_eq!(flagged(&port, "staged"), ["Y"], "{port}");
}

/// bsd.options.mk:410-440 puts what a listed option implies in PORT_OPTIONS
/// whether listed or not: C, a group member escaping the exclusion pass,
/// implies FOO, which OPTIONS_EXCLUDE took out, and pre-check-config checks
/// FOO's own PREVENTS of the default B1. check-config fails, and optique
/// reports the conflict, then settles it as make does once B1 is off.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn an_implied_excluded_options_prevents_is_checked() {
    let f = Fixture::new();
    let origin = "misc/model-implied-prevents";
    assert_eq!(f.make_v(origin, "PORT_OPTIONS"), ["B1", "C", "FOO"]);
    assert_eq!(f.make_v(origin, "COMPLETE_OPTIONS_LIST"), ["B1", "C"]);
    let check = f.make(origin, &["check-config"]);
    assert!(!check.status.success(), "check-config passes: {}", common::stdout(&check));
    let port = f.scan_port(&[origin], origin);
    assert_eq!(port["status"], "conflict", "{port}");
    assert_eq!(port["violations"], serde_json::json!(["FOO conflicts with B1: mutually exclusive"]), "{port}");

    let plan = f.decide(&[origin], r#"{"misc/model-implied-prevents": {"B1": false}}"#);
    assert_eq!(plan["applied"], true, "{plan}");
    let check = f.make(origin, &["check-config"]);
    assert!(check.status.success(), "check-config fails: {}", common::stdout(&check));
    let port = f.scan_port(&[origin], origin);
    assert_eq!(port["violations"], serde_json::json!([]), "{port}");
}

/// bsd.options.mk:300-304: a make.conf OPTIONS_OVERRIDE replaces the whole
/// decision — defaults, make.conf SET/UNSET and the options file are not
/// read, PORT_OPTIONS becomes its value as given. The file here says the
/// opposite (DOCS off, AUDIO and NAS on) and is read right past: make and
/// optique both end up with only DOCS on, the port reads configured (not
/// unconfigured, not stale), and sync leaves the file exactly as it was.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn options_override_decides_the_port_and_the_file_is_untouched() {
    let f = Fixture::new();
    let origin = "misc/cond";
    std::fs::write(&f.make_conf, "OPTIONS_OVERRIDE=DOCS\n").unwrap();
    let file = "_OPTIONS_READ=cond-1.0\n_FILE_COMPLETE_OPTIONS_LIST=AUDIO DOCS NAS SOX\n\
                OPTIONS_FILE_UNSET+=DOCS\nOPTIONS_FILE_SET+=AUDIO\nOPTIONS_FILE_SET+=NAS\n";
    f.write_file("misc_cond", file);

    let make = f.make_v(origin, "PORT_OPTIONS");
    assert_eq!(make, ["DOCS"]);
    let port = f.scan_port(&[origin], origin);
    assert_eq!(flagged(&port, "staged"), make, "{port}");
    assert_eq!(port["status"], "ok", "{port}");
    assert_eq!(port["undecided"], serde_json::json!([]), "{port}");
    for opt in ["DOCS", "AUDIO", "NAS", "SOX"] {
        let o = common::option(&port, opt);
        let on = opt == "DOCS";
        assert_eq!(o["makeconf"], serde_json::json!({"value": on, "scope": "override"}), "{o}");
        assert_eq!(o["locked"], true, "{o}");
        assert_eq!(o["locked_by"], "make.conf OPTIONS_OVERRIDE", "{o}");
    }

    let out = f.run("sync", &[origin]);
    assert!(
        common::stderr(&out).contains("everything up to date, nothing to write"),
        "{}",
        common::stderr(&out)
    );
    assert_eq!(std::fs::read_to_string(f.file("misc_cond")).unwrap(), file, "sync must not touch it");
}

/// bsd.options.mk:300-304: an OPTIONS_OVERRIDE defined empty is still
/// defined — PORT_OPTIONS starts empty, not at the port's defaults — so
/// nothing ends up on even though DOCS is misc/cond's default.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn an_empty_options_override_turns_everything_off() {
    let f = Fixture::new();
    let origin = "misc/cond";
    std::fs::write(&f.make_conf, "OPTIONS_OVERRIDE=\n").unwrap();

    let make = f.make_v(origin, "PORT_OPTIONS");
    assert_eq!(make, Vec::<String>::new());
    let port = f.scan_port(&[origin], origin);
    assert_eq!(flagged(&port, "staged"), make, "{port}");
    assert_eq!(port["status"], "ok", "{port}");

    let out = f.run("sync", &[origin]);
    assert!(!f.file("misc_cond").exists(), "no file existed, none must appear: {}", common::stderr(&out));
}

/// bsd.options.mk:399-409: make.conf's WITH and WITHOUT apply after FORCE
/// and take what they name out of NEW_OPTIONS. With WITH=C and WITHOUT=A*,
/// make leaves only B new; optique leaves only B undecided, and reports C
/// and A2 pinned by them, whatever the options file says.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn with_and_without_pin_like_force() {
    let f = Fixture::new();
    let origin = "misc/model-globs";
    std::fs::write(&f.make_conf, "WITH=C\nWITHOUT=A*\n").unwrap();
    assert_eq!(f.make_v(origin, "PORT_OPTIONS"), ["B", "C"]);
    let new = f.make_v(origin, "NEW_OPTIONS");
    assert_eq!(new, ["B"]);
    let port = f.scan_port(&[origin], origin);
    assert_eq!(port["undecided"], serde_json::json!(new), "{port}");
    for (opt, on) in [("C", true), ("A2", false)] {
        let o = common::option(&port, opt);
        assert_eq!(o["makeconf"], serde_json::json!({"value": on, "scope": "with"}), "{o}");
        assert_eq!(o["locked_by"], "make.conf WITH/WITHOUT", "{o}");
    }
}

/// make reads `:=` as `=` and `?=` only for a variable no earlier line
/// assigned, the options file and options.local read as one makefile. The
/// file resets SET to AUDIO NAS, its `?=SOX` assigns nothing, its
/// `?=DOCS` does (UNSET had no value). make has AUDIO and NAS on and SOX
/// new; so does optique. options.local's `?=NAS` then meets an UNSET the
/// file assigned and changes nothing, but would once a write left UNSET
/// out: the file is left to make, its outcome unchanged.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn colon_and_question_assignments_are_read_as_make_reads_them() {
    let f = Fixture::new();
    let origin = "misc/cond";
    f.write_file(
        "misc_cond",
        "OPTIONS_FILE_SET+=DOCS\nOPTIONS_FILE_SET:=AUDIO NAS\nOPTIONS_FILE_SET?=SOX\n\
         OPTIONS_FILE_UNSET?=DOCS\n",
    );
    let make = f.make_v(origin, "PORT_OPTIONS");
    assert_eq!(make, ["AUDIO", "NAS"]);
    let new = f.make_v(origin, "NEW_OPTIONS");
    assert_eq!(new, ["SOX"]);
    let port = f.scan_port(&[origin], origin);
    assert_eq!(flagged(&port, "staged"), make, "{port}");
    assert_eq!(flagged(&port, "new"), new, "{port}");

    std::fs::write(f.options.join("misc_cond/options.local"), "OPTIONS_FILE_UNSET ?= NAS\n").unwrap();
    assert_eq!(f.make_v(origin, "PORT_OPTIONS"), make);
    let port = f.scan_port(&[origin], origin);
    assert_eq!(flagged(&port, "staged"), make, "{port}");
    assert_eq!(common::option(&port, "NAS")["locked_by"], "options.local", "{port}");
}

/// bsd.options.mk:343 `.sinclude`s options.local whether or not the
/// options file is there. With options.local alone setting AUDIO and NAS
/// and unsetting DOCS and SOX, make has AUDIO and NAS on and nothing new;
/// optique reads the port configured, nothing undecided or new. sync still
/// writes the options file a port without one gets, and leaves
/// options.local alone.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn options_local_without_an_options_file_is_read() {
    let f = Fixture::new();
    let origin = "misc/cond";
    let local = "OPTIONS_FILE_SET+=AUDIO NAS\nOPTIONS_FILE_UNSET+=DOCS SOX\n";
    let local_path = f.options.join("misc_cond/options.local");
    std::fs::create_dir_all(f.options.join("misc_cond")).unwrap();
    std::fs::write(&local_path, local).unwrap();
    let make = f.make_v(origin, "PORT_OPTIONS");
    assert_eq!(make, ["AUDIO", "NAS"]);
    assert_eq!(f.make_v(origin, "NEW_OPTIONS"), Vec::<String>::new());
    let port = f.scan_port(&[origin], origin);
    assert_eq!(port["status"], "ok", "{port}");
    assert_eq!(port["undecided"], serde_json::json!([]), "{port}");
    assert_eq!(flagged(&port, "staged"), make, "{port}");
    assert_eq!(flagged(&port, "new"), Vec::<String>::new(), "{port}");

    f.run("sync", &[origin]);
    assert!(f.file("misc_cond").exists(), "a port with no options file gets one");
    assert_eq!(std::fs::read_to_string(&local_path).unwrap(), local);
    assert_eq!(f.make_v(origin, "PORT_OPTIONS"), make);
}

/// make joins a line ending in `\` with the next before parsing it. The
/// file sets AUDIO and, on a continued line, NAS: make has AUDIO and NAS
/// on and nothing new; optique too, and the port is not stale.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_continued_line_is_read_as_make_reads_it() {
    let f = Fixture::new();
    let origin = "misc/cond";
    f.write_file("misc_cond", "OPTIONS_FILE_SET+=AUDIO \\\n\tNAS\nOPTIONS_FILE_UNSET+=DOCS SOX\n");
    let make = f.make_v(origin, "PORT_OPTIONS");
    assert_eq!(make, ["AUDIO", "NAS"]);
    assert_eq!(f.make_v(origin, "NEW_OPTIONS"), Vec::<String>::new());
    let port = f.scan_port(&[origin], origin);
    assert_eq!(flagged(&port, "staged"), make, "{port}");
    assert_eq!(flagged(&port, "new"), Vec::<String>::new(), "{port}");
    assert_eq!(port["status"], "ok", "{port}");
}

/// bsd.port.mk's pre-check-config takes a SINGLE group for conditional when
/// ALL_OPTIONS (OPTIONS_DEFINE) matches its name. AUDIO is defined and a
/// member of the GROUP G, and names the SINGLE group AUDIO: with AUDIO off,
/// check-config passes and PORT_OPTIONS is empty; optique reports no
/// conflict and nothing on.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_group_named_after_a_defined_member_is_conditional() {
    let f = Fixture::new();
    let origin = "misc/model-defined-group";
    let make = f.make_v(origin, "PORT_OPTIONS");
    assert_eq!(make, Vec::<String>::new());
    let check = f.make(origin, &["check-config"]);
    assert!(check.status.success(), "check-config fails: {}", common::stdout(&check));
    let port = f.scan_port(&[origin], origin);
    assert_eq!(port["violations"], serde_json::json!([]), "{port}");
    assert_ne!(port["status"], "conflict", "{port}");
    assert_eq!(flagged(&port, "staged"), make, "{port}");
}

/// bsd.options.mk:410-430 closes PORT_OPTIONS under IMPLIES, chains
/// included, after the options file: A implies B, B implies C. With the
/// file setting A and unsetting the rest, and with no file at all, scan's
/// staged set is make's PORT_OPTIONS.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn an_implies_chain_ends_as_make_ends_it() {
    let f = Fixture::new();
    let origin = "misc/model-implies";
    let make = f.make_v(origin, "PORT_OPTIONS");
    let port = f.scan_port(&[origin], origin);
    assert_eq!(flagged(&port, "staged"), make, "no file: {port}");

    f.write_file("misc_model-implies", "OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B C D\n");
    let make = f.make_v(origin, "PORT_OPTIONS");
    assert_eq!(make, ["A", "B", "C"]);
    let port = f.scan_port(&[origin], origin);
    assert_eq!(flagged(&port, "staged"), make, "{port}");
}

/// bsd.port.mk's pre-check-config fails a RADIO group with more than one
/// member on. The options file sets one member, then both: optique
/// reports a conflict exactly when `make check-config` fails.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn an_overfull_radio_group_conflicts_as_check_config_fails() {
    let f = Fixture::new();
    let origin = "misc/model-radio";
    for (set, unset, overfull) in [("X", "Y", false), ("X Y", "", true)] {
        f.write_file(
            "misc_model-radio",
            &format!("OPTIONS_FILE_SET+={set}\nOPTIONS_FILE_UNSET+={unset}\n"),
        );
        let check = f.make(origin, &["check-config"]);
        assert_eq!(!check.status.success(), overfull, "check-config with {set}: {}", common::stdout(&check));
        let port = f.scan_port(&[origin], origin);
        assert_eq!(port["status"] == "conflict", overfull, "{set}: {port}");
        if overfull {
            assert_eq!(
                port["violations"],
                serde_json::json!(["group R allows at most one member enabled (2 enabled)"]),
                "{port}"
            );
        }
    }
}

/// bsd.options.mk:352-363 applies every OPTIONS_FILE_SET entry, the
/// options file's and options.local's, before every OPTIONS_FILE_UNSET one:
/// options.local's SET of D loses to the file's UNSET, make has nothing
/// on, and D is not locked. `decide` turns it on, and make follows.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn an_options_local_set_is_a_default_the_file_overrides() {
    let f = Fixture::new();
    let origin = "misc/model-implies";
    f.run("sync", &[origin]);
    std::fs::write(f.options.join("misc_model-implies/options.local"), "OPTIONS_FILE_SET+=D\n").unwrap();
    assert_eq!(f.make_v(origin, "PORT_OPTIONS"), Vec::<String>::new());
    let port = f.scan_port(&[origin], origin);
    let d = common::option(&port, "D");
    assert_eq!((&d["staged"], &d["locked"]), (&serde_json::json!(false), &serde_json::json!(false)), "{d}");

    let plan = f.decide(&[origin], r#"{"misc/model-implies": {"D": true}}"#);
    assert_eq!(plan["rejected"], serde_json::json!([]), "{plan}");
    assert_eq!(f.make_v(origin, "PORT_OPTIONS"), ["D"]);
}

/// An options file holding a line optique cannot model: its `OPTIONS_SET=C`
/// comes after bsd.options.mk's OPTIONS_SET loop and turns nothing on,
/// though make still dumps it. optique says so, file and line, shows
/// make's own PORT_OPTIONS with no make.conf verdict, every option locked,
/// and neither sync nor clean --redundant touches the file. options.local's
/// WITH=C does take effect, and the state follows make.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn an_options_file_optique_cannot_model_is_left_to_make() {
    let f = Fixture::new();
    let origin = "misc/model-globs";
    f.run("sync", &[origin]);
    let file = std::fs::read_to_string(f.file("misc_model-globs")).unwrap() + "OPTIONS_SET=C\n";
    f.write_file("misc_model-globs", &file);
    let line = file.lines().count();
    let make = f.make_v(origin, "PORT_OPTIONS");
    assert_eq!(make, ["A", "A2", "B"]);
    let port = f.scan_port(&[origin], origin);
    assert_eq!(flagged(&port, "staged"), make, "{port}");
    assert_eq!((&port["status"], &port["undecided"]), (&serde_json::json!("ok"), &serde_json::json!([])), "{port}");
    for o in port["options"].as_array().unwrap() {
        assert!(o.get("makeconf").is_none(), "{o}");
        assert_eq!(o["locked_by"], "options file", "{o}");
    }

    let out = f.run("sync", &[origin]);
    let warning = format!(
        "misc_model-globs/options line {line} assigns OPTIONS_SET, which optique does not model: \
         optique leaves misc_model-globs/options alone"
    );
    assert!(common::stderr(&out).contains(&warning), "{}", common::stderr(&out));
    assert_eq!(std::fs::read_to_string(f.file("misc_model-globs")).unwrap(), file);
    f.run("clean", &["--redundant"]);
    assert_eq!(std::fs::read_to_string(f.file("misc_model-globs")).unwrap(), file, "clean --redundant kept it");

    std::fs::write(f.options.join("misc_model-globs/options.local"), "WITH=C\n").unwrap();
    let make = f.make_v(origin, "PORT_OPTIONS");
    assert_eq!(make, ["A", "A2", "B", "C"]);
    let port = f.scan_port(&[origin], origin);
    assert_eq!(flagged(&port, "staged"), make, "{port}");
}

/// bsd.options.mk:183 assigns OPTIONS_FILE with `?=`: make.conf moves it,
/// and make reads neither the file optique manages nor its options.local.
/// optique warns, reads the port as configured, and sync writes nothing.
/// PORT_DBDIR, given on make's command line, stays what optique passes.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_moved_options_file_leaves_the_port_alone() {
    let f = Fixture::new();
    let origin = "misc/cond";
    std::fs::write(&f.make_conf, "PORT_DBDIR=/nonexistent\nOPTIONS_FILE=${PORT_DBDIR}/${OPTIONS_NAME}/moved\n").unwrap();
    let moved = f.options.join("misc_cond/moved").display().to_string();
    assert_eq!(f.make_v(origin, "OPTIONS_FILE"), std::slice::from_ref(&moved));
    let out = f.run("scan", &["--json", "--options", origin]);
    let stderr = common::stderr(&out);
    assert!(stderr.contains(&format!("make.conf sets OPTIONS_FILE to {moved}")), "{stderr}");
    let report = common::json(&out);
    let port = report["ports"].as_array().unwrap().iter().find(|p| p["port"] == origin).unwrap();
    assert_eq!(port["status"], "ok", "{port}");
    assert_eq!(common::option(port, "DOCS")["locked_by"], "make.conf OPTIONS_FILE", "{port}");
    f.run("sync", &[origin]);
    assert!(!f.file("misc_cond").exists(), "sync wrote the file make does not read");
}

/// bsd.options.mk:312-409: a make.conf set word is a pattern for the test
/// only. `OPTIONS_SET=C*` and `WITH=A*` add those words, which turn nothing
/// on, and make takes every match out of NEW_OPTIONS, leaving only B new.
/// That last is a defect optique does not follow: a glob nobody resolved
/// to an option decides none, so A, A2, B and C stay undecided, with C off
/// and nothing pinned.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn make_conf_set_globs_turn_nothing_on_and_decide_nothing() {
    let f = Fixture::new();
    let origin = "misc/model-globs";
    std::fs::write(&f.make_conf, "OPTIONS_SET=C*\nWITH=A*\n").unwrap();
    let make = f.make_v(origin, "PORT_OPTIONS");
    assert_eq!(make, ["A", "A*", "A2", "B", "C*"]);
    assert_eq!(f.make_v(origin, "NEW_OPTIONS"), ["B"]);
    let port = f.scan_port(&[origin], origin);
    assert_eq!(port["undecided"], serde_json::json!(["A", "A2", "B", "C"]), "{port}");
    assert_eq!(flagged(&port, "staged"), ["A", "A2", "B"], "{port}");
    assert_eq!(flagged(&port, "locked"), Vec::<String>::new(), "{port}");
}

/// bsd.port.mk's pre-check-config matches PREVENTS words as patterns, and
/// bsd.options.mk:410-440 adds what an option implies, listed or not. A
/// with B1 on breaks A's `B*`. FOO is excluded, and so is E, which implies
/// it (bsd.options.mk:244-251), but that pass looks at OPTIONS_DEFINE only:
/// C, a group member implying FOO, stays and turns FOO on, which breaks
/// D's PREVENTS of it. optique lists what make lists, and reports a
/// conflict exactly when check-config fails, naming the word make names.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn prevents_patterns_and_unlisted_implies_conflict_as_check_config_fails() {
    let f = Fixture::new();
    let origin = "misc/model-patterns";
    let complete = f.make_v(origin, "COMPLETE_OPTIONS_LIST");
    assert_eq!(complete, ["A", "B1", "B2", "C", "D"]);
    let port = f.scan_port(&[origin], origin);
    let mut listed: Vec<String> =
        port["options"].as_array().unwrap().iter().map(|o| o["name"].as_str().unwrap().to_string()).collect();
    listed.sort();
    assert_eq!(listed, complete);
    for (set, unset, violation) in [
        ("A B1", "B2 C D", Some("A conflicts with B*: mutually exclusive")),
        ("A", "B1 B2 C D", None),
        ("C D", "A B1 B2", Some("D conflicts with FOO: mutually exclusive")),
    ] {
        f.write_file("misc_model-patterns", &format!("OPTIONS_FILE_SET+={set}\nOPTIONS_FILE_UNSET+={unset}\n"));
        let check = f.make(origin, &["check-config"]);
        assert_eq!(!check.status.success(), violation.is_some(), "{set}: {}", common::stdout(&check));
        let port = f.scan_port(&[origin], origin);
        let want: Vec<&str> = violation.into_iter().collect();
        assert_eq!(port["violations"], serde_json::json!(want), "{set}: {port}");
    }
    assert!(f.make_v(origin, "PORT_OPTIONS").contains(&"FOO".to_string()));
}

/// bsd.port.mk drops the members of a conditional SINGLE group whose
/// option is off only after the option helpers ran (the mail/faces case):
/// with WITH=SOX and AUDIO off, make has SOX off yet SOX_RUN_DEPENDS in
/// RUN_DEPENDS. optique reads the dependencies from make: misc/condep is
/// in its closure, SOX off.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn a_pruned_group_members_dependencies_stay() {
    let f = Fixture::new();
    let origin = "misc/cond";
    std::fs::write(&f.make_conf, "WITH=SOX\n").unwrap();
    assert_eq!(f.make_v(origin, "PORT_OPTIONS"), ["DOCS"]);
    assert!(f.make_v(origin, "RUN_DEPENDS").contains(&"true:misc/condep".to_string()));
    let report = common::json(&f.run("scan", &["--json", "--options", origin]));
    let ports: Vec<&str> = report["ports"].as_array().unwrap().iter().map(|p| p["port"].as_str().unwrap()).collect();
    assert!(ports.contains(&"misc/condep"), "{ports:?}");
    let cond = report["ports"].as_array().unwrap().iter().find(|p| p["port"] == origin).unwrap();
    assert_eq!(flagged(cond, "staged"), ["DOCS"], "{cond}");
}
