# Closing a round on optique

## Gates

Run in order; a red gate is answered before anything else.

- [ ] `make check` -- man page lint, one release everywhere (Cargo.toml,
      cargo, Cargo.lock, the tags). Any `unchecked` line it prints is a
      reader this host could not ask.
- [ ] `make build`
- [ ] `make tests`
- [ ] `make tests-live` -- every round, wherever a real `/usr/ports` is
      at hand; it also drives the TUI, so a changed key or help line can
      break it. Where it cannot run, say it was skipped.
- [ ] A port test build (docs/port.md, "Test build") -- when the round
      touched a path the FreeBSD port installs from (`man/optique.8`, the
      binary's name). No test reaches the port's Makefile.

## Documents

- [ ] `README.md` and `docs/` -- does every flag, key, path and command they
      name still exist, and do they describe what this round changed? A
      changed screen or a ports tree update: `make screens`.
- [ ] `man/optique.8` -- does a changed flag, key, JSON key or exit status read
      the same here as in the README?
- [ ] `src/tui/ui/popups.rs` help overlay -- does a changed key or marker appear in
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
