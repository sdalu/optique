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
/// `?=DOCS` does (UNSET had no value); options.local's `?=NAS` meets an
/// UNSET the file assigned. make has AUDIO and NAS on and SOX new; so does
/// optique.
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
    std::fs::write(f.options.join("misc_cond/options.local"), "OPTIONS_FILE_UNSET ?= NAS\n").unwrap();
    let make = f.make_v(origin, "PORT_OPTIONS");
    assert_eq!(make, ["AUDIO", "NAS"]);
    let new = f.make_v(origin, "NEW_OPTIONS");
    assert_eq!(new, ["SOX"]);
    let port = f.scan_port(&[origin], origin);
    assert_eq!(flagged(&port, "staged"), make, "{port}");
    assert_eq!(flagged(&port, "new"), new, "{port}");
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
