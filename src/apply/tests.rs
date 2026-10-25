use super::*;
use crate::model::options::PortOptions;

fn mk_info(complete: &[&str], effective: &[&str]) -> PortInfo {
    PortInfo {
        pkgname: "port-1.0".into(),
        options_name: "cat_port".into(),
        options: PortOptions {
            complete: complete.iter().map(ToString::to_string).collect(),
            effective: effective.iter().map(ToString::to_string).collect(),
            ..Default::default()
        },
        ..PortInfo::stub("cat/port")
    }
}

#[test]
fn sync_keeps_choices_and_adopts_new() {
    // Saved file: A on, B off. Tree update added C (effective: on).
    let info = mk_info(&["A", "B", "C"], &["A", "C"]);
    let saved = SavedOptionsFile::parse(
        "_OPTIONS_READ=port-0.9\n_FILE_COMPLETE_OPTIONS_LIST=A B D\n\
         OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n",
    );
    let enabled = sync_enabled_set(&info, Some(&saved));
    // A kept on, B kept off (even though effective would differ), C adopted, D dropped.
    assert_eq!(
        enabled,
        ["A", "C"].iter().map(ToString::to_string).collect()
    );
}

/// A file written before `_FILE_COMPLETE_OPTIONS_LIST` existed knows
/// its options only from the SET/UNSET lines: those choices still hold.
#[test]
fn sync_keeps_choices_a_file_knows_only_from_its_set_lines() {
    let info = mk_info(&["A", "B"], &["B"]);
    let saved = SavedOptionsFile::parse("OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n");
    let enabled = sync_enabled_set(&info, Some(&saved));
    assert_eq!(enabled, ["A"].iter().map(ToString::to_string).collect());
}

/// bsd.options.mk applies OPTIONS_FILE_SET then OPTIONS_FILE_UNSET, so
/// an option on both lines is off; an option the file only lists in
/// `_FILE_COMPLETE_OPTIONS_LIST` is left at its default/make.conf value.
/// Sync must read a hand-edited file the way make does, or its rewrite
/// flips options the reported diff never mentions.
#[test]
fn sync_lets_unset_win_and_leaves_complete_only_options_to_make() {
    // What make computes with this file in place: A off (UNSET wins), B
    // on, C at its default (on), D at its default (off).
    let info = mk_info(&["A", "B", "C", "D"], &["B", "C"]);
    let saved = SavedOptionsFile::parse(
        "_FILE_COMPLETE_OPTIONS_LIST=A B C D\n\
         OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_SET+=B\n",
    );
    let enabled = sync_enabled_set(&info, Some(&saved));
    assert_eq!(
        enabled,
        ["B", "C"].iter().map(ToString::to_string).collect()
    );
    // Nobody decided C or D: the file says nothing about them.
    assert_eq!(
        crate::session::undecided_options(&info, Some(&saved), false),
        ["C", "D"]
    );
}

fn pending(old: Option<&str>, complete: &[&str], enabled: &[&str]) -> PendingWrite {
    PendingWrite {
        key: PortKey::parse("cat/port").unwrap(),
        options_name: "cat_port".into(),
        path: PathBuf::from("/nonexistent/cat_port/options"),
        old: old.map(SavedOptionsFile::parse),
        enabled: enabled.iter().map(ToString::to_string).collect(),
        complete: complete.iter().map(ToString::to_string).collect(),
        kept: vec![],
        content: String::new(),
    }
}

/// The old file is read as make reads it: UNSET wins over SET, and an
/// option only in the complete list holds no value of its own, so a
/// rewrite that settles either is reported, never passed over.
#[test]
fn changes_read_the_old_file_as_make_does() {
    let old = "_FILE_COMPLETE_OPTIONS_LIST=A B C\n\
               OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_SET+=C\n";
    let c = pending(Some(old), &["A", "B", "C"], &["A", "B", "C"]).changes();
    assert_eq!(c.turned_on, ["A"], "A was off for make");
    assert_eq!(
        c.adopted,
        [("B".to_string(), true)],
        "B had no value in the file"
    );
    assert!(c.turned_off.is_empty() && c.dropped.is_empty(), "{c:?}");
}

/// bsd.options.mk:354-363: an UNSET entry is a pattern, deciding every
/// option it matches. A file unsetting `A*` and B, rewritten with A on,
/// turns A on; A2, which `A*` settles, is not new, and `A*`, matching
/// options the port has, is not dropped.
#[test]
fn changes_read_pattern_entries_as_make_does() {
    let old = "_FILE_COMPLETE_OPTIONS_LIST=A A2 B\nOPTIONS_FILE_UNSET+=A* B\n";
    let p = pending(Some(old), &["A", "A2", "B"], &["A"]);
    let c = p.changes();
    assert_eq!(
        c,
        Changes {
            turned_on: vec!["A".into()],
            ..Default::default()
        }
    );
    assert_eq!(p.describe(), "+A");
    // Once the port has no option `A*` matches, the entry goes.
    let c = pending(Some("OPTIONS_FILE_UNSET+=A* B\n"), &["B", "C"], &["C"]).changes();
    assert_eq!(c.dropped, ["A*"]);
    assert_eq!(c.adopted, [("C".to_string(), true)]);
}

/// An obsolete entry options.local holds is not dropped: the write leaves
/// options.local as it is, and the entry with it.
#[test]
fn an_options_local_entry_is_not_dropped() {
    let mut p = pending(Some("OPTIONS_FILE_UNSET+=B\n"), &["B"], &[]);
    p.old = p.old.map(|o| o.with_local("OPTIONS_FILE_UNSET+=GONE\n"));
    assert!(p.changes().dropped.is_empty(), "{:?}", p.changes());
}

/// A brand-new file adopts every option, at the value it is written with.
#[test]
fn a_new_file_adopts_every_option() {
    let c = pending(None, &["A", "B"], &["B"]).changes();
    assert_eq!(
        c.adopted,
        [("A".to_string(), false), ("B".to_string(), true)]
    );
    assert!(c.turned_on.is_empty() && c.turned_off.is_empty() && c.dropped.is_empty());
}

/// An option the port no longer has is dropped, not turned off.
#[test]
fn a_dropped_option_is_not_turned_off() {
    let old = "_FILE_COMPLETE_OPTIONS_LIST=A Z\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_SET+=Z\n";
    let c = pending(Some(old), &["A"], &[]).changes();
    assert_eq!(c.turned_off, ["A"]);
    assert_eq!(c.dropped, ["Z"]);
}

#[test]
fn unconfigured_takes_effective() {
    let info = mk_info(&["A", "B"], &["B"]);
    let enabled = sync_enabled_set(&info, None);
    assert_eq!(enabled, ["B"].iter().map(ToString::to_string).collect());
}

/// Same option list, different choices: the file is rewritten.
#[test]
fn plan_writes_a_changed_choice_on_an_unchanged_list() {
    let tmp = tempfile::tempdir().unwrap();
    let info = mk_info(&["A", "B"], &["A"]);
    fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    fs::write(
        tmp.path().join("cat_port/options"),
        "_FILE_COMPLETE_OPTIONS_LIST=A B\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n",
    )
    .unwrap();
    let enabled: BTreeSet<String> = ["B".to_string()].into();
    let planned = plan_writes(
        std::iter::once((&info.key.clone(), &info, enabled)),
        &OptionsFiles::new(tmp.path()),
        false,
    );
    assert_eq!(planned.writes.len(), 1);
}

/// bsd.options.mk:300-304: a port with an OPTIONS_OVERRIDE never reads
/// its options file, so a disagreeing one already on disk must survive
/// untouched: no write, whatever `enabled` is handed in.
#[test]
fn plan_writes_leaves_an_overridden_ports_file_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let mut info = mk_info(&["A", "B"], &["A"]);
    info.options.r#override = Some(["A".to_string()].into());
    fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    fs::write(
        tmp.path().join("cat_port/options"),
        "_FILE_COMPLETE_OPTIONS_LIST=A B\nOPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_SET+=B\n",
    )
    .unwrap();
    // Whatever the caller computed as "enabled" (A, the override's own
    // outcome, disagreeing with the file's B) must not trigger a write.
    let enabled: BTreeSet<String> = ["A".to_string()].into();
    let planned = plan_writes(
        std::iter::once((&info.key.clone(), &info, enabled)),
        &OptionsFiles::new(tmp.path()),
        false,
    );
    assert!(planned.writes.is_empty(), "{:?}", planned.writes);
    assert!(planned.removals.is_empty(), "{:?}", planned.removals);
}

/// The same, under `-m`: without the OPTIONS_OVERRIDE guard, minimal
/// mode would see the override's outcome equal defaults + make.conf
/// (both computed the same way with the file ignored) and queue the
/// file for removal as redundant. It must not: the file is left alone,
/// not merely rewritten as absent.
#[test]
fn plan_writes_leaves_an_overridden_ports_file_alone_under_minimal() {
    let tmp = tempfile::tempdir().unwrap();
    let mut info = mk_info(&["A", "B"], &["A"]);
    info.options.r#override = Some(["A".to_string()].into());
    fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    fs::write(
        tmp.path().join("cat_port/options"),
        "_FILE_COMPLETE_OPTIONS_LIST=A B\nOPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_SET+=B\n",
    )
    .unwrap();
    let enabled: BTreeSet<String> = ["A".to_string()].into();
    let planned = plan_writes(
        std::iter::once((&info.key.clone(), &info, enabled)),
        &OptionsFiles::new(tmp.path()),
        true,
    );
    assert!(planned.writes.is_empty(), "{:?}", planned.writes);
    assert!(planned.removals.is_empty(), "{:?}", planned.removals);
}

/// bsd.options.mk:341-363: options.local is read after the file and
/// joins its lists, UNSET winning. With the file setting A and
/// options.local unsetting it, make has A off: so does the session,
/// the port is configured, and sync finds nothing to write, leaving
/// options.local alone.
#[test]
fn options_local_is_read_with_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let info = mk_info(&["A", "B", "C"], &[]);
    fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    fs::write(
        tmp.path().join("cat_port/options"),
        "_FILE_COMPLETE_OPTIONS_LIST=A B C\n\
         OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\nOPTIONS_FILE_UNSET+=C\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("cat_port/options.local"),
        "OPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_SET+=B\n",
    )
    .unwrap();
    let files = OptionsFiles::new(tmp.path());
    let mut ports = std::collections::BTreeMap::new();
    ports.insert(info.key.clone(), info.clone());
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    let sess = crate::session::Session::new(ports, Default::default(), &roots, &files, false);
    let staged = sess.state(&info).unwrap().staged.clone();
    assert!(staged.is_empty(), "{staged:?}");
    assert_eq!(sess.status(&info), crate::session::UiStatus::Ok);
    let planned = plan_writes(
        std::iter::once((&info.key.clone(), &info, staged)),
        &files,
        false,
    );
    assert!(planned.writes.is_empty(), "{:?}", planned.writes);
}

/// An options.local SET glob (`OPTIONS_FILE_SET+=C*`) joins the file's
/// SET list as make reads it, though it turns nothing on: the file
/// written for A, A2 and B, read with it, is up to date, and the next
/// plan writes nothing. The would-be file is read with options.local
/// as the old one is.
#[test]
fn an_options_local_set_glob_leaves_a_written_file_up_to_date() {
    let tmp = tempfile::tempdir().unwrap();
    let mut info = mk_info(&["A", "A2", "B", "C"], &["A", "A2", "B"]);
    info.options.defaults = ["A", "A2", "B"].iter().map(ToString::to_string).collect();
    fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    fs::write(
        tmp.path().join("cat_port/options.local"),
        "OPTIONS_FILE_SET+=C*\n",
    )
    .unwrap();
    let enabled: BTreeSet<String> = info.options.defaults.iter().cloned().collect();
    let plan = || {
        plan_writes(
            std::iter::once((&info.key, &info, enabled.clone())),
            &OptionsFiles::new(tmp.path()),
            false,
        )
    };
    let first = plan();
    assert_eq!(first.writes.len(), 1, "no file yet");
    fs::write(
        tmp.path().join("cat_port/options"),
        &first.writes[0].content,
    )
    .unwrap();
    let second = plan();
    assert!(
        second.writes.is_empty(),
        "{:?}",
        second.writes.iter().map(|w| &w.content).collect::<Vec<_>>()
    );
}

/// bsd.options.mk:342 reads options.local whether or not the options
/// file is there. The file says the defaults (A on, B off) and
/// options.local sets B: with the file make has A alone (its UNSET of
/// B wins), without it A and B. The file is not redundant under -m.
#[test]
fn minimal_keeps_a_file_options_local_needs() {
    let tmp = tempfile::tempdir().unwrap();
    let mut info = mk_info(&["A", "B"], &["A"]);
    info.options.defaults = ["A".to_string()].into();
    fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    fs::write(
        tmp.path().join("cat_port/options"),
        "_FILE_COMPLETE_OPTIONS_LIST=A B\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("cat_port/options.local"),
        "OPTIONS_FILE_SET+=B\n",
    )
    .unwrap();
    let enabled: BTreeSet<String> = ["A".to_string()].into();
    let planned = plan_writes(
        std::iter::once((&info.key.clone(), &info, enabled)),
        &OptionsFiles::new(tmp.path()),
        true,
    );
    assert!(planned.removals.is_empty(), "{:?}", planned.removals);
    assert!(planned.writes.is_empty(), "{:?}", planned.writes);
}

/// A file `make config` wrote with LDAPS on and LDAP unset, LDAPS
/// implying LDAP, is up to date: make turns LDAP on either way. Sync
/// plans from the file as written, the session from its IMPLIES-closed
/// baseline; both must find nothing to rewrite.
#[test]
fn sync_and_session_baseline_agree_under_implies() {
    let tmp = tempfile::tempdir().unwrap();
    let mut info = mk_info(&["LDAP", "LDAPS"], &["LDAP", "LDAPS"]);
    info.options.defs.insert(
        "LDAPS".into(),
        crate::model::options::OptionDef {
            implies: vec!["LDAP".into()],
            ..Default::default()
        },
    );
    fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    fs::write(
        tmp.path().join("cat_port/options"),
        "_FILE_COMPLETE_OPTIONS_LIST=LDAP LDAPS\n\
         OPTIONS_FILE_UNSET+=LDAP\nOPTIONS_FILE_SET+=LDAPS\n",
    )
    .unwrap();
    let files = OptionsFiles::new(tmp.path());

    let synced = sync_enabled_set(&info, files.saved("cat_port").as_ref());
    let planned = plan_writes(
        std::iter::once((&info.key.clone(), &info, synced)),
        &files,
        false,
    );
    assert!(planned.writes.is_empty(), "sync: {:?}", planned.writes);

    let mut ports = std::collections::BTreeMap::new();
    ports.insert(info.key.clone(), info.clone());
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    let sess = crate::session::Session::new(ports, Default::default(), &roots, &files, false);
    let staged = sess.state(&info).unwrap().staged.clone();
    let planned = plan_writes(
        std::iter::once((&info.key.clone(), &info, staged)),
        &files,
        false,
    );
    assert!(planned.writes.is_empty(), "session: {:?}", planned.writes);
}

/// The up-to-date check reads the file as make does. A hand-edited file
/// with A on both lines has A off (UNSET wins), so turning A on is a
/// change; comparing the raw SET lines missed it. A file missing a line
/// is completed even when make already reads it as wanted.
#[test]
fn plan_reads_the_file_as_make_does() {
    let tmp = tempfile::tempdir().unwrap();
    let mut info = mk_info(&["A", "B"], &["A"]);
    info.options.defaults = ["A".to_string()].into();
    let dir = tmp.path().join("cat_port");
    fs::create_dir_all(&dir).unwrap();
    let plan = |file: &str, enabled: &[&str]| {
        fs::write(dir.join("options"), file).unwrap();
        let enabled: BTreeSet<String> = enabled.iter().map(ToString::to_string).collect();
        let files = OptionsFiles::new(tmp.path());
        plan_writes(
            std::iter::once((&info.key.clone(), &info, enabled)),
            &files,
            false,
        )
    };
    let both = "_FILE_COMPLETE_OPTIONS_LIST=A B\n\
                OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_UNSET+=B\n";
    assert_eq!(plan(both, &["A"]).writes.len(), 1, "A was off, now on");
    assert!(plan(both, &[]).writes.is_empty(), "A was off, stays off");
    let no_line = "_FILE_COMPLETE_OPTIONS_LIST=A B\nOPTIONS_FILE_UNSET+=B\n";
    assert_eq!(
        plan(no_line, &[]).writes.len(),
        1,
        "A was on by default, now off"
    );
    let completed = plan(no_line, &["A"]);
    assert_eq!(
        completed.writes.len(),
        1,
        "A at its default, but the file has no line for it"
    );
    assert!(
        completed.writes[0]
            .content
            .contains("OPTIONS_FILE_SET+=A\n")
    );
}

/// In the TUI a background refresh queries make with the staged edits in
/// place, so `effective` already holds them. An option the file has no
/// line for must still be judged at its no-file value (defaults +
/// make.conf), or turning it on looks like nothing to write.
#[test]
fn plan_ignores_a_refreshed_effective_for_unlined_options() {
    let tmp = tempfile::tempdir().unwrap();
    // A default on, B default off; the refresh reports the staged A B.
    let mut info = mk_info(&["A", "B"], &["A", "B"]);
    info.options.defaults = ["A".to_string()].into();
    let dir = tmp.path().join("cat_port");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("options"),
        "_FILE_COMPLETE_OPTIONS_LIST=A B\nOPTIONS_FILE_SET+=A\n",
    )
    .unwrap();
    let enabled: BTreeSet<String> = ["A".to_string(), "B".to_string()].into();
    let files = OptionsFiles::new(tmp.path());
    let planned = plan_writes(
        std::iter::once((&info.key.clone(), &info, enabled)),
        &files,
        false,
    );
    assert_eq!(planned.writes.len(), 1, "B was off by default, now on");
}

/// make applies IMPLIES after the file: X on by default and implying Y,
/// the file unsetting both, leaves Y off. Asking for Y must write; asking
/// for nothing must not.
#[test]
fn plan_writes_an_option_implied_only_by_an_unset_default() {
    let tmp = tempfile::tempdir().unwrap();
    let mut info = mk_info(&["X", "Y"], &[]);
    info.options.defaults = ["X".to_string()].into();
    info.options.defs.insert(
        "X".into(),
        crate::model::options::OptionDef {
            implies: vec!["Y".into()],
            ..Default::default()
        },
    );
    let dir = tmp.path().join("cat_port");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("options"),
        "_FILE_COMPLETE_OPTIONS_LIST=X Y\nOPTIONS_FILE_UNSET+=X\nOPTIONS_FILE_UNSET+=Y\n",
    )
    .unwrap();
    let plan = |enabled: &[&str]| {
        let enabled: BTreeSet<String> = enabled.iter().map(ToString::to_string).collect();
        let files = OptionsFiles::new(tmp.path());
        plan_writes(
            std::iter::once((&info.key.clone(), &info, enabled)),
            &files,
            false,
        )
    };
    assert_eq!(plan(&["Y"]).writes.len(), 1, "Y was off, now on");
    assert!(plan(&[]).writes.is_empty(), "nothing changes");
}

#[test]
fn plan_skips_semantically_identical() {
    let tmp = tempfile::tempdir().unwrap();
    let info = mk_info(&["A", "B"], &["A"]);
    let dir = tmp.path().join("cat_port");
    fs::create_dir_all(&dir).unwrap();
    // pkgname differs from info.pkgname but list+set match -> skip
    fs::write(
        dir.join("options"),
        "_OPTIONS_READ=port-0.9\n_FILE_COMPLETE_OPTIONS_LIST=A B\n\
         OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n",
    )
    .unwrap();
    let enabled: BTreeSet<String> = ["A".to_string()].into();
    let planned = plan_writes(
        std::iter::once((&info.key.clone(), &info, enabled)),
        &OptionsFiles::new(tmp.path()),
        false,
    );
    assert!(planned.writes.is_empty());
    assert!(planned.warnings.is_empty());
}

#[test]
fn shared_file_uses_default_flavor() {
    let tmp = tempfile::tempdir().unwrap();
    // Two flavors of one origin, default flavor first in FLAVORS.
    let mut a = mk_info(&["A", "B"], &["A"]);
    a.key = PortKey::parse("devel/git@default").unwrap();
    a.canonical = a.key.clone();
    a.flavors = vec!["default".into(), "lite".into()];
    let mut b = mk_info(&["A"], &[]);
    b.key = PortKey::parse("devel/git@lite").unwrap();
    b.canonical = b.key.clone();
    b.flavors = vec!["default".into(), "lite".into()];

    let ports = [
        (b.key.clone(), &b, BTreeSet::new()), // divergent non-default first
        (a.key.clone(), &a, ["A".to_string()].into()),
    ];
    let planned = plan_writes(
        ports.iter().map(|(k, i, e)| (k, *i, e.clone())),
        &OptionsFiles::new(tmp.path()),
        false,
    );
    assert_eq!(planned.writes.len(), 1);
    assert_eq!(planned.writes[0].key.to_string(), "devel/git@default");
    assert!(planned.writes[0].enabled.contains("A"));
    // A exists in BOTH flavors and they disagree -> real warning.
    assert_eq!(planned.warnings.len(), 1);
    assert!(planned.warnings[0].contains("devel/git@lite"));
    assert!(planned.warnings[0].contains("A"));
}

#[test]
fn flavor_excluded_options_do_not_warn() {
    let tmp = tempfile::tempdir().unwrap();
    // Default flavor has X11 (on); nox flavor excludes X11 entirely.
    // Both agree on the shared option A.
    let mut full = mk_info(&["A", "X11"], &["A", "X11"]);
    full.key = PortKey::parse("editors/emacs@full").unwrap();
    full.canonical = full.key.clone();
    full.flavors = vec!["full".into(), "nox".into()];
    let mut nox = mk_info(&["A"], &["A"]);
    nox.key = PortKey::parse("editors/emacs@nox").unwrap();
    nox.canonical = nox.key.clone();
    nox.flavors = vec!["full".into(), "nox".into()];

    let ports: Vec<(PortKey, &PortInfo, BTreeSet<String>)> = vec![
        (nox.key.clone(), &nox, ["A".to_string()].into()),
        (
            full.key.clone(),
            &full,
            ["A".to_string(), "X11".to_string()].into(),
        ),
    ];
    let planned = plan_writes(
        ports.iter().map(|(k, i, e)| (k, *i, e.clone())),
        &OptionsFiles::new(tmp.path()),
        false,
    );
    assert_eq!(planned.writes.len(), 1);
    assert_eq!(planned.writes[0].key.to_string(), "editors/emacs@full");
    assert!(
        planned.warnings.is_empty(),
        "structural exclusion must not warn: {:?}",
        planned.warnings
    );
}

/// A non-default flavor alone sees only its own options: the default
/// flavor's others in the shared file are not its to drop.
#[test]
fn a_non_default_flavor_keeps_the_options_it_lacks() {
    let tmp = tempfile::tempdir().unwrap();
    let mut nox = mk_info(&["A", "B"], &["A"]);
    nox.key = PortKey::parse("editors/emacs@nox").unwrap();
    nox.canonical = nox.key.clone();
    nox.flavors = vec!["full".into(), "nox".into()];
    nox.options_name = "editors_emacs".into();
    fs::create_dir_all(tmp.path().join("editors_emacs")).unwrap();
    fs::write(
        tmp.path().join("editors_emacs/options"),
        "_OPTIONS_READ=emacs-1.0\n_FILE_COMPLETE_OPTIONS_LIST=A B X11 GTK\n\
         OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\nOPTIONS_FILE_SET+=X11\nOPTIONS_FILE_UNSET+=GTK\n",
    )
    .unwrap();
    let set = |opts: &[&str]| {
        opts.iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<String>>()
    };
    let plan = |enabled: BTreeSet<String>| {
        plan_writes(
            std::iter::once((&nox.key, &nox, enabled)),
            &OptionsFiles::new(tmp.path()),
            false,
        )
    };

    // Nothing @nox sees changed: the file is up to date.
    assert!(
        plan(set(&["A"])).writes.is_empty(),
        "a superset list is up to date for @nox"
    );

    // B turned on: written, X11 and GTK kept as they were, not dropped.
    let planned = plan(set(&["A", "B"]));
    assert_eq!(planned.writes.len(), 1);
    let w = &planned.writes[0];
    let c = w.changes();
    assert_eq!(c.turned_on, ["B"]);
    assert!(c.dropped.is_empty() && c.turned_off.is_empty(), "{c:?}");
    let written = SavedOptionsFile::parse(&w.content);
    assert_eq!(written.complete, ["A", "B", "X11", "GTK"]);
    assert_eq!(written.set, set(&["A", "B", "X11"]));
    assert_eq!(written.unset, set(&["GTK"]));
}

/// A hand-written UNSET pattern is no option the non-default flavor lacks:
/// carried over, make would apply it after the SET lines and take off the
/// option just turned on.
#[test]
fn a_non_default_flavor_does_not_carry_a_pattern_over() {
    let tmp = tempfile::tempdir().unwrap();
    let mut b = mk_info(&["A1", "OTHER"], &[]);
    b.key = PortKey::parse("cat/foo@b").unwrap();
    b.canonical = b.key.clone();
    b.flavors = vec!["a".into(), "b".into()];
    b.options_name = "cat_foo".into();
    fs::create_dir_all(tmp.path().join("cat_foo")).unwrap();
    fs::write(
        tmp.path().join("cat_foo/options"),
        "_OPTIONS_READ=foo-1.0\n_FILE_COMPLETE_OPTIONS_LIST=A1 OTHER\n\
         OPTIONS_FILE_UNSET+=A1\nOPTIONS_FILE_UNSET+=OTHER\nOPTIONS_FILE_UNSET+=A*\n",
    )
    .unwrap();
    let planned = plan_writes(
        std::iter::once((&b.key, &b, ["A1".to_string()].into())),
        &OptionsFiles::new(tmp.path()),
        false,
    );
    assert_eq!(planned.writes.len(), 1);
    let written = SavedOptionsFile::parse(&planned.writes[0].content);
    assert!(
        !written.unset.contains("A*"),
        "{}",
        planned.writes[0].content
    );
    assert_eq!(
        crate::session::effective_with(&b, Some(&written)),
        ["A1".to_string()].into()
    );
}

/// An unconfigured port whose conditional group's option is off: make drops
/// the default-on member from PORT_OPTIONS, but its helpers ran with it, so
/// -m writes no file recording it off (comms/splat's HRM_4X4).
#[test]
fn minimal_mode_keeps_a_dropped_members_default() {
    use crate::model::options::{GroupKind, OptionGroup};
    let tmp = tempfile::tempdir().unwrap();
    let mut info = mk_info(&["HRM", "HRM_4X4", "HRM_8X8"], &[]);
    info.options.defaults = ["HRM_4X4".to_string()].into();
    info.options.all = ["HRM".to_string()].into();
    info.options.groups = vec![OptionGroup {
        kind: GroupKind::Single,
        name: "HRM".into(),
        desc: String::new(),
        members: vec!["HRM_4X4".into(), "HRM_8X8".into()],
    }];
    // make's PORT_OPTIONS: HRM off, so HRM_4X4 dropped.
    info.options.effective = BTreeSet::new();
    let enabled = sync_enabled_set(&info, None);
    assert!(enabled.contains("HRM_4X4"), "{enabled:?}");
    let p = plan_writes(
        std::iter::once((&info.key, &info, enabled)),
        &OptionsFiles::new(tmp.path()),
        true,
    );
    assert!(
        p.writes.is_empty(),
        "{:?}",
        p.writes.iter().map(|w| &w.content).collect::<Vec<_>>()
    );
}

/// The pattern dropped from a non-default flavor's write also turned off an
/// option outside its view (the default flavor's A2): A2 gets a line of its
/// own, and stays off.
#[test]
fn a_dropped_pattern_keeps_what_it_turned_off_outside_the_view() {
    let tmp = tempfile::tempdir().unwrap();
    let mut f = mk_info(&["A1", "X"], &[]);
    f.key = PortKey::parse("cat/foo@f").unwrap();
    f.canonical = f.key.clone();
    f.flavors = vec!["a".into(), "f".into()];
    f.options_name = "cat_foo".into();
    fs::create_dir_all(tmp.path().join("cat_foo")).unwrap();
    fs::write(
        tmp.path().join("cat_foo/options"),
        "_FILE_COMPLETE_OPTIONS_LIST=A1 A2 X\nOPTIONS_FILE_UNSET+=A* X\n",
    )
    .unwrap();
    let planned = plan_writes(
        std::iter::once((&f.key, &f, ["X".to_string()].into())),
        &OptionsFiles::new(tmp.path()),
        false,
    );
    let written = SavedOptionsFile::parse(&planned.writes[0].content);
    assert_eq!(
        written.value("A2"),
        Some(false),
        "{}",
        planned.writes[0].content
    );
}

/// Under -m the file goes when the file it would write gives the no-file
/// outcome, OPTIONS_SLAVE included: www/w3m-img with nothing chosen
/// needs no file, and one repeating its defaults is removed.
#[test]
fn minimal_mode_judges_the_file_it_would_write() {
    let tmp = tempfile::tempdir().unwrap();
    let mut info = mk_info(&["A", "B"], &["A", "S"]);
    info.options.defaults = ["A".to_string()].into();
    info.options.slave = ["S".to_string()].into();
    let set = |opts: &[&str]| {
        opts.iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<String>>()
    };
    let plan = |enabled: BTreeSet<String>| {
        plan_writes(
            std::iter::once((&info.key, &info, enabled)),
            &OptionsFiles::new(tmp.path()),
            true,
        )
    };
    let p = plan(set(&["A"]));
    assert!(
        p.writes.is_empty() && p.removals.is_empty(),
        "no file needed"
    );
    fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    fs::write(
        tmp.path().join("cat_port/options"),
        "_FILE_COMPLETE_OPTIONS_LIST=A B\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n",
    )
    .unwrap();
    let p = plan(set(&["A"]));
    assert!(p.writes.is_empty());
    assert_eq!(p.removals.len(), 1, "a file repeating the defaults goes");
}

/// Under -m a non-default flavor alone never removes a shared file
/// holding lines it cannot see: they are the default flavor's choices.
#[test]
fn minimal_mode_keeps_lines_a_lone_flavor_cannot_see() {
    let tmp = tempfile::tempdir().unwrap();
    let mut nox = mk_info(&["A"], &["A"]);
    nox.options.defaults = ["A".to_string()].into();
    nox.key = PortKey::parse("editors/emacs@nox").unwrap();
    nox.canonical = nox.key.clone();
    nox.flavors = vec!["full".into(), "nox".into()];
    nox.options_name = "editors_emacs".into();
    fs::create_dir_all(tmp.path().join("editors_emacs")).unwrap();
    fs::write(
        tmp.path().join("editors_emacs/options"),
        "_FILE_COMPLETE_OPTIONS_LIST=A X11\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=X11\n",
    )
    .unwrap();
    let enabled: BTreeSet<String> = ["A".to_string()].into();
    let p = plan_writes(
        std::iter::once((&nox.key, &nox, enabled)),
        &OptionsFiles::new(tmp.path()),
        true,
    );
    assert!(
        p.removals.is_empty(),
        "X11 is @full's choice: {:?}",
        p.removals.len()
    );
    assert!(p.writes.is_empty(), "nothing @nox sees changed");
}

/// Two non-default flavors and no default one: an edit to an option only
/// one of them has is written, with that flavor's value.
#[test]
fn two_lone_flavors_each_write_what_they_have() {
    let tmp = tempfile::tempdir().unwrap();
    let flavor = |name: &str, complete: &[&str]| {
        let mut i = mk_info(complete, &[]);
        i.key = PortKey::parse(&format!("editors/emacs@{name}")).unwrap();
        i.canonical = i.key.clone();
        i.flavors = vec!["full".into(), "nox".into(), "pgtk".into()];
        i.options_name = "editors_emacs".into();
        i
    };
    let nox = flavor("nox", &["A"]);
    let pgtk = flavor("pgtk", &["A", "Y"]);
    fs::create_dir_all(tmp.path().join("editors_emacs")).unwrap();
    fs::write(
        tmp.path().join("editors_emacs/options"),
        "_FILE_COMPLETE_OPTIONS_LIST=A Y\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=Y\n",
    )
    .unwrap();
    let set = |opts: &[&str]| {
        opts.iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<String>>()
    };
    let p = plan_writes(
        [
            (&nox.key, &nox, set(&["A"])),
            (&pgtk.key, &pgtk, set(&["A", "Y"])),
        ]
        .into_iter(),
        &OptionsFiles::new(tmp.path()),
        false,
    );
    assert_eq!(p.writes.len(), 1, "@pgtk turned Y on");
    let written = SavedOptionsFile::parse(&p.writes[0].content);
    assert_eq!(written.set, set(&["A", "Y"]));
    assert!(written.unset.is_empty());
}

/// Without the default flavor, the write follows the session's owner
/// (`session::outranks`): the widest view, wherever it comes in the
/// closure, and the first met on ties — not the first candidate.
#[test]
fn without_the_default_flavor_the_widest_view_writes_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let flavor = |name: &str, complete: &[&str]| {
        let mut i = mk_info(complete, &[]);
        i.key = PortKey::parse(&format!("editors/emacs@{name}")).unwrap();
        i.canonical = i.key.clone();
        i.flavors = vec!["full".into(), "nox".into(), "pgtk".into()];
        i.options_name = "editors_emacs".into();
        i
    };
    let set = |opts: &[&str]| {
        opts.iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<String>>()
    };
    // @nox first and narrower, @pgtk second and wider; they disagree on B.
    let nox = flavor("nox", &["A", "B"]);
    let pgtk = flavor("pgtk", &["A", "B", "Y"]);
    let p = plan_writes(
        [
            (&nox.key, &nox, set(&["A"])),
            (&pgtk.key, &pgtk, set(&["A", "B", "Y"])),
        ]
        .into_iter(),
        &OptionsFiles::new(tmp.path()),
        false,
    );
    assert_eq!(p.writes.len(), 1);
    assert_eq!(
        p.writes[0].key.to_string(),
        "editors/emacs@pgtk",
        "the widest view owns the file"
    );
    let written = SavedOptionsFile::parse(&p.writes[0].content);
    assert_eq!(written.complete, vec!["A", "B", "Y"]);
    assert_eq!(
        written.set,
        set(&["A", "B", "Y"]),
        "B as @pgtk has it, not @nox"
    );
    assert!(written.unset.is_empty());
    assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
    assert!(
        p.warnings[0].starts_with("editors/emacs@nox:"),
        "{}",
        p.warnings[0]
    );
    assert!(
        p.warnings[0].contains("using editors/emacs@pgtk (widest view)"),
        "{}",
        p.warnings[0]
    );

    // Same width: the first met owns it, and the other's option is kept.
    let x = flavor("nox", &["A", "B"]);
    let y = flavor("pgtk", &["A", "Y"]);
    let p = plan_writes(
        [(&x.key, &x, set(&["A", "B"])), (&y.key, &y, set(&["A"]))].into_iter(),
        &OptionsFiles::new(tmp.path()),
        false,
    );
    assert_eq!(p.writes.len(), 1);
    assert_eq!(
        p.writes[0].key.to_string(),
        "editors/emacs@nox",
        "first on ties"
    );
    let written = SavedOptionsFile::parse(&p.writes[0].content);
    assert_eq!(written.complete, vec!["A", "B", "Y"]);
    assert_eq!(written.set, set(&["A", "B"]));
    assert_eq!(written.unset, set(&["Y"]));
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
}

#[test]
fn describe_covers_all_change_kinds() {
    // Old file: knows A(on) B(off) GONE(off); current: A B NEWOPT.
    let old = SavedOptionsFile::parse(
        "_OPTIONS_READ=p-1\n_FILE_COMPLETE_OPTIONS_LIST=A B GONE\n\
         OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\nOPTIONS_FILE_UNSET+=GONE\n",
    );
    let w = PendingWrite {
        key: PortKey::parse("cat/port").unwrap(),
        options_name: "cat_port".into(),
        path: PathBuf::from("/nonexistent"),
        old: Some(old),
        // A turned off, B turned on, NEWOPT adopted on, GONE dropped.
        enabled: ["B".to_string(), "NEWOPT".to_string()].into(),
        complete: vec!["A".into(), "B".into(), "NEWOPT".into()],
        kept: vec![],
        content: String::new(),
    };
    let d = w.describe();
    assert!(d.contains("+B"), "{d}");
    assert!(d.contains("-A"), "{d}");
    assert!(d.contains("NEWOPT(on)"), "{d}");
    assert!(d.contains("dropped: GONE"), "{d}");
    // A brand-new file describes itself by option count.
    let w2 = PendingWrite { old: None, ..w };
    assert!(w2.describe().contains("new file"));
}

/// A new file records every option of the port, not just the enabled
/// ones: the summary counts both.
#[test]
fn describe_new_file_counts_every_option() {
    let w = pending(None, &["A", "B", "C"], &["B"]);
    assert_eq!(w.describe(), "new file (1 of 3 on)");
}

#[test]
fn stale_removal_spares_files_shared_with_optioned_flavors() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("devel_glib20")).unwrap();
    fs::write(tmp.path().join("devel_glib20/options"), "# x\n").unwrap();
    fs::create_dir_all(tmp.path().join("print_indexinfo")).unwrap();
    fs::write(tmp.path().join("print_indexinfo/options"), "# x\n").unwrap();

    // glib20@default has options; glib20@bootstrap shares the file and
    // has none. indexinfo has none at all.
    let mut default = mk_info(&["DOCS"], &[]);
    default.key = PortKey::parse("devel/glib20@default").unwrap();
    default.canonical = default.key.clone();
    default.options_name = "devel_glib20".into();
    let mut bootstrap = mk_info(&[], &[]);
    bootstrap.key = PortKey::parse("devel/glib20@bootstrap").unwrap();
    bootstrap.canonical = bootstrap.key.clone();
    bootstrap.options_name = "devel_glib20".into();
    let mut indexinfo = mk_info(&[], &[]);
    indexinfo.key = PortKey::parse("print/indexinfo").unwrap();
    indexinfo.canonical = indexinfo.key.clone();
    indexinfo.options_name = "print_indexinfo".into();

    let mut ports = std::collections::BTreeMap::new();
    for i in [default, bootstrap, indexinfo] {
        ports.insert(i.key.clone(), i);
    }
    let removals = plan_stale_removals(&ports, &OptionsFiles::new(tmp.path()));
    let names: Vec<&str> = removals.iter().map(|r| r.options_name.as_str()).collect();
    assert_eq!(
        names,
        vec!["print_indexinfo"],
        "glib20's shared file must survive"
    );
}

/// A closure holding only an optionless non-default flavor of a port
/// (gobject-introspection@bootstrap, reached from editors/emacs@nox)
/// proves nothing about the shared file its default flavor reads: kept.
/// With the default flavor itself optionless, the file goes.
#[test]
fn stale_removal_needs_the_default_flavors_view() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("devel_gi")).unwrap();
    fs::write(tmp.path().join("devel_gi/options"), "# x\n").unwrap();
    let flavored = |flavor: &str| {
        let mut i = mk_info(&[], &[]);
        i.key = PortKey::parse(&format!("devel/gi@{flavor}")).unwrap();
        i.canonical = i.key.clone();
        i.flavors = vec!["default".into(), "bootstrap".into()];
        i.options_name = "devel_gi".into();
        i
    };
    let plan = |infos: Vec<PortInfo>| {
        let ports: std::collections::BTreeMap<_, _> =
            infos.into_iter().map(|i| (i.key.clone(), i)).collect();
        plan_stale_removals(&ports, &OptionsFiles::new(tmp.path())).len()
    };
    assert_eq!(
        plan(vec![flavored("bootstrap")]),
        0,
        "@bootstrap alone keeps the file"
    );
    assert_eq!(plan(vec![flavored("default"), flavored("bootstrap")]), 1);
}

/// Planning a whole closure reads nothing the session already read, and
/// judges each file as the session saw it.
#[test]
fn plans_against_the_store_without_reading_again() {
    let tmp = tempfile::tempdir().unwrap();
    let info = mk_info(&["A"], &[]);
    fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    fs::write(
        tmp.path().join("cat_port/options"),
        "_FILE_COMPLETE_OPTIONS_LIST=A\nOPTIONS_FILE_SET+=A\n",
    )
    .unwrap();
    let files = OptionsFiles::new(tmp.path());
    assert!(files.saved("cat_port").is_some());
    let mut gone = mk_info(&[], &[]);
    gone.key = PortKey::parse("cat/gone").unwrap();
    gone.options_name = "cat_gone".into();
    assert!(files.bytes("cat_gone").is_none());
    let reads = files.disk_reads();

    // Changed on disk since: the plan still compares with what was read,
    // so a port nobody edited is left alone rather than rewritten.
    fs::write(
        tmp.path().join("cat_port/options"),
        "OPTIONS_FILE_UNSET+=A\n",
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join("cat_gone")).unwrap();
    fs::write(tmp.path().join("cat_gone/options"), "# x\n").unwrap();

    let enabled: BTreeSet<String> = ["A".to_string()].into();
    let planned = plan_writes(
        std::iter::once((&info.key.clone(), &info, enabled)),
        &files,
        false,
    );
    assert!(planned.writes.is_empty());
    let mut ports = std::collections::BTreeMap::new();
    ports.insert(gone.key.clone(), gone);
    assert!(
        plan_stale_removals(&ports, &files).is_empty(),
        "absent when read"
    );
    assert_eq!(files.disk_reads(), reads);
}

#[test]
fn minimal_mode_persists_only_deviations() {
    let tmp = tempfile::tempdir().unwrap();
    // defaults {A}, no make.conf: the no-file outcome is {A}.
    let mut info = mk_info(&["A", "B"], &["A"]);
    info.options.defaults = ["A".to_string()].into();

    // Redundant state + existing file -> removal, no write.
    fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    fs::write(tmp.path().join("cat_port/options"), "# x\n").unwrap();
    let enabled: BTreeSet<String> = ["A".to_string()].into();
    let planned = plan_writes(
        std::iter::once((&info.key.clone(), &info, enabled.clone())),
        &OptionsFiles::new(tmp.path()),
        true,
    );
    assert!(planned.writes.is_empty());
    assert_eq!(planned.removals.len(), 1);
    assert_eq!(planned.removals[0].options_name, "cat_port");

    // Redundant state, no file -> nothing at all.
    fs::remove_dir_all(tmp.path().join("cat_port")).unwrap();
    let planned = plan_writes(
        std::iter::once((&info.key.clone(), &info, enabled)),
        &OptionsFiles::new(tmp.path()),
        true,
    );
    assert!(planned.writes.is_empty() && planned.removals.is_empty());

    // Deviating state -> written as usual even in minimal mode.
    let deviating: BTreeSet<String> = ["A".to_string(), "B".to_string()].into();
    let planned = plan_writes(
        std::iter::once((&info.key.clone(), &info, deviating)),
        &OptionsFiles::new(tmp.path()),
        true,
    );
    assert_eq!(planned.writes.len(), 1);
    assert!(planned.removals.is_empty());
}

/// A file the default flavor could drop still decides another flavor
/// sharing it: www/sogo@pgsql defaults to PGSQL, the file says MYSQL.
#[test]
fn minimal_mode_keeps_a_file_another_flavor_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let flavor = |f: &str, default: &str| {
        let mut info = mk_info(&["MYSQL", "PGSQL"], &[default]);
        info.key = PortKey::parse(&format!("www/sogo@{f}")).unwrap();
        info.canonical = info.key.clone();
        info.flavors = vec!["mysql".into(), "pgsql".into()];
        info.options_name = "www_sogo".into();
        info.options.defaults = [default.to_string()].into();
        info
    };
    let (mysql, pgsql) = (flavor("mysql", "MYSQL"), flavor("pgsql", "PGSQL"));
    fs::create_dir_all(tmp.path().join("www_sogo")).unwrap();
    fs::write(
        tmp.path().join("www_sogo/options"),
        "_OPTIONS_READ=port-1.0\n_FILE_COMPLETE_OPTIONS_LIST=MYSQL PGSQL\n\
         OPTIONS_FILE_SET+=MYSQL\nOPTIONS_FILE_UNSET+=PGSQL\n",
    )
    .unwrap();
    let mysql_on: BTreeSet<String> = ["MYSQL".to_string()].into();
    let ports = [(&mysql, mysql_on.clone()), (&pgsql, mysql_on.clone())];
    let planned = plan_writes(
        ports.iter().map(|(i, e)| (&i.key, *i, e.clone())),
        &OptionsFiles::new(tmp.path()),
        true,
    );
    assert!(
        planned.removals.is_empty(),
        "@pgsql reads MYSQL from the file: {:?}",
        planned.removals
    );
    assert!(planned.writes.is_empty(), "the file is up to date");

    // Alone, the default flavor cannot see @pgsql: the file goes.
    let planned = plan_writes(
        std::iter::once((&mysql.key, &mysql, mysql_on)),
        &OptionsFiles::new(tmp.path()),
        true,
    );
    assert_eq!(planned.removals.len(), 1);
}

#[test]
fn apply_reports_failures() {
    let w = PendingWrite {
        key: PortKey::parse("cat/port").unwrap(),
        options_name: "cat_port".into(),
        // Parent is a file, not a dir -> create_dir_all fails.
        path: PathBuf::from("/dev/null/cat_port/options"),
        old: None,
        enabled: BTreeSet::new(),
        complete: vec![],
        kept: vec![],
        content: "x\n".into(),
    };
    let summary = apply(&[w]);
    assert_eq!(summary.written, 0);
    assert_eq!(summary.failed.len(), 1);
}

/// `decide` is all-or-nothing: when one file of the pass cannot be
/// written, none is, and no temporary file is left behind.
#[test]
fn decide_write_failure_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let w = |name: &str| PendingWrite {
        key: PortKey::parse(&name.replace('_', "/")).unwrap(),
        options_name: name.into(),
        path: tmp.path().join(name).join("options"),
        old: None,
        enabled: BTreeSet::new(),
        complete: vec![],
        kept: vec![],
        content: "x\n".into(),
    };
    // Sorted first, so it is the one that would land before the failure.
    let ok = w("sysutils_tmux");
    // A directory squatting the temp name: creating the temp file fails.
    let bad = w("textproc_expat2");
    fs::create_dir_all(tmp.path().join("textproc_expat2/.options.optique.tmp")).unwrap();
    let summary = apply(&[ok, bad]);
    assert_eq!(summary.written, 0);
    assert_eq!(summary.failed.len(), 1, "{:?}", summary.failed);
    assert_eq!(summary.failed[0].0.to_string(), "textproc/expat2");
    assert!(
        !tmp.path().join("sysutils_tmux/options").exists(),
        "nothing written"
    );
    assert!(
        !tmp.path()
            .join("sysutils_tmux/.options.optique.tmp")
            .exists(),
        "temp removed"
    );
}

/// A rename that fails (a directory where the file goes) stops the pass:
/// the file renamed before it stays and is counted, the one after is not
/// renamed, and no temporary file is left behind.
#[test]
fn rename_failure_stops_the_pass_and_counts_what_landed() {
    let tmp = tempfile::tempdir().unwrap();
    let w = |name: &str| PendingWrite {
        key: PortKey::parse(&name.replace('_', "/")).unwrap(),
        options_name: name.into(),
        path: tmp.path().join(name).join("options"),
        old: None,
        enabled: BTreeSet::new(),
        complete: vec![],
        kept: vec![],
        content: "x\n".into(),
    };
    fs::create_dir_all(tmp.path().join("textproc_expat2/options")).unwrap();
    let summary = apply(&[w("sysutils_tmux"), w("textproc_expat2"), w("www_nginx")]);
    assert_eq!(summary.written, 1);
    let failed: Vec<String> = summary.failed.iter().map(|(k, _)| k.to_string()).collect();
    assert_eq!(failed, ["textproc/expat2", "www/nginx"]);
    assert!(tmp.path().join("sysutils_tmux/options").is_file());
    assert!(
        !tmp.path().join("www_nginx/options").exists(),
        "not renamed"
    );
    for name in ["textproc_expat2", "www_nginx"] {
        assert!(
            !tmp.path().join(name).join(".options.optique.tmp").exists(),
            "temp removed"
        );
    }
}

#[test]
fn apply_writes_atomically() {
    let tmp = tempfile::tempdir().unwrap();
    let info = mk_info(&["A"], &["A"]);
    let enabled: BTreeSet<String> = ["A".to_string()].into();
    let planned = plan_writes(
        std::iter::once((&info.key.clone(), &info, enabled)),
        &OptionsFiles::new(tmp.path()),
        false,
    );
    let writes = planned.writes;
    assert_eq!(writes.len(), 1);
    let summary = apply(&writes);
    assert_eq!(summary.written, 1);
    assert!(summary.failed.is_empty());
    let saved = OptionsFiles::new(tmp.path()).saved("cat_port").unwrap();
    assert!(saved.set.contains("A"));
}

/// Skipping a file drops its write and its removal, and nothing else.
#[test]
fn a_plan_without_skipped_files_keeps_the_others() {
    let tmp = tempfile::tempdir().unwrap();
    // Defaults {A}: {A} repeats them (a removal under -m), {A, B} does not.
    let mk = |origin: &str| {
        let mut options = mk_info(&["A", "B"], &["A"]).options;
        options.defaults = ["A".to_string()].into();
        PortInfo {
            options,
            ..PortInfo::stub(origin)
        }
    };
    let ports = [mk("cat/one"), mk("cat/two"), mk("cat/three")];
    for name in ["cat_one", "cat_three"] {
        fs::create_dir_all(tmp.path().join(name)).unwrap();
        fs::write(tmp.path().join(name).join("options"), "# x\n").unwrap();
    }
    let set = |opts: &[&str]| {
        opts.iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<String>>()
    };
    let enabled = [set(&["A", "B"]), set(&["A", "B"]), set(&["A"])];
    let plan = || {
        plan_writes(
            ports
                .iter()
                .zip(&enabled)
                .map(|(i, e)| (&i.key, i, e.clone())),
            &OptionsFiles::new(tmp.path()),
            true,
        )
    };
    let all = plan();
    assert_eq!(all.writes.len(), 2);
    assert_eq!(all.removals.len(), 1);
    assert_eq!(
        all.touched().collect::<BTreeSet<_>>(),
        ["cat_one", "cat_three", "cat_two"].into()
    );
    let kept = all.without(&set(&["cat_one", "cat_three"]));
    assert_eq!(kept.touched().collect::<Vec<_>>(), ["cat_two"]);
    let kept = plan().without(&set(&["cat_two"]));
    assert_eq!(
        kept.writes
            .iter()
            .map(|w| w.options_name.as_str())
            .collect::<Vec<_>>(),
        ["cat_one"]
    );
    assert_eq!(kept.removals[0].options_name, "cat_three");
}

/// Two files of one entry, a port's `options` and a flavor's own
/// `lite-options`, written in one pass: each goes through a temporary file
/// of its own and lands with its own content.
#[test]
fn two_files_of_one_entry_are_written_in_one_pass() {
    let tmp = tempfile::tempdir().unwrap();
    let w = |name: &str, file: &str, content: &str| PendingWrite {
        key: PortKey::parse("devel/git").unwrap(),
        options_name: name.into(),
        path: tmp.path().join("devel_git").join(file),
        old: None,
        enabled: BTreeSet::new(),
        complete: vec![],
        kept: vec![],
        content: content.into(),
    };
    let summary = apply(&[
        w("devel_git", "options", "default\n"),
        w("devel_git/lite-options", "lite-options", "lite\n"),
    ]);
    assert_eq!(
        (summary.written, summary.failed.len()),
        (2, 0),
        "{:?}",
        summary.failed
    );
    let read = |file: &str| fs::read_to_string(tmp.path().join("devel_git").join(file)).unwrap();
    assert_eq!(read("options"), "default\n");
    assert_eq!(read("lite-options"), "lite\n");
}

fn set_of(options: &[&str]) -> BTreeSet<String> {
    options.iter().map(ToString::to_string).collect()
}

/// Like `make config-conditional`, sync and the TUI's apply create no file
/// for a port make.conf fully decides, staged as sync would stage it: the
/// file would outrank make.conf from then on. decide's plan writes it, and
/// so does an edit, or an option nothing decides.
#[test]
fn leave_creates_no_file_for_a_port_make_conf_decides() {
    let tmp = tempfile::tempdir().unwrap();
    let files = OptionsFiles::new(tmp.path());
    let mut info = mk_info(&["A", "B"], &["A"]);
    info.options.mc_set = set_of(&["A"]);
    info.options.mc_unset = set_of(&["B"]);
    let plan = |info: &PortInfo, enabled: BTreeSet<String>, covered| {
        plan_writes_for(
            std::iter::once((&info.key.clone(), info, enabled)),
            &files,
            false,
            covered,
        )
        .writes
        .len()
    };
    let none = BTreeSet::new();
    let leave = Covered::Leave { edited: &none };
    let as_sync = sync_enabled_set(&info, None);
    assert_eq!(plan(&info, as_sync.clone(), leave), 0);
    assert_eq!(plan(&info, as_sync, Covered::Write), 1, "decide writes it");
    // B turned on, and a refresh queried the edit into PORT_OPTIONS: only
    // the caller can tell it from what sync records.
    let mut refreshed = info.clone();
    refreshed.options.effective = set_of(&["A", "B"]);
    let edited = set_of(&["cat_port"]);
    assert_eq!(
        plan(
            &refreshed,
            set_of(&["A", "B"]),
            Covered::Leave { edited: &edited }
        ),
        1,
        "an edit is written"
    );
    let mut open = info.clone();
    open.options.mc_unset.clear();
    let staged = sync_enabled_set(&open, None);
    assert_eq!(plan(&open, staged, leave), 1, "B is undecided");
}

/// Flavors sharing a file make.conf decides for each, their option lists
/// apart (devel/git@lite), get none either: the shared staged set is no
/// flavor's own, and is not compared with one.
#[test]
fn leave_creates_no_file_for_flavors_make_conf_decides() {
    let tmp = tempfile::tempdir().unwrap();
    let flavored = |flavor: &str, complete: &[&str]| {
        let mut info = mk_info(complete, &["A"]);
        info.key = PortKey::parse(&format!("devel/git@{flavor}")).unwrap();
        info.canonical = info.key.clone();
        info.flavors = vec!["default".into(), "lite".into()];
        info.options.mc_set = set_of(&["A", "GUI"]);
        info.options.mc_unset = set_of(&["B"]);
        info
    };
    let mut full = flavored("default", &["A", "B", "GUI"]);
    full.options.effective = set_of(&["A", "GUI"]);
    let lite = flavored("lite", &["A", "B"]);
    let shared = sync_enabled_set(&full, None);
    assert_ne!(shared, sync_enabled_set(&lite, None), "GUI is full's alone");
    let ports = [(&full.key, &full), (&lite.key, &lite)];
    let none = BTreeSet::new();
    let planned = plan_writes_for(
        ports.iter().map(|(k, i)| (*k, *i, shared.clone())),
        &OptionsFiles::new(tmp.path()),
        false,
        Covered::Leave { edited: &none },
    );
    assert!(planned.writes.is_empty(), "{:?}", planned.writes);
}

/// Under a default flavor, which writes only its own options, another
/// flavor's option the file never records (audacious-plugins@gtk3's AOSD)
/// asks for no file, which would settle nothing for it; and its row owes
/// nothing either, so scan and the TUI count it done as sync leaves it.
#[test]
fn leave_judges_a_default_flavor_owner_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let flavored = |flavor: &str, complete: &[&str]| {
        let mut info = mk_info(complete, &["A"]);
        info.key = PortKey::parse(&format!("audio/plugins@{flavor}")).unwrap();
        info.canonical = info.key.clone();
        info.flavors = vec!["qt".into(), "gtk3".into()];
        info.options.mc_set = set_of(&["A"]);
        info
    };
    let qt = flavored("qt", &["A"]);
    let gtk3 = flavored("gtk3", &["A", "AOSD"]);
    let files = OptionsFiles::new(tmp.path());
    let ports = [(&qt.key, &qt), (&gtk3.key, &gtk3)];
    let none = BTreeSet::new();
    let planned = plan_writes_for(
        ports
            .iter()
            .map(|(k, i)| (*k, *i, sync_enabled_set(&qt, None))),
        &files,
        false,
        Covered::Leave { edited: &none },
    );
    assert!(planned.writes.is_empty(), "{:?}", planned.writes);

    let by_key: std::collections::BTreeMap<PortKey, PortInfo> = [qt.clone(), gtk3.clone()]
        .into_iter()
        .map(|i| (i.key.clone(), i))
        .collect();
    let roots: Vec<PortKey> = by_key.keys().cloned().collect();
    let session = crate::session::Session::new(
        by_key,
        std::collections::HashMap::new(),
        &roots,
        &files,
        false,
    );
    let row = &session.ports[&gtk3.key];
    assert!(session.unstored(row, "AOSD").is_some(), "the premise");
    assert!(
        session.undecided(row).is_empty(),
        "{:?}",
        session.undecided(row)
    );
    assert!(session.covered_by_makeconf(row), "done, as sync leaves it");
}

/// A stale file is rewritten all the same, its port nothing undecided: it
/// has outranked make.conf since it was written, and left as it is it would
/// keep an option the port lacks, which synth refuses.
#[test]
fn leave_still_rewrites_a_stale_file() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    fs::write(
        tmp.path().join("cat_port/options"),
        "_FILE_COMPLETE_OPTIONS_LIST=DOCS OBSOLETE\nOPTIONS_FILE_SET+=OBSOLETE\nOPTIONS_FILE_UNSET+=DOCS\n",
    )
    .unwrap();
    let files = OptionsFiles::new(tmp.path());
    let mut info = mk_info(&["DOCS", "NEW"], &[]);
    info.options.mc_unset = set_of(&["NEW"]);
    let saved = files.saved("cat_port");
    let enabled = sync_enabled_set(&info, saved.as_ref());
    let planned = plan_writes_for(
        std::iter::once((&info.key.clone(), &info, enabled)),
        &files,
        false,
        Covered::Leave {
            edited: &BTreeSet::new(),
        },
    );
    assert_eq!(planned.writes.len(), 1, "{:?}", planned.writes);
    assert!(
        !planned.writes[0].content.contains("OBSOLETE"),
        "{}",
        planned.writes[0].content
    );
}
