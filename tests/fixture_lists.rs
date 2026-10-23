//! End-to-end tests of how a list names its ports, against tests/ports
//! laid over the host's real framework (see tests/fixture.rs).
//! Run with: cargo test --test fixture_lists -- --ignored

mod common;

use std::collections::BTreeSet;

use common::{json, stderr, Fixture};

/// The misc/flav keys a `scan --json` of `args` reports.
fn flav_ports(f: &Fixture, args: &[&str]) -> BTreeSet<String> {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let out = f.run("scan", &all);
    let report = json(&out);
    assert_eq!(report["errors"], serde_json::json!([]), "{}", stderr(&out));
    report["ports"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["port"].as_str())
        .filter(|p| p.starts_with("misc/flav"))
        .map(str::to_string)
        .collect()
}

fn set(keys: &[&str]) -> BTreeSet<String> {
    keys.iter().map(|k| k.to_string()).collect()
}

/// `origin@all` roots every flavor of the port, `origin@-` its default
/// one, on the command line and in a list, as poudriere reads them.
#[test]
#[ignore = "needs /usr/ports/Mk on a FreeBSD host"]
fn all_and_default_flavor_specs_root_what_poudriere_builds() {
    let f = Fixture::new();
    let every = set(&["misc/flav@full", "misc/flav@nox", "misc/flav@pgtk"]);
    assert_eq!(flav_ports(&f, &["misc/flav@all"]), every);
    assert_eq!(flav_ports(&f, &["misc/flav@-"]), set(&["misc/flav@full"]));

    let list = f.options.parent().unwrap().join("pkglist");
    std::fs::write(&list, "misc/flav@all\n").unwrap();
    assert_eq!(flav_ports(&f, &["-f", list.to_str().unwrap()]), every);
}
