//! `decide`: set option values from a JSON plan instead of from keystrokes.
//!
//! The plan is the whole interface — an object of ports, each an object of
//! option names to booleans:
//!
//! ```json
//! { "www/nginx": { "LUA": true, "DOCS": false } }
//! ```
//!
//! Values are applied through the same [`Session`] rules the interface
//! enforces (group kinds, `IMPLIES` closure, `*_FORCE` and implied locks), so
//! a plan can never record a state the TUI would refuse. Anything the rules
//! turn down is reported rather than silently dropped, and the caller writes
//! nothing unless the plan was honoured in full.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};

use crate::model::options::on_off;
use crate::model::origin::PortKey;
use crate::session::Session;

/// A parsed plan: ports in the order the JSON object sorted them, each with
/// the option values asked for.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub ports: BTreeMap<PortKey, BTreeMap<String, bool>>,
}

/// What applying a plan did and did not achieve.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Ports the plan actually changed, in plan order.
    pub touched: Vec<PortKey>,
    /// (port, option, why) for every value the port's rules refused.
    pub rejected: Vec<(PortKey, String, String)>,
    /// Ports named in the plan that the closure does not hold.
    pub unknown: Vec<PortKey>,
}

impl Outcome {
    /// Was every value in the plan honoured?
    pub fn complete(&self) -> bool {
        self.rejected.is_empty() && self.unknown.is_empty()
    }
}

impl Plan {
    /// Parse the JSON plan, naming the offending entry on bad input. An empty
    /// plan is an error: a caller that meant "change nothing" should not be
    /// running `decide`.
    pub fn parse(text: &str) -> Result<Plan> {
        if text.trim().is_empty() {
            bail!("empty plan on stdin; expected {{\"category/port\": {{\"OPTION\": true}}}}");
        }
        let root: serde_json::Value =
            serde_json::from_str(text).context("plan is not valid JSON")?;
        let obj = root.as_object().context(
            "plan must be a JSON object of ports: {\"category/port\": {\"OPTION\": true}}",
        )?;
        let mut ports = BTreeMap::new();
        for (name, value) in obj {
            let key = PortKey::parse(name)
                .with_context(|| format!("{name:?} is not a port origin (category/name[@flavor])"))?;
            let wanted = value
                .as_object()
                .with_context(|| format!("{name}: expected an object of OPTION: true|false"))?;
            let mut opts = BTreeMap::new();
            for (opt, v) in wanted {
                let on = v.as_bool().with_context(|| {
                    format!("{name}.{opt}: expected true or false, got {v}")
                })?;
                opts.insert(opt.clone(), on);
            }
            if opts.is_empty() {
                bail!("{name}: no options given");
            }
            if ports.insert(key, opts).is_some() {
                bail!("{name}: named twice in the plan");
            }
        }
        if ports.is_empty() {
            bail!("the plan names no port");
        }
        Ok(Plan { ports })
    }

    /// The ports the plan is about, for rooting a scan on the plan alone.
    pub fn roots(&self) -> Vec<PortKey> {
        self.ports.keys().cloned().collect()
    }
}

/// The port a plan entry names: the closure's own, or, for a listed origin
/// `--follow-moved` followed, the port it moved to. That old name is kept
/// out of the session's aliases: a dependency on it, in no tree, is an
/// error, and must not lead to the target and close a loop through it.
fn resolve(sess: &Session, requested: &PortKey) -> Option<PortKey> {
    sess.resolve(requested).or_else(|| {
        let m = sess.listed_moves.iter().find(|m| m.followed && m.listed == *requested)?;
        sess.resolve(&m.to)
    })
}

/// Apply the plan to the session's staged state.
///
/// Options are applied in name order within each port, which matters when one
/// implies another or they share a single-choice group: the order is fixed so
/// the same plan always produces the same state.
pub fn apply_plan(sess: &mut Session, plan: &Plan) -> Outcome {
    let mut outcome = Outcome::default();
    // (port, values asked, options its own toggles refused) per entry, for
    // the final-state check once the whole plan has run.
    let mut asked: Vec<(PortKey, &BTreeMap<String, bool>, Vec<String>)> = Vec::new();
    for (requested, wanted) in &plan.ports {
        let Some(key) = resolve(sess, requested) else {
            outcome.unknown.push(requested.clone());
            continue;
        };
        let Some(info) = sess.ports.get(&key).cloned() else {
            outcome.unknown.push(requested.clone());
            continue;
        };
        if !info.options.has_options() {
            outcome.rejected.push((
                key.clone(),
                String::new(),
                "port has no options".to_string(),
            ));
            continue;
        }
        let mut changed = false;
        let mut refused = Vec::new();
        for (opt, on) in wanted {
            if !info.options.complete.iter().any(|o| o == opt) {
                outcome.rejected.push((
                    key.clone(),
                    opt.clone(),
                    format!("{} has no option {opt}", info.canonical),
                ));
                refused.push(opt.clone());
                continue;
            }
            // Already the wanted value: not a change, and not an error even
            // when the option is locked there.
            if sess.is_staged(&info, opt) == *on {
                continue;
            }
            match sess.toggle(&key, opt) {
                Ok(()) => changed = true,
                Err(reason) => {
                    outcome.rejected.push((key.clone(), opt.clone(), reason));
                    refused.push(opt.clone());
                }
            }
        }
        if changed {
            outcome.touched.push(key.clone());
        }
        asked.push((key, wanted, refused));
    }
    // Order must not hide a broken promise. A value can be undone after it
    // was set — an IMPLIES chain dragging an option back on, a single-choice
    // group deselecting a sibling, a later entry on the same options state
    // (another flavor sharing the file) — without any single toggle refusing
    // anything. Check what each port ended up with once the whole plan ran,
    // so `complete()` means every asked value is the final value.
    for (key, wanted, refused) in asked {
        let Some(info) = sess.ports.get(&key) else { continue };
        for (opt, on) in wanted {
            if refused.contains(opt) {
                continue;
            }
            if sess.is_staged(info, opt) != *on {
                outcome.rejected.push((key.clone(), opt.clone(), why_not(sess, &key, opt, *on)));
            }
        }
    }
    outcome.rejected.sort();
    outcome
}

/// Why an option did not end up at the requested value, said as concretely as
/// the port's own rules allow.
fn why_not(sess: &Session, key: &PortKey, opt: &str, wanted: bool) -> String {
    let Some(info) = sess.ports.get(key) else {
        return "the port left the closure".to_string();
    };
    if let Some(value) = info.options.forced_value(opt) {
        return format!(
            "make.conf forces it {}; the options file cannot override a *_FORCE knob",
            on_off(value)
        );
    }
    if wanted {
        // Something took it away again: in practice the sibling of a
        // single-choice or radio group enabled later in the plan.
        if let Some(g) = info.options.group_of(opt) {
            let on: Vec<&str> =
                g.members.iter().filter(|m| sess.is_staged(info, m)).map(String::as_str).collect();
            if !on.is_empty() {
                return format!(
                    "group {} ({}) ended up with {} instead",
                    g.name,
                    g.kind.label(),
                    on.join(" ")
                );
            }
        }
        return "another value in the plan turned it off again".to_string();
    }
    if let Some(by) = sess.implied_by(info, opt) {
        return format!("implied by {by}, which the plan leaves on");
    }
    "another value in the plan turned it on again".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_plain_plan() {
        let plan = Plan::parse(r#"{"www/nginx": {"LUA": true, "DOCS": false}}"#).unwrap();
        assert_eq!(plan.ports.len(), 1);
        let key = PortKey::parse("www/nginx").unwrap();
        assert!(plan.ports[&key]["LUA"]);
        assert!(!plan.ports[&key]["DOCS"]);
        assert_eq!(plan.roots(), vec![key]);
    }

    #[test]
    fn a_flavor_is_part_of_the_port_name() {
        let plan = Plan::parse(r#"{"devel/git@lite": {"CONTRIB": false}}"#).unwrap();
        assert_eq!(plan.roots(), vec![PortKey::parse("devel/git@lite").unwrap()]);
    }

    #[test]
    fn bad_plans_say_what_is_wrong() {
        let cases = [
            ("", "empty plan"),
            ("   \n", "empty plan"),
            ("not json", "valid JSON"),
            ("[]", "must be a JSON object"),
            (r#"{"nginx": {"LUA": true}}"#, "not a port origin"),
            (r#"{"www/nginx": []}"#, "expected an object"),
            (r#"{"www/nginx": {"LUA": "yes"}}"#, "expected true or false"),
            (r#"{"www/nginx": {}}"#, "no options given"),
            ("{}", "names no port"),
        ];
        for (input, needle) in cases {
            let err = Plan::parse(input).unwrap_err().to_string();
            assert!(err.contains(needle), "{input:?} -> {err:?} must mention {needle:?}");
        }
    }

    /// A port with the given option list, defaults and option definitions.
    fn port(
        origin: &str,
        complete: &[&str],
        defaults: &[&str],
        defs: &[(&str, crate::model::options::OptionDef)],
    ) -> crate::model::port::PortInfo {
        use crate::model::options::PortOptions;
        let effective: std::collections::BTreeSet<String> =
            defaults.iter().map(ToString::to_string).collect();
        crate::model::port::PortInfo {
            options: PortOptions {
                complete: complete.iter().map(ToString::to_string).collect(),
                defaults: defaults.iter().map(ToString::to_string).collect(),
                effective,
                defs: defs.iter().map(|(n, d)| (n.to_string(), d.clone())).collect(),
                ..Default::default()
            },
            ..crate::model::port::PortInfo::stub(origin)
        }
    }

    /// A session over those ports, with an empty options dir (so every port
    /// starts unconfigured and staged at its defaults).
    fn session(ports: Vec<crate::model::port::PortInfo>) -> (Session, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let map: BTreeMap<_, _> =
            ports.into_iter().map(|i| (i.canonical.clone(), i)).collect();
        let roots: Vec<PortKey> = map.keys().cloned().collect();
        let files = crate::optfiles::OptionsFiles::new(tmp.path());
        let sess = Session::new(map, Default::default(), &roots, &files, false);
        (sess, tmp)
    }

    fn staged(sess: &Session, origin: &str) -> Vec<String> {
        let key = PortKey::parse(origin).unwrap();
        let info = &sess.ports[&key];
        sess.state(info).unwrap().staged.iter().cloned().collect()
    }

    #[test]
    fn applies_values_and_names_options_the_port_lacks() {
        let (mut sess, _tmp) =
            session(vec![port("cat/one", &["DOCS", "SSL"], &["DOCS"], &[])]);
        let plan =
            Plan::parse(r#"{"cat/one": {"SSL": true, "DOCS": false, "NOPE": true}}"#).unwrap();
        let out = apply_plan(&mut sess, &plan);
        assert_eq!(staged(&sess, "cat/one"), vec!["SSL"]);
        assert_eq!(out.touched, vec![PortKey::parse("cat/one").unwrap()]);
        assert_eq!(out.rejected.len(), 1);
        assert_eq!(out.rejected[0].1, "NOPE");
        assert!(out.rejected[0].2.contains("has no option NOPE"));
        assert!(!out.complete(), "a refused value means the plan was not honoured");
    }

    /// The plan goes through the same rules as the interface: an option held
    /// on by IMPLIES cannot be turned off behind the framework's back.
    #[test]
    fn implied_options_cannot_be_turned_off() {
        use crate::model::options::OptionDef;
        let defs = [("SSL", OptionDef { implies: vec!["CRYPTO".into()], ..Default::default() })];
        let (mut sess, _tmp) =
            session(vec![port("cat/one", &["SSL", "CRYPTO"], &[], &defs)]);
        let plan = Plan::parse(r#"{"cat/one": {"SSL": true, "CRYPTO": false}}"#).unwrap();
        let out = apply_plan(&mut sess, &plan);
        // Options are applied in name order, so CRYPTO=false runs first and
        // is a no-op (it is already off); SSL=true then drags CRYPTO back on.
        // No single toggle refused anything, and the plan is still not
        // honoured — the final-state check is what says so.
        assert_eq!(staged(&sess, "cat/one"), vec!["CRYPTO", "SSL"]);
        assert_eq!(out.rejected.len(), 1, "{:?}", out.rejected);
        assert_eq!(out.rejected[0].1, "CRYPTO");
        assert!(out.rejected[0].2.contains("implied by SSL"), "{:?}", out.rejected[0]);
        assert!(!out.complete());
    }

    /// The same trap the other way round: a single-choice group where the
    /// plan asks for both members. The loser is reported, not swallowed.
    #[test]
    fn a_group_member_the_plan_loses_is_reported() {
        use crate::model::options::{GroupKind, OptionGroup};
        let mut info = port("cat/one", &["AAA", "ZZZ"], &["AAA"], &[]);
        info.options.groups = vec![OptionGroup {
            kind: GroupKind::Single,
            name: "IMPL".into(),
            desc: String::new(),
            members: vec!["AAA".into(), "ZZZ".into()],
        }];
        let (mut sess, _tmp) = session(vec![info]);
        // AAA applies first (name order) and is already on; ZZZ then wins the
        // group and deselects it.
        let plan = Plan::parse(r#"{"cat/one": {"AAA": true, "ZZZ": true}}"#).unwrap();
        let out = apply_plan(&mut sess, &plan);
        assert_eq!(staged(&sess, "cat/one"), vec!["ZZZ"]);
        assert_eq!(out.rejected.len(), 1, "{:?}", out.rejected);
        assert_eq!(out.rejected[0].1, "AAA");
        assert!(out.rejected[0].2.contains("group IMPL"), "{:?}", out.rejected[0]);
    }

    /// Two flavors share one options state: a later entry asking the other
    /// value of the same option undoes the earlier entry, and the earlier
    /// one is reported rather than checked before the later one ran.
    #[test]
    fn a_later_entry_undoing_an_earlier_one_is_reported() {
        let flavor = |f: &str| {
            let mut info = port(&format!("devel/git@{f}"), &["NLS"], &[], &[]);
            info.flavors = vec!["default".into(), "lite".into()];
            info.options_name = "devel_git".into();
            info
        };
        let (mut sess, _tmp) = session(vec![flavor("default"), flavor("lite")]);
        let plan =
            Plan::parse(r#"{"devel/git@default": {"NLS": true}, "devel/git@lite": {"NLS": false}}"#)
                .unwrap();
        let out = apply_plan(&mut sess, &plan);
        assert!(staged(&sess, "devel/git@default").is_empty());
        assert_eq!(out.rejected.len(), 1, "{:?}", out.rejected);
        assert_eq!(out.rejected[0].0, PortKey::parse("devel/git@default").unwrap());
        assert_eq!(out.rejected[0].1, "NLS");
        assert!(out.rejected[0].2.contains("turned it off again"), "{:?}", out.rejected[0]);
        assert!(!out.complete(), "a plan contradicting itself is not honoured");
    }

    /// A `*_FORCE` knob is named as such rather than as a mystery.
    #[test]
    fn a_forced_option_says_so() {
        let mut info = port("cat/one", &["DOCS"], &["DOCS"], &[]);
        info.options.force_unset = ["DOCS".to_string()].into_iter().collect();
        // What make would report: the FORCE knob already beat the default.
        info.options.effective.clear();
        let (mut sess, _tmp) = session(vec![info]);
        let plan = Plan::parse(r#"{"cat/one": {"DOCS": true}}"#).unwrap();
        let out = apply_plan(&mut sess, &plan);
        assert_eq!(out.rejected.len(), 1, "{:?}", out.rejected);
        assert!(out.rejected[0].2.contains("*_FORCE"), "{:?}", out.rejected[0]);
    }

    /// A single-choice group switches rather than refusing, exactly as Space
    /// does in the interface.
    #[test]
    fn a_single_group_switches_member() {
        use crate::model::options::{GroupKind, OptionGroup};
        let mut info = port("cat/one", &["MIT", "HEIMDAL"], &["MIT"], &[]);
        info.options.groups = vec![OptionGroup {
            kind: GroupKind::Single,
            name: "GSSAPI".into(),
            desc: String::new(),
            members: vec!["MIT".into(), "HEIMDAL".into()],
        }];
        let (mut sess, _tmp) = session(vec![info]);
        let plan = Plan::parse(r#"{"cat/one": {"HEIMDAL": true}}"#).unwrap();
        let out = apply_plan(&mut sess, &plan);
        assert_eq!(staged(&sess, "cat/one"), vec!["HEIMDAL"]);
        assert!(out.complete());
    }

    #[test]
    fn a_port_outside_the_closure_is_reported_not_guessed() {
        let (mut sess, _tmp) = session(vec![port("cat/one", &["DOCS"], &["DOCS"], &[])]);
        let plan = Plan::parse(r#"{"cat/absent": {"DOCS": false}}"#).unwrap();
        let out = apply_plan(&mut sess, &plan);
        assert_eq!(out.unknown, vec![PortKey::parse("cat/absent").unwrap()]);
        assert!(out.touched.is_empty());
        assert!(!out.complete());
    }

    /// Asking for the value a port already has changes nothing and is not an
    /// error, so a plan can be replayed idempotently.
    #[test]
    fn asking_for_the_current_value_is_not_a_change() {
        let (mut sess, _tmp) =
            session(vec![port("cat/one", &["DOCS", "SSL"], &["DOCS"], &[])]);
        let plan = Plan::parse(r#"{"cat/one": {"DOCS": true, "SSL": false}}"#).unwrap();
        let out = apply_plan(&mut sess, &plan);
        assert!(out.touched.is_empty(), "nothing to do");
        assert!(out.complete());
        assert_eq!(staged(&sess, "cat/one"), vec!["DOCS"]);
    }

    /// A port with no options at all cannot be decided about.
    #[test]
    fn an_optionless_port_is_refused() {
        let (mut sess, _tmp) = session(vec![port("cat/bare", &[], &[], &[])]);
        let plan = Plan::parse(r#"{"cat/bare": {"DOCS": true}}"#).unwrap();
        let out = apply_plan(&mut sess, &plan);
        assert_eq!(out.rejected.len(), 1);
        assert!(out.rejected[0].2.contains("no options"));
    }
}
