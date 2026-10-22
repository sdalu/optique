# Closing a round on optique

## Gates

Run in order; a red gate is answered before anything else.

- [ ] `make check` -- man page lint, one release everywhere, the port's
      crate list matches `Cargo.lock`. Any `unchecked` line it prints is a
      reader this host could not ask.
- [ ] `make build`
- [ ] `make tests`
- [ ] `make tests-live` -- every round, wherever a real `/usr/ports` is
      at hand; it also drives the TUI, so a changed key or help line can
      break it. Where it cannot run, say it was skipped.
- [ ] The host-side port validation (ports/README.md, "Test build") --
      when the round touched `ports/ports-mgmt/optique/` or a path the port
      installs from (`man/optique.8`, the binary's name). No test reaches the
      port's Makefile.

## Documents

- [ ] `README.md` and `docs/` -- does every flag, key, path and command they
      name still exist, and do they describe what this round changed? A
      changed screen or a ports tree update: `make screens`.
- [ ] `man/optique.8` -- does a changed flag, key, JSON key or exit status read
      the same here as in the README?
- [ ] `src/tui/ui.rs` help overlay -- does a changed key or marker appear in
      the `?` help as well as in the README and the man page?
- [ ] `DESIGN.md` -- did this round change a decision it records, or make
      one it should?
- [ ] `ports/README.md` and `ports/ports-mgmt/optique/` -- after a
      dependency change, are `CARGO_CRATES` and `distinfo` refreshed?
      (`make check-crates` fails until they are.)

## Release

- [ ] Does the number move? The one file that holds it: `Cargo.toml`.
- [ ] If it moves: bump `Cargo.toml`, `cargo build` so `Cargo.lock` follows,
      refresh the port (`ports/README.md`, "Refreshing for a release", steps
      1-3), commit, then `make tag` (it reads the number from `Cargo.toml`
      and pushes nothing).
- [ ] After pushing the tag: is `distinfo` finished from GitHub's tarball
      (steps 4-5) and committed?

Then capture: whatever this round learned goes to the document that owns
it, and anything this list failed to ask becomes a line here.
