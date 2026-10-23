use std::collections::BTreeSet;

use crate::model::options::{mentions, unsets};

/// Parsed form of a `${PORT_DBDIR}/<OPTIONS_NAME>/options` file as written
/// by `make config`, or by hand.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedOptionsFile {
    /// `_OPTIONS_READ` (pkgname at the time the file was written).
    pub options_read: String,
    /// `_FILE_COMPLETE_OPTIONS_LIST`.
    pub complete: Vec<String>,
    pub set: BTreeSet<String>,
    pub unset: BTreeSet<String>,
    /// The variables optique reads that some line has assigned so far, by
    /// any operator: a later `?=` to one of them assigns nothing.
    pub assigned: BTreeSet<String>,
    /// `(file, line)` of each `!=` to a variable optique reads: make runs
    /// the shell command, optique does not, and ignores the line. `file` is
    /// `options` or `options.local`, `line` counts from 1.
    pub shell: Vec<(&'static str, usize)>,
    /// The SET and UNSET entries `options.local` contributes, as they stand
    /// once it is read (`with_local`): a `?=` that assigns nothing adds none.
    pub local_set: BTreeSet<String>,
    pub local_unset: BTreeSet<String>,
    /// The lists `options.local` assigns afresh (`=`, `:=`, a `?=` that
    /// assigns), dropping what the options file put in them, in order.
    pub local_resets: Vec<String>,
}

/// How a line assigns its variable: make's `=` and `:=` (afresh), `+=`,
/// `?=` (only when the variable has no value yet) and `!=` (a shell
/// command's output).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Assign,
    Append,
    IfUnset,
    Shell,
}

/// The variable a line assigns, how, and the value; None for a line that
/// assigns nothing.
fn assignment(line: &str) -> Option<(&str, Op, &str)> {
    let (lhs, value) = line.split_once('=')?;
    let (name, op) = match lhs.as_bytes().last() {
        Some(b'+') => (&lhs[..lhs.len() - 1], Op::Append),
        Some(b':') => (&lhs[..lhs.len() - 1], Op::Assign),
        Some(b'?') => (&lhs[..lhs.len() - 1], Op::IfUnset),
        Some(b'!') => (&lhs[..lhs.len() - 1], Op::Shell),
        _ => (lhs, Op::Assign),
    };
    Some((name.trim(), op, value))
}

impl SavedOptionsFile {
    /// The file read as the makefile bsd.options.mk includes (line 341):
    /// a value is the whitespace-separated words after the operator, with
    /// or without spaces around it. `+=` appends to the variable; `=` and
    /// `:=` assign it afresh, as `OPTIONS_FILE_SET=X` resets the list; `?=`
    /// assigns only a variable no earlier line assigned; `!=` (a shell
    /// command) is ignored and recorded in `shell`. Lines that assign
    /// nothing optique reads are skipped.
    pub fn parse(text: &str) -> Self {
        let mut out = SavedOptionsFile::default();
        out.read(text, false);
        out
    }

    /// The file as make sees it with `options.local` (`local`) read after
    /// it: bsd.options.mk:341-363 includes the options file, then
    /// `.sinclude`s `${OPTIONS_FILE}.local`, and only then applies
    /// OPTIONS_FILE_SET and OPTIONS_FILE_UNSET as both left them. The local
    /// file's SET and UNSET lines carry on the file's lists (`+=` adds to
    /// them, `=` assigns them afresh); its list and `_OPTIONS_READ`, which
    /// make never reads, are not taken. optique never writes it.
    pub fn with_local(mut self, local: &str) -> Self {
        self.read(local, true);
        self
    }

    /// Read `text`'s assignments into `self`, in order; `lists_only`
    /// takes nothing but the SET and UNSET lines.
    fn read(&mut self, text: &str, lists_only: bool) {
        let file = if lists_only { "options.local" } else { "options" };
        for (n, line) in text.lines().enumerate() {
            // A `#` starts a comment, at the head of a line or after a value.
            let line = line.split('#').next().unwrap_or_default().trim();
            let Some((name, op, value)) = assignment(line) else { continue };
            let read = match name {
                "OPTIONS_FILE_SET" | "OPTIONS_FILE_UNSET" => true,
                "_OPTIONS_READ" | "_FILE_COMPLETE_OPTIONS_LIST" => !lists_only,
                _ => false,
            };
            if !read {
                continue;
            }
            // Whatever the operator, the variable has a value from here on.
            let first = self.assigned.insert(name.to_string());
            let append = match op {
                Op::Append => true,
                Op::Assign => false,
                Op::IfUnset if first => false,
                Op::IfUnset => continue,
                Op::Shell => {
                    self.shell.push((file, n + 1));
                    continue;
                }
            };
            let words = value.split_whitespace().map(str::to_string);
            match name {
                "_OPTIONS_READ" => {
                    let v = value.trim();
                    if append && !self.options_read.is_empty() {
                        self.options_read = format!("{} {v}", self.options_read);
                    } else {
                        self.options_read = v.to_string();
                    }
                }
                "_FILE_COMPLETE_OPTIONS_LIST" => {
                    if !append {
                        self.complete.clear();
                    }
                    self.complete.extend(words);
                }
                "OPTIONS_FILE_SET" | "OPTIONS_FILE_UNSET" => {
                    let set = name == "OPTIONS_FILE_SET";
                    let mut lists = vec![if set { &mut self.set } else { &mut self.unset }];
                    if lists_only {
                        lists.push(if set { &mut self.local_set } else { &mut self.local_unset });
                        if !append && !self.local_resets.iter().any(|r| r == name) {
                            self.local_resets.push(name.to_string());
                        }
                    }
                    for list in lists {
                        if !append {
                            list.clear();
                        }
                        list.extend(words.clone());
                    }
                }
                _ => {}
            }
        }
    }

    /// The option's value as make reads the file: bsd.options.mk applies
    /// `OPTIONS_FILE_SET`, then `OPTIONS_FILE_UNSET`, so UNSET wins; an option
    /// with neither line (only in the complete list, or absent) has none,
    /// and make leaves it at its default.
    ///
    /// An UNSET entry takes out every option it matches as a make pattern
    /// (`${PORT_OPTIONS:N${opt}}`); a SET entry goes in word for word, so
    /// only one naming the option turns it on.
    pub fn value(&self, opt: &str) -> Option<bool> {
        if unsets(&self.unset, opt) {
            Some(false)
        } else if self.set.contains(opt) {
            Some(true)
        } else {
            None
        }
    }

    /// Whether an entry options.local contributes (`local_set`,
    /// `local_unset`) settles the option, as `decides` judges the file's:
    /// make reads it after the options file, so the file cannot change it.
    pub fn local_decides(&self, opt: &str) -> bool {
        mentions(&self.local_set, opt) || mentions(&self.local_unset, opt)
    }

    /// Why optique cannot manage the options file, when options.local
    /// assigns OPTIONS_FILE_SET or OPTIONS_FILE_UNSET afresh: make drops
    /// what the file put in that list (bsd.options.mk:341-363), so no write
    /// of the file can say what the port ends up with.
    pub fn local_reset(&self) -> Option<String> {
        (!self.local_resets.is_empty()).then(|| {
            format!(
                "options.local resets {}: the options file's lines are not read; edit options.local",
                self.local_resets.join(" and ")
            )
        })
    }

    /// Options with a SET or UNSET line: the ones the file gives a value.
    pub fn recorded(&self) -> impl Iterator<Item = &String> {
        self.set.iter().chain(self.unset.iter())
    }

    /// Options of the file's own list that it gives no SET or UNSET line:
    /// make leaves them at their defaults, and counts them as new, so
    /// `make config-conditional` asks about them again. `make config` never
    /// writes such a file; a hand edit or a write cut short does.
    pub fn unlined(&self) -> Vec<&str> {
        self.complete.iter().map(String::as_str).filter(|o| !self.decides(o)).collect()
    }

    /// Whether the file settles the option: it has a SET or UNSET line for
    /// it. bsd.options.mk starts NEW_OPTIONS as the port's whole list and
    /// takes out every option a SET or UNSET line names (the file's own
    /// `_FILE_COMPLETE_OPTIONS_LIST` is written, never read), so an option
    /// short of a line is new to make, listed or not, and one with a line
    /// is settled, listed or not. Apply rewrites a file whose list differs
    /// all the same (`apply::up_to_date`): that is the rewrite rule, not
    /// newness.
    ///
    /// Every entry, SET or UNSET, takes what it matches as a make pattern
    /// out of NEW_OPTIONS: `OPTIONS_FILE_SET+=A*` settles A2, though it
    /// turns no A2 on.
    pub fn decides(&self, opt: &str) -> bool {
        mentions(&self.set, opt) || mentions(&self.unset, opt)
    }
}

/// Render an options file exactly the way `make config` does: every option of
/// `complete` gets a SET or UNSET line, in `complete` order.
pub fn render(pkgname: &str, complete: &[String], enabled: &BTreeSet<String>) -> String {
    let mut out = String::new();
    out.push_str("# This file is auto-generated by 'make config'.\n");
    out.push_str(&format!("# Options for {pkgname}\n"));
    out.push_str(&format!("_OPTIONS_READ={pkgname}\n"));
    out.push_str(&format!("_FILE_COMPLETE_OPTIONS_LIST={}\n", complete.join(" ")));
    for opt in complete {
        if enabled.contains(opt) {
            out.push_str(&format!("OPTIONS_FILE_SET+={opt}\n"));
        } else {
            out.push_str(&format!("OPTIONS_FILE_UNSET+={opt}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// bsd.options.mk applies OPTIONS_FILE_SET, then OPTIONS_FILE_UNSET, and
    /// leaves an option only in the complete list at its default.
    #[test]
    fn value_is_makes_reading() {
        let f = SavedOptionsFile::parse(
            "_FILE_COMPLETE_OPTIONS_LIST=A B C D\n\
             OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=A\n\
             OPTIONS_FILE_SET+=B\nOPTIONS_FILE_UNSET+=C\n",
        );
        let v: Vec<_> = ["A", "B", "C", "D", "E"].iter().map(|o| f.value(o)).collect();
        assert_eq!(v, [Some(false), Some(true), Some(false), None, None]);
    }

    const SAMPLE: &str = "\
# This file is auto-generated by 'make config'.
# Options for qt5-speech-5.12.1_1
_OPTIONS_READ=qt5-speech-5.12.1_1
_FILE_COMPLETE_OPTIONS_LIST= SPEECHD FLITE
OPTIONS_FILE_SET+=SPEECHD
OPTIONS_FILE_UNSET+=FLITE
";

    #[test]
    fn parse_sample() {
        let f = SavedOptionsFile::parse(SAMPLE);
        assert_eq!(f.options_read, "qt5-speech-5.12.1_1");
        assert_eq!(f.complete, vec!["SPEECHD", "FLITE"]);
        assert!(f.set.contains("SPEECHD"));
        assert!(f.unset.contains("FLITE"));
    }

    /// bsd.options.mk:306-408: NEW_OPTIONS loses every option a SET or
    /// UNSET line names, and the file's list is never read: C, with a line
    /// but not listed, is settled; B, listed with no line, is not.
    #[test]
    fn a_line_settles_an_option_whatever_the_list() {
        let f = SavedOptionsFile::parse(
            "_FILE_COMPLETE_OPTIONS_LIST=A B\nOPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=C\n",
        );
        let settled: Vec<bool> = ["A", "B", "C", "D"].iter().map(|o| f.decides(o)).collect();
        assert_eq!(settled, [true, false, true, false]);
        assert_eq!(f.unlined(), ["B"]);
    }

    /// The file is a makefile (bsd.options.mk:341): several words to a
    /// line, spaces around `+=`, and `=` assigning the list afresh.
    #[test]
    fn parse_reads_the_file_as_make_does() {
        let f = SavedOptionsFile::parse(
            "OPTIONS_FILE_SET+=A B\nOPTIONS_FILE_UNSET += C\tD\n\
             OPTIONS_FILE_SET+=E\nOPTIONS_FILE_SET = X\nOPTIONS_FILE_SET+=Y\n\
             _FILE_COMPLETE_OPTIONS_LIST = A B\n_FILE_COMPLETE_OPTIONS_LIST+=X\n\
             # OPTIONS_FILE_SET+=W\nOPTIONS_FILE_UNSET+=Y # not V\n",
        );
        let words = |s: &[&str]| s.iter().map(|o| o.to_string()).collect::<BTreeSet<_>>();
        assert_eq!(f.set, words(&["X", "Y"]));
        assert_eq!(f.unset, words(&["C", "D", "Y"]));
        assert_eq!(f.complete, ["A", "B", "X"]);
        let v: Vec<_> = ["A", "C", "X", "Y"].iter().map(|o| f.value(o)).collect();
        assert_eq!(v, [None, Some(false), Some(true), Some(false)]);
    }

    /// make's other operators, as make on this host reads them: `:=`
    /// assigns afresh like `=`; `?=` assigns only a variable no earlier
    /// line assigned, the options file and options.local read as one
    /// makefile; `!=` runs a shell command, which optique does not, so the
    /// line is ignored and its place recorded.
    #[test]
    fn colon_question_and_shell_assignments() {
        let f = SavedOptionsFile::parse(
            "OPTIONS_FILE_SET+=A\nOPTIONS_FILE_SET:=B C\nOPTIONS_FILE_SET?=D\n\
             OPTIONS_FILE_UNSET ?= E\nOPTIONS_FILE_UNSET?=F\n\
             _FILE_COMPLETE_OPTIONS_LIST!=echo G\n",
        );
        let words = |s: &[&str]| s.iter().map(|o| o.to_string()).collect::<BTreeSet<_>>();
        assert_eq!(f.set, words(&["B", "C"]), "`:=` resets, `?=` after it assigns nothing");
        assert_eq!(f.unset, words(&["E"]), "the first `?=` assigns, the second not");
        assert!(f.complete.is_empty());
        assert_eq!(f.shell, [("options", 6)]);
        // options.local carries on: its `?=` meets a variable the file assigned.
        let l = f.with_local("OPTIONS_FILE_SET?=H\nOPTIONS_FILE_UNSET:=I\nOPTIONS_FILE_SET != echo J\n");
        assert_eq!(l.set, words(&["B", "C"]));
        assert_eq!(l.unset, words(&["I"]));
        assert_eq!(l.shell, [("options", 6), ("options.local", 3)]);
        // With no options file, options.local's `?=` assigns.
        let l = SavedOptionsFile::default().with_local("OPTIONS_FILE_SET?=H\n");
        assert_eq!(l.set, words(&["H"]));
    }

    /// bsd.options.mk:341-363: options.local is read after the file and
    /// its lines join the file's lists before either is applied, so UNSET
    /// wins across the two: A, set by the file and unset by options.local,
    /// is off; B, unset by the file and set by options.local, too.
    #[test]
    fn options_local_joins_the_files_lists() {
        let f = SavedOptionsFile::parse(
            "_FILE_COMPLETE_OPTIONS_LIST=A B C\n\
             OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B\nOPTIONS_FILE_UNSET+=C\n",
        )
        .with_local("OPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_SET+=B\n_FILE_COMPLETE_OPTIONS_LIST=Z\n");
        let v: Vec<_> = ["A", "B", "C"].iter().map(|o| f.value(o)).collect();
        assert_eq!(v, [Some(false), Some(false), Some(false)]);
        assert_eq!(f.complete, ["A", "B", "C"], "the file's own list");
        // An assignment in options.local resets what the file added.
        let f = SavedOptionsFile::parse("OPTIONS_FILE_SET+=A B\n").with_local("OPTIONS_FILE_SET=C\n");
        let v: Vec<_> = ["A", "B", "C"].iter().map(|o| f.value(o)).collect();
        assert_eq!(v, [None, None, Some(true)]);
        assert_eq!(f.local_set, ["C".to_string()].into());
        let f = SavedOptionsFile::parse("OPTIONS_FILE_SET+=A\n")
            .with_local("OPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_SET+=B\n");
        let words = |s: &[&str]| s.iter().map(|o| o.to_string()).collect::<BTreeSet<_>>();
        assert_eq!((f.local_set, f.local_unset), (words(&["B"]), words(&["A"])));
    }

    /// An options.local that assigns a list afresh drops the file's lines
    /// for it: `=` and `:=` always, `?=` only when the file left the list
    /// unassigned; `+=` never.
    #[test]
    fn options_local_resets_are_recorded() {
        let file = SavedOptionsFile::parse("OPTIONS_FILE_SET+=A\n");
        let resets = |local: &str| file.clone().with_local(local).local_resets;
        assert_eq!(resets("OPTIONS_FILE_SET+=B\nOPTIONS_FILE_UNSET+=A\n"), Vec::<String>::new());
        assert_eq!(resets("OPTIONS_FILE_SET=B\n"), ["OPTIONS_FILE_SET"]);
        assert_eq!(resets("OPTIONS_FILE_UNSET:=B\nOPTIONS_FILE_UNSET=C\n"), ["OPTIONS_FILE_UNSET"]);
        assert_eq!(resets("OPTIONS_FILE_SET?=B\nOPTIONS_FILE_UNSET?=C\n"), ["OPTIONS_FILE_UNSET"]);
        let f = file.with_local("OPTIONS_FILE_UNSET?=C\nOPTIONS_FILE_SET=B\n");
        assert_eq!(
            f.local_reset().as_deref(),
            Some(
                "options.local resets OPTIONS_FILE_UNSET and OPTIONS_FILE_SET: \
                 the options file's lines are not read; edit options.local"
            )
        );
        assert_eq!(SavedOptionsFile::parse("OPTIONS_FILE_SET=A\n").local_reset(), None, "the file's own `=`");
    }

    /// bsd.options.mk:354-363: an UNSET entry takes out every option it
    /// matches as a pattern; a SET entry adds itself word for word, yet
    /// settles what it matches.
    #[test]
    fn file_entries_are_make_patterns() {
        let f = SavedOptionsFile::parse("OPTIONS_FILE_UNSET+=A*\nOPTIONS_FILE_SET+=[BC] D\n");
        let v: Vec<_> = ["A", "A2", "B", "D", "E"].iter().map(|o| f.value(o)).collect();
        assert_eq!(v, [Some(false), Some(false), None, Some(true), None]);
        let settled: Vec<_> = ["A2", "B", "C", "E"].iter().map(|o| f.decides(o)).collect();
        assert_eq!(settled, [true, true, true, false]);
    }

    #[test]
    fn parse_tolerates_junk_and_empty() {
        let f = SavedOptionsFile::parse("");
        assert!(f.complete.is_empty() && f.set.is_empty() && f.unset.is_empty());
        let f = SavedOptionsFile::parse(
            "# only comments\n\n   \nGARBAGE LINE\nOPTIONS_FILE_SET+= SPACED \n",
        );
        assert!(f.set.contains("SPACED"), "values are trimmed");
    }

    #[test]
    fn render_empty_options_list() {
        let text = render("foo-1.0", &[], &BTreeSet::new());
        let f = SavedOptionsFile::parse(&text);
        assert_eq!(f.options_read, "foo-1.0");
        assert!(f.complete.is_empty() && f.set.is_empty() && f.unset.is_empty());
    }

    #[test]
    fn render_round_trip() {
        let complete = vec!["SPEECHD".to_string(), "FLITE".to_string()];
        let enabled: BTreeSet<String> = ["SPEECHD".to_string()].into();
        let text = render("qt5-speech-5.12.1_1", &complete, &enabled);
        let f = SavedOptionsFile::parse(&text);
        assert_eq!(f.options_read, "qt5-speech-5.12.1_1");
        assert_eq!(f.complete, complete);
        assert_eq!(f.set, enabled);
        assert!(f.unset.contains("FLITE"));
    }
}
