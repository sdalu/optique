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
    assert!(common::stderr(&out).contains("options.local overrides the options file for AUDIO DOCS"));
    assert!(common::stderr(&out).contains("up to date"), "{}", common::stderr(&out));
    assert_eq!(std::fs::read_to_string(f.file("misc_cond")).unwrap(), file);
    assert_eq!(std::fs::read_to_string(&local_path).unwrap(), local);
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
