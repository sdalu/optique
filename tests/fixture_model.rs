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
