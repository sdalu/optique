# optique

- What it does: `README.md`; how to drive it, page by page: `docs/`; every
  flag, key, JSON key and exit status: `man/optique.8`.
- Why it is shaped so, and what it rejected: `DESIGN.md`.
- Closing a round: `CHECKLIST.md`. `make` alone lists the targets.

Gate: `make tests` (preflight before it: `make check`)

Upstream defects: do not conform to the "Likely defects" listed at the
end of each `docs/reference/` page, except those marked **radical**
(DESIGN.md, "Upstream's behaviour, not its bugs").

Traps:

- A change to what the wrapper dumps or the parser keeps needs `SCHEMA`
  bumped in `src/cache/mod.rs`; the cache is keyed on the tree and make.conf,
  not on optique, so old entries are otherwise served as if current.
- The tree is not rustfmt-formatted (`cargo fmt --check` differs in
  hundreds of places); do not run `cargo fmt` over it, format only what
  you write.
- There is no terminal here for the TUI; drive it with `optique tui
  --drive` (`docs/tui.md`, "Driving the TUI headless").
