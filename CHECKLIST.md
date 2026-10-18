# Closing a round on optique

## Gates

Run in order; a red gate is answered before anything else.

- [ ] `make check` -- man page lint, one release everywhere, the port's
      crate list matches `Cargo.lock`. Any `unchecked` line it prints is a
      reader this host could not ask.
- [ ] `make build`
- [ ] `make tests`
- [ ] `make tests-live` -- on a FreeBSD host with a real `/usr/ports`,
      when the round touched querying, the cache, sync/clean/decide or
      their JSON; otherwise say it was skipped.

## Documents

- [ ] `README.md` -- does every flag, key, path and command it names still
      exist, and does it describe what this round changed?
- [ ] `man/optique.8` -- does a changed flag, key, JSON key or exit status read
      the same here as in the README?
- [ ] `src/tui/ui.rs` help overlay -- does a changed key or marker appear in
      the `?` help as well as in the README and the man page?
- [ ] `DESIGN.md` -- did this round change a decision it records, or make
      one it should?
- [ ] `ports/README.md` and `ports/sysutils/optique/` -- after a
      dependency change, are `CARGO_CRATES` and `distinfo` refreshed?
      (`make check-crates` fails until they are.)

## Release

- [ ] Does the number move? The one file that holds it: `Cargo.toml`.
- [ ] If it moves: bump `Cargo.toml`, `cargo build` so `Cargo.lock` follows,
      bump the port's `DISTVERSION`, commit, then `make tag` (it reads the
      number from `Cargo.toml` and pushes nothing).

Then capture: whatever this round learned goes to the document that owns
it, and anything this list failed to ask becomes a line here.
