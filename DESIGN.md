# Design

Why optique is shaped the way it is, for a developer who has read the
[README](README.md) and is about to change the code. One decision per
section, present tense, with the alternative it rejected.

## Query engine: one `make` per port, run in parallel

optique resolves each port by piping a wrapper makefile to
`make -f /dev/stdin optique-config` (`src/query/wrapper.rs`): the wrapper
includes the port's own `Makefile` and dumps options, groups, descriptions,
`IMPLIES`/`PREVENTS`, `BROKEN`/`IGNORE`, make.conf layers and
`_UNIFIED_DEPENDS` as parse-time `.info` lines, run under the layered
`__MAKE_CONF` (`src/query/makerunner.rs`). Up to `min(16, ncpu)` of these run
at once (`src/main.rs`, `default_jobs`).

The framework itself is the only faithful evaluator of a port's
options/`IMPLIES`/make.conf — it is what `poudriere options -C` runs for one
port at a time, and reimplementing bmake's variable expansion would drift
from it the moment a port leans on something clever. What optique changes is
the walk: `poudriere options -C` visits ports serially with a dialog each,
while independent make processes parallelise freely. A cold run over about a
thousand ports takes roughly 56 seconds, a warm one (cache hit) about 0.4.

## Persistent cache, keyed on what actually changed

Results are cached under `~/.cache/optique/`, keyed on the ports tree's git
HEAD, the make.conf hash, and the options-file content (`src/cache.rs`), with
a schema number bumped whenever the parsed shape changes. `tree_key` reads
`.git` directly — the loose ref, packed-refs, or a `gitdir` file — rather
than spawning `git`, falling back to directory mtimes when `.git` is anything
it does not recognise.

A cache generation corresponds to one tree commit, so whatever the tree
alone decides is cached with the query: whether a port ships a `pkg-help`
file is settled at query time, since it cannot change behind a cached entry,
while the file's contents are read at every open.

## Nothing on disk changes before apply

TUI edits are staged against a `PORT_DBDIR` seeded from the real options
directory (`src/staging.rs`), re-queried in the background after a 300 ms
debounce so a toggle's effect on the dependency closure shows up without the
UI blocking. Apply writes every file atomically — temp file, `fsync`,
rename — and skips a file that is semantically identical (same option list,
same enabled set), so a pkgname-only change does not force a rewrite, as with
`make config-conditional` (`src/apply.rs`). Quitting without applying
therefore always leaves the options dir as it was.

## Flavors sharing one options file: the default flavor owns it

Flavors of one origin can share an `OPTIONS_NAME` and so an options file, but
compute different option sets for it (`devel/git@lite` excludes options and
changes defaults from `devel/git`'s view; so does `editors/emacs@nox`
against `@full`). The file is owned by the default flavor, written and
judged from its view — exactly what `make config` on the plain origin
produces — and a divergent flavor's difference is reported as a warning,
not an error (`src/apply.rs`, `src/session.rs`).

Rejected: treating the divergence as a conflict that aborts planning, which
stopped every plan touching such an origin.

## make.conf decisions are judged by outcome, not by mention

A make.conf-decided option is one where the actual outcome, not mere
mention in a `_SET`/`_UNSET` list, settles the value: `nofile_effective`
applies defaults, then one `SET`/`UNSET` pass per layer in
`bsd.options.mk`'s own order, each scope's UNSET before the next scope's SET
so a per-port knob overrides the global one, closes the `IMPLIES` chain, and only
then asks whether the option ended up on (`src/session.rs`). A group
(`SINGLE`/`RADIO`) counts as decided only when a member of it really wins
that outcome, not merely because some layer names one.

The alternative — treating membership in `OPTIONS_SET`/`_UNSET` as the
decision — gets the order wrong: a later layer can undo an earlier one, so
an option named in a global `_SET` but overridden by a per-port `_UNSET` was
never actually decided by that mention. `bsd.options.mk` also force-enables
`DOCS`/`NLS`/`EXAMPLES`/`IPV6` by default whenever a port carries them,
independent of `OPTIONS_DEFAULT`; the parser folds these into the modeled
defaults so the no-file outcome matches the framework's.

## Dependency loops: Tarjan, iterative, only poudriere's own edges

Loop detection runs Tarjan's algorithm with an explicit stack rather than
recursion, because a real closure can be a thousand ports deep
(`src/deps.rs`). Each strongly connected component is reported once, with a
shortest closed walk through it found by breadth-first search, so the
report reads as `devel/a → devel/b → devel/a` rather than whichever cycle
the search happened to meet first.

Only the dependency edges poudriere actually builds count toward a loop;
`TEST_DEPENDS`-only edges are excluded. Counting every `_UNIFIED_DEPENDS`
edge is rejected: the tree's test dependencies tangle on a grand scale, and
`poudriere bulk` builds them only under `-t`. Counting every edge reported
components of 500, 57 and 17 ports on the `www/nginx` closure, and no gate
would ever have passed.

## Machine interface: one JSON object, all-or-nothing, replayable

Every subcommand can answer on stdout with one JSON object per invocation;
everything else — progress, banner, warnings — goes to stderr, so stdout is
always safe to pipe. `decide` applies a plan of ports and option values
under the same rules the TUI enforces, all-or-nothing: a plan that cannot be
honoured in full changes nothing, so no caller has to work out which half
landed. Exit status answers one question: is anything still left to do.

`tui --drive` speaks keystrokes to the real TUI and is a debugging tool,
not a second way to drive optique from a script.
