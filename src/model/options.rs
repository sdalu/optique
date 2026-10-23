use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Kind of an options group, with the constraint it imposes on its members.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupKind {
    /// Free choice.
    Group,
    /// At least one member must be selected.
    Multi,
    /// Exactly one member must be selected.
    Single,
    /// At most one member may be selected.
    Radio,
}

/// "on" or "off".
pub fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// Whether `word` matches the make pattern `pat`, as bmake's `:M` and
/// `:N` modifiers test it (Str_Match in bmake's str.c, ported as is): `*`
/// any run of characters, `?` any one, `[...]` one of a list with `a-z`
/// ranges either way round and `^` negating it, `\\` taking the next
/// character literally. bsd.options.mk removes an UNSET entry with
/// `${PORT_OPTIONS:N${opt}}` and every mention with `${NEW_OPTIONS:N${opt}}`,
/// so `A*` takes out every option starting with A.
pub fn str_match(word: &str, pat: &str) -> bool {
    let (s, p) = (word.as_bytes(), pat.as_bytes());
    // C strings: past the end reads as NUL.
    let at = |b: &[u8], i: usize| b.get(i).copied().unwrap_or(0);
    let mut asterisk = false;
    let (mut fixed_s, mut fixed_p) = (0usize, 0usize);
    'fixed: loop {
        let (mut si, mut pi) = (fixed_s, fixed_p);
        let mut no_match = false;
        while at(p, pi) != 0 && at(p, pi) != b'*' {
            if at(s, si) == 0 {
                return false;
            }
            let c = at(p, pi);
            if c == b'?' {
                si += 1;
                pi += 1;
                continue;
            }
            if c == b'[' {
                let neg = at(p, pi + 1) == b'^';
                pi += if neg { 2 } else { 1 };
                let sc = at(s, si);
                // Whether the list ends in end_of_char_list (else no_match).
                let listed = loop {
                    let pc = at(p, pi);
                    if pc == b']' || pc == 0 {
                        break neg;
                    }
                    if pc == sc {
                        break true;
                    }
                    if at(p, pi + 1) == b'-' && at(p, pi + 2) == 0 {
                        return neg;
                    }
                    if at(p, pi + 1) == b'-' {
                        let (e1, e2) = (pc, at(p, pi + 2));
                        if (e1 <= sc && sc <= e2) || (e2 <= sc && sc <= e1) {
                            break true;
                        }
                        pi += 2;
                    }
                    pi += 1;
                };
                if !listed || (neg && at(p, pi) != b']' && at(p, pi) != 0) {
                    no_match = true;
                    break;
                }
                while at(p, pi) != b']' && at(p, pi) != 0 {
                    pi += 1;
                }
                if at(p, pi) == 0 {
                    pi -= 1;
                }
                si += 1;
                pi += 1;
                continue;
            }
            let mut pc = c;
            if c == b'\\' {
                pi += 1;
                pc = at(p, pi);
            }
            if pc != at(s, si) {
                if asterisk && si == fixed_s {
                    while at(s, si) != 0 && at(s, si) != pc {
                        si += 1;
                    }
                    fixed_s = si;
                    continue 'fixed;
                }
                no_match = true;
                break;
            }
            si += 1;
            pi += 1;
        }
        if no_match {
            if !asterisk {
                return false;
            }
            fixed_s += 1;
            continue 'fixed;
        }
        if at(p, pi) == b'*' {
            asterisk = true;
            while at(p, pi) == b'*' {
                pi += 1;
            }
            if at(p, pi) == 0 {
                return true;
            }
            fixed_s = si;
            fixed_p = pi;
            continue 'fixed;
        }
        if asterisk && at(s, si) != 0 {
            fixed_s += s.len() - si;
            continue 'fixed;
        }
        return at(s, si) == 0;
    }
}

/// Whether an UNSET entry of `list` takes `opt` out: any entry matching it
/// as a make pattern (`${PORT_OPTIONS:N${entry}}`).
pub fn unsets(list: &BTreeSet<String>, opt: &str) -> bool {
    list.iter().any(|entry| str_match(opt, entry))
}

/// Whether an entry of `list` takes `opt` out of NEW_OPTIONS, SET and
/// UNSET alike (`${NEW_OPTIONS:N${entry}}`): the same pattern test. A SET
/// entry does so only when it matches some option of the port, which one
/// matching `opt`, an option of the port, does.
pub fn mentions(list: &BTreeSet<String>, opt: &str) -> bool {
    unsets(list, opt)
}

impl GroupKind {
    pub fn label(self) -> &'static str {
        match self {
            GroupKind::Group => "group",
            GroupKind::Multi => "multi (pick at least one)",
            GroupKind::Single => "single (pick exactly one)",
            GroupKind::Radio => "radio (pick at most one)",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OptionGroup {
    pub kind: GroupKind,
    pub name: String,
    pub desc: String,
    pub members: Vec<String>,
}

impl OptionGroup {
    /// How many members `set` holds.
    pub fn count_on(&self, set: &BTreeSet<String>) -> usize {
        self.members.iter().filter(|m| set.contains(*m)).count()
    }
}

/// Per-option metadata gathered from the ports framework.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OptionDef {
    pub desc: String,
    /// Options force-enabled when this one is on (`<OPT>_IMPLIES`).
    pub implies: Vec<String>,
    /// Options incompatible with this one (`<OPT>_PREVENTS`).
    pub prevents: Vec<String>,
    pub prevents_msg: Option<String>,
    /// Enabling this option marks the port BROKEN with this message.
    pub broken: Option<String>,
    /// Enabling this option marks the port IGNOREd with this message.
    pub ignore: Option<String>,
    /// Dependencies this option adds, as (class, origins) — class is
    /// "lib"/"run"/"build"/"fetch"/"extract"/"patch".
    #[serde(default)]
    pub deps: Vec<(String, Vec<String>)>,
    /// USES frameworks the option activates (raw tokens).
    #[serde(default)]
    pub uses: Vec<String>,
}

/// Everything the ports framework reports about a port's options,
/// including the make.conf layers that feed into the final PORT_OPTIONS.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PortOptions {
    /// COMPLETE_OPTIONS_LIST, in the framework's order (display order).
    pub complete: Vec<String>,
    /// OPTIONS_DEFAULT (intersected with complete at display time).
    pub defaults: BTreeSet<String>,
    /// Final effective set as make computed it (PORT_OPTIONS).
    pub effective: BTreeSet<String>,
    pub groups: Vec<OptionGroup>,
    pub defs: BTreeMap<String, OptionDef>,
    /// Global make.conf OPTIONS_SET / OPTIONS_UNSET (filtered to complete).
    pub mc_set: BTreeSet<String>,
    pub mc_unset: BTreeSet<String>,
    /// Per-port make.conf ${OPTIONS_NAME}_SET / _UNSET.
    pub port_set: BTreeSet<String>,
    pub port_unset: BTreeSet<String>,
    /// Global OPTIONS_SET_FORCE / OPTIONS_UNSET_FORCE (these override the
    /// options file). Kept apart from the per-port pair below because the
    /// framework applies the two scopes in order, so a per-port FORCE knob
    /// overrides a global one that says the opposite.
    pub force_set: BTreeSet<String>,
    pub force_unset: BTreeSet<String>,
    /// Per-port ${OPTIONS_NAME}_SET_FORCE / _UNSET_FORCE.
    #[serde(default)]
    pub port_force_set: BTreeSet<String>,
    #[serde(default)]
    pub port_force_unset: BTreeSet<String>,
    /// OPTIONS_SLAVE: options a slave port or flavor takes out of the
    /// list and turns on whatever the file, make.conf or FORCE say
    /// (bsd.options.mk adds them to PORT_OPTIONS last, after IMPLIES).
    #[serde(default)]
    pub slave: BTreeSet<String>,
    /// make.conf's WITH / WITHOUT, which bsd.options.mk:399-409 applies
    /// after the FORCE knobs and before IMPLIES ("Cmdline always win"); the
    /// queries run with a cleared environment, so make.conf is their only
    /// source. They pin an option as FORCE does.
    #[serde(default)]
    pub with: BTreeSet<String>,
    #[serde(default)]
    pub without: BTreeSet<String>,
    /// make.conf's OPTIONS_OVERRIDE, Some only when the wrapper saw it
    /// `defined` (bsd.options.mk:300-304): then it alone decides
    /// PORT_OPTIONS, taken as is and not filtered by `complete` — defaults,
    /// the SET/UNSET layers and the options file are never read. An empty
    /// set (defined empty) is not the same as None (undefined).
    #[serde(default)]
    pub r#override: Option<BTreeSet<String>>,
}

/// Where an option's current value comes from, following the
/// bsd.options.mk precedence chain (lowest to highest).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    Default,
    MakeConfGlobal,
    MakeConfPort,
    File,
    Forced,
    /// make.conf's OPTIONS_OVERRIDE: the file is not read at all.
    Override,
}

/// The make.conf layer that names an option last: the global
/// OPTIONS_SET/UNSET, the per-port ${OPTIONS_NAME}_SET/_UNSET, one of the
/// *_FORCE knobs (global or per-port), or WITH/WITHOUT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McLayer {
    Global,
    Port,
    Force,
    /// WITH / WITHOUT: applied after FORCE, pinning the option likewise.
    With,
}

impl PortOptions {
    pub fn has_options(&self) -> bool {
        !self.complete.is_empty()
    }

    /// The group `opt` belongs to, if any.
    pub fn group_of(&self, opt: &str) -> Option<&OptionGroup> {
        self.groups.iter().find(|g| g.members.iter().any(|m| m == opt))
    }

    /// The SINGLE or RADIO group `opt` belongs to, if any: the groups where
    /// turning one member on turns the others off.
    pub fn exclusive_group_of(&self, opt: &str) -> Option<&OptionGroup> {
        self.groups.iter().find(|g| {
            matches!(g.kind, GroupKind::Single | GroupKind::Radio) && g.members.iter().any(|m| m == opt)
        })
    }

    /// Whether make.conf names the option, globally or for this port,
    /// FORCE knobs aside: whether one of those lists takes it out of
    /// NEW_OPTIONS, an entry naming it or matching it as a pattern.
    pub fn mc_mentions(&self, opt: &str) -> bool {
        [&self.mc_set, &self.mc_unset, &self.port_set, &self.port_unset]
            .into_iter()
            .any(|list| mentions(list, opt))
    }

    /// Whether any make.conf layer, FORCE knobs included, takes the option
    /// out of NEW_OPTIONS: make counts it as decided.
    pub fn mc_decides(&self, opt: &str) -> bool {
        self.mc_mentions(opt)
            || [
                &self.force_set,
                &self.force_unset,
                &self.port_force_set,
                &self.port_force_unset,
                &self.with,
                &self.without,
            ]
            .into_iter()
            .any(|list| mentions(list, opt))
    }

    /// Whether toggling this option in the options file can have any effect.
    pub fn is_forced(&self, opt: &str) -> bool {
        self.forced_value(opt).is_some()
    }

    /// The value a *_FORCE knob or WITH/WITHOUT pins the option to, None
    /// when none does. They are applied in the framework's order, last one
    /// wins: global SET, global UNSET, per-port SET, per-port UNSET, WITH,
    /// WITHOUT (bsd.options.mk:368-409). A SET or WITH entry
    /// adds itself, word for word, so only one naming the option turns it
    /// on; an UNSET entry turns off every option it matches as a pattern.
    pub fn forced_value(&self, opt: &str) -> Option<bool> {
        let mut value = None;
        for (v, list) in [
            (true, &self.force_set),
            (false, &self.force_unset),
            (true, &self.port_force_set),
            (false, &self.port_force_unset),
            (true, &self.with),
            (false, &self.without),
        ] {
            if layer_hits(v, list, opt) {
                value = Some(v);
            }
        }
        value
    }

    /// What make.conf says of the option, whatever decides its value now:
    /// the value of the last layer naming it in the framework's order
    /// (global SET, UNSET, per-port SET, UNSET, then the four *_FORCE knobs,
    /// then WITH and WITHOUT; later wins), with that layer. None when no layer names it. A SET
    /// entry names the option word for word, an UNSET one as a pattern, as
    /// `forced_value` reads them.
    pub fn mc_value(&self, opt: &str) -> Option<(bool, McLayer)> {
        let mut value = None;
        for (v, layer, list) in [
            (true, McLayer::Global, &self.mc_set),
            (false, McLayer::Global, &self.mc_unset),
            (true, McLayer::Port, &self.port_set),
            (false, McLayer::Port, &self.port_unset),
            (true, McLayer::Force, &self.force_set),
            (false, McLayer::Force, &self.force_unset),
            (true, McLayer::Force, &self.port_force_set),
            (false, McLayer::Force, &self.port_force_unset),
            (true, McLayer::With, &self.with),
            (false, McLayer::With, &self.without),
        ] {
            if layer_hits(v, list, opt) {
                value = Some((v, layer));
            }
        }
        value
    }

    /// Provenance of the option's value given the enabled set recorded in the
    /// options file (None = no file entry for it).
    pub fn provenance(&self, opt: &str, file_state: Option<bool>) -> Provenance {
        if self.is_forced(opt) {
            Provenance::Forced
        } else if self.r#override.is_some() {
            // bsd.options.mk:300-304: nothing but FORCE and WITH comes after it.
            Provenance::Override
        } else if file_state.is_some() {
            Provenance::File
        } else if layer_hits(true, &self.port_set, opt) || unsets(&self.port_unset, opt) {
            Provenance::MakeConfPort
        } else if layer_hits(true, &self.mc_set, opt) || unsets(&self.mc_unset, opt) {
            Provenance::MakeConfGlobal
        } else {
            Provenance::Default
        }
    }
}

/// Whether a layer's list sets (`adds`) or unsets the option: a SET entry
/// adds itself to PORT_OPTIONS word for word, an UNSET entry takes out
/// every option it matches.
fn layer_hits(adds: bool, list: &BTreeSet<String>, opt: &str) -> bool {
    if adds { list.contains(opt) } else { unsets(list, opt) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What bmake's :M answers (checked against make(1) 20251111), word by
    /// word: `*`, `?`, lists, negated lists, ranges either way, escapes,
    /// and a range swallowing the list's end.
    #[test]
    fn str_match_is_bmakes() {
        let words = [
            "A", "A2", "B", "C", "b", "x", "xy", "abc", "abcd", "ab*", "abx", "aXbYc", "a-b", "]x", "^", "ac",
        ];
        let cases: [(&str, &[&str]); 17] = [
            ("A*", &["A", "A2"]),
            ("[AB]", &["A", "B"]),
            ("[^AB]", &["C", "b", "x", "^"]),
            ("[a-c]", &["b"]),
            ("[c-a]", &["b"]),
            ("?", &["A", "B", "C", "b", "x", "^"]),
            ("*c", &["abc", "aXbYc", "ac"]),
            ("a*c", &["abc", "aXbYc", "ac"]),
            ("ab\\*", &["ab*"]),
            ("a*b*c", &["abc", "aXbYc"]),
            ("[!A]", &["A"]),
            ("[a-]", &["^"]),
            ("[]x]", &[]),
            ("*a*c*", &["abc", "abcd", "aXbYc", "ac"]),
            ("a?c", &["abc"]),
            ("[a-c]?", &["ac"]),
            ("A", &["A"]),
        ];
        for (pat, want) in cases {
            let got: Vec<&str> = words.iter().copied().filter(|w| str_match(w, pat)).collect();
            assert_eq!(got, want, "{pat}");
        }
        assert!(str_match("", "*") && str_match("", "") && !str_match("a", ""));
    }

    /// bsd.options.mk:321-394: an UNSET entry takes out every option it
    /// matches, in make.conf and FORCE alike; a SET entry adds itself word
    /// for word, but takes what it matches out of NEW_OPTIONS.
    #[test]
    fn unset_entries_are_patterns() {
        let words = |s: &[&str]| s.iter().map(|o| o.to_string()).collect::<BTreeSet<_>>();
        let o = PortOptions {
            complete: ["A", "A2", "B", "C"].iter().map(|s| s.to_string()).collect(),
            mc_unset: words(&["A*"]),
            port_set: words(&["C*"]),
            force_unset: words(&["[B]"]),
            ..Default::default()
        };
        let mentioned: Vec<bool> = ["A", "A2", "B", "C"].iter().map(|x| o.mc_mentions(x)).collect();
        assert_eq!(mentioned, [true, true, false, true]);
        assert!(o.mc_decides("B"));
        assert_eq!(o.mc_value("A2"), Some((false, McLayer::Global)));
        assert_eq!(o.mc_value("C"), None, "C* turns no C on");
        assert_eq!(o.forced_value("B"), Some(false));
        assert!(!o.is_forced("A"));
    }

    /// bsd.options.mk:399-409: WITH and WITHOUT come after the FORCE knobs
    /// and pin the option as they do; WITHOUT entries are patterns.
    #[test]
    fn with_and_without_pin_after_force() {
        let words = |s: &[&str]| s.iter().map(|o| o.to_string()).collect::<BTreeSet<_>>();
        let o = PortOptions {
            complete: ["A", "A2", "B", "C"].iter().map(|s| s.to_string()).collect(),
            force_unset: words(&["C"]),
            port_force_set: words(&["A"]),
            with: words(&["C"]),
            without: words(&["A*"]),
            ..Default::default()
        };
        assert_eq!(o.forced_value("C"), Some(true), "WITH after UNSET_FORCE");
        assert_eq!(o.forced_value("A"), Some(false), "WITHOUT after SET_FORCE");
        assert_eq!(o.mc_value("A2"), Some((false, McLayer::With)));
        assert!(o.mc_decides("A2") && !o.mc_decides("B"));
        assert_eq!(o.forced_value("B"), None);
    }
}
