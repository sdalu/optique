use super::*;
use crate::model::options::{OptionDef, OptionGroup, PortOptions};

fn mk_port(
    complete: &[&str],
    defaults: &[&str],
    groups: Vec<OptionGroup>,
    defs: Vec<(&str, OptionDef)>,
) -> (BTreeMap<PortKey, PortInfo>, PortKey) {
    let key = PortKey::parse("cat/port").unwrap();
    let mut d = BTreeMap::new();
    for (name, def) in defs {
        d.insert(name.to_string(), def);
    }
    let info = PortInfo {
        pkgname: "port-1.0".into(),
        options_name: "cat_port".into(),
        options: PortOptions {
            complete: complete.iter().map(ToString::to_string).collect(),
            defaults: defaults.iter().map(ToString::to_string).collect(),
            effective: defaults.iter().map(ToString::to_string).collect(),
            groups,
            defs: d,
            ..Default::default()
        },
        ..PortInfo::stub(&key.to_string())
    };
    let mut ports = BTreeMap::new();
    ports.insert(key.clone(), info);
    (ports, key)
}

fn session_for(ports: BTreeMap<PortKey, PortInfo>) -> Session {
    let tmp = tempfile::tempdir().unwrap();
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    Session::new(ports, HashMap::new(), &roots, &OptionsFiles::new(tmp.path()), false)
}

fn set(opts: &[&str]) -> BTreeSet<String> {
    opts.iter().map(ToString::to_string).collect()
}

#[test]
fn staged_diff_formats_changes() {
    assert_eq!(staged_diff(&set(&["A"]), &set(&["A", "B"])), "+B");
    // Additions first, each side in option order.
    assert_eq!(staged_diff(&set(&["A", "B"]), &set(&["C"])), "+C -A -B");
    assert_eq!(staged_diff(&set(&["A", "B"]), &set(&["B", "A"])), "");
    assert_eq!(staged_diff(&BTreeSet::new(), &BTreeSet::new()), "");
    assert_eq!(staged_diff(&set(&["A"]), &BTreeSet::new()), "-A");
}

#[test]
fn is_blocked_covers_broken_and_ignore() {
    let (ports, key) = mk_port(&["A"], &[], vec![], vec![]);
    let mut info = ports[&key].clone();
    assert!(!is_blocked(&info));
    info.ignore = Some("needs a GSSAPI implementation".into());
    assert!(is_blocked(&info));
    info.ignore = None;
    info.broken = Some("does not build".into());
    assert!(is_blocked(&info));
    // DEPRECATED alone is not a build blocker.
    info.broken = None;
    info.deprecated = Some("use something else".into());
    assert!(!is_blocked(&info));
}

#[test]
fn single_group_switches_selection() {
    let g = OptionGroup {
        kind: GroupKind::Single,
        name: "IMPL".into(),
        desc: String::new(),
        members: vec!["A".into(), "B".into()],
    };
    let (ports, key) = mk_port(&["A", "B"], &["A"], vec![g], vec![]);
    let mut s = session_for(ports);
    // Turning off the only selected member is refused.
    assert!(s.toggle(&key, "A").is_err());
    // Selecting B deselects A.
    s.toggle(&key, "B").unwrap();
    let info = s.ports.get(&key).unwrap();
    let st = s.state(info).unwrap();
    assert!(st.staged.contains("B") && !st.staged.contains("A"));
    assert!(s.violations(info).is_empty());
}

#[test]
fn multi_group_keeps_one() {
    let g = OptionGroup {
        kind: GroupKind::Multi,
        name: "M".into(),
        desc: String::new(),
        members: vec!["A".into(), "B".into()],
    };
    let (ports, key) = mk_port(&["A", "B"], &["A"], vec![g], vec![]);
    let mut s = session_for(ports);
    assert!(s.toggle(&key, "A").is_err()); // last enabled member
    s.toggle(&key, "B").unwrap();
    s.toggle(&key, "A").unwrap(); // now B carries the group
    let info = s.ports.get(&key).unwrap();
    assert!(s.violations(info).is_empty());
}

/// Every group rule has a staged state that breaks it, and one that does
/// not: SINGLE wants exactly one member, MULTI at least one, RADIO at
/// most one.
#[test]
fn group_rules_flag_each_violation() {
    let cases: [(GroupKind, &[&str], bool); 9] = [
        (GroupKind::Single, &[], true),
        (GroupKind::Single, &["A"], false),
        (GroupKind::Single, &["A", "B"], true),
        (GroupKind::Multi, &[], true),
        (GroupKind::Multi, &["A"], false),
        (GroupKind::Multi, &["A", "B"], false),
        (GroupKind::Radio, &[], false),
        (GroupKind::Radio, &["A"], false),
        (GroupKind::Radio, &["A", "B"], true),
    ];
    for (kind, on, broken) in cases {
        let g = OptionGroup {
            kind,
            name: "G".into(),
            desc: String::new(),
            members: vec!["A".into(), "B".into()],
        };
        let (ports, key) = mk_port(&["A", "B"], &[], vec![g], vec![]);
        let mut s = session_for(ports);
        let name = s.ports[&key].options_name.clone();
        s.states.get_mut(&name).unwrap().staged = on.iter().map(ToString::to_string).collect();
        let v = s.violations(&s.ports[&key]);
        assert_eq!(!v.is_empty(), broken, "{kind:?} with {on:?} on: {v:?}");
    }
}

/// bsd.port.mk's pre-check-config checks a SINGLE or MULTI group named
/// after one of the port's options only while that option is on; off,
/// it drops the members from PORT_OPTIONS and checks nothing, PREVENTS
/// included. RADIO is checked either way. mail/faces (OPTIONS_SINGLE=
/// AUDIO, AUDIO off) used to be a conflict that make never reports.
#[test]
fn a_conditional_group_is_checked_only_while_its_option_is_on() {
    let group = |kind, name: &str| OptionGroup {
        kind,
        name: name.into(),
        desc: String::new(),
        members: vec![format!("{name}1"), format!("{name}2")],
    };
    let defs = vec![(
        "M1",
        OptionDef { prevents: vec!["M2".into()], ..Default::default() },
    )];
    let (mut ports, key) = mk_port(
        &["AUDIO", "M", "R", "AUDIO1", "AUDIO2", "M1", "M2", "R1", "R2"],
        &[],
        vec![group(GroupKind::Single, "AUDIO"), group(GroupKind::Multi, "M"), group(GroupKind::Radio, "R")],
        defs,
    );
    ports.get_mut(&key).unwrap().options.all = set(&["AUDIO", "M", "R"]);
    let mut s = session_for(ports);
    assert!(s.violations(&s.ports[&key]).is_empty(), "{:?}", s.violations(&s.ports[&key]));
    let name = s.ports[&key].options_name.clone();
    s.states.get_mut(&name).unwrap().staged = set(&["M1", "M2", "R1", "R2"]);
    let v = s.violations(&s.ports[&key]);
    assert_eq!(v.len(), 1, "only the RADIO group, unconditional: {v:?}");
    assert!(v[0].contains("group R "), "{v:?}");
    // With its option on, the group is checked.
    s.states.get_mut(&name).unwrap().staged = set(&["AUDIO", "M", "M1"]);
    let v = s.violations(&s.ports[&key]);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(v[0].contains("group AUDIO "), "{v:?}");
    // Off, its last member is let go; on, it is held.
    s.states.get_mut(&name).unwrap().staged = set(&["AUDIO1", "M", "M1"]);
    s.toggle(&key, "AUDIO1").unwrap();
    s.toggle(&key, "AUDIO").unwrap();
    s.toggle(&key, "AUDIO2").unwrap();
    assert!(s.toggle(&key, "AUDIO2").is_err(), "the last member of a checked group stays");
    assert!(s.toggle(&key, "M1").is_err(), "the last member of a checked group stays");
}

/// A SINGLE group with two members on (a hand-edited file, say) can be
/// fixed by turning one off: only the last member is held.
#[test]
fn an_overfull_single_group_lets_a_member_go() {
    let g = OptionGroup {
        kind: GroupKind::Single,
        name: "G".into(),
        desc: String::new(),
        members: vec!["A".into(), "B".into()],
    };
    let (ports, key) = mk_port(&["A", "B"], &[], vec![g], vec![]);
    let mut s = session_for(ports);
    let name = s.ports[&key].options_name.clone();
    s.states.get_mut(&name).unwrap().staged = ["A".to_string(), "B".to_string()].into();
    s.toggle(&key, "B").unwrap();
    assert!(s.violations(&s.ports[&key]).is_empty());
    assert!(s.toggle(&key, "A").is_err(), "the last one stays");
}

/// An option outside a group is toggled freely beside it: turning it
/// off is no group's last member, turning it on deselects nobody.
#[test]
fn options_outside_a_group_ignore_its_rules() {
    for kind in [GroupKind::Single, GroupKind::Multi, GroupKind::Radio] {
        let g = OptionGroup {
            kind,
            name: "G".into(),
            desc: String::new(),
            members: vec!["A".into(), "B".into()],
        };
        let (ports, key) = mk_port(&["A", "B", "X"], &["A", "X"], vec![g], vec![]);
        let mut s = session_for(ports);
        s.toggle(&key, "X").unwrap_or_else(|e| panic!("{kind:?}: X off refused: {e}"));
        s.toggle(&key, "X").unwrap();
        let st = s.state(&s.ports[&key]).unwrap();
        assert!(st.staged.contains("A") && st.staged.contains("X"), "{kind:?}: {st:?}");
    }
}

#[test]
fn implies_are_closed_and_locked() {
    let defs = vec![(
        "KRB",
        OptionDef { implies: vec!["SSL".into()], ..Default::default() },
    )];
    let (ports, key) = mk_port(&["KRB", "SSL"], &[], vec![], defs);
    let mut s = session_for(ports);
    s.toggle(&key, "KRB").unwrap();
    let info = s.ports.get(&key).unwrap().clone();
    let st = s.state(&info).unwrap();
    assert!(st.staged.contains("SSL"), "implied option auto-enabled");
    assert_eq!(s.implied_by(&info, "SSL").as_deref(), Some("KRB"));
    assert!(s.toggle(&key, "SSL").is_err(), "implied option locked on");
    s.toggle(&key, "KRB").unwrap(); // off
    s.toggle(&key, "SSL").unwrap(); // now free
}

#[test]
fn prevents_reported_as_conflict() {
    let defs = vec![(
        "X",
        OptionDef {
            prevents: vec!["Y".into()],
            prevents_msg: Some("pick one".into()),
            ..Default::default()
        },
    )];
    let (ports, key) = mk_port(&["X", "Y"], &["Y"], vec![], defs);
    let mut s = session_for(ports);
    s.toggle(&key, "X").unwrap();
    let info = s.ports.get(&key).unwrap();
    assert_eq!(s.status(info), UiStatus::Conflict);
    assert!(s.violations(info)[0].contains("pick one"));
}

/// bsd.options.mk applies each scope's SET and UNSET in turn (312, 320,
/// 326, 334, then the FORCE quartet at 370-392), so the per-port knob
/// overrides the global one. Applying every SET and only then every UNSET
/// used to let a global UNSET erase a per-port SET: the exact shape of
/// `OPTIONS_UNSET= DOCS` plus `<port>_SET= DOCS`, which then showed up as
/// a spurious ≠mc deviation on a port that agreed with make.conf.
/// A member merely *mentioned* by an earlier make.conf layer does not
/// decide its SINGLE/RADIO group: a later layer can drop it again, and
/// then the member actually in force is the port's default, which nobody
/// decided. Counting the group as covered kept `scan` at exit 0 for a
/// port that still owed an answer.
/// check-config judges PORT_OPTIONS, which *_FORCE has rewritten after
/// the options file: a forced-off option conflicts with nothing, and a
/// forced-on one conflicts though the staged set leaves it off.
#[test]
fn conflicts_are_judged_on_what_force_leaves() {
    let defs = vec![(
        "A",
        OptionDef { prevents: vec!["B".into()], ..Default::default() },
    )];
    let (mut ports, key) = mk_port(&["A", "B"], &["A", "B"], vec![], defs);
    ports.get_mut(&key).unwrap().options.force_unset.insert("A".into());
    let s = session_for(ports);
    assert_eq!(s.state(&s.ports[&key]).unwrap().staged, set(&["A", "B"]));
    assert!(s.violations(&s.ports[&key]).is_empty(), "{:?}", s.violations(&s.ports[&key]));

    let defs = vec![(
        "A",
        OptionDef { prevents: vec!["B".into()], ..Default::default() },
    )];
    let (mut ports, key) = mk_port(&["A", "B"], &["A"], vec![], defs);
    ports.get_mut(&key).unwrap().options.force_set.insert("B".into());
    let mut s = session_for(ports);
    let name = s.ports[&key].options_name.clone();
    s.states.get_mut(&name).unwrap().staged = set(&["A"]);
    let c = s.conflicts(&s.ports[&key]);
    assert_eq!(c.len(), 1, "{c:?}");
    assert_eq!(c[0].members, ["A", "B"]);
    assert_eq!(c[0].cause.as_deref(), Some("B forced on by make.conf; fix it there"));
}

/// bsd.port.mk pre-check-config checks the PREVENTS of every option in
/// PORT_OPTIONS, an OPTIONS_SLAVE one too: X, on by OPTIONS_SLAVE and
/// out of the list, conflicts with the default Y. Its IMPLIES, which
/// bsd.options.mk:414 reads only for the list's options and runs before
/// OPTIONS_SLAVE goes in, turns no Z on.
#[test]
fn an_options_slave_option_prevents_and_implies_nothing() {
    let defs = vec![(
        "X",
        OptionDef { prevents: vec!["Y".into()], implies: vec!["Z".into()], ..Default::default() },
    )];
    let (mut ports, key) = mk_port(&["Y", "Z"], &["Y"], vec![], defs);
    ports.get_mut(&key).unwrap().options.slave.insert("X".into());
    let s = session_for(ports);
    let info = &s.ports[&key];
    assert_eq!(s.violations(info), ["X conflicts with Y: mutually exclusive"]);
    assert_eq!(nofile_effective(info), set(&["X", "Y"]));
}

#[test]
fn a_group_is_decided_only_when_makeconf_really_selects_a_member() {
    let group = || OptionGroup {
        kind: GroupKind::Single,
        name: "G".into(),
        desc: String::new(),
        members: vec!["A".into(), "B".into()],
    };
    // Global picks A, the per-port knob drops it: B is on only because it
    // is the port default, so both members still need a decision.
    let (mut ports, key) = mk_port(&["A", "B"], &["B"], vec![group()], vec![]);
    {
        let o = &mut ports.get_mut(&key).unwrap().options;
        o.mc_set.insert("A".into());
        o.port_unset.insert("A".into());
    }
    // A itself is decided — make.conf resolves it to off — but it does
    // not carry B with it, so B is what the port still owes an answer for.
    assert_eq!(
        undecided_options(&ports[&key], None, false),
        vec!["B".to_string()],
        "an overridden member decides nothing for the rest of its group"
    );

    // Un-overridden, the same global knob does decide the whole group.
    let (mut ports2, key2) = mk_port(&["A", "B"], &["B"], vec![group()], vec![]);
    ports2.get_mut(&key2).unwrap().options.mc_set.insert("A".into());
    assert!(
        undecided_options(&ports2[&key2], None, false).is_empty(),
        "a selected member decides its group"
    );
}

#[test]
fn per_port_makeconf_knob_beats_the_global_one() {
    let (mut ports, key) = mk_port(&["DOCS", "NLS", "X11"], &[], vec![], vec![]);
    {
        let o = &mut ports.get_mut(&key).unwrap().options;
        o.mc_unset.insert("DOCS".into());
        o.port_set.insert("DOCS".into());
        // The opposite order too: global on, per-port off.
        o.mc_set.insert("NLS".into());
        o.port_unset.insert("NLS".into());
        // FORCE has the same two scopes, applied after both.
        o.force_unset.insert("X11".into());
        o.port_force_set.insert("X11".into());
        // What make itself computes for those layers (PORT_OPTIONS), i.e.
        // the staged set of an unconfigured port.
        o.effective = ["DOCS", "X11"].iter().map(ToString::to_string).collect();
    }
    let info = &ports[&key];
    let eff = nofile_effective(info);
    assert!(eff.contains("DOCS"), "per-port _SET must survive the global UNSET");
    assert!(!eff.contains("NLS"), "per-port _UNSET must survive the global SET");
    assert!(eff.contains("X11"), "per-port _SET_FORCE must beat the global UNSET_FORCE");
    assert_eq!(info.options.forced_value("X11"), Some(true));

    // ...and the staged state agreeing with make.conf is not a deviation.
    let tmp = tempfile::tempdir().unwrap();
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    let s = Session::new(ports, HashMap::new(), &roots, &OptionsFiles::new(tmp.path()), false);
    assert!(s.mc_deviations(&s.ports[&key]).is_empty());
}

#[test]
fn minimal_treats_bare_defaults_as_decided() {
    // Options with no make.conf stance at all.
    let (ports, key) = mk_port(&["DOCS", "X11"], &["DOCS"], vec![], vec![]);
    let tmp = tempfile::tempdir().unwrap();
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    let s = Session::new(ports.clone(), HashMap::new(), &roots, &OptionsFiles::new(tmp.path()), true);
    assert!(s.covered_by_makeconf(&s.ports[&key]), "minimal: defaults decide");
    assert!(undecided_options(&s.ports[&key], None, true).is_empty());
    // Same port without minimal: X11 and DOCS are undecided.
    let s2 = Session::new(ports, HashMap::new(), &roots, &OptionsFiles::new(tmp.path()), false);
    assert!(!s2.covered_by_makeconf(&s2.ports[&key]));
}

/// New is what the file gives no SET or UNSET line, as make's
/// NEW_OPTIONS counts it; obsolete is whatever it mentions that the tree
/// lost. On a hand-edited file a list entry with no line (POP3) or
/// nothing at all (MQTT) is new, a line with no list entry (IMAP) is
/// settled; a gone option with a line only (OLD) is obsolete.
#[test]
fn stale_delta_follows_what_the_file_settles() {
    let (ports, key) = mk_port(&["A", "IMAP", "MQTT", "POP3"], &["A"], vec![], vec![]);
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    std::fs::write(
        tmp.path().join("cat_port/options"),
        "_OPTIONS_READ=port-0.9\n_FILE_COMPLETE_OPTIONS_LIST=A POP3\n\
         OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=IMAP\nOPTIONS_FILE_SET+=OLD\n",
    )
    .unwrap();
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    let s = Session::new(ports, HashMap::new(), &roots, &OptionsFiles::new(tmp.path()), false);
    let (added, removed) = s.stale_delta(&s.ports[&key]).unwrap();
    assert_eq!(added, ["MQTT", "POP3"]);
    assert_eq!(removed, ["OLD"]);
    assert_eq!(s.status(&s.ports[&key]), UiStatus::Stale);
    // scan --json's per-option `new` says the same.
    let new: Vec<String> = crate::report::option_reports(&s, &s.ports[&key])
        .into_iter()
        .filter(|o| o.new)
        .map(|o| o.name)
        .collect();
    assert_eq!(new, ["MQTT", "POP3"]);
}

/// bsd.options.mk:321: make.conf's `OPTIONS_UNSET=A*` takes A and A2
/// out of PORT_OPTIONS and NEW_OPTIONS alike: both are off and decided,
/// B and C stay undecided. `OPTIONS_SET=C*` adds the word `C*`, no C,
/// and decides nothing: make takes C out of NEW_OPTIONS, a defect
/// optique does not follow.
#[test]
fn make_conf_entries_are_patterns() {
    let (mut ports, key) = mk_port(&["A", "A2", "B", "C"], &["A", "A2", "B"], vec![], vec![]);
    ports.get_mut(&key).unwrap().options.mc_unset.insert("A*".into());
    let info = &ports[&key];
    assert_eq!(nofile_effective(info), set(&["B"]));
    assert_eq!(undecided_options(info, None, false), ["B", "C"]);
    ports.get_mut(&key).unwrap().options.mc_set.insert("C*".into());
    let info = &ports[&key];
    assert_eq!(nofile_effective(info), set(&["B", "C*"]));
    assert_eq!(undecided_options(info, None, false), ["B", "C"]);
}

/// bsd.options.mk:399-409: make.conf's WITH and WITHOUT apply after the
/// FORCE knobs and before IMPLIES, and decide what they name: WITH=C
/// beats OPTIONS_UNSET_FORCE=C and its IMPLIES turns D on, WITHOUT=A*
/// takes A and A2 off; only B stays undecided.
#[test]
fn with_and_without_come_after_force() {
    let defs = vec![("C", OptionDef { implies: vec!["D".into()], ..Default::default() })];
    let (mut ports, key) = mk_port(&["A", "A2", "B", "C", "D"], &["A", "A2"], vec![], defs);
    {
        let o = &mut ports.get_mut(&key).unwrap().options;
        o.force_unset.insert("C".into());
        o.with.insert("C".into());
        o.without.insert("A*".into());
    }
    let info = &ports[&key];
    assert_eq!(nofile_effective(info), set(&["C", "D"]));
    assert_eq!(undecided_options(info, None, false), ["B"]);
    // The options file cannot change what they pin.
    let mut s = session_for(ports);
    assert!(s.toggle(&key, "A2").unwrap_err().contains("WITH/WITHOUT"));
}

/// bsd.options.mk:300-304: OPTIONS_OVERRIDE, defined, replaces the whole
/// decision below it — OPTIONS_DEFAULT (here D, never read), the global
/// SET/UNSET pair and the options file are not read — but what follows
/// `.endif` (368-411: the *_FORCE knobs, then IMPLIES) still runs on top
/// of the override's own value, taken as is.
#[test]
fn effective_with_under_override_skips_defaults_mc_and_file_but_not_force_or_implies() {
    let defs = vec![("A", OptionDef { implies: vec!["D".into()], ..Default::default() })];
    let (mut ports, key) = mk_port(&["A", "B", "C", "D"], &["D"], vec![], defs);
    {
        let o = &mut ports.get_mut(&key).unwrap().options;
        o.r#override = Some(set(&["A", "B"]));
        // Unread once OPTIONS_OVERRIDE is defined: the global SET/UNSET
        // pair.
        o.mc_set.insert("C".into());
        o.mc_unset.insert("A".into());
        // Still applied on top of it: the *_FORCE knobs.
        o.force_set.insert("C".into());
        o.force_unset.insert("B".into());
    }
    let info = &ports[&key];
    // A: from the override, FORCE leaves it. B: the override turns it
    // on, FORCE_UNSET turns it back off. C: never in the override,
    // FORCE_SET adds it anyway. D: not in the override or forced, but
    // A's IMPLIES (after FORCE) turns it on regardless — OPTIONS_DEFAULT
    // never put it there.
    assert_eq!(nofile_effective(info), set(&["A", "C", "D"]));
    // A hand file disagreeing with all of this changes nothing: it is
    // never read either.
    let file = SavedOptionsFile {
        complete: info.options.complete.clone(),
        set: set(&["B"]),
        unset: set(&["A", "C", "D"]),
        ..Default::default()
    };
    assert_eq!(effective_with(info, Some(&file)), set(&["A", "C", "D"]));
}

/// An OPTIONS_OVERRIDE defined empty is still defined: PORT_OPTIONS
/// starts empty, not at the port's own defaults.
#[test]
fn an_empty_override_turns_everything_off() {
    let (mut ports, key) = mk_port(&["A", "B"], &["A", "B"], vec![], vec![]);
    ports.get_mut(&key).unwrap().options.r#override = Some(BTreeSet::new());
    assert_eq!(nofile_effective(&ports[&key]), BTreeSet::new());
}

/// OPTIONS_OVERRIDE decides every option of the port; none is left for
/// a human, whatever the (unread) options file says.
#[test]
fn undecided_is_always_empty_under_override() {
    let (mut ports, key) = mk_port(&["A", "B"], &["A"], vec![], vec![]);
    ports.get_mut(&key).unwrap().options.r#override = Some(set(&["A"]));
    let info = &ports[&key];
    assert!(undecided_options(info, None, false).is_empty());
    let saved = SavedOptionsFile::parse("OPTIONS_FILE_SET+=B\n");
    assert!(undecided_options(info, Some(&saved), false).is_empty());
}

/// The staged baseline follows the override's own outcome, not a
/// leftover file on disk that disagrees with it: the file is never read.
#[test]
fn baseline_follows_the_override_not_a_disagreeing_file() {
    let (mut ports, key) = mk_port(&["A", "B"], &["A"], vec![], vec![]);
    ports.get_mut(&key).unwrap().options.r#override = Some(set(&["B"]));
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    std::fs::write(
        tmp.path().join("cat_port/options"),
        "_FILE_COMPLETE_OPTIONS_LIST=A B\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n",
    )
    .unwrap();
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    let s = Session::new(ports, HashMap::new(), &roots, &OptionsFiles::new(tmp.path()), false);
    let info = &s.ports[&key];
    assert!(s.is_staged(info, "B"), "override says B, not the file's A");
    assert!(!s.is_staged(info, "A"));
}

/// A status never `unconfigured` or `stale` for an overridden port,
/// even with a leftover file that disagrees with the override.
#[test]
fn status_is_ok_for_an_override_port_even_with_a_disagreeing_file() {
    let (mut ports, key) = mk_port(&["A", "B"], &["A"], vec![], vec![]);
    ports.get_mut(&key).unwrap().options.r#override = Some(set(&["A"]));
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    std::fs::write(tmp.path().join("cat_port/options"), "_FILE_COMPLETE_OPTIONS_LIST=A\nOPTIONS_FILE_UNSET+=A\n")
        .unwrap();
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    let s = Session::new(ports, HashMap::new(), &roots, &OptionsFiles::new(tmp.path()), false);
    assert_eq!(s.status(&s.ports[&key]), UiStatus::Ok);
}

/// bsd.port.mk pre-check-config still checks PREVENTS after
/// OPTIONS_OVERRIDE: naming both halves of a conflicting pair in it
/// fails check-config, and optique reports Conflict, same as any other
/// port.
#[test]
fn an_override_that_breaks_prevents_is_a_conflict() {
    let defs = vec![("A", OptionDef { prevents: vec!["B".into()], ..Default::default() })];
    let (mut ports, key) = mk_port(&["A", "B"], &[], vec![], defs);
    ports.get_mut(&key).unwrap().options.r#override = Some(set(&["A", "B"]));
    let s = session_for(ports);
    assert_eq!(s.status(&s.ports[&key]), UiStatus::Conflict);
}

/// Editing an overridden port's options is pointless: the file is
/// never read, so `toggle` (and so `bulk_set` and `decide`) refuses
/// every option of it, naming OPTIONS_OVERRIDE as the reason.
#[test]
fn toggle_is_refused_under_override() {
    let (mut ports, key) = mk_port(&["A", "B"], &["A"], vec![], vec![]);
    ports.get_mut(&key).unwrap().options.r#override = Some(set(&["A"]));
    let mut s = session_for(ports);
    let err = s.toggle(&key, "A").unwrap_err();
    assert!(err.contains("OPTIONS_OVERRIDE"), "{err}");
    let err = s.toggle(&key, "B").unwrap_err();
    assert!(err.contains("OPTIONS_OVERRIDE"), "{err}");
}

/// bsd.options.mk:352-363: every OPTIONS_FILE_SET entry, options.local's
/// included, goes in before any OPTIONS_FILE_UNSET one. The file sets
/// A, options.local unsets A*: turning A on would write a file make
/// reads with A still off, so toggle refuses it, and A and A2 report
/// locked by options.local. Its SETs lock nothing: B, which the file
/// unsets, is off and can be turned on; D, which the file gives no
/// line, is on as a default; E* turns no E on.
#[test]
fn toggle_is_refused_for_what_options_local_unsets() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    std::fs::write(tmp.path().join("cat_port/options"), "OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B C\n").unwrap();
    std::fs::write(tmp.path().join("cat_port/options.local"), "OPTIONS_FILE_UNSET+=A*\nOPTIONS_FILE_SET+=B D E*\n")
        .unwrap();
    let (ports, key) = mk_port(&["A", "A2", "B", "C", "D", "E"], &[], vec![], vec![]);
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    let files = OptionsFiles::new(tmp.path());
    let mut s = Session::new(ports, HashMap::new(), &roots, &files, false);
    let info = s.ports[&key].clone();
    // make appends the word E* itself, which is no option.
    assert_eq!(effective_with(&info, Some(&files.combined("cat_port"))), set(&["D", "E*"]));
    assert_eq!(s.state(&info).unwrap().staged, set(&["D"]));
    for opt in ["A", "A2"] {
        let err = s.toggle(&key, opt).unwrap_err();
        assert_eq!(err, format!("options.local decides {opt}; edit it there"));
    }
    assert!(!s.edited(&info));
    let reports = crate::report::option_reports(&s, &info);
    let locked: Vec<_> = reports.iter().map(|o| (o.name.as_str(), o.locked_by.as_deref())).collect();
    let local = Some("options.local");
    assert_eq!(locked, [("A", local), ("A2", local), ("B", None), ("C", None), ("D", None), ("E", None)]);
    s.toggle(&key, "B").expect("options.local's SET does not lock B");
    s.toggle(&key, "D").expect("nor D");
    // Written, the file has make end up with what is staged.
    let staged = s.state(&info).unwrap().staged.clone();
    assert_eq!(staged, set(&["B"]));
    let planned = apply::plan_writes(std::iter::once((&key, &info, staged.clone())), &files, false);
    let written = SavedOptionsFile::parse(&planned.writes[0].content);
    let mut outcome = effective_with(&info, Some(&files.under_local("cat_port", written)));
    outcome.remove("E*");
    assert_eq!(outcome, staged);
}

/// bsd.options.mk:341-363: `OPTIONS_FILE_SET=B` in options.local drops
/// the file's SET lines, so make counts A new however the file is
/// written. The port is left alone: not stale, edits refused, and no
/// write planned for its file, which sync would otherwise rewrite
/// byte for byte every run.
#[test]
fn an_options_local_reset_leaves_the_file_alone() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    let file = "_OPTIONS_READ=port-1.0\n_FILE_COMPLETE_OPTIONS_LIST=A B\n\
                OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n";
    std::fs::write(tmp.path().join("cat_port/options"), file).unwrap();
    std::fs::write(tmp.path().join("cat_port/options.local"), "OPTIONS_FILE_SET=B\n").unwrap();
    let (ports, key) = mk_port(&["A", "B"], &[], vec![], vec![]);
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    let files = OptionsFiles::new(tmp.path());
    let mut s = Session::new(ports, HashMap::new(), &roots, &files, false);
    let info = s.ports[&key].clone();
    assert_eq!(s.status(&info), UiStatus::Ok);
    let why = "options.local resets OPTIONS_FILE_SET: the options file's lines are not read; edit options.local";
    assert_eq!(s.toggle(&key, "A").unwrap_err(), why);
    assert_eq!(s.toggle(&key, "B").unwrap_err(), why);
    for minimal in [false, true] {
        let staged = s.state(&info).unwrap().staged.clone();
        let planned = apply::plan_writes(std::iter::once((&key, &info, staged)), &files, minimal);
        assert!(planned.writes.is_empty(), "{:?}", planned.writes);
        assert!(planned.removals.is_empty(), "{:?}", planned.removals);
    }
}

/// scan --json --options reports every option of an overridden port
/// with scope "override", locked, and `locked_by` naming it — whatever
/// the unread SET/UNSET pairs or an IMPLIES chain would otherwise have
/// said.
#[test]
fn report_scope_is_override_for_every_option() {
    let (mut ports, key) = mk_port(&["A", "B"], &["A"], vec![], vec![]);
    {
        let o = &mut ports.get_mut(&key).unwrap().options;
        o.r#override = Some(set(&["A"]));
        // Unread under the override: not what the report says.
        o.mc_unset.insert("A".into());
        o.port_set.insert("B".into());
    }
    let s = session_for(ports);
    let reports = crate::report::option_reports(&s, &s.ports[&key]);
    assert_eq!(reports.len(), 2);
    for o in &reports {
        let want_on = o.name == "A";
        let m = o.makeconf.as_ref().unwrap_or_else(|| panic!("{}: no makeconf", o.name));
        assert_eq!((m.value, m.scope), (want_on, "override"), "{}", o.name);
        assert!(o.locked, "{}", o.name);
        assert_eq!(o.locked_by.as_deref(), Some("make.conf OPTIONS_OVERRIDE"), "{}", o.name);
    }
}

/// bsd.options.mk:300-304 replaces the defaults, the SET/UNSET pairs
/// and the file, but the *_FORCE knobs (367-395) and WITH/WITHOUT
/// (398-409) run after its `.endif` (365): an option they pin ends up
/// at the pin, and the report says so — scope "force" or "with", the
/// matching `locked_by` — while the others keep the override's verdict.
#[test]
fn report_lets_force_and_with_pin_over_the_override() {
    let (mut ports, key) = mk_port(&["A", "B", "C", "D"], &[], vec![], vec![]);
    {
        let o = &mut ports.get_mut(&key).unwrap().options;
        o.r#override = Some(set(&["A", "B"]));
        // B: the override turns it on, FORCE_UNSET turns it back off.
        o.force_unset.insert("B".into());
        // C: never in the override, WITH adds it anyway.
        o.with.insert("C".into());
    }
    let s = session_for(ports);
    let info = &s.ports[&key];
    let reports = crate::report::option_reports(&s, info);
    let find = |name: &str| reports.iter().find(|o| o.name == name).unwrap();
    for (name, value, scope, by) in [
        ("A", true, "override", "make.conf OPTIONS_OVERRIDE"),
        ("B", false, "force", "make.conf *_FORCE"),
        ("C", true, "with", "make.conf WITH/WITHOUT"),
        ("D", false, "override", "make.conf OPTIONS_OVERRIDE"),
    ] {
        let o = find(name);
        let m = o.makeconf.as_ref().unwrap_or_else(|| panic!("{name}: no makeconf"));
        assert_eq!((m.value, m.scope), (value, scope), "{name}");
        assert!(o.locked, "{name}");
        assert_eq!(o.locked_by.as_deref(), Some(by), "{name}");
        // The verdict is what make ends up with.
        assert_eq!(o.staged, value, "{name}");
        assert_eq!(nofile_effective(info).contains(name), value, "{name}");
    }
}

#[test]
fn mc_coverage_of_stale_ports() {
    // Saved file knows A; tree update added DOCS and NLS.
    let (ports, key) = mk_port(&["A", "DOCS", "NLS"], &["A"], vec![], vec![]);
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    std::fs::write(
        tmp.path().join("cat_port/options"),
        "_OPTIONS_READ=port-0.9\n_FILE_COMPLETE_OPTIONS_LIST=A\nOPTIONS_FILE_SET+=A\n",
    )
    .unwrap();
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    let mut s = Session::new(ports, HashMap::new(), &roots, &OptionsFiles::new(tmp.path()), false);
    let info_key = key.clone();

    // Not covered: DOCS/NLS undecided by make.conf.
    assert!(!s.covered_by_makeconf(&s.ports[&info_key]));
    assert_eq!(s.status(&s.ports[&info_key]), UiStatus::Stale);

    // Covered once make.conf unsets both globally.
    {
        let o = &mut s.ports.get_mut(&info_key).unwrap().options;
        o.mc_unset.insert("DOCS".into());
        o.mc_unset.insert("NLS".into());
    }
    assert!(s.covered_by_makeconf(&s.ports[&info_key]));

    // No stance -> no deviation, even when differing from the port default
    // (that's the plain yellow case, not ≠mc).
    assert!(!s.mc_deviates(&s.ports[&info_key], "A"));
    // Per-port knobs count as make.conf stance too.
    {
        let o = &mut s.ports.get_mut(&info_key).unwrap().options;
        o.port_unset.insert("NLS".into());
    }
    // NLS staged? baseline had no file... staged came from effective which
    // is defaults ({A}) here, so NLS is off == nofile outcome -> no deviation.
    assert!(!s.mc_deviates(&s.ports[&info_key], "NLS"));
    // Deviation: staged keeps A on although make.conf globally unsets it.
    {
        let o = &mut s.ports.get_mut(&info_key).unwrap().options;
        o.mc_unset.insert("A".into());
    }
    assert_eq!(s.mc_deviations(&s.ports[&info_key]), vec!["A".to_string()]);
    // Forced options never count as deviations.
    {
        let o = &mut s.ports.get_mut(&info_key).unwrap().options;
        o.force_set.insert("A".into());
    }
    assert!(s.mc_deviations(&s.ports[&info_key]).is_empty());

    // A removed-options-only staleness is always covered.
    let (ports2, key2) = mk_port(&["A"], &["A"], vec![], vec![]);
    let tmp2 = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp2.path().join("cat_port")).unwrap();
    std::fs::write(
        tmp2.path().join("cat_port/options"),
        "_OPTIONS_READ=port-0.9\n_FILE_COMPLETE_OPTIONS_LIST=A GONE\n\
         OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=GONE\n",
    )
    .unwrap();
    let roots2: Vec<PortKey> = ports2.keys().cloned().collect();
    let s2 = Session::new(ports2, HashMap::new(), &roots2, &OptionsFiles::new(tmp2.path()), false);
    assert_eq!(s2.status(&s2.ports[&key2]), UiStatus::Stale);
    assert!(s2.covered_by_makeconf(&s2.ports[&key2]));

    // Unconfigured port (no saved file): covered only when make.conf
    // decides every single option.
    let (ports3, key3) = mk_port(&["DOCS", "X11"], &["DOCS"], vec![], vec![]);
    let tmp3 = tempfile::tempdir().unwrap();
    let roots3: Vec<PortKey> = ports3.keys().cloned().collect();
    let mut s3 = Session::new(ports3, HashMap::new(), &roots3, &OptionsFiles::new(tmp3.path()), false);
    assert_eq!(s3.status(&s3.ports[&key3]), UiStatus::Unconfigured);
    assert!(!s3.covered_by_makeconf(&s3.ports[&key3]));
    {
        let o = &mut s3.ports.get_mut(&key3).unwrap().options;
        o.mc_unset.insert("DOCS".into());
        o.port_set.insert("X11".into());
    }
    assert!(s3.covered_by_makeconf(&s3.ports[&key3]));

    // A SINGLE group is decided as a whole when make.conf selects one
    // member (e.g. OPTIONS_SET += GSSAPI_NONE).
    let g = OptionGroup {
        kind: GroupKind::Single,
        name: "GSSAPI".into(),
        desc: String::new(),
        members: vec![
            "GSSAPI_BASE".into(),
            "GSSAPI_MIT".into(),
            "GSSAPI_NONE".into(),
        ],
    };
    let (ports4, key4) =
        mk_port(&["GSSAPI_BASE", "GSSAPI_MIT", "GSSAPI_NONE"], &["GSSAPI_BASE"], vec![g], vec![]);
    let tmp4 = tempfile::tempdir().unwrap();
    let roots4: Vec<PortKey> = ports4.keys().cloned().collect();
    let mut s4 = Session::new(ports4, HashMap::new(), &roots4, &OptionsFiles::new(tmp4.path()), false);
    assert!(!s4.covered_by_makeconf(&s4.ports[&key4]));
    {
        let o = &mut s4.ports.get_mut(&key4).unwrap().options;
        o.mc_set.insert("GSSAPI_NONE".into());
    }
    assert!(s4.covered_by_makeconf(&s4.ports[&key4]));
}

fn linked_info(origin: &str, deps: &[&str], complete: &[&str]) -> PortInfo {
    PortInfo {
        options: PortOptions {
            complete: complete.iter().map(ToString::to_string).collect(),
            ..Default::default()
        },
        deps: deps
            .iter()
            .map(|d| crate::model::port::DepEdge {
                target: PortKey::parse(d).unwrap(),
                spec: format!("dep:{d}"),
                test_only: false,
            })
            .collect(),
        ..PortInfo::stub(origin)
    }
}

/// Without the default flavor, the flavor with the most options owns the
/// shared file, so an edit to an option only it has reaches the staged
/// set; and none of the file's options reads as obsolete, a non-default
/// view being unable to tell them from options it merely lacks.
#[test]
fn without_the_default_flavor_the_widest_view_owns_the_file() {
    let mk = |flavor: &str, complete: &[&str]| {
        let key = PortKey::parse(&format!("editors/emacs@{flavor}")).unwrap();
        let (mut p, k) = mk_port(complete, &[], vec![], vec![]);
        let mut info = p.remove(&k).unwrap();
        info.key = key.clone();
        info.canonical = key;
        info.flavors = vec!["full".into(), "nox".into(), "pgtk".into()];
        info.options_name = "editors_emacs".into();
        info
    };
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("editors_emacs")).unwrap();
    std::fs::write(
        tmp.path().join("editors_emacs/options"),
        "_FILE_COMPLETE_OPTIONS_LIST=A X11 Y\nOPTIONS_FILE_SET+=A\n\
         OPTIONS_FILE_UNSET+=X11\nOPTIONS_FILE_SET+=Y\n",
    )
    .unwrap();
    let ports: BTreeMap<_, _> =
        [mk("nox", &["A"]), mk("pgtk", &["A", "Y"])].into_iter().map(|i| (i.key.clone(), i)).collect();
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    let s = Session::new(ports, HashMap::new(), &roots, &OptionsFiles::new(tmp.path()), false);
    assert_eq!(s.owners["editors_emacs"].to_string(), "editors/emacs@pgtk");
    let nox = &s.ports[&PortKey::parse("editors/emacs@nox").unwrap()];
    assert!(s.state(nox).unwrap().staged.contains("Y"), "the shared set holds @pgtk's Y");
    assert_eq!(s.stale_delta(nox), Some((vec![], vec![])), "X11 is not obsolete for them");
    assert_eq!(s.status(nox), UiStatus::Ok);
}

/// The owner is the first flavor in FLAVORS, whatever order the keys
/// sort in: here the default `@nox` sorts after `@full`, so the sorted
/// ports map cannot pick it by accident.
#[test]
fn owner_is_the_default_flavor_not_the_first_key() {
    let mk = |flavor: &str| {
        let key = PortKey::parse(&format!("editors/emacs@{flavor}")).unwrap();
        let (mut p, k) = mk_port(&["A"], &[], vec![], vec![]);
        let mut info = p.remove(&k).unwrap();
        info.key = key.clone();
        info.canonical = key;
        info.flavors = vec!["nox".into(), "full".into()];
        info.options_name = "editors_emacs".into();
        info
    };
    let mut ports = BTreeMap::new();
    for info in [mk("full"), mk("nox")] {
        ports.insert(info.key.clone(), info);
    }
    assert_eq!(ports.keys().next().unwrap().to_string(), "editors/emacs@full");
    let s = session_for(ports.clone());
    assert_eq!(s.owners["editors_emacs"].to_string(), "editors/emacs@nox");

    // No default flavor in the closure: the first key keeps the file,
    // a later non-default flavor does not take it over.
    ports.retain(|k, _| k.to_string() != "editors/emacs@nox");
    let mut lite = mk("full");
    lite.key = PortKey::parse("editors/emacs@lite").unwrap();
    lite.canonical = lite.key.clone();
    ports.insert(lite.key.clone(), lite);
    let s = session_for(ports);
    assert_eq!(s.owners["editors_emacs"].to_string(), "editors/emacs@full");
}

#[test]
fn shared_file_judged_against_owner_flavor() {
    // @full (default) owns editors_emacs with options A + X11;
    // @nox excludes X11. The file records the owner's view.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("editors_emacs")).unwrap();
    std::fs::write(
        tmp.path().join("editors_emacs/options"),
        "_OPTIONS_READ=emacs-30\n_FILE_COMPLETE_OPTIONS_LIST=A X11\n\
         OPTIONS_FILE_SET+=A\nOPTIONS_FILE_SET+=X11\n",
    )
    .unwrap();

    let mk = |flavor: &str, complete: &[&str]| {
        let key = PortKey::parse(&format!("editors/emacs@{flavor}")).unwrap();
        PortInfo {
            pkgname: format!("emacs-{flavor}-30"),
            flavors: vec!["full".into(), "nox".into()],
            options_name: "editors_emacs".into(),
            options: PortOptions {
                complete: complete.iter().map(ToString::to_string).collect(),
                effective: complete.iter().map(ToString::to_string).collect(),
                ..Default::default()
            },
            ..PortInfo::stub(&key.to_string())
        }
    };
    let full = mk("full", &["A", "X11"]);
    let nox = mk("nox", &["A"]);
    let full_key = full.key.clone();
    let nox_key = nox.key.clone();
    let mut ports = BTreeMap::new();
    // Insert the non-default flavor first: owner selection must still
    // pick @full.
    ports.insert(nox_key.clone(), nox);
    ports.insert(full_key.clone(), full);
    let roots = vec![full_key.clone(), nox_key.clone()];
    let files = OptionsFiles::new(tmp.path());
    let s = Session::new(ports, HashMap::new(), &roots, &files, false);
    assert_eq!(files.disk_reads(), 1, "two flavors, one shared file, one read");

    assert_eq!(s.owners["editors_emacs"], full_key);
    assert_eq!(s.status(&s.ports[&full_key]), UiStatus::Ok);
    assert_eq!(
        s.status(&s.ports[&nox_key]),
        UiStatus::Ok,
        "nox flavor must not read the owner-written file as stale"
    );
}

#[test]
fn siblings_lists_closure_flavors() {
    // devel/git declares three flavors but only two are in the closure.
    let mk = |origin: &str, flavors: &[&str]| {
        PortInfo {
            flavors: flavors.iter().map(ToString::to_string).collect(),
            options: PortOptions {
                complete: vec!["A".into()],
                ..Default::default()
            },
            ..PortInfo::stub(origin)
        }
    };
    let flavors = ["default", "lite", "tiny"];
    let mut ports = BTreeMap::new();
    for info in [
        mk("devel/git@default", &flavors),
        mk("devel/git@lite", &flavors),
        mk("www/nginx", &[]),
    ] {
        ports.insert(info.canonical.clone(), info);
    }
    let s = session_for(ports);

    let default_key = PortKey::parse("devel/git@default").unwrap();
    let lite_key = PortKey::parse("devel/git@lite").unwrap();
    let nginx_key = PortKey::parse("www/nginx").unwrap();
    assert_eq!(s.siblings(&default_key), vec![default_key.clone(), lite_key.clone()]);
    assert_eq!(s.siblings(&lite_key), vec![default_key, lite_key]);
    assert_eq!(s.siblings(&nginx_key), vec![nginx_key]);
}

#[test]
fn merge_gc_drops_unreachable_and_keeps_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let root = linked_info("cat/root", &["cat/dep"], &["ROOTOPT"]);
    let dep = linked_info("cat/dep", &[], &["DEPOPT"]);
    let root_key = root.key.clone();
    let dep_key = dep.key.clone();
    let mut ports = BTreeMap::new();
    ports.insert(root_key.clone(), root);
    ports.insert(dep_key.clone(), dep.clone());
    let mut s = Session::new(ports, HashMap::new(), std::slice::from_ref(&root_key), &OptionsFiles::new(tmp.path()), false);

    // User edits the dependency's options.
    s.toggle(&dep_key, "DEPOPT").unwrap();
    assert!(s.edited(&s.ports[&dep_key]));

    // Refresh: the root no longer depends on cat/dep -> GC'd.
    let updated_root = linked_info("cat/root", &[], &["ROOTOPT"]);
    let mut result = crate::query::scanner::ScanResult::default();
    result.ports.insert(root_key.clone(), updated_root);
    let (added, removed) = s.merge(result, &OptionsFiles::new(tmp.path()));
    assert_eq!((added.len(), removed), (0, vec![dep_key.clone()]));
    assert!(!s.ports.contains_key(&dep_key));

    // Flip back: dep reappears and the staged edit survived.
    let mut result = crate::query::scanner::ScanResult::default();
    result.ports.insert(root_key.clone(), linked_info("cat/root", &["cat/dep"], &["ROOTOPT"]));
    result.ports.insert(dep_key.clone(), dep);
    let (added, removed) = s.merge(result, &OptionsFiles::new(tmp.path()));
    assert_eq!((added, removed.len()), (vec![dep_key.clone()], 0));
    let st = s.state(&s.ports[&dep_key]).unwrap();
    assert!(st.staged.contains("DEPOPT"), "edit preserved across GC round-trip");
    assert!(s.edited(&s.ports[&dep_key]));
}

/// The GC walks the closure as the scan does: not through a blacklisted
/// port. A port reached both through it and another way goes when the
/// other way does, as a fresh scan would drop it.
#[test]
fn merge_gc_does_not_walk_through_a_blacklisted_port() {
    let tmp = tempfile::tempdir().unwrap();
    let pd = tmp.path().join("poudriere.d");
    std::fs::create_dir_all(&pd).unwrap();
    std::fs::write(pd.join("blacklist"), "cat/bl\n").unwrap();
    let ports: BTreeMap<_, _> = [
        linked_info("cat/root", &["cat/x", "cat/bl"], &["R"]),
        linked_info("cat/x", &["cat/d"], &["X"]),
        linked_info("cat/bl", &["cat/d"], &["B"]),
        linked_info("cat/d", &[], &["D"]),
    ]
    .into_iter()
    .map(|i| (i.key.clone(), i))
    .collect();
    let root = PortKey::parse("cat/root").unwrap();
    let files = OptionsFiles::new(tmp.path());
    let mut s = Session::new(ports, HashMap::new(), std::slice::from_ref(&root), &files, false);
    s.blacklist = crate::config::Blacklist::load(&pd, None, "default", None);

    // Refresh: cat/x no longer depends on cat/d; only cat/bl still does.
    let mut result = crate::query::scanner::ScanResult::default();
    result.ports.insert(PortKey::parse("cat/x").unwrap(), linked_info("cat/x", &[], &["X"]));
    let (_, removed) = s.merge(result, &files);
    assert_eq!(removed, vec![PortKey::parse("cat/d").unwrap()]);
    assert!(s.ports.contains_key(&PortKey::parse("cat/bl").unwrap()), "the blacklisted port stays listed");
}

/// When the GC drops the flavor that owned a shared options file while a
/// sibling stays, the sibling takes the file over: an owner naming a
/// removed port leaves every owner lookup for that file empty-handed.
#[test]
fn merge_gc_reassigns_the_owner_of_a_shared_file() {
    let tmp = tempfile::tempdir().unwrap();
    let flavor = |f: &str| {
        let mut info = linked_info(&format!("cat/x@{f}"), &[], &["A"]);
        info.flavors = vec!["a".into(), "b".into()];
        info.options_name = "cat_x".into();
        info
    };
    let root_key = PortKey::parse("cat/root").unwrap();
    let mut ports = BTreeMap::new();
    ports.insert(root_key.clone(), linked_info("cat/root", &["cat/x@a", "cat/x@b"], &["R"]));
    for f in ["a", "b"] {
        let info = flavor(f);
        ports.insert(info.key.clone(), info);
    }
    let files = OptionsFiles::new(tmp.path());
    let mut s = Session::new(ports, HashMap::new(), std::slice::from_ref(&root_key), &files, false);
    assert_eq!(s.owners["cat_x"].to_string(), "cat/x@a");

    // Refresh: the root keeps only the non-default flavor.
    let mut result = crate::query::scanner::ScanResult::default();
    result.ports.insert(root_key.clone(), linked_info("cat/root", &["cat/x@b"], &["R"]));
    let (added, removed) = s.merge(result, &files);
    assert_eq!((added.len(), removed.len()), (0, 1));
    assert_eq!(s.owners["cat_x"].to_string(), "cat/x@b", "the owner is a port still held");
}

#[test]
fn why_chain_and_dependents() {
    let tmp = tempfile::tempdir().unwrap();
    // root -> mid -> leaf and other -> leaf; orphan hangs off nothing.
    let infos = [
        linked_info("cat/root", &["cat/mid"], &["ROOTOPT"]),
        linked_info("cat/mid", &["cat/leaf"], &["MIDOPT"]),
        linked_info("cat/other", &["cat/leaf"], &["OTHEROPT"]),
        linked_info("cat/leaf", &[], &["LEAFOPT"]),
        linked_info("cat/orphan", &[], &["ORPHANOPT"]),
    ];
    let mut ports = BTreeMap::new();
    for info in infos {
        ports.insert(info.key.clone(), info);
    }
    let root = PortKey::parse("cat/root").unwrap();
    let other = PortKey::parse("cat/other").unwrap();
    let mid = PortKey::parse("cat/mid").unwrap();
    let leaf = PortKey::parse("cat/leaf").unwrap();
    let orphan = PortKey::parse("cat/orphan").unwrap();
    let s = Session::new(ports, HashMap::new(), &[root.clone(), other.clone()], &OptionsFiles::new(tmp.path()), false);

    let chain = s.why_chain(&leaf).expect("leaf is reachable");
    assert_eq!(chain.len(), 2, "shortest chain goes through cat/other");
    assert!(s.roots.contains(&chain[0]), "chain starts at a root");
    assert_eq!(chain.last(), Some(&leaf), "chain ends at the target");
    assert_eq!(chain, vec![other.clone(), leaf.clone()]);

    assert_eq!(s.why_chain(&root), Some(vec![root]));
    assert_eq!(s.why_chain(&orphan), None, "not reachable from the roots");
    assert_eq!(s.dependents(&leaf), vec![mid, other]);
    assert!(s.dependents(&orphan).is_empty());
}

/// A cycle in the dep graph must neither hang the chain walk nor lengthen
/// the chain: the predecessor of an already-visited port stays put.
#[test]
fn why_chain_survives_dependency_cycle() {
    let tmp = tempfile::tempdir().unwrap();
    let infos = [
        linked_info("cat/root", &["cat/mid"], &["ROOTOPT"]),
        linked_info("cat/mid", &["cat/leaf"], &["MIDOPT"]),
        linked_info("cat/leaf", &["cat/mid", "cat/deep"], &["LEAFOPT"]),
        linked_info("cat/other", &["cat/leaf"], &["OTHEROPT"]),
        linked_info("cat/deep", &[], &["DEEPOPT"]),
    ];
    let mut ports = BTreeMap::new();
    for info in infos {
        ports.insert(info.key.clone(), info);
    }
    let roots = [
        PortKey::parse("cat/root").unwrap(),
        PortKey::parse("cat/other").unwrap(),
    ];
    let s = Session::new(ports, HashMap::new(), &roots, &OptionsFiles::new(tmp.path()), false);
    let deep = PortKey::parse("cat/deep").unwrap();
    assert_eq!(
        s.why_chain(&deep),
        Some(vec![
            PortKey::parse("cat/other").unwrap(),
            PortKey::parse("cat/leaf").unwrap(),
            deep,
        ])
    );
}

#[test]
fn an_edit_leaves_the_state_showing() {
    let defs = vec![(
        "X",
        OptionDef { prevents: vec!["Y".into()], ..Default::default() },
    )];
    let (ports, key) = mk_port(&["X", "Y"], &["Y"], vec![], defs);
    let mut s = session_for(ports);
    s.toggle(&key, "X").unwrap(); // staged != baseline AND a conflict
    let info = s.ports.get(&key).unwrap();
    assert_eq!(s.status(info), UiStatus::Conflict, "the edit does not hide the conflict");
    assert!(s.edited(info));
}

#[test]
fn revert_and_reset_lifecycle() {
    let (ports, key) = mk_port(&["A", "B"], &["A"], vec![], vec![]);
    let mut s = session_for(ports);
    assert!(!s.dirty());
    s.toggle(&key, "B").unwrap();
    assert!(s.dirty());
    s.revert(&key);
    assert!(!s.dirty(), "revert restores baseline");
    s.toggle(&key, "A").unwrap(); // A off (deviates from defaults)
    s.reset_to_defaults(&key);
    let st = s.state(&s.ports[&key]).unwrap();
    assert!(st.staged.contains("A") && !st.staged.contains("B"));
}

/// bsd.options.mk:300-304: under OPTIONS_OVERRIDE the file is never
/// read, so `d` must not stage the defaults any more than a toggle may:
/// the state stays as the override has it, not dirty, and the refusal
/// is the toggle's. The same for a file options.local resets.
#[test]
fn reset_to_defaults_does_nothing_where_toggle_refuses() {
    let (mut ports, key) = mk_port(&["A", "B"], &["A"], vec![], vec![]);
    ports.get_mut(&key).unwrap().options.r#override = Some(set(&["B"]));
    let mut s = session_for(ports);
    let why = s.toggle(&key, "A").unwrap_err();
    assert_eq!(s.edit_refusal(&key), Some(why));
    s.reset_to_defaults(&key);
    assert_eq!(s.state(&s.ports[&key]).unwrap().staged, set(&["B"]));
    assert!(!s.dirty());

    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    std::fs::write(tmp.path().join("cat_port/options"), "OPTIONS_FILE_SET+=B\nOPTIONS_FILE_UNSET+=A\n").unwrap();
    std::fs::write(tmp.path().join("cat_port/options.local"), "OPTIONS_FILE_UNSET=\n").unwrap();
    let (ports, key) = mk_port(&["A", "B"], &["A"], vec![], vec![]);
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    let mut s = Session::new(ports, HashMap::new(), &roots, &OptionsFiles::new(tmp.path()), false);
    let before = s.state(&s.ports[&key]).unwrap().staged.clone();
    let why = s.toggle(&key, "A").unwrap_err();
    assert!(why.starts_with("options.local resets OPTIONS_FILE_UNSET"), "{why}");
    assert_eq!(s.edit_refusal(&key), Some(why));
    s.reset_to_defaults(&key);
    assert_eq!(s.state(&s.ports[&key]).unwrap().staged, before);
    assert!(!s.dirty());
}

/// Flavors share one options state, built from the owner's view: a reset
/// asked from a flavor that lacks some of the owner's options must not
/// drop them, or apply writes them out as unset.
#[test]
fn reset_on_a_non_default_flavor_keeps_the_owners_defaults() {
    let mk = |flavor: &str, complete: &[&str]| {
        let (mut p, k) = mk_port(complete, complete, vec![], vec![]);
        let mut info = p.remove(&k).unwrap();
        info.key = PortKey::parse(&format!("editors/emacs@{flavor}")).unwrap();
        info.canonical = info.key.clone();
        info.flavors = vec!["full".into(), "nox".into()];
        info.options_name = "editors_emacs".into();
        info
    };
    let mut ports = BTreeMap::new();
    for info in [mk("full", &["A", "X11"]), mk("nox", &["A"])] {
        ports.insert(info.key.clone(), info);
    }
    let nox = PortKey::parse("editors/emacs@nox").unwrap();
    let mut s = session_for(ports);
    s.toggle(&nox, "A").unwrap();
    s.reset_to_defaults(&nox);
    let st = s.state(&s.ports[&nox]).unwrap();
    assert_eq!(st.staged, set(&["A", "X11"]), "the owner's defaults, X11 included");
    assert!(!s.dirty(), "a reset to the defaults of an unconfigured port is no edit");
}

#[test]
fn reload_saved_refreshes_baseline() {
    let (ports, key) = mk_port(&["A"], &[], vec![], vec![]);
    let tmp = tempfile::tempdir().unwrap();
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    let mut s = Session::new(ports, HashMap::new(), &roots, &OptionsFiles::new(tmp.path()), false);
    assert_eq!(s.status(&s.ports[&key]), UiStatus::Unconfigured);

    // Simulate an apply: a file appears on disk recording A=on.
    std::fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    std::fs::write(
        tmp.path().join("cat_port/options"),
        "_OPTIONS_READ=port-1.0\n_FILE_COMPLETE_OPTIONS_LIST=A\nOPTIONS_FILE_SET+=A\n",
    )
    .unwrap();
    s.reload_saved(&OptionsFiles::new(tmp.path()), ["cat_port"]);
    // Baseline now follows the file; staged still empty -> edited.
    let st = s.state(&s.ports[&key]).unwrap();
    assert!(st.baseline.contains("A"));
    assert!(s.edited(&s.ports[&key]));
    assert_eq!(s.status(&s.ports[&key]), UiStatus::Ok);
}

/// bsd.options.mk:343 reads options.local whether or not the options
/// file is there: with options.local alone deciding every option, the
/// port is configured, nothing is undecided, and the baseline is what
/// options.local says; an option it leaves out is new, as with an
/// empty options file.
#[test]
fn options_local_alone_decides_what_it_names() {
    let (ports, key) = mk_port(&["A", "B", "C"], &["B"], vec![], vec![]);
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    let local = tmp.path().join("cat_port/options.local");
    std::fs::write(&local, "OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B C\n").unwrap();
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    let s = Session::new(ports.clone(), HashMap::new(), &roots, &OptionsFiles::new(tmp.path()), false);
    let info = &s.ports[&key];
    assert_eq!(s.status(info), UiStatus::Ok);
    assert_eq!(undecided_options(info, s.state(info).unwrap().saved.as_ref(), false), Vec::<String>::new());
    assert_eq!(s.state(info).unwrap().baseline, set(&["A"]));

    std::fs::write(&local, "OPTIONS_FILE_SET+=A\n").unwrap();
    let s = Session::new(ports, HashMap::new(), &roots, &OptionsFiles::new(tmp.path()), false);
    let info = &s.ports[&key];
    assert_eq!(s.status(info), UiStatus::Stale);
    assert_eq!(s.stale_delta(info), Some((vec!["B".to_string(), "C".to_string()], vec![])));
    assert_eq!(undecided_options(info, s.state(info).unwrap().saved.as_ref(), false), ["B", "C"]);
}

/// A TUI refresh queries make with the staged edits in place, so a
/// port's `effective` can hold an edit since undone. Under -m an apply
/// writes no file for a port back at its defaults; the baseline it then
/// reloads must not come from that stale `effective`.
#[test]
fn reload_saved_ignores_a_refreshed_effective() {
    let (ports, key) = mk_port(&["A", "B"], &["A"], vec![], vec![]);
    let tmp = tempfile::tempdir().unwrap();
    let files = OptionsFiles::new(tmp.path());
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    let mut s = Session::new(ports, HashMap::new(), &roots, &files, true);
    // B turned on, a refresh merged with it staged, B turned off again.
    s.ports.get_mut(&key).unwrap().options.effective = set(&["A", "B"]);
    s.toggle(&key, "B").unwrap();
    s.toggle(&key, "B").unwrap();
    assert!(!s.dirty());
    // The apply writes nothing (A alone is the no-file outcome).
    s.reload_saved(&files, []);
    assert!(!s.dirty(), "{:?}", s.state(&s.ports[&key]));

    // Same, with a file the apply removes.
    std::fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    let file = tmp.path().join("cat_port/options");
    std::fs::write(&file, "_FILE_COMPLETE_OPTIONS_LIST=A B\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\n")
        .unwrap();
    files.forget(["cat_port"]);
    s.reload_saved(&files, ["cat_port"]);
    s.toggle(&key, "B").unwrap();
    s.toggle(&key, "B").unwrap();
    std::fs::remove_file(&file).unwrap();
    files.forget(["cat_port"]);
    s.reload_saved(&files, ["cat_port"]);
    assert!(!s.dirty(), "{:?}", s.state(&s.ports[&key]));
}

/// After an apply only the files it touched are read again: the store
/// forgets those, the rest are as the scan read them.
#[test]
fn reload_saved_rereads_only_forgotten_files() {
    let tmp = tempfile::tempdir().unwrap();
    let write = |name: &str, set: &str| {
        std::fs::create_dir_all(tmp.path().join(name)).unwrap();
        std::fs::write(
            tmp.path().join(name).join("options"),
            format!("_FILE_COMPLETE_OPTIONS_LIST=A\nOPTIONS_FILE_{set}+=A\n"),
        )
        .unwrap();
    };
    let mut ports = BTreeMap::new();
    for name in ["cat_a", "cat_b"] {
        let (mut p, key) = mk_port(&["A"], &[], vec![], vec![]);
        let mut info = p.remove(&key).unwrap();
        info.key = PortKey::parse(&name.replace('_', "/")).unwrap();
        info.canonical = info.key.clone();
        info.options_name = name.into();
        ports.insert(info.key.clone(), info);
        write(name, "UNSET");
    }
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    let files = OptionsFiles::new(tmp.path());
    let mut s = Session::new(ports, HashMap::new(), &roots, &files, false);
    assert_eq!(files.disk_reads(), 2);

    write("cat_a", "SET");
    write("cat_b", "SET");
    files.forget(["cat_a"]);
    s.reload_saved(&files, ["cat_a"]);
    assert_eq!(files.disk_reads(), 3, "only the forgotten file is read");
    let a = PortKey::parse("cat/a").unwrap();
    let b = PortKey::parse("cat/b").unwrap();
    assert!(s.state(&s.ports[&a]).unwrap().baseline.contains("A"));
    assert!(!s.state(&s.ports[&b]).unwrap().baseline.contains("A"));
}

#[test]
fn bulk_set_applies_rules_and_dedups_shared_state() {
    let mk = |origin: &str, options_name: &str, complete: &[&str]| {
        PortInfo {
            pkgname: format!("{}-1.0", origin.replace('/', "-")),
            flavors: vec!["f1".into(), "f2".into()],
            options_name: options_name.into(),
            options: PortOptions {
                complete: complete.iter().map(ToString::to_string).collect(),
                ..Default::default()
            },
            ..PortInfo::stub(origin)
        }
    };
    let mut ports = BTreeMap::new();
    for info in [
        mk("cat/a", "cat_a", &["DOCS", "X11"]),
        mk("cat/b", "cat_b", &["DOCS"]),
        mk("cat/c", "cat_c", &["ZZZ"]),
        // Two flavors of one origin share cat_d's options file/state.
        mk("cat/d@f1", "cat_d", &["DOCS"]),
        mk("cat/d@f2", "cat_d", &["DOCS"]),
    ] {
        ports.insert(info.key.clone(), info);
    }
    let a = PortKey::parse("cat/a").unwrap();
    let b = PortKey::parse("cat/b").unwrap();
    let c = PortKey::parse("cat/c").unwrap();
    let d1 = PortKey::parse("cat/d@f1").unwrap();
    let d2 = PortKey::parse("cat/d@f2").unwrap();
    // cat/b's DOCS is nailed down by a make.conf *_FORCE knob.
    ports.get_mut(&b).unwrap().options.force_set.insert("DOCS".into());
    let keys: Vec<PortKey> = ports.keys().cloned().collect();
    let mut s = session_for(ports);

    let (changed, skipped) = s.bulk_set(&keys, "DOCS", true);
    assert!(changed.contains(&a), "cat/a takes the decision");
    assert_eq!(
        changed.iter().filter(|k| **k == d1 || **k == d2).count(),
        1,
        "one shared state change counts as one changed port"
    );
    assert_eq!(changed.len(), 2, "only cat/a and one cat/d flavor changed");
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].0, b);
    assert!(skipped[0].1.contains("forced"), "reason: {}", skipped[0].1);
    assert!(!changed.contains(&c) && !skipped.iter().any(|(k, _)| *k == c));

    for key in [&a, &d1, &d2] {
        let st = s.state(&s.ports[key]).unwrap();
        assert!(st.staged.contains("DOCS"), "{key} staged DOCS");
    }
    assert!(!s.state(&s.ports[&c]).unwrap().staged.contains("DOCS"));

    // Idempotent: already-equal ports are skipped silently.
    let (changed, skipped) = s.bulk_set(&keys, "DOCS", true);
    assert!(changed.is_empty(), "nothing left to change");
    assert!(!skipped.iter().any(|(k, _)| *k == a), "cat/a not an error");
}

#[test]
fn bulk_set_respects_groups() {
    let g = OptionGroup {
        kind: GroupKind::Single,
        name: "IMPL".into(),
        desc: String::new(),
        members: vec!["A".into(), "B".into()],
    };
    let (ports, key) = mk_port(&["A", "B"], &["A"], vec![g], vec![]);
    let mut s = session_for(ports);
    let (changed, skipped) = s.bulk_set(std::slice::from_ref(&key), "A", false);
    assert!(changed.is_empty());
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].0, key);
    assert!(skipped[0].1.contains("single-choice group"), "reason: {}", skipped[0].1);
    assert!(s.state(&s.ports[&key]).unwrap().staged.contains("A"), "A stays on");
}

#[test]
fn forced_options_refuse_toggle() {
    let (ports, key) = mk_port(&["A"], &[], vec![], vec![]);
    let mut s = session_for(ports);
    s.ports.get_mut(&key).unwrap().options.force_set.insert("A".into());
    let err = s.toggle(&key, "A").unwrap_err();
    assert!(err.contains("forced"));
}

/// Turning on a member of a SINGLE or RADIO group drops its siblings,
/// but a *_FORCE knob puts a forced-on one back after the options file:
/// the toggle is refused rather than leave the group with two members.
#[test]
fn a_forced_on_sibling_refuses_the_switch() {
    for kind in [GroupKind::Single, GroupKind::Radio] {
        let g = OptionGroup {
            kind,
            name: "G".into(),
            desc: String::new(),
            members: vec!["A".into(), "B".into()],
        };
        let (mut ports, key) = mk_port(&["A", "B"], &["A"], vec![g], vec![]);
        ports.get_mut(&key).unwrap().options.force_set.insert("A".into());
        let mut s = session_for(ports);
        let err = s.toggle(&key, "B").unwrap_err();
        assert!(err.contains("forced on"), "{kind:?}: {err}");
        assert_eq!(s.state(&s.ports[&key]).unwrap().staged, set(&["A"]), "{kind:?}");
    }
}

/// A member turned off while a sibling is forced on leaves the group as
/// make sees it satisfied: the toggle is allowed, and it is the way out
/// of the conflict a forced-on sibling and a chosen one make together.
#[test]
fn a_forced_on_sibling_lets_the_chosen_member_go() {
    for (kind, staged) in [(GroupKind::Single, &["B"][..]), (GroupKind::Multi, &["A", "B"][..])] {
        let g = OptionGroup {
            kind,
            name: "G".into(),
            desc: String::new(),
            members: vec!["A".into(), "B".into()],
        };
        let (mut ports, key) = mk_port(&["A", "B"], staged, vec![g], vec![]);
        ports.get_mut(&key).unwrap().options.force_set.insert("A".into());
        let mut s = session_for(ports);
        assert_eq!(s.toggle(&key, "B"), Ok(()), "{kind:?}");
        assert!(s.violations(&s.ports[&key]).is_empty(), "{kind:?}");
    }
}

/// Inside a conditional group whose option is off, a member make.conf
/// sets and the file keeps on agrees with make.conf: make drops it from
/// both outcomes.
#[test]
fn an_unchecked_group_member_does_not_deviate() {
    let g = OptionGroup {
        kind: GroupKind::Single,
        name: "AUDIO".into(),
        desc: String::new(),
        members: vec!["NAS".into(), "RPLAY".into()],
    };
    let (mut ports, key) = mk_port(&["DOCS", "AUDIO", "NAS", "RPLAY"], &["NAS"], vec![g], vec![]);
    ports.get_mut(&key).unwrap().options.mc_set.insert("NAS".into());
    let s = session_for(ports);
    assert!(!s.mc_deviates(&s.ports[&key], "NAS"));
}

/// bsd.options.mk applies each scope's UNSET after its SET, so an
/// option one scope both sets and unsets ends up off: the JSON verdict
/// says so, with the scope that has the last word.
#[test]
fn makeconf_verdict_follows_the_framework_layering() {
    let (mut ports, key) = mk_port(&["A", "B"], &[], vec![], vec![]);
    {
        let o = &mut ports.get_mut(&key).unwrap().options;
        o.mc_set.insert("A".into());
        o.mc_unset.insert("A".into());
        o.mc_unset.insert("B".into());
        o.port_set.insert("B".into());
        o.port_unset.insert("B".into());
    }
    let s = session_for(ports);
    let verdicts: Vec<(String, bool, &str)> = crate::report::option_reports(&s, &s.ports[&key])
        .into_iter()
        .map(|r| {
            let m = r.makeconf.expect("make.conf names both");
            (r.name, m.value, m.scope)
        })
        .collect();
    assert_eq!(verdicts, [("A".to_string(), false, "global"), ("B".to_string(), false, "port")]);
}

/// IMPLIES runs after FORCE (bsd.options.mk:411), so an option a
/// forced-on one implies is on whatever the file says: no decision is
/// owed for it.
#[test]
fn what_a_forced_option_implies_is_decided() {
    let defs = vec![("A", OptionDef { implies: vec!["B".into()], ..Default::default() })];
    let (mut ports, key) = mk_port(&["A", "B", "C"], &[], vec![], defs);
    ports.get_mut(&key).unwrap().options.force_set.insert("A".into());
    assert_eq!(undecided_options(&ports[&key], None, false), ["C"]);
}

/// OPTIONS_SLAVE is on whatever the layers say, out of the list:
/// databases/sqlite3@icu has ICU with or without a file, so a file
/// repeating the defaults is not judged to differ from none.
#[test]
fn a_slave_option_is_on_with_or_without_the_file() {
    let (mut ports, key) = mk_port(&["FTS", "JSON"], &["JSON"], vec![], vec![]);
    ports.get_mut(&key).unwrap().options.slave.insert("ICU".into());
    let info = &ports[&key];
    let file = SavedOptionsFile {
        complete: info.options.complete.clone(),
        set: set(&["JSON"]),
        unset: set(&["FTS"]),
        ..Default::default()
    };
    assert_eq!(nofile_effective(info), set(&["ICU", "JSON"]));
    assert_eq!(effective_with(info, Some(&file)), nofile_effective(info));
}

/// bsd.port.mk drops the members of a conditional SINGLE or MULTI group
/// whose option is off from PORT_OPTIONS: mail/faces with AUDIO off and
/// NAS set in its file builds without NAS.
#[test]
fn an_unchecked_conditional_group_loses_its_members() {
    let g = OptionGroup {
        kind: GroupKind::Single,
        name: "AUDIO".into(),
        desc: String::new(),
        members: vec!["NAS".into(), "RPLAY".into(), "SOX".into()],
    };
    let (mut ports, key) =
        mk_port(&["DOCS", "NLS", "AUDIO", "NAS", "RPLAY", "SOX"], &[], vec![g], vec![]);
    ports.get_mut(&key).unwrap().options.all = set(&["AUDIO", "DOCS", "NLS"]);
    let info = &ports[&key];
    let file = SavedOptionsFile {
        complete: info.options.complete.clone(),
        set: set(&["NLS", "NAS"]),
        unset: set(&["DOCS", "AUDIO", "RPLAY", "SOX"]),
        ..Default::default()
    };
    assert_eq!(effective_with(info, Some(&file)), set(&["NLS"]));
    let mut on = file.clone();
    on.set.insert("AUDIO".into());
    on.unset.remove("AUDIO");
    assert_eq!(effective_with(info, Some(&on)), set(&["AUDIO", "NAS", "NLS"]));
}

/// bsd.port.mk's pre-check-config takes a SINGLE or MULTI group for
/// conditional when ALL_OPTIONS (OPTIONS_DEFINE) matches its name, a
/// group member or not: AUDIO, defined and a member of the GROUP G,
/// names the SINGLE group AUDIO, which goes unchecked while AUDIO is
/// off. A group named after an option that is only a group member
/// (not in ALL_OPTIONS) is always checked.
#[test]
fn a_group_is_conditional_by_all_options() {
    let group = |kind, name: &str, members: &[&str]| OptionGroup {
        kind,
        name: name.into(),
        desc: String::new(),
        members: members.iter().map(ToString::to_string).collect(),
    };
    let (mut ports, key) = mk_port(
        &["AUDIO", "NAS", "SOX"],
        &[],
        vec![group(GroupKind::Group, "G", &["AUDIO"]), group(GroupKind::Single, "AUDIO", &["NAS", "SOX"])],
        vec![],
    );
    ports.get_mut(&key).unwrap().options.all = set(&["AUDIO"]);
    let s = session_for(ports.clone());
    assert!(s.violations(&s.ports[&key]).is_empty(), "{:?}", s.violations(&s.ports[&key]));
    assert_eq!(nofile_effective(&s.ports[&key]), set(&[]));

    ports.get_mut(&key).unwrap().options.all = set(&[]);
    let s = session_for(ports);
    assert_eq!(
        s.violations(&s.ports[&key]),
        ["group AUDIO needs exactly one member enabled (0 enabled)"]
    );
}

fn draft_with(entries: &[(&str, &[&str])]) -> crate::draft::Draft {
    crate::draft::Draft {
        options_dir: std::path::PathBuf::from("/nonexistent"),
        saved_at_secs: 0,
        staged: entries
            .iter()
            .map(|(name, opts)| {
                (name.to_string(), opts.iter().map(ToString::to_string).collect())
            })
            .collect(),
    }
}

#[test]
fn restore_draft_filters_options_gone_from_the_port() {
    let (ports, key) = mk_port(&["A", "B"], &["B"], vec![], vec![]);
    let mut s = session_for(ports);
    let draft = draft_with(&[("cat_port", &["A", "GONE"])]);
    assert_eq!(s.restore_draft(&draft), 1);
    let st = s.state(&s.ports[&key]).unwrap();
    assert_eq!(st.staged, ["A".to_string()].into_iter().collect::<BTreeSet<_>>());
    // Baseline is untouched, so the restored state still reads as edited.
    assert!(s.edited(&s.ports[&key]));
    // Re-applying the same draft changes nothing.
    assert_eq!(s.restore_draft(&draft), 0);
}

#[test]
fn restore_draft_drops_unknown_options_names() {
    let (ports, key) = mk_port(&["A"], &["A"], vec![], vec![]);
    let mut s = session_for(ports);
    let before = s.state(&s.ports[&key]).unwrap().staged.clone();
    assert_eq!(s.restore_draft(&draft_with(&[("other_port", &["A"])])), 0);
    assert_eq!(s.state(&s.ports[&key]).unwrap().staged, before);
}

/// A session over `ports` and the options dir `dir`.
fn session_in(ports: BTreeMap<PortKey, PortInfo>, dir: &std::path::Path) -> Session {
    let roots: Vec<PortKey> = ports.keys().cloned().collect();
    Session::new(ports, HashMap::new(), &roots, &OptionsFiles::new(dir), false)
}

/// An options file holding a line optique cannot model (here an
/// `OPTIONS_SET`, inert in make since its loop ran before the include,
/// though the wrapper dumps it as a make.conf layer) claims nothing:
/// no saved values, make's own PORT_OPTIONS as the state, ok, nothing
/// undecided, every option locked by the file with no make.conf
/// verdict, no edit or draft taken, and no write or removal planned.
#[test]
fn an_unmanageable_file_is_left_to_make() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    std::fs::write(
        tmp.path().join("cat_port/options"),
        "_OPTIONS_READ=port-1.0\n_FILE_COMPLETE_OPTIONS_LIST=A B C\nOPTIONS_FILE_SET+=A\n\
         OPTIONS_FILE_UNSET+=B\nOPTIONS_SET=B\n",
    )
    .unwrap();
    let (mut ports, key) = mk_port(&["A", "B", "C"], &["C"], vec![], vec![]);
    {
        let o = &mut ports.get_mut(&key).unwrap().options;
        o.mc_set.insert("B".into());
        o.effective = set(&["A", "C"]);
    }
    let mut s = session_in(ports, tmp.path());
    let info = s.ports[&key].clone();
    let why = "options line 5 assigns OPTIONS_SET, which optique does not model: optique leaves \
               the options file alone; edit it by hand";
    assert_eq!(s.unmanaged(&info), Some(Unmanaged { by: "options file", why: why.to_string() }));
    let st = s.state(&info).unwrap();
    assert!(st.saved.is_none());
    assert_eq!(st.staged, set(&["A", "C"]));
    assert_eq!(s.status(&info), UiStatus::Ok);
    assert!(s.undecided(&info).is_empty() && s.covered_by_makeconf(&info));
    assert!(!s.mc_deviates(&info, "B"));
    assert_eq!(s.toggle(&key, "B").unwrap_err(), why);
    s.reset_to_defaults(&key);
    assert_eq!(s.restore_draft(&draft_with(&[("cat_port", &["B"])])), 0);
    assert!(!s.edited(&info));
    for o in crate::report::option_reports(&s, &info) {
        assert!(o.makeconf.is_none() && o.saved.is_none() && !o.new, "{}", o.name);
        assert_eq!(o.locked_by.as_deref(), Some("options file"), "{}", o.name);
    }
    let files = OptionsFiles::new(tmp.path());
    for minimal in [false, true] {
        let planned = apply::plan_writes(std::iter::once((&key, &info, set(&["B"]))), &files, minimal);
        assert!(planned.writes.is_empty() && planned.removals.is_empty(), "minimal {minimal}");
    }
    // Nor is it removed once its port has no options left.
    let mut optionless = info.clone();
    optionless.options.complete.clear();
    let ports: BTreeMap<PortKey, PortInfo> = [(key.clone(), optionless)].into();
    assert!(apply::plan_stale_removals(&ports, &files).is_empty());
}

/// bsd.options.mk:183: make.conf moved OPTIONS_FILE, so make reads
/// neither the file optique manages nor its options.local. With none
/// on disk the port is not unconfigured, edits are refused, and sync
/// writes nothing.
#[test]
fn a_moved_options_file_is_left_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut ports, key) = mk_port(&["A", "B"], &["A"], vec![], vec![]);
    ports.get_mut(&key).unwrap().options.moved_file = Some("/elsewhere/options".into());
    let mut s = session_in(ports, tmp.path());
    let info = s.ports[&key].clone();
    assert_eq!(s.status(&info), UiStatus::Ok);
    let why = "make.conf sets OPTIONS_FILE to /elsewhere/options, not the options file optique manages; edit it there";
    assert_eq!(s.toggle(&key, "A").unwrap_err(), why);
    let reports = crate::report::option_reports(&s, &info);
    assert!(reports.iter().all(|o| o.locked_by.as_deref() == Some("make.conf OPTIONS_FILE")));
    let planned =
        apply::plan_writes(std::iter::once((&key, &info, set(&["A"]))), &OptionsFiles::new(tmp.path()), false);
    assert!(planned.writes.is_empty());
}

/// bsd.options.mk:183: make.conf moves OPTIONS_FILE for one flavor
/// only. It reads a file of its own, so it is no reader of the shared
/// one: its edits are refused and its state is its own PORT_OPTIONS,
/// while the default flavor owns, judges and writes the shared file.
/// Whichever flavor the closure meets first.
#[test]
fn a_flavor_moving_options_file_leaves_the_shared_file_to_its_owner() {
    let tmp = tempfile::tempdir().unwrap();
    for (default, moved_flavor) in [("a", "b"), ("b", "a")] {
        let flavor = |f: &str| {
            let mut info = PortInfo::stub(&format!("cat/x@{f}"));
            info.flavors = vec![default.into(), moved_flavor.into()];
            info.options_name = "cat_x".into();
            info.options.complete = vec!["A".into(), "B".into()];
            info.options.defaults = set(&["A"]);
            info.options.effective = set(&["A"]);
            info
        };
        let mut moved_info = flavor(moved_flavor);
        moved_info.options.moved_file = Some("/elsewhere/options".into());
        moved_info.options.effective = set(&["B"]);
        let (owner, other) = (flavor(default).key, moved_info.key.clone());
        let ports: BTreeMap<PortKey, PortInfo> =
            [(owner.clone(), flavor(default)), (other.clone(), moved_info)].into();
        let mut s = session_in(ports, tmp.path());
        assert_eq!(s.owners["cat_x"], owner, "default {default}");
        let (o, m) = (s.ports[&owner].clone(), s.ports[&other].clone());
        assert_eq!(s.unmanaged(&o), None, "default {default}");
        assert_eq!(s.unmanaged(&m).map(|u| u.by), Some("make.conf OPTIONS_FILE"), "default {default}");
        assert_eq!(s.status(&o), UiStatus::Unconfigured, "default {default}");
        assert_eq!(s.status(&m), UiStatus::Ok, "default {default}");
        assert_eq!(s.state(&m).unwrap().staged, set(&["B"]), "its own outcome, default {default}");
        assert!(s.toggle(&other, "A").is_err(), "default {default}");
        s.revert(&other);
        s.toggle(&owner, "B").unwrap();
        assert_eq!(s.state(&o).unwrap().staged, set(&["A", "B"]), "default {default}");
        assert_eq!(s.state(&m).unwrap().staged, set(&["B"]), "default {default}");
        let (changed, skipped) = s.bulk_set(&[other.clone(), owner.clone()], "A", false);
        assert_eq!((changed, skipped.len()), (vec![owner.clone()], 0), "default {default}");
        for r in crate::report::option_reports(&s, &m) {
            assert_eq!(r.locked_by.as_deref(), Some("make.conf OPTIONS_FILE"), "{}", r.name);
            assert_eq!(r.staged, r.name == "B", "{}", r.name);
        }
        let staged = s.ports.iter().map(|(k, i)| (k, i, s.state(i).unwrap().staged.clone()));
        let planned = apply::plan_writes(staged, &OptionsFiles::new(tmp.path()), false);
        assert_eq!(planned.writes.len(), 1, "default {default}");
        assert_eq!(planned.writes[0].key, owner, "default {default}");
        assert_eq!(planned.writes[0].enabled, set(&["B"]), "default {default}");
        assert!(planned.warnings.is_empty(), "{:?}", planned.warnings);
    }
}

/// A newcomer in the TUI is first queried against the staging options
/// dir, which does not hold its file yet, then again with it. For a
/// file optique cannot manage, the state follows the owner's fresh
/// PORT_OPTIONS on every merge: it takes no edit, so nothing staged is
/// lost, and the first query's outcome without the file does not stay.
#[test]
fn merge_gives_an_unmanaged_file_its_owners_fresh_outcome() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    std::fs::write(tmp.path().join("cat_port/options"), "OPTIONS_FILE_SET+=X\nFOO=bar\n").unwrap();
    let files = OptionsFiles::new(tmp.path());
    let (ports, key) = mk_port(&["X"], &[], vec![], vec![]);
    let mut s = session_in(ports, tmp.path());
    assert_eq!(s.state(&s.ports[&key]).unwrap().staged, set(&[]), "queried without the file");
    let mut seeded = s.ports[&key].clone();
    seeded.options.effective = set(&["X"]);
    let mut result = crate::query::scanner::ScanResult::default();
    result.ports.insert(key.clone(), seeded);
    s.merge(result, &files);
    let info = s.ports[&key].clone();
    let st = s.state(&info).unwrap();
    assert_eq!((&st.baseline, &st.staged), (&set(&["X"]), &set(&["X"])));
    assert!(!s.edited(&info) && !s.dirty());
}

/// bsd.options.mk:300-304: under an OPTIONS_OVERRIDE make reads no
/// options file, so a file optique cannot manage is not the reason the
/// port takes no edit: the report names the override, as the refusal
/// does, scope and all.
#[test]
fn report_prefers_the_override_over_an_unmanaged_file() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("cat_port")).unwrap();
    std::fs::write(tmp.path().join("cat_port/options"), "OPTIONS_FILE_SET+=A\nFOO=bar\n").unwrap();
    let (mut ports, key) = mk_port(&["A", "B"], &["A"], vec![], vec![]);
    {
        let o = &mut ports.get_mut(&key).unwrap().options;
        o.r#override = Some(set(&["B"]));
        o.effective = set(&["B"]);
    }
    let s = session_in(ports, tmp.path());
    let info = s.ports[&key].clone();
    assert!(s.unmanaged(&info).is_some());
    assert!(s.edit_refusal(&key).unwrap().starts_with("OPTIONS_OVERRIDE"));
    for r in crate::report::option_reports(&s, &info) {
        assert_eq!(r.locked_by.as_deref(), Some("make.conf OPTIONS_OVERRIDE"), "{}", r.name);
        let mc = r.makeconf.as_ref().unwrap();
        assert_eq!((mc.value, mc.scope), (r.name == "B", "override"), "{}", r.name);
    }
}

/// bsd.options.mk:312-409: an UNSET word of any make.conf layer settles
/// what it matches as a pattern (F*); a SET, FORCE SET or WITH word
/// adds itself, which turns no option on, and settles only an option it
/// names. make takes what such a glob matches out of NEW_OPTIONS too, a
/// defect optique does not follow: A1 to E1 stay undecided, none is on,
/// and none is pinned, so the file can still turn C1 on.
#[test]
fn glob_words_decide_in_every_layer_and_turn_nothing_on() {
    let opts = ["A1", "B1", "C1", "D1", "E1", "F1"];
    let (mut ports, key) = mk_port(&opts, &[], vec![], vec![]);
    {
        let o = &mut ports.get_mut(&key).unwrap().options;
        o.mc_set.insert("A*".into());
        o.port_set.insert("B*".into());
        o.force_set.insert("C*".into());
        o.port_force_set.insert("D*".into());
        o.with.insert("E*".into());
        o.port_unset.insert("F*".into());
    }
    let info = &ports[&key];
    assert_eq!(undecided_options(info, None, false), ["A1", "B1", "C1", "D1", "E1"]);
    let on = nofile_effective(info);
    assert!(opts.iter().all(|o| !on.contains(*o)), "{on:?}");
    assert!(opts.iter().all(|o| !info.options.is_forced(o)));
    let mut s = session_for(ports);
    s.toggle(&key, "C1").expect("C* pins nothing");
}

/// bsd.port.mk's pre-check-config matches each PREVENTS word as a
/// pattern against every option on (`${PORT_OPTIONS:M${conflict}}`),
/// the option itself included, and names the word: A prevents B*,
/// which B1 matches; X prevents X*, itself.
#[test]
fn prevents_words_are_patterns() {
    let defs = vec![
        ("A", OptionDef { prevents: vec!["B*".into()], ..Default::default() }),
        ("X", OptionDef { prevents: vec!["X*".into()], ..Default::default() }),
    ];
    let (ports, key) = mk_port(&["A", "B1", "B2", "X"], &["A", "B1"], vec![], defs);
    let mut s = session_for(ports);
    let conflicts = s.conflicts(&s.ports[&key]);
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].text, "A conflicts with B*: mutually exclusive");
    assert_eq!(conflicts[0].members, ["A", "B1"]);
    s.toggle(&key, "B1").unwrap();
    s.toggle(&key, "X").unwrap();
    assert_eq!(s.violations(&s.ports[&key]), ["X conflicts with X*: mutually exclusive"]);
}

/// bsd.options.mk:410-440 adds what an option of the list implies
/// unchecked: C implies FOO, no option of the port (one excluded on
/// this platform, say), and make's PORT_OPTIONS holds it, so D's
/// PREVENTS of FOO fails check-config. The staged set keeps to the
/// list, and a file repeating the defaults is still redundant against
/// make's PORT_OPTIONS.
#[test]
fn what_an_option_implies_goes_in_listed_or_not() {
    let defs = vec![
        ("C", OptionDef { implies: vec!["FOO".into()], ..Default::default() }),
        ("D", OptionDef { prevents: vec!["FOO".into()], ..Default::default() }),
    ];
    let (mut ports, key) = mk_port(&["C", "D"], &["C", "D"], vec![], defs);
    ports.get_mut(&key).unwrap().options.effective = set(&["C", "D", "FOO"]);
    let info = ports[&key].clone();
    assert_eq!(nofile_effective(&info), set(&["C", "D", "FOO"]));
    assert!(crate::clean::redundancy_diff(&info, &SavedOptionsFile::default()).is_empty());
    let s = session_for(ports);
    assert_eq!(s.state(&info).unwrap().staged, set(&["C", "D"]));
    assert_eq!(s.violations(&info), ["D conflicts with FOO: mutually exclusive"]);
}
