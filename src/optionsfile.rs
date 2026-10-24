//! One saved options file, `${PORT_DBDIR}/<OPTIONS_NAME>/options`, with its
//! options.local: parsed as make reads them, the lines optique cannot model
//! kept aside, and rendered as `make config` writes it.

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
    /// Each line optique cannot model, in the order read: a `!=`, a make
    /// directive, a `$` expansion, an assignment to a variable other than
    /// the SET and UNSET lists and the bookkeeping make config writes, and
    /// a `?=` to one of those lists in options.local. Any one makes the
    /// file unmanageable (`unmanaged`).
    pub unmodeled: Vec<Unmodeled>,
    /// The SET and UNSET entries `options.local` contributes, as they stand
    /// once it is read (`with_local`): a `?=` that assigns nothing adds none.
    pub local_set: BTreeSet<String>,
    pub local_unset: BTreeSet<String>,
    /// The lists `options.local` assigns afresh (`=`, `:=`), dropping what
    /// the options file put in them, in order. A `?=` there is in
    /// `unmodeled`: whether it assigns depends on the options file.
    pub local_resets: Vec<String>,
}

/// A line of the options file or options.local that optique cannot model:
/// make reads it, and what it does to the rest of the parse is not in
/// optique's reading of the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unmodeled {
    /// `options` or `options.local`.
    pub file: &'static str,
    /// The line it starts on, from 1.
    pub line: usize,
    /// What the line does, as a phrase: `assigns with != (a shell command)`.
    pub what: String,
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

/// What make splits words on and trims: ASCII blanks, never Unicode's.
const MAKE_BLANKS: &[char] = &[' ', '\t', '\n', '\r', '\x0c'];

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

/// The lines make parses, each with the number (from 1) of the first line
/// it starts on: a line ending in an odd number of backslashes goes on
/// over the next, the backslash, the newline and the next line's leading
/// blanks becoming one space. Make joins them before it looks for a
/// comment, so a comment ending in `\` swallows the next line too; an
/// even number of backslashes ends the line and stays in it.
fn logical_lines(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut open = false;
    for (n, line) in text.lines().enumerate() {
        let line = if open {
            line.trim_start_matches([' ', '\t'])
        } else {
            line
        };
        let trailing = line.len() - line.trim_end_matches('\\').len();
        let (body, more) = if trailing % 2 == 1 {
            (&line[..line.len() - 1], true)
        } else {
            (line, false)
        };
        match out.last_mut() {
            Some((_, joined)) if open => {
                joined.push(' ');
                joined.push_str(body);
            }
            _ => out.push((n + 1, body.to_string())),
        }
        open = more;
    }
    out
}

impl SavedOptionsFile {
    /// The file read as the makefile bsd.options.mk includes (line 341):
    /// a value is the whitespace-separated words after the operator, with
    /// or without spaces around it. `+=` appends to the variable; `=` and
    /// `:=` assign it afresh, as `OPTIONS_FILE_SET=X` resets the list; `?=`
    /// assigns only a variable no earlier line assigned. A line ending in
    /// `\` goes on over the next (`logical_lines`). Every other line make
    /// reads, a `!=` (a shell command) included, is one optique cannot
    /// model: it is skipped and recorded in `unmodeled`.
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
    /// takes nothing but the SET and UNSET lines. A line optique cannot
    /// model is recorded in `unmodeled` (`unmodeled_line`); the SET and
    /// UNSET lines around it are read all the same.
    fn read(&mut self, text: &str, lists_only: bool) {
        let file = if lists_only {
            "options.local"
        } else {
            "options"
        };
        for (n, line) in logical_lines(text) {
            // A `#` starts a comment, at the head of a line or after a value;
            // `\#` is a literal `#`, as make reads it.
            let line = strip_comment(&line);
            let line = line.trim_matches(MAKE_BLANKS);
            if line.is_empty() {
                continue;
            }
            let parsed = assignment(line).filter(|(name, _, _)| plain_name(name));
            if let Some(what) = unmodeled_line(line, parsed) {
                self.unmodeled.push(Unmodeled {
                    file,
                    line: n,
                    what,
                });
            }
            let Some((name, op, value)) = parsed else {
                continue;
            };
            // A `?=` to a list in options.local assigns or not by what the
            // options file holds: inert while the file assigns the list,
            // it resets it once a write leaves the list out (`render`
            // writes no UNSET line when every option is on). No write of
            // the file can say what make then reads. In the options file
            // itself it assigns or not by what the port's Makefile set
            // before (devel/subversion's `OPTIONS_FILE_SET?=""`), which
            // the file does not show.
            let list = matches!(name, "OPTIONS_FILE_SET" | "OPTIONS_FILE_UNSET");
            if list && op == Op::IfUnset {
                self.unmodeled.push(Unmodeled {
                    file,
                    line: n,
                    what: if lists_only {
                        format!("assigns {name} with ?=, which a write of the options file can turn on or off")
                    } else {
                        format!("assigns {name} with ?=, which assigns nothing where the port's Makefile set it first")
                    },
                });
            }
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
                // make runs the command; optique cannot (`unmodeled`).
                Op::Shell => continue,
            };
            // make's blanks, not Unicode's: a no-break space a copy and
            // paste brings in is part of the word, as is a vertical tab.
            let words = value
                .split(MAKE_BLANKS)
                .filter(|w| !w.is_empty())
                .map(str::to_string);
            match name {
                "_OPTIONS_READ" => {
                    let v = value.trim_matches(MAKE_BLANKS);
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
                        lists.push(if set {
                            &mut self.local_set
                        } else {
                            &mut self.local_unset
                        });
                        // A `?=` is recorded as unmodeled above, line and all.
                        if !append
                            && op != Op::IfUnset
                            && !self.local_resets.iter().any(|r| r == name)
                        {
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

    /// Whether options.local turns the option off whatever the options
    /// file says: one of its UNSET entries matches it as a pattern.
    /// bsd.options.mk:352-363 applies every OPTIONS_FILE_SET entry, the
    /// file's and options.local's, before every OPTIONS_FILE_UNSET one, so
    /// only an UNSET there outranks the file; a SET there is a default the
    /// file's UNSET line overrides, and a SET pattern turns nothing on.
    pub fn local_decides(&self, opt: &str) -> bool {
        unsets(&self.local_unset, opt)
    }

    /// Why optique cannot manage the options file, with the file (`options`
    /// or `options.local`) that makes it so; None when it can. Either
    /// options.local assigns OPTIONS_FILE_SET or OPTIONS_FILE_UNSET afresh,
    /// so make drops what the file put in that list (bsd.options.mk:341-363)
    /// and no write of the file can say what the port ends up with; or a
    /// line of either is one optique cannot model (`unmodeled`), an
    /// options.local `?=` to a list among them. Its lines
    /// would be lost to a write, and what make makes of them is not known.
    pub fn unmanaged(&self) -> Option<(&'static str, String)> {
        if !self.local_resets.is_empty() {
            return Some((
                "options.local",
                format!(
                    "options.local resets {}: the options file's lines are not read; edit options.local",
                    self.local_resets.join(" and ")
                ),
            ));
        }
        self.unmodeled.first().map(|u| {
            (
                u.file,
                format!(
                    "{} line {} {}: optique leaves the options file alone; edit it by hand",
                    u.file, u.line, u.what
                ),
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
        self.complete
            .iter()
            .map(String::as_str)
            .filter(|o| !self.decides(o))
            .collect()
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
    /// An UNSET entry settles every option it matches as a make pattern, as
    /// it turns them all off; a SET entry settles only the option it names,
    /// as it turns on only that one. make also takes what a SET glob matches
    /// out of NEW_OPTIONS (`OPTIONS_FILE_SET+=A*` settles A2 though it turns
    /// no A2 on), an upstream defect that changes nothing built, so it is
    /// not followed (docs/reference/ports-framework.md, "Likely defects").
    pub fn decides(&self, opt: &str) -> bool {
        self.set.contains(opt) || mentions(&self.unset, opt)
    }
}

/// Whether `name` is a variable name as written: nothing make would expand,
/// nor anything that makes the line a directive or a dependency line.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

/// What a line does that optique cannot model, None for a line it reads
/// (`parsed`: the line's assignment, when it is a plain one). Inert in
/// make or not, each is outside what optique reads and writes back:
/// - a `!=` runs a shell command at every parse, a `make -V` included;
/// - a directive (`.if`, `.include`, `.sinclude`, `.undef`, `.for`, ...)
///   or a `$` expansion makes the file's meaning depend on what optique
///   does not evaluate;
/// - an assignment to any variable but the SET and UNSET lists and the
///   bookkeeping `make config` writes (`_OPTIONS_READ`,
///   `_FILE_COMPLETE_OPTIONS_LIST`) is seen by the rest of the parse:
///   `OPTIONS_SET`, `${OPTIONS_NAME}_SET` and `OPTIONS_OVERRIDE` there come
///   too late to count (bsd.options.mk:300-334, before the include at
///   341), while the `*_FORCE` lists and `WITH`/`WITHOUT` take effect
///   (368-409);
/// - any other line (a dependency line, `include`) is no assignment.
fn unmodeled_line(line: &str, parsed: Option<(&str, Op, &str)>) -> Option<String> {
    let Some((name, op, value)) = parsed else {
        return Some(if let Some(rest) = line.strip_prefix('.') {
            let word: String = rest
                .trim_start()
                .chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect();
            format!("is a make directive (.{word})")
        } else if line.contains('$') {
            "expands a variable ($)".to_string()
        } else {
            "is not a variable assignment".to_string()
        });
    };
    if op == Op::Shell {
        Some("assigns with != (a shell command)".to_string())
    } else if value.contains('$') {
        Some("expands a variable ($)".to_string())
    } else if matches!(
        name,
        "OPTIONS_FILE_SET" | "OPTIONS_FILE_UNSET" | "_OPTIONS_READ" | "_FILE_COMPLETE_OPTIONS_LIST"
    ) {
        None
    } else {
        Some(format!("assigns {name}, which optique does not model"))
    }
}

/// Render an options file as `make config` does: every option of `complete`
/// gets a SET or UNSET line, in `complete` order. Two departures keep synth
/// from aborting on it: its cached options check slices bytes 17-18 and
/// 19-20 of every line unchecked, so a line shorter than 20 bytes without
/// `+=` at 17-18 crashes it (docs/reference/synth.md, "Likely defects"),
/// as `make config`'s own `_OPTIONS_READ=wmx-8` and `# Options for wmx-8`
/// do for x11-wm/wmx. `_OPTIONS_READ`, which nothing reads, is left out,
/// and the pkgname comment carries a tail that makes it 20 bytes or more
/// whatever the name.
pub fn render(pkgname: &str, complete: &[String], enabled: &BTreeSet<String>) -> String {
    let mut out = String::new();
    out.push_str("# This file is auto-generated by 'make config'.\n");
    out.push_str(&format!("# Options for {pkgname}, by optique\n"));
    out.push_str(&format!(
        "_FILE_COMPLETE_OPTIONS_LIST={}\n",
        complete.join(" ")
    ));
    for opt in complete {
        if enabled.contains(opt) {
            out.push_str(&format!("OPTIONS_FILE_SET+={opt}\n"));
        } else {
            out.push_str(&format!("OPTIONS_FILE_UNSET+={opt}\n"));
        }
    }
    out
}

/// `line` up to its comment: an unescaped `#` ends it, and `\#` stands for
/// a `#` of the value, as make reads a makefile line.
fn strip_comment(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'#') => {
                out.push('#');
                chars.next();
            }
            // An escaped backslash is kept whole, and escapes nothing
            // after it: `E\\#F` is `E\\` and a comment.
            '\\' if chars.peek() == Some(&'\\') => {
                out.push_str("\\\\");
                chars.next();
            }
            '#' => break,
            c => out.push(c),
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
        let v: Vec<_> = ["A", "B", "C", "D", "E"]
            .iter()
            .map(|o| f.value(o))
            .collect();
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
        let words = |s: &[&str]| s.iter().map(ToString::to_string).collect::<BTreeSet<_>>();
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
    /// An unescaped `#` starts a comment; `\#` is a `#` of the value, as
    /// make reads `J+=k\#l m # note` as `k#l m`.
    #[test]
    fn an_escaped_hash_is_part_of_the_value() {
        let f = SavedOptionsFile::parse(
            "OPTIONS_FILE_SET+=A\\#B C # note D\nOPTIONS_FILE_UNSET+=E#F\n",
        );
        let set: Vec<&str> = f.set.iter().map(String::as_str).collect();
        let unset: Vec<&str> = f.unset.iter().map(String::as_str).collect();
        assert_eq!((set, unset), (vec!["A#B", "C"], vec!["E"]));
        // An escaped backslash escapes no `#` after it: make reads `E\\`.
        let f = SavedOptionsFile::parse("OPTIONS_FILE_SET+=E\\\\#F G\n");
        assert_eq!(f.set, ["E\\\\".to_string()].into());
    }

    /// Words split on make's blanks: a no-break space or a vertical tab is
    /// part of the word, which matches no option.
    #[test]
    fn words_split_on_make_blanks_only() {
        let f = SavedOptionsFile::parse("OPTIONS_FILE_UNSET+=X\u{a0}Y H\x0bI\tJ\n");
        let unset: Vec<&str> = f.unset.iter().map(String::as_str).collect();
        assert_eq!(unset, ["H\x0bI", "J", "X\u{a0}Y"]);
    }

    #[test]
    fn colon_question_and_shell_assignments() {
        let f = SavedOptionsFile::parse(
            "OPTIONS_FILE_SET+=A\nOPTIONS_FILE_SET:=B C\nOPTIONS_FILE_SET?=D\n\
             OPTIONS_FILE_UNSET ?= E\nOPTIONS_FILE_UNSET?=F\n\
             _FILE_COMPLETE_OPTIONS_LIST!=echo G\n",
        );
        let words = |s: &[&str]| s.iter().map(ToString::to_string).collect::<BTreeSet<_>>();
        assert_eq!(
            f.set,
            words(&["B", "C"]),
            "`:=` resets, `?=` after it assigns nothing"
        );
        assert_eq!(
            f.unset,
            words(&["E"]),
            "the first `?=` assigns, the second not"
        );
        assert!(f.complete.is_empty());
        // A list's `?=` in the file assigns or not by what the port's
        // Makefile set first: recorded, as is the `!=`.
        assert_eq!(
            places(&f),
            [
                ("options", 3),
                ("options", 4),
                ("options", 5),
                ("options", 6)
            ]
        );
        // options.local carries on: its `?=` meets a variable the file
        // assigned, and is recorded, as whether it does depends on the file.
        let l = f
            .with_local("OPTIONS_FILE_SET?=H\nOPTIONS_FILE_UNSET:=I\nOPTIONS_FILE_SET != echo J\n");
        assert_eq!(l.set, words(&["B", "C"]));
        assert_eq!(l.unset, words(&["I"]));
        assert_eq!(
            places(&l),
            [
                ("options", 3),
                ("options", 4),
                ("options", 5),
                ("options", 6),
                ("options.local", 1),
                ("options.local", 3)
            ]
        );
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
        let f =
            SavedOptionsFile::parse("OPTIONS_FILE_SET+=A B\n").with_local("OPTIONS_FILE_SET=C\n");
        let v: Vec<_> = ["A", "B", "C"].iter().map(|o| f.value(o)).collect();
        assert_eq!(v, [None, None, Some(true)]);
        assert_eq!(f.local_set, ["C".to_string()].into());
        let f = SavedOptionsFile::parse("OPTIONS_FILE_SET+=A\n")
            .with_local("OPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_SET+=B\n");
        let words = |s: &[&str]| s.iter().map(ToString::to_string).collect::<BTreeSet<_>>();
        assert_eq!((f.local_set, f.local_unset), (words(&["B"]), words(&["A"])));
    }

    /// An options.local that assigns a list afresh drops the file's lines
    /// for it: `=` and `:=` always; `+=` never.
    #[test]
    fn options_local_resets_are_recorded() {
        let file = SavedOptionsFile::parse("OPTIONS_FILE_SET+=A\n");
        let resets = |local: &str| file.clone().with_local(local).local_resets;
        assert_eq!(
            resets("OPTIONS_FILE_SET+=B\nOPTIONS_FILE_UNSET+=A\n"),
            Vec::<String>::new()
        );
        assert_eq!(resets("OPTIONS_FILE_SET=B\n"), ["OPTIONS_FILE_SET"]);
        assert_eq!(
            resets("OPTIONS_FILE_UNSET:=B\nOPTIONS_FILE_UNSET=C\n"),
            ["OPTIONS_FILE_UNSET"]
        );
        let f = file.with_local("OPTIONS_FILE_UNSET=C\nOPTIONS_FILE_SET=B\n");
        assert_eq!(
            f.unmanaged().map(|(_, why)| why).as_deref(),
            Some(
                "options.local resets OPTIONS_FILE_UNSET and OPTIONS_FILE_SET: \
                 the options file's lines are not read; edit options.local"
            )
        );
        assert_eq!(
            SavedOptionsFile::parse("OPTIONS_FILE_SET=A\n").unmanaged(),
            None,
            "the file's own `=`"
        );
    }

    /// A `?=` to a list in options.local assigns only while the options
    /// file leaves that list unassigned, and `render` writes no UNSET line
    /// when every option is on: the same line is inert before a write and
    /// turns an option off after it. So it leaves the file unmanaged
    /// whatever the file holds now, the line named; it is still read as
    /// make reads it.
    #[test]
    fn an_options_local_ifunset_leaves_the_file_unmanaged() {
        let why = "options.local line 2 assigns OPTIONS_FILE_UNSET with ?=, which a write of the \
                   options file can turn on or off: optique leaves the options file alone; edit it by hand";
        let local = "OPTIONS_FILE_SET+=B\nOPTIONS_FILE_UNSET?=C\n";
        let inert = SavedOptionsFile::parse("OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=C\n")
            .with_local(local);
        let all_on = SavedOptionsFile::parse(&render(
            "port-1.0",
            &["A".into(), "C".into()],
            &["A".to_string(), "C".to_string()].into(),
        ))
        .with_local(local);
        for (f, c) in [(&inert, Some(false)), (&all_on, Some(false))] {
            assert_eq!(f.unmanaged(), Some(("options.local", why.to_string())));
            assert_eq!(f.value("C"), c);
            assert!(f.local_resets.is_empty());
        }
        assert_eq!(all_on.local_unset, ["C".to_string()].into(), "it assigned");
        assert!(inert.local_unset.is_empty(), "it did not");
        assert_eq!(
            SavedOptionsFile::parse("OPTIONS_FILE_SET?=A\n")
                .unmanaged()
                .map(|(file, _)| file),
            Some("options"),
            "the file's own `?=`: devel/subversion sets the list first"
        );
    }

    /// Where each unmodeled line is.
    fn places(f: &SavedOptionsFile) -> Vec<(&'static str, usize)> {
        f.unmodeled.iter().map(|u| (u.file, u.line)).collect()
    }

    /// bsd.options.mk:341-363 includes both files as makefiles: optique
    /// reads their SET and UNSET lines and the bookkeeping `make config`
    /// writes, and records every other line make reads, each with what it
    /// does. A directive, a `$` and a `!=` are recorded whatever they
    /// assign; comments and blank lines are not lines at all.
    #[test]
    fn every_line_optique_cannot_model_is_recorded() {
        let f = SavedOptionsFile::parse(
            "# comment\n\n_OPTIONS_READ=port-1.0\n_FILE_COMPLETE_OPTIONS_LIST=A B\n\
             OPTIONS_FILE_SET+=A\nOPTIONS_SET=B\n.if exists(/x)\nOPTIONS_FILE_UNSET+=B\n.  endif\n\
             OPTIONS_FILE_SET+=${X}\nFOO!=echo 1\n${OPTIONS_NAME}_SET=A\ninclude /x\n",
        )
        .with_local(".sinclude \"/y\"\nWITH=A\nOPTIONS_FILE_UNSET+=B\n_OPTIONS_READ=x\n");
        let got: Vec<(&str, usize, &str)> = f
            .unmodeled
            .iter()
            .map(|u| (u.file, u.line, u.what.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                (
                    "options",
                    6,
                    "assigns OPTIONS_SET, which optique does not model"
                ),
                ("options", 7, "is a make directive (.if)"),
                ("options", 9, "is a make directive (.endif)"),
                ("options", 10, "expands a variable ($)"),
                ("options", 11, "assigns with != (a shell command)"),
                ("options", 12, "expands a variable ($)"),
                ("options", 13, "is not a variable assignment"),
                ("options.local", 1, "is a make directive (.sinclude)"),
                (
                    "options.local",
                    2,
                    "assigns WITH, which optique does not model"
                ),
            ]
        );
        // The SET and UNSET lines around them are still read.
        assert!(f.set.contains("A") && f.unset.contains("B"));
        assert_eq!(
            f.unmanaged(),
            Some((
                "options",
                "options line 6 assigns OPTIONS_SET, which optique does not model: optique leaves \
                 the options file alone; edit it by hand"
                    .to_string()
            ))
        );
        // A file make config wrote, and an options.local of SET and UNSET
        // lines, are managed.
        let clean = SavedOptionsFile::parse(&render("port-1.0", &["A".into()], &BTreeSet::new()))
            .with_local("OPTIONS_FILE_SET+=A\n_FILE_COMPLETE_OPTIONS_LIST=A\n");
        assert_eq!(clean.unmanaged(), None);
    }

    /// bsd.options.mk:352-363: every OPTIONS_FILE_SET entry, options.local's
    /// included, goes in before any OPTIONS_FILE_UNSET one: options.local
    /// decides only what its UNSET entries match, patterns included; its
    /// SET of B loses to the file's UNSET.
    #[test]
    fn options_local_decides_only_what_it_unsets() {
        let f = SavedOptionsFile::parse("OPTIONS_FILE_SET+=A\nOPTIONS_FILE_UNSET+=B C\n")
            .with_local("OPTIONS_FILE_UNSET+=A*\nOPTIONS_FILE_SET+=B C*\n");
        let decided: Vec<bool> = ["A", "A2", "B", "C"]
            .iter()
            .map(|o| f.local_decides(o))
            .collect();
        assert_eq!(decided, [true, true, false, false]);
        assert_eq!(
            f.value("B"),
            Some(false),
            "the file's UNSET beats options.local's SET"
        );
    }

    /// bsd.options.mk:354-363: an UNSET entry takes out every option it
    /// matches as a pattern; a SET entry adds itself word for word, and
    /// settles only that word (make's NEW_OPTIONS defect not followed).
    #[test]
    fn file_entries_are_make_patterns() {
        let f = SavedOptionsFile::parse("OPTIONS_FILE_UNSET+=A*\nOPTIONS_FILE_SET+=[BC] D\n");
        let v: Vec<_> = ["A", "A2", "B", "D", "E"]
            .iter()
            .map(|o| f.value(o))
            .collect();
        assert_eq!(v, [Some(false), Some(false), None, Some(true), None]);
        let settled: Vec<_> = ["A2", "B", "C", "E"].iter().map(|o| f.decides(o)).collect();
        assert_eq!(settled, [true, false, false, false]);
    }

    /// make joins a line ending in an odd number of backslashes with the
    /// next before parsing it, as this host's make does: a value goes on
    /// over a tab-indented line, a comment ending in `\` swallows the next
    /// line, an empty line ends the run, and `\\` ends the line. A `!=` is
    /// placed on the line it starts on.
    #[test]
    fn backslash_newline_goes_on_over_the_next_line() {
        let f = SavedOptionsFile::parse(
            "OPTIONS_FILE_SET+=A \\\n\tB\nOPTIONS_FILE_UNSET+=C # x \\\nOPTIONS_FILE_UNSET+=D\n\
             OPTIONS_FILE_SET+=E\\\n   F\\\n\nOPTIONS_FILE_SET+=G\nOPTIONS_FILE_UNSET+=H\\\\\n\
             OPTIONS_FILE_UNSET+=I\n_FILE_COMPLETE_OPTIONS_LIST!=echo \\\n J\n",
        );
        let words = |s: &[&str]| s.iter().map(ToString::to_string).collect::<BTreeSet<_>>();
        assert_eq!(f.set, words(&["A", "B", "E", "F", "G"]));
        assert_eq!(f.unset, words(&["C", "H\\\\", "I"]));
        assert_eq!(places(&f), [("options", 11)]);
        let lines = logical_lines("A \\\n\tB\nC\\\\\\\nD\n");
        assert_eq!(lines, [(1, "A  B".to_string()), (3, "C\\\\ D".to_string())]);
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
        assert!(f.complete.is_empty() && f.set.is_empty() && f.unset.is_empty());
    }

    #[test]
    fn render_round_trip() {
        let complete = vec!["SPEECHD".to_string(), "FLITE".to_string()];
        let enabled: BTreeSet<String> = ["SPEECHD".to_string()].into();
        let text = render("qt5-speech-5.12.1_1", &complete, &enabled);
        let f = SavedOptionsFile::parse(&text);
        assert_eq!(f.complete, complete);
        assert_eq!(f.set, enabled);
        assert!(f.unset.contains("FLITE"));
    }

    /// No line optique writes trips synth's cached options check, which
    /// reads bytes 17-18 and 19-20 of each line without checking its length:
    /// every line is 20 bytes or more, or has `+=` at 17-18. x11-wm/wmx's
    /// `wmx-8` is the case `make config` itself gets wrong; a one-letter
    /// name, an empty list and a one-letter option are the extremes.
    #[test]
    fn nothing_written_trips_synths_line_probe() {
        let one: BTreeSet<String> = ["X".to_string()].into();
        for (pkgname, complete) in [
            ("wmx-8", vec!["ALT_L", "XTERM", "DOCS"]),
            ("a", vec![]),
            ("b-1", vec!["X", "Y"]),
        ] {
            let complete: Vec<String> = complete.into_iter().map(String::from).collect();
            let text = render(pkgname, &complete, &one);
            assert!(
                text.ends_with('\n') && !text.contains("\n\n"),
                "no blank line: {text:?}"
            );
            for line in text.lines() {
                let b = line.as_bytes();
                assert!(
                    b.len() >= 20 || b.get(16..18) == Some(b"+="),
                    "{pkgname}: {line:?}"
                );
            }
        }
    }
}
