# optique

- What it does and how to drive it: `README.md`; every flag, key, JSON key
  and exit status: `man/optique.8`.
- Why it is shaped so, and what it rejected: `DESIGN.md`.
- Closing a round: `CHECKLIST.md`. `make` alone lists the targets.

Gate: `make tests` (preflight before it: `make check`)

Traps:

- A change to what the wrapper dumps or the parser keeps needs `SCHEMA`
  bumped in `src/cache.rs`; the cache is keyed on the tree and make.conf,
  not on optique, so old entries are otherwise served as if current.
- The tree is not rustfmt-formatted (`cargo fmt --check` differs in
  hundreds of places); do not run `cargo fmt` over it, format only what
  you write.
- There is no terminal here for the TUI; drive it with `optique tui
  --drive` (README, "Debugging the TUI").
