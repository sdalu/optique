# Closing a round on optique

## Gates

Run in order; a red gate is answered before anything else.

- [ ] `make check` -- man page lint, one release everywhere (Cargo.toml,
      cargo, Cargo.lock, the tags), the sources as rustfmt writes them.
      Any `unchecked` line it prints is a reader this host could not ask.
- [ ] `make build`
- [ ] `make tests`
- [ ] `make tests-live` -- every round, wherever a real `/usr/ports` is
      at hand; it also drives the TUI, so a changed key or help line can
      break it, and checks optique against make on real ports (the
      `oracle_*` tests), so a change to what is staged, written or
      judged identical can. Where it cannot run, say it was skipped.
- [ ] A port test build (docs/port.md, "Test build") -- when the round
      touched a path the FreeBSD port installs from (`man/optique.8`, the
      binary's name). No test reaches the port's Makefile.

## Before committing a fix

- [ ] Every user of what the fix changes -- the function, the set, the
      rule -- found by grep and judged: changed too, or why not. The
      commit message lists them. (bc77c0e changed how "identical" is
      judged and missed the baseline built from the same set: 1951abe.)
- [ ] The fix's own assumptions checked against the cases the docs name:
      a non-default flavor owning the file, a flavor make.conf moves, a
      port with no file, `-m`. (2d224e9 assumed the owner was always the
      default flavor.)
- [ ] A regression test for the trigger, cycled: it fails with the fix
      reverted and passes with it. Plus one for the nearest neighbour the
      fix could break, not only the reported case.
- [ ] Every gate above, `make tests-live` included, before the commit:
      chain the commit on them (`&&`) rather than reading their output
      after. Ask any session running a benchmark on this host first.

## Before pushing

- [ ] A fresh reviewer, given only the diff and the commit messages,
      reads `git log -p origin/main..` for regressions and sibling sites
      the fixes left; its findings are settled, and a commit settling
      them is reviewed in turn, before the push.
- [ ] Batches small enough that the review reads every diff in full.

## Documents

- [ ] `README.md` and `docs/` -- does every flag, key, path and command they
      name still exist, and do they describe what this round changed? A
      changed screen or a ports tree update: `make screens`.
- [ ] `man/optique.8` -- does a changed flag, key, JSON key or exit status read
      the same here as in the README?
- [ ] `src/tui/ui/help.rs` help overlay -- does a changed key or marker appear in
      the `?` help as well as in the README and the man page?
- [ ] `DESIGN.md` -- did this round change a decision it records, or make
      one it should?

## Release

- [ ] Does the number move? The one file that holds it: `Cargo.toml`.
- [ ] If it moves: bump `Cargo.toml`, `cargo build` so `Cargo.lock` follows,
      commit, then `make tag` (it reads the number from `Cargo.toml` and
      pushes nothing).
- [ ] Before `make tag`: it tags HEAD, so is HEAD still the commit the
      gates passed on? Another session may have committed on top since;
      `git log --oneline -3` says, and `git tag -a -m 'optique <release>'
      v<release> <commit>` tags an earlier one.
- [ ] After pushing the tag: update the FreeBSD port in the ports tree
      (docs/port.md, "Updating the port for a release"), then
      `make check-port` says `up to date`.

Then capture: whatever this round learned goes to the document that owns
it, and anything this list failed to ask becomes a line here.
