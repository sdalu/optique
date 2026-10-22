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
them. `NO_PACKAGE_BUILDING`, which drops two of them, is read from the
poudriere.conf files in poudriere's own order (`include_poudriere_confs`):
plain assignment lines only, since sourcing them as shell would run
whatever they hold, so a value set by shell logic is missed. Probes that
spell `/usr/local` literally still see the host (one in the tree,
`math/primecount`, affecting only a CMake flag). synth keeps the host's
view on purpose: it builds on the host it configures, so what that host has
installed is part of the truth, as it is for a plain `/var/db/ports`.
That truth changes with every `pkg install` or `pkg delete`, so for these
host builds the size and mtime of pkg's `local.sqlite` (under `PKG_DBDIR`,
else `/var/db/pkg`) are folded into the make.conf hash: a package change
starts a new cache generation, and a missing database adds nothing.

## Which build is configured: poudriere.d, unless `-l` says otherwise

The existence of `/usr/local/etc/poudriere.d` selects a poudriere build;
without it, or under `-l`, a local build is configured: the tree and
options directory are `PORTSDIR` and `PORT_DBDIR` as `make -V` reports
them (so `/etc/make.conf` counts as it does for `make config`), the host
environment applies, and the make.conf (`__MAKE_CONF`, else
`/etc/make.conf`) is hashed like a layer so the cache keys its content,
but read in place: a staged copy lost the directory its relative
`.include`s resolve from, and every query failed. Included files are not
hashed, in any build; `--clear-cache` is the documented answer, since
following includes means evaluating make.conf the way make does. Detection stays the default because it is right
for most hosts; `-l` exists because a host running poudriere may still
build some ports itself, and only the flag can say so (`src/config.rs`,
`resolve`).

`-p` names a poudriere tree and nothing else: without poudriere.d it is
an error, as `-j` and `-z` are, and a tree poudriere does not know is an
error rather than `/usr/ports`, which answered a typo from the wrong tree.
A literal tree is `-P dir`, the counterpart of `-o`, valid for every
build. Every query is given `PORTSDIR` explicitly, canonicalised as
make's own autodetection would, because an inherited one silently
replaced the framework of the tree being queried.

Rejected: `-p` taking a path when there is no poudriere.d, which makes
an argument's meaning depend on a directory that installing poudriere
creates; guessing a local build from `-o /var/db/ports`; reading
`PORTSDIR` from the environment alone, which misses `/etc/make.conf`.

## Persistent cache, keyed on what actually changed

Results are cached under `~/.cache/optique/`, keyed on the ports tree's git
HEAD, the make.conf hash, and the options-file content (`src/cache.rs`), with
a schema number bumped whenever the parsed shape changes. `tree_key` reads
`.git` directly — the loose ref, packed-refs, or a `gitdir` file — rather
than spawning `git`. When `.git` is missing or anything it does not
recognise, the key is the mtimes of `Mk/bsd.port.mk`, `MOVED` and
`UPDATING`, kept whole: most updates leave the framework alone but touch
`MOVED` or `UPDATING`, and an mtime cut to a prefix would let updates hours
apart share a generation.

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

## An unreadable options file is an error, never a missing one

An options file that is there but cannot be read (permissions, an NFS
fault) was taken for a missing one, and so was its port for an
unconfigured one: `sync` or an apply then replaced the saved choices with
defaults, since replacing a file needs only the directory's permission.
make does not save it: bsd.options.mk includes the file once it exists, and
make reports `Cannot open`, carries on as if there were no file, prints a
complete dump and exits 1, which the query used to accept. Now
(`src/query/makerunner.rs`) a `Cannot open` for a file in the options dir
fails the port with the file and the reason; the store hashes such a file
as `UNREADABLE`, never `ABSENT`, so no cache entry made while it was missing
answers for it (`src/optfiles.rs`); `clean` leaves a file it cannot read in
place (`src/clean.rs`); and a failed query counts in scan's exit gate,
since neither the port nor what it depends on was checked. A file that is
not UTF-8 is not unreadable: make reads it, so its choices are parsed
around the stray bytes rather than dropped.

Rejected: a status of its own in the session, write-protected everywhere,
which would have needed every writer guarded; with the port failing its
query it never reaches one. Also rejected, aborting the whole run on such a
file, which a single bad permission would turn into no report at all.

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

The closure itself is `_UNIFIED_DEPENDS`, whatever the tree puts in it.
Since ports commit e7679b71cf89 (2026-09-25) that leaves `TEST_DEPENDS` out
("It pollutes all the convenience -recursive targets including poudriere
options"), so test-only ports are no longer configured, and the
`test_only` marking only matters on older trees. Dumping `TEST_DEPENDS`
explicitly to keep them is rejected: it would ask for options of ports an
ordinary build never installs (chromium, through py-great-table's test
suite, was the framework's own motivation), against both the framework and
`poudriere options`.

## The action comes first

`optique scan -z ws -f list`: the action, then its options, as `poudriere
bulk -j …` and `pkg install -y` read. With no action the options open the
TUI (`optique -z ws www/nginx`). An option before an action is a usage
error that prints the command reordered (`src/cli.rs`,
`check_action_first`, run after the `-s` rewrite so it knows which words
are option values).

Options used to be accepted on both sides. clap checks `conflicts_with`
and `requires` per command level before global options are merged, so a
pair straddling the action slipped through (`-l scan -s` ran as synth,
`-p x scan -P dir` mixed two trees) while `-s scan --repo X` was refused;
and `-s scan` read the action as a synth profile. Accepting both sides and
re-checking every pair by hand (`Cli::check_globals`, kept as the one list
of constraints) treated the symptom; one side removes the class.

Rejected: options before the action only, the git style, which reads
worse for long option lists and would break `scan -f list`, the form every
document used; and accepting both sides with a warning, which keeps two
places to look for one setting.

## Machine interface: one JSON object, all-or-nothing, replayable

Every subcommand can answer on stdout with one JSON object per invocation;
everything else — progress, banner, warnings — goes to stderr, so stdout is
always safe to pipe. `decide` applies a plan of ports and option values
under the same rules the TUI enforces, all-or-nothing: a plan that cannot be
honoured in full changes nothing, so no caller has to work out which half
landed. Exit status answers one question: is anything still left to do.

`tui --drive` speaks keystrokes to the real TUI and is a debugging tool,
not a second way to drive optique from a script.
