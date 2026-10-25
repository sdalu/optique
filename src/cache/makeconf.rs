//! Carrying entries over an edit to make.conf that only changes option
//! policy: which variables the edit touched, provided nothing else did,
//! and which entries the change in their values reaches.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::model::options::str_match;
use crate::model::port::PortInfo;

/// The make.conf variables that hold option policy for every port, each
/// with the `PortOptions` field the query dumps it into.
const GLOBAL: [&str; 6] = [
    "OPTIONS_SET",
    "OPTIONS_UNSET",
    "OPTIONS_SET_FORCE",
    "OPTIONS_UNSET_FORCE",
    "WITH",
    "WITHOUT",
];

/// The suffixes of the per-port knobs, `${OPTIONS_NAME}<suffix>`.
const PER_PORT: [&str; 4] = ["_SET_FORCE", "_UNSET_FORCE", "_SET", "_UNSET"];

/// One logical line of a make.conf: its text, continuations joined, and how
/// many conditionals or loops enclose it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Line {
    text: String,
    depth: usize,
}

/// The logical lines of `text`, a backslash at the end of a line joining
/// the next to it, each with its depth.
fn lines(text: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut pending = String::new();
    for raw in text.lines() {
        if let Some(head) = raw.strip_suffix('\\') {
            pending.push_str(head);
            pending.push(' ');
            continue;
        }
        pending.push_str(raw);
        let text = std::mem::take(&mut pending).trim_end().to_string();
        let d = directive(&text);
        let closes = matches!(d, Some("endif" | "endfor"));
        let middle = matches!(d, Some(d) if d.starts_with("el"));
        let opens = matches!(d, Some(d) if d.starts_with("if") || d == "for");
        if closes {
            depth = depth.saturating_sub(1);
        }
        let here = if middle {
            depth.saturating_sub(1)
        } else {
            depth
        };
        out.push(Line { text, depth: here });
        if opens {
            depth += 1;
        }
    }
    out
}

/// The directive a line holds (`if`, `ifdef`, `endif`, `include`, ...),
/// None for any other line.
fn directive(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('.')?.trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_alphabetic() && c != '-')
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// The variable a line assigns and its operator, None for any other line.
fn assignment(line: &str) -> Option<(&str, &str)> {
    let line = line.trim_start();
    if line.starts_with('#') || line.starts_with('.') {
        return None;
    }
    let at = line.find('=')?;
    let (head, _) = line.split_at(at);
    let (name, op) = match head.trim_end().char_indices().last() {
        Some((i, c)) if "+?:!".contains(c) => (head[..i].trim_end(), &line[i..=at]),
        _ => (head.trim_end(), "="),
    };
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
    valid.then_some((name, op))
}

/// Is `name` a make.conf variable holding option policy: a global one, or a
/// per-port knob (`<NAME>_SET` and the like), OPTIONS_FILE's own excepted?
fn is_option_variable(name: &str) -> bool {
    GLOBAL.contains(&name)
        || PER_PORT.iter().any(|suffix| {
            name.strip_suffix(suffix)
                .is_some_and(|stem| !stem.is_empty() && stem != "OPTIONS" && stem != "OPTIONS_FILE")
        })
}

/// The option variables an edit from `old` to `new` touches, when that is
/// all it does: every line one of the texts has and the other has not is a
/// comment, a blank, or a top-level assignment to such a variable whose value
/// references nothing (a `$` may name a port's variable, `${PORTNAME}`,
/// which the probe outside any port reads empty), each such
/// variable is assigned nowhere under a conditional and read by no line,
/// and neither text includes another file, whose content the texts do not
/// show. None for any other edit, which is no option edit, and for no edit.
pub(super) fn option_edit(old: &str, new: &str) -> Option<Vec<String>> {
    let (old, new) = (lines(old), lines(new));
    let includes = |ls: &[Line]| {
        ls.iter()
            .any(|l| matches!(directive(&l.text), Some(d) if d.ends_with("include")))
    };
    if includes(&old) || includes(&new) {
        return None;
    }
    let (before, after) = (count(&old), count(&new));
    let changed: BTreeSet<&Line> = before
        .iter()
        .filter(|(l, n)| after.get(*l) != Some(*n))
        .chain(after.iter().filter(|(l, n)| before.get(*l) != Some(*n)))
        .map(|(l, _)| *l)
        .collect();
    let mut vars = BTreeSet::new();
    for line in changed {
        let text = line.text.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let (name, op) = assignment(text)?;
        if line.depth > 0 || op == "!=" || !is_option_variable(name) || text.contains('$') {
            return None;
        }
        vars.insert(name.to_string());
    }
    if vars.is_empty() {
        return None;
    }
    // In order, not as a multiset: make's value of a variable assigned on
    // several lines follows their order. With the changed variables'
    // lines, the comments and the blanks taken out, what is left is the
    // same sequence of lines at the same depths, or the edit moved
    // something no probe asks about (two DEFAULT_VERSIONS lines swapped).
    let rest = |ls: &[Line]| -> Vec<(String, usize)> {
        ls.iter()
            .filter(|l| {
                let t = l.text.trim();
                !t.is_empty()
                    && !t.starts_with('#')
                    && !assignment(t).is_some_and(|(name, _)| vars.contains(name))
            })
            .map(|l| (l.text.trim().to_string(), l.depth))
            .collect()
    };
    if rest(&old) != rest(&new) {
        return None;
    }
    for var in &vars {
        let reads = [
            format!("{{{var}}}"),
            format!("{{{var}:"),
            format!("({var})"),
            format!("({var}:"),
        ];
        for line in old.iter().chain(&new) {
            let assigns = assignment(&line.text).is_some_and(|(name, _)| name == var);
            if assigns && line.depth > 0 {
                return None;
            }
            if reads.iter().any(|r| line.text.contains(r.as_str())) {
                return None;
            }
            // A directive naming it reads it (`.ifdef WITH`, `.if
            // defined(WITH)`, `.elifndef`) or drops it (`.undef WITH`).
            if directive(&line.text).is_some() && names_word(&line.text, var) {
                return None;
            }
        }
    }
    Some(vars.into_iter().collect())
}

/// Does `text` name `var` as a whole word (no letter, digit, `_`, `-` or
/// `.` either side, the characters a variable name holds)?
fn names_word(text: &str, var: &str) -> bool {
    let part = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.');
    text.match_indices(var).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + var.len()..].chars().next();
        !before.is_some_and(part) && !after.is_some_and(part)
    })
}

/// How many times each line occurs in `lines`.
fn count(lines: &[Line]) -> HashMap<&Line, isize> {
    let mut m = HashMap::new();
    for l in lines {
        *m.entry(l).or_default() += 1;
    }
    m
}

/// What an option edit changed: the words added to or taken from each
/// global variable, and the per-port knobs' OPTIONS_NAMEs whose value
/// changed.
#[derive(Debug, Default)]
pub(super) struct Reach {
    /// Each global variable's value after the edit, for the entries kept.
    new_values: BTreeMap<String, BTreeSet<String>>,
    /// Each global variable's value before it, which a kept entry's dump
    /// must show: one that shows another got it from somewhere make.conf
    /// alone does not say.
    old_values: BTreeMap<String, BTreeSet<String>>,
    /// The words in one value and not the other, all variables together.
    words: BTreeSet<String>,
    /// The OPTIONS_NAMEs whose per-port knobs changed.
    names: BTreeSet<String>,
}

impl Reach {
    /// The reach of an edit to `vars`, from their values before and after
    /// it (`old`, `new`, in `vars`' order), as make expands them outside any
    /// port.
    pub(super) fn of(vars: &[String], old: &[String], new: &[String]) -> Reach {
        let mut reach = Reach::default();
        let words = |v: &str| {
            v.split_whitespace()
                .map(str::to_string)
                .collect::<BTreeSet<_>>()
        };
        for ((var, old), new) in vars.iter().zip(old).zip(new) {
            let (old, new) = (words(old), words(new));
            if old == new {
                continue;
            }
            if GLOBAL.contains(&var.as_str()) {
                reach.words.extend(old.symmetric_difference(&new).cloned());
                reach.old_values.insert(var.clone(), old);
                reach.new_values.insert(var.clone(), new);
            } else if let Some(stem) = PER_PORT.iter().find_map(|s| var.strip_suffix(s)) {
                reach.names.insert(stem.to_string());
            }
        }
        reach
    }

    /// `info` as a query under the new make.conf would answer, when the edit
    /// cannot change what it says: no changed word matches one of its
    /// options, as a pattern (as make matches an UNSET entry, and so a SET
    /// one naming the option), its OPTIONS_NAME has no changed knob, and
    /// each changed global variable shows in its dump as make.conf alone
    /// sets it. None when the entry is reached, and must be queried again.
    pub(super) fn carry(&self, info: &PortInfo) -> Option<PortInfo> {
        let o = &info.options;
        if self.names.contains(&o.name) {
            return None;
        }
        let options = o.complete.iter().chain(&o.slave);
        if options
            .clone()
            .any(|opt| self.words.iter().any(|w| str_match(opt, w)))
        {
            return None;
        }
        let mut info = info.clone();
        for (var, new) in &self.new_values {
            let field = field(&mut info, var);
            if *field != self.old_values[var] {
                return None;
            }
            field.clone_from(new);
        }
        Some(info)
    }
}

/// The `PortOptions` field the query dumps global variable `var` into.
fn field<'a>(info: &'a mut PortInfo, var: &str) -> &'a mut BTreeSet<String> {
    let o = &mut info.options;
    match var {
        "OPTIONS_SET" => &mut o.mc_set,
        "OPTIONS_UNSET" => &mut o.mc_unset,
        "OPTIONS_SET_FORCE" => &mut o.force_set,
        "OPTIONS_UNSET_FORCE" => &mut o.force_unset,
        "WITH" => &mut o.with,
        _ => &mut o.without,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(old: &str, new: &str) -> Option<Vec<String>> {
        option_edit(old, new)
    }

    /// An edit to option policy alone names its variables; a comment or a
    /// blank changes nothing, so an edit of nothing else names none.
    #[test]
    fn an_option_edit_names_its_variables() {
        let base = "CPUTYPE?=native\nOPTIONS_UNSET+=DOCS\n";
        assert_eq!(
            edit(base, "CPUTYPE?=native\nOPTIONS_UNSET+=DOCS NLS\n"),
            Some(vec!["OPTIONS_UNSET".to_string()])
        );
        assert_eq!(
            edit(
                base,
                &format!("{base}# a comment\n\nWITH=X11\nwww_firefox_SET=PULSEAUDIO\n")
            ),
            Some(vec!["WITH".to_string(), "www_firefox_SET".to_string()])
        );
        assert_eq!(edit(base, &format!("{base}# only a comment\n")), None);
        assert_eq!(edit(base, base), None);
    }

    /// Anything else, or option policy the text alone cannot tell the value
    /// of, is no option edit.
    #[test]
    fn anything_else_is_no_option_edit() {
        let base = "OPTIONS_UNSET+=DOCS\n";
        for new in [
            "OPTIONS_UNSET+=DOCS\nDEFAULT_VERSIONS+=python=3.12\n",
            "OPTIONS_UNSET+=NLS\nCFLAGS+=-O3\n",
            ".if ${.CURDIR:M*/www/*}\nOPTIONS_UNSET+=NLS\n.endif\nOPTIONS_UNSET+=DOCS\n",
            "OPTIONS_UNSET!=echo DOCS\n",
            "OPTIONS_UNSET+=DOCS\n.include \"/etc/opts.conf\"\n",
            "OPTIONS_UNSET+=DOCS NLS\nX=${OPTIONS_UNSET}\n",
            "OPTIONS_FILE_SET+=A\nOPTIONS_UNSET+=DOCS\n",
            // Its value per port: the probe reads it empty outside one.
            "OPTIONS_UNSET+=DOCS\nOPTIONS_SET+=${PORTNAME:S/tmux/UTF8PROC/}\n",
            "OPTIONS_UNSET+=DOCS\nsysutils_tmux_SET=${FLAVOR}\n",
        ] {
            assert_eq!(edit(base, new), None, "{new}");
        }
        // An option variable also assigned under a conditional, even on a
        // line the edit leaves alone, has a value per port.
        let cond = ".if ${.CURDIR:M*/www/*}\nOPTIONS_UNSET+=NLS\n.endif\n";
        assert_eq!(
            edit(
                &format!("{cond}OPTIONS_UNSET+=DOCS\n"),
                &format!("{cond}OPTIONS_UNSET+=X11\n")
            ),
            None
        );
        // Lines moved: make's value follows their order, so the rest must
        // read the same in order, an option variable outside the edit too.
        assert_eq!(
            edit(
                "DEFAULT_VERSIONS=ssl=openssl\nDEFAULT_VERSIONS+=python=3.11\n",
                "DEFAULT_VERSIONS+=python=3.11\nDEFAULT_VERSIONS=ssl=openssl\nOPTIONS_UNSET+=DOCS\n"
            ),
            None
        );
        assert_eq!(
            edit(
                "OPTIONS_SET=A\nOPTIONS_SET+=B\n",
                "OPTIONS_SET+=B\nOPTIONS_SET=A\nOPTIONS_UNSET+=DOCS\n"
            ),
            None
        );
        // A directive naming an edited variable reads or drops it.
        for directive in [
            ".ifdef WITH\nDEFAULT_VERSIONS+=ssl=openssl\n.endif\n",
            ".if defined(WITH)\nDEFAULT_VERSIONS+=ssl=openssl\n.endif\n",
            ".undef WITH\n",
        ] {
            assert_eq!(
                edit(directive, &format!("WITH=X11\n{directive}")),
                None,
                "{directive}"
            );
        }
        // One naming another variable sharing a prefix is no read of it.
        assert_eq!(
            edit(
                ".ifdef WITHOUT_X\n.endif\n",
                "WITH=X11\n.ifdef WITHOUT_X\n.endif\n"
            ),
            Some(vec!["WITH".to_string()])
        );
        // A continued line is read whole.
        assert_eq!(
            edit("OPTIONS_UNSET+=DOCS \\\n\tNLS\n", "OPTIONS_UNSET+=DOCS\n"),
            Some(vec!["OPTIONS_UNSET".to_string()])
        );
    }

    fn port(origin: &str, name: &str, options: &[&str]) -> PortInfo {
        let mut info = PortInfo::stub(origin);
        info.options.name = name.to_string();
        info.options.complete = options.iter().map(ToString::to_string).collect();
        info.options.mc_unset = ["DOCS".to_string()].into();
        info
    }

    /// A port none of whose options a changed word matches is carried, its
    /// dump showing the new value; one it matches, as a pattern too, or
    /// whose per-port knob changed, is not.
    #[test]
    fn the_reach_is_the_ports_a_changed_word_or_knob_names() {
        let vars = ["OPTIONS_UNSET".to_string(), "net_curl_SET".to_string()];
        let reach = Reach::of(
            &vars,
            &["DOCS".into(), String::new()],
            &["DOCS NLS*".into(), "HTTP2".into()],
        );
        let plain = port("misc/plain", "misc_plain", &["DOCS", "X11"]);
        let carried = reach.carry(&plain).expect("NLS* names none of its options");
        assert_eq!(
            carried.options.mc_unset,
            ["DOCS".to_string(), "NLS*".to_string()].into()
        );
        assert!(
            reach
                .carry(&port("misc/nls", "misc_nls", &["NLS_EXTRA"]))
                .is_none()
        );
        assert!(
            reach
                .carry(&port("net/curl", "net_curl", &["X11"]))
                .is_none()
        );
        // A dump showing another value than make.conf's got it elsewhere.
        let mut odd = port("misc/odd", "misc_odd", &["X11"]);
        odd.options.mc_unset = ["X11".to_string()].into();
        assert!(reach.carry(&odd).is_none());
    }
}
