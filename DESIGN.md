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

## Queries see a jail, not the host

poudriere reads a port inside a jail whose `/usr/local` is empty, but
optique runs `make` on the host, where the Makefiles' `exists()` probes
find whatever the host has installed. On a real poudriere host, 68 of a
470-port closure answered differently: `www/apache24` IGNOREd LDAP because
the host's apr-util was built without it, and `Mk/Uses/iconv.mk` gave 56
ports a libiconv dependency that poudriere's own gathered graph does not
have. Other probes in `Mk/` pick perl, openssl, ncurses, mysql,
pgsql and ldap versions the same way.

So under a poudriere.d the staged make.conf starts with what poudriere
writes at the top of every jail's make.conf (`PACKAGE_BUILDING` and the
three others, copied from `setup_makeconf` in its `common.sh`, which skip
more probes and change a few option defaults) and ends with
`LOCALBASE=/nonexistent`, after the layers so none can undo it
(`src/config.rs`). Being part of the staged text, the environment is
covered by the make.conf hash, so it keys its own cache generation. On the
same closure it removed both leaks, in agreement with poudriere's graph,
and changed nothing else but dependency spec paths and the defaults
`PACKAGE_BUILDING` is meant to change.

Rejected: re-querying only ports that come out IGNORE/BROKEN, which misses
leaks into dependencies and defaults; running `make` inside poudriere's
reference jail, which needs root and mounts and only exists during a build;
hiding `/usr/local` with a preload library, fragile for a whole `make` tree.
Copying four lines of poudriere's prelude can drift if poudriere changes
them, and `NO_PACKAGE_BUILDING` in poudriere.conf is not read. Probes that
spell `/usr/local` literally still see the host (one in the tree,
`math/primecount`, affecting only a CMake flag). synth keeps the host's
view: it builds in a clean chroot, but its dependency scan appears to run
`make` on the host, so a jail's view is not known to be its own.

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
UI blocking. The staging copy is never rewritten while a re-scan is
outstanding: its cache entries pair the bytes a lookup saw with what make
computed, so a file changed under it would file one option value's result
under the other's bytes; due ports wait and go together once it is done.
Apply writes every file atomically — temp file, `fsync`,
rename — and skips a file that is semantically identical (same option list,
same enabled set), so a pkgname-only change does not force a rewrite, as with
`make config-conditional` (`src/apply.rs`). Quitting without applying
therefore always leaves the options dir as it was.

## Options files are read once

Every options file of the closure has several readers: the scan checks each
cache entry against the file's hash, the session parses the saved choices,
the TUI's staging dir is seeded with a copy, and planning an apply or a
sync compares each file with what would be written. They all read through
one store per options dir (`src/optfiles.rs`) that reads each file — or
finds it absent — once, so a startup costs one pass over the directory,
cold or warm. On an NFS-mounted poudriere.d every read is a round trip:
with 1,502 files, three serial passes before the TUI opened took over ten
minutes, and each apply added two more.

The store keeps what it read, so it is the view the session judged: staging
gets those bytes exactly (the background re-queries share the scan's cache,
which matches entries on the file's hash), and apply compares with them, so
a file edited outside optique while the TUI is open is neither seen nor
overwritten unless its port is edited in the TUI. After an apply the store
forgets the files it wrote or removed, and only those are read again. The
staging dir itself changes under its reader, so the background re-queries
read it through a fresh store per scan.

Rejected: seeding staging alone from the session's reads, which left the
scan's pass, the largest, as it was; and copying the files in parallel,
which still read everything twice.

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
