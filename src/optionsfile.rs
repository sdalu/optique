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
}

impl SavedOptionsFile {
    /// The file read as the makefile bsd.options.mk includes (line 341):
    /// a value is the whitespace-separated words after `+=` or `=`, with
    /// or without spaces around either, `+=` appending to the variable and
    /// `=` assigning it afresh, as `OPTIONS_FILE_SET=X` resets the list.
    /// Lines that assign nothing optique reads are skipped.
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

    /// The options `options.local` text `local` names on a SET or UNSET
    /// line.
    pub fn local_names(local: &str) -> BTreeSet<String> {
        let mut f = SavedOptionsFile::default();
        f.read(local, true);
        f.set.into_iter().chain(f.unset).collect()
    }

    /// Read `text`'s assignments into `self`, in order; `lists_only`
    /// takes nothing but the SET and UNSET lines.
    fn read(&mut self, text: &str, lists_only: bool) {
        for line in text.lines() {
            // A `#` starts a comment, at the head of a line or after a value.
            let line = line.split('#').next().unwrap_or_default().trim();
            let Some((lhs, value)) = line.split_once('=') else { continue };
            let (name, append) = match lhs.strip_suffix('+') {
                Some(name) => (name.trim(), true),
                None => (lhs.trim(), false),
            };
            let words = value.split_whitespace().map(str::to_string);
            match name {
                "_OPTIONS_READ" if !lists_only => {
                    let v = value.trim();
                    if append && !self.options_read.is_empty() {
                        self.options_read = format!("{} {v}", self.options_read);
                    } else {
                        self.options_read = v.to_string();
                    }
                }
                "_FILE_COMPLETE_OPTIONS_LIST" if !lists_only => {
                    if !append {
                        self.complete.clear();
                    }
                    self.complete.extend(words);
                }
                "OPTIONS_FILE_SET" | "OPTIONS_FILE_UNSET" => {
                    let list = if name == "OPTIONS_FILE_SET" { &mut self.set } else { &mut self.unset };
                    if !append {
                        list.clear();
                    }
                    list.extend(words);
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
             OPTIONS_FILE_SET:=Z\n# OPTIONS_FILE_SET+=W\nOPTIONS_FILE_UNSET+=Y # not V\n",
        );
        let words = |s: &[&str]| s.iter().map(|o| o.to_string()).collect::<BTreeSet<_>>();
        assert_eq!(f.set, words(&["X", "Y"]));
        assert_eq!(f.unset, words(&["C", "D", "Y"]));
        assert_eq!(f.complete, ["A", "B", "X"]);
        let v: Vec<_> = ["A", "C", "X", "Y"].iter().map(|o| f.value(o)).collect();
        assert_eq!(v, [None, Some(false), Some(true), Some(false)]);
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
        let names: Vec<String> =
            SavedOptionsFile::local_names("OPTIONS_FILE_UNSET+=A\nOPTIONS_FILE_SET+=B\n").into_iter().collect();
        assert_eq!(names, ["A", "B"]);
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
