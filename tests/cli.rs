//! Hermetic binary-level tests: no ports tree, no make, no network.

use std::process::Command;

fn optique() -> Command {
    Command::new(env!("CARGO_BIN_EXE_optique"))
}

#[test]
fn help_mentions_every_command_and_global_flag() {
    let out = optique().arg("--help").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for needle in [
        "tui", "scan", "sync", "clean", "--dry-run", "--verbose", "--file", "--no-cache",
        "--quiet", "--color",
    ] {
        assert!(text.contains(needle), "--help must mention {needle}\n{text}");
    }
}

#[test]
fn subcommand_help_shows_specific_flags() {
    let out = optique().args(["clean", "--help"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("--redundant"));
    assert!(text.contains("--unused"));
    assert!(text.contains("--dry-run"));
}

/// `tui --drive` is the headless debugging driver. Its help text is all that
/// can be checked hermetically: driving it needs a ports tree to scan first.
#[test]
fn tui_help_documents_the_drive_flag() {
    let out = optique().args(["tui", "--help"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("--drive"), "tui --help must mention --drive\n{text}");
}

#[test]
fn clean_unused_requires_a_list() {
    // No origins, no -f: there is nothing to compute a closure from, and the
    // check must fire before any ports tree is touched.
    let out = optique().args(["clean", "--unused"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("package list"), "{err}");
}

#[test]
fn clean_origins_require_unused() {
    // Plain clean walks the whole options dir; a list would be silently
    // ignored, so it is an error instead.
    let out = optique().args(["clean", "ports-mgmt/pkg"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--unused"), "{err}");
}

#[test]
fn scan_help_advertises_json() {
    let out = optique().args(["scan", "--help"]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("--json"), "scan --help must mention --json\n{text}");
    // The marker column is the one thing --color affects today.
    assert!(text.contains("--color"), "scan --help must mention --color\n{text}");
}

#[test]
fn color_is_rejected_unless_it_names_a_mode() {
    let out = optique().args(["--color", "sometimes", "scan", "www/nginx"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("sometimes"), "{err}");
}

#[test]
fn quiet_is_a_global_flag() {
    // -Q must be accepted before the subcommand and must not swallow errors:
    // the malformed origin is still reported on stderr.
    let out = optique().args(["-Q", "scan", "not-an-origin"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not-an-origin"), "{err}");
}

#[test]
fn malformed_origin_is_a_clean_error() {
    let out = optique().args(["scan", "not-an-origin"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not-an-origin"), "{err}");
}

#[test]
fn no_arguments_is_a_helpful_error() {
    let out = optique().output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no ports given"), "{err}");
}

#[test]
fn missing_list_file_is_reported_with_its_path() {
    let out = optique().args(["scan", "-f", "/nonexistent/pkglist"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("/nonexistent/pkglist"), "{err}");
}

#[test]
fn man_page_is_valid_mdoc() {
    // mandoc exits nonzero for mere STYLE/WARNING output too (e.g. "referenced
    // manual not found" for poudriere(8) on hosts without it installed), so the
    // assertion is on the absence of ERROR/UNSUPP diagnostics, not on the exit
    // status. Skipped where mandoc is not available.
    let man = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("optique.8");
    assert!(man.is_file(), "{} must exist", man.display());

    let out = match Command::new("mandoc").args(["-T", "lint"]).arg(&man).output() {
        Ok(out) => out,
        Err(_) => return, // no mandoc on this host
    };
    let text =
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    for bad in ["ERROR:", "UNSUPP:", "SYSERR:"] {
        assert!(!text.contains(bad), "mandoc reported {bad} on optique.8:\n{text}");
    }
}

#[test]
fn man_page_documents_every_subcommand_and_flag() {
    // Cheap guard against the man page drifting from the CLI surface.
    let man = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("optique.8");
    let text = std::fs::read_to_string(&man).unwrap();
    for needle in [
        "tui", "scan", "sync", "clean", "-json", "-redundant", "-unused", "-no-cache",
        "-dry-run", "-options-dir", "-color", "NO_COLOR", "EXIT STATUS",
        "POUDRIERE INTEGRATION",
    ] {
        assert!(text.contains(needle), "optique.8 must document {needle}");
    }
}

#[test]
fn tui_refuses_without_terminal_before_scanning() {
    // Origins are given, stdin/stdout are pipes -> must fail fast with the
    // terminal message, not attempt a scan.
    let out = optique().args(["tui", "ports-mgmt/pkg"]).output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("real terminal"), "{err}");
}
