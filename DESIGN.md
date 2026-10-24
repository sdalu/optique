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
at once (`src/cmd/mod.rs`, `default_jobs`).

The framework itself is the only faithful evaluator of a port's
options/`IMPLIES`/make.conf — it is what `poudriere options -C` runs for one
port at a time, and reimplementing bmake's variable expansion would drift
from it the moment a port leans on something clever. What optique changes is
the walk: `poudriere options -C` visits ports serially with a dialog each,
while independent make processes parallelise freely. A single query costs
more than `config-conditional` does, since it reads far more of the port,
so the cold gain is the parallelism alone: on 876 ports, 27 seconds against
poudriere's 49 to check, 0.2 warm, 174.6 with one job
([docs/benchmark.md](docs/benchmark.md)).

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
(`src/config/poudriere.rs`). Between the two, after the layers, come the hook
plugins' `hooks/plugins/*/make.conf`, which poudriere appends there too.
Being part of the staged text, the environment is
covered by the make.conf hash, so it keys its own cache generation. On the
same closure it removed both leaks, in agreement with poudriere's graph,
and changed nothing else but dependency spec paths and the defaults
`PACKAGE_BUILDING` is meant to change.

The host's release leaks the same way: bsd.port.mk takes `OSREL` from
`uname -r` and `OSVERSION` from the host's param.h, so on a 15.1 host an
11.2 jail's `security/ca_root_nss` lost the `ETCSYMLINK` it excludes on
15. Given `-j`, the prelude therefore also sets `_OSRELEASE`, `OSREL` and
`OSVERSION` from the jail's poudriere.d metadata (its param.h, else
derived from its version), plus poudriere's own cross-build lines when
its arch is not the host's; being in the staged text, they key the cache.

poudriere gathers no dependency of an IGNOREd port, blacklisted ones
included; optique stops its walk only at blacklisted ones, and keeps
walking through an IGNOREd port, on purpose, because its IGNORE can
depend on the options the user is configuring.

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
`math/primecount`, affecting only a CMake flag).

synth is wrapped the same way. It reads each port in a scan chroot whose
`/usr/local` is an empty tmpfs or nullfs, under a make.conf it writes
itself (`create_make_conf` in its `replicant.adb`) followed by the
profile's file, never the host's `/etc/make.conf`. So the staged make.conf
starts with synth's own lines that bear on a Makefile (`SYNTHPROFILE`,
`PACKAGE_BUILDING` without `_FLAVORS`, `BATCH`,
`USE_PACKAGE_DEPENDS_ONLY`) and ends with `LOCALBASE=/nonexistent`.
optique 0.3.0 gave synth the host's view instead, on the belief that synth
builds against what its host has installed: on a host with php82 and
`DEFAULT_VERSIONS+= php=8.4`, `Mk/Uses/php.mk` read the installed
`php.conf` and resolved every `php84-*` extension to `lang/php82`, and
an httpd probe picked the `-zts` extension directory, where synth itself
resolved `lang/php84`. synth's `varcache.conf` (host values like
`OSVERSION` and `ARCH` it precomputes) is not copied: make on the same
host computes them alike.

A plain `/var/db/ports` build does keep the host's view: it builds on the
host it configures, so what that host has installed is part of the truth.
That truth changes with every `pkg install` or `pkg delete`, so for a
local build the size and mtime of pkg's `local.sqlite` (under
`PKG_DBDIR`, else `/var/db/pkg`) are folded into the make.conf hash: a
package change starts a new cache generation, and a missing database adds
nothing.

## Upstream's behaviour, not its bugs

optique follows what the ports framework, poudriere and synth mean to do,
read from their source; docs/reference/ describes each upstream, and
docs/conformance.md records how optique stands against each behaviour.
Each reference page ends with "Likely defects": behaviour that reads as a
bug rather than a rule. optique does not conform to a likely defect until
it is shown not to be one; it does the sane thing and says where it
differs ("differs on purpose" in the conformance table).

The exception is a defect that changes behaviour radically: what gets
built (the options a port ends up with, its dependencies, which origin is
built), or the tool failing on something optique wrote. Those are marked
**radical** in
docs/reference/, and optique conforms to them: it predicts what the
upstream tool will actually do, tells the user that this follows an
upstream defect, and the conformance table records it as "conforms to an
upstream defect". So optique reads a synth.ini key with a tab in it as
synth does (ignored, the default used) and names the line; it follows
poudriere's collapsed MOVED chains; and it never writes an options-file
line that synth's fixed-offset probe would overrun, so no file optique
writes makes synth abort. A tool failing on its own input is not
optique's business: a file optique did not write and synth aborts on, or
a synth.ini header with no `]` that stops synth loading its
configuration, is left to the user; optique neither reports nor rejects
it, and reads past it sanely.

Rejected: copying every defect so that optique and the tool always agree,
which makes optique wrong in the same way and hides the fault; and
ignoring every defect, which leaves optique promising a result the tool
will not produce where the difference is the one the user would notice.

## Which build is configured: poudriere.d, unless `-l` says otherwise

The existence of `/usr/local/etc/poudriere.d` selects a poudriere build
(`$POUDRIERE_ETC/poudriere.d` when that is set, poudriere.conf moving with
it, as poudriere's own `-e` does; a value that is no directory is an
error rather than a quiet fallback to `/usr/local/etc`); without it, or under `-l`, a local build is configured: the tree and
options directory are `PORTSDIR` and `PORT_DBDIR` as `make -V` reports
them (so `/etc/make.conf` counts as it does for `make config`), the host
environment applies, and the make.conf (`__MAKE_CONF`, else
`/etc/make.conf`) is hashed like a layer so the cache keys its content,
but read in place: a staged copy lost the directory its relative
`.include`s resolve from, and every query failed. What a make.conf
includes is keyed per answer, from what make read for it ("Persistent
cache, keyed on what actually changed"). Detection stays the default because it is right
for most hosts; `-l` exists because a host running poudriere may still
build some ports itself, and only the flag can say so
(`src/config/mod.rs`, `resolve`).

`-p` names a poudriere tree and nothing else: without poudriere.d it is
an error, as `-j` and `-z` are, and a tree poudriere does not know is an
error rather than `/usr/ports`, which answered a typo from the wrong tree.
A literal tree is `-P dir`, the counterpart of `-o`, valid for every
build. Every query is given `PORTSDIR` explicitly, canonicalised as
make's own autodetection would, because an inherited one silently
replaced the framework of the tree being queried.

The same holds for the rest of the environment: a query runs with
`PATH`, `TMPDIR` and pkg's `PKG_DBDIR` alone, plus the `USER` and `HOME`
of whoever runs the build (`src/query/makerunner.rs`, `Identity`), so an
exported `OPTIONS_SET=UTF8PROC` no longer turns `sysutils/tmux`'s
4-port closure into a 22-port one behind a cache key that does not
see it, nor reaches a poudriere query no builder would answer that
way. What a query needs is passed on the command line or staged in
the make.conf. Rejected: removing the variables known to matter
(`MAKEFLAGS`, `OVERLAYS`, ...) one by one, which missed every make
variable a user may export; keying the cache on the environment,
which still let a poudriere or synth query see what its builder
never does.

`USER` and `HOME` are the builder's, not optique's: `root` and `/root`
for a poudriere or synth query, as `jail_start` exports them and synth's
scan environment passes them, the invoking user's for a local build,
whose `USER` then keys its cache generation. `Mk/bsd.ccache.mk` reads
`USER`: under `WITH_CCACHE_BUILD` with no `CCACHE_DIR` it keeps ccache for
root alone, so a poudriere query that saw optique's `HOME` and no `USER`
dropped `devel/ccache` from every port's dependencies, where the builder
adds it. Rejected: passing optique's own `HOME` (and `USER`) to every
query, which answers for whoever runs optique rather than for the build.

Rejected: `-p` taking a path when there is no poudriere.d, which makes
an argument's meaning depend on a directory that installing poudriere
creates; guessing a local build from `-o /var/db/ports`; reading
`PORTSDIR` from the environment alone, which misses `/etc/make.conf`.

## Persistent cache, keyed on what actually changed

Results are cached under `~/.cache/optique/`, keyed on the ports tree's
canonical path and git HEAD, the make.conf hash, and the content of the
options file the port reads and of the `.local` bsd.options.mk reads after
it, or, when `OPTIONS_FILE` lies outside the options dir, of the file it
names and its `.local` (`src/cache/mod.rs`), with a schema number bumped whenever the parsed
shape changes. The path is part of the key because two trees at one
commit are not one tree: poudriere's `-p default` and `-p dev`, or a
clone, hold their own uncommitted edits, and an entry holds absolute
paths into its tree (`pkg-descr`, `pkg-help`); keyed on the commit alone,
they shared a generation and carried each other's entries over. `tree_key` reads
`.git` directly — the loose ref, packed-refs, or a `gitdir` file — rather
than spawning `git`. When `.git` is missing or anything it does not
recognise, the key is the mtimes of `Mk/bsd.port.mk`, `MOVED` and
`UPDATING`, kept whole: most updates leave the framework alone but touch
`MOVED` or `UPDATING`, and an mtime cut to a prefix would let updates hours
apart share a generation.

An answer is also keyed on the files make read for it that no other key
covers (`PortInfo::outside_files`): `.MAKE.MAKEFILES`, less the trees, the
options dir, the query's own make.conf and `/dev`. That leaves what
make.conf `.include`s, for every port or only for some (`.if
${.CURDIR:M*/www/*}`), and `/usr/share/mk`. Each entry records their
hashes, and opening a generation hashes each distinct file once (about ten
on a closure of 93 ports) and leaves out the entries whose files changed,
saying how many. An edit to an included file used to be served stale until
`--clear-cache`. What make never read is not seen: a file a `.sinclude`
skipped because it did not exist, then created. Rejected: asking the
make.conf probe outside any port (`makerunner::options_place`) for its
`.MAKE.MAKEFILES` and folding those files into the make.conf hash, which
misses an include made for some ports only and discards every entry for
any change; and evaluating make.conf to follow its includes by hand, which
is make's job.

A cache generation corresponds to one tree commit, so whatever the tree
alone decides is cached with the query: whether a port ships a `pkg-help`
file is settled at query time, since it cannot change behind a cached entry,
while the file's contents are read at every open.

### A tree update keeps the entries it leaves alone

A new commit used to mean a new, empty generation, so every `git pull`
paid a full cold scan. synth judges its flavor index port by port, by the
mtimes of each port's directory; optique now does the same per entry, from
what make itself read. The wrapper dumps `.MAKE.MAKEFILES`, and each entry
keeps the main tree's files in it (`PortInfo::makefiles`). A generation
file starts with a header naming its tree's path, commit, overlays and
make.conf hash. When a new generation of a git tree opens, the latest
earlier one made for the same tree path, make.conf and overlays is diffed
against it (`git diff-tree`),
and its entries that the changed paths leave alone start the new file
(`cache::Invalidation`):

- An entry goes when make read a changed file for it.
- It also goes when anything changed in a directory it owns, its own and,
  for a slave, its master's (the directories whose `Makefile` it read).
  bsd.port.mk probes there with `exists()` for what it never includes:
  `pkg-help`, `Makefile.local`, `Makefile.${ARCH}`, `files/`.
- A change to a category's own files, other than its SUBDIR `Makefile`,
  takes the entries owning a port there (`../Makefile.inc` is probed).
- Everything goes for a new `.mk` anywhere under `Mk/` and for a
  top-level file not known to be unread (PORTSDIR's `Makefile.inc` is
  probed). `bsd.local.mk` is `.sinclude`d; and a port whose `USES` named
  `X` before `Mk/Uses/X.mk` existed was only probed for it, bsd.port.mk
  setting an `ERROR` the query answers with, so no entry's makefiles name
  the file its arrival changes. A modified `.mk` takes only its readers.
  `MOVED`, `UPDATING`, `UIDs`, `GIDs` and the like, `Tools/`, `Templates/`
  and `Keywords/` reach make only through a target, never while it
  parses.
- A script under `Mk/` that make runs while parsing counts as a change to
  the makefile running it (`parse_time_readers`). `Uses/cargo.mk` runs
  three awk scripts in a `${:!...!}` through `${_CARGO_AWK}`, and the
  crates they list add `BUILD_DEPENDS` and `LIB_DEPENDS` (cmake, libgit2,
  openssl, ...), so a change to one of them takes every cargo port. A
  script is found run while parsing when a command make runs then (`VAR!=
  cmd`, `${:!cmd!}`, `${VAR:sh}`) names it, the variables it uses expanded
  from `Mk/`'s assignments; recipe lines are skipped. On the tree those are
  cargo's three scripts and no other of the 38 non-makefiles in `Mk/`, so
  the fifteen script changes of three months (`do-fetch.sh`, `qa.sh`, ...,
  all run by targets) take nothing. Matching a script's name anywhere in
  `Mk/` was rejected: `bsd.port.mk` names all fifteen, which would have
  made each a cold scan.

Another port's file is matched by name, not by directory: every python
port reads `lang/python312/Makefile.version`, and that port's `pkg-plist`
changing must not take them all. On the 841-port closure, a day of commits
(182, 1,024 paths) left 838 entries and a week (999 commits, 6,078 paths,
five `Mk/` files) left 797, the scan taking 1.1 and 2.0 seconds instead of
29, with the JSON byte-identical to a `--no-cache` scan of the new tree
both times. A month that touched `bsd.port.mk` carries nothing over, as it
should.

git is run only when a generation is new, and its failure (git missing,
the old commit gone, a shallow clone) only means a cold scan, as before.
The tree is usually root's and optique not, which git refuses as dubious
ownership, so `safe.directory=*` is given: `diff-tree` reads no work tree
and runs no hook, and optique runs the tree's Makefiles anyway. A tree
without git and an overlay that changed carry nothing over.

Known gap: a port that a query found missing through an `exists()`
probe, and that a later commit adds, is not noticed. Two probes in `Mk/`
do this: `Uses/zig.mk` IGNOREs a port whose `lang/zig<N>` does not exist,
and `Uses/electron.mk` one whose `devel/electron<N>/Makefile.version`
does not, so such an IGNORE is carried over until the port, its master or
the `.mk` changes (ten ports use either, and their version moves with the
port that provides it). The opposite cases are caught: electron's file
once present is included, so it is matched by name, and a `lang/zig<N>`
that vanishes fails as the dependency on it. `Uses/python.mk` probes only
under `_PORTS_ENV_CHECK`, which no query sets, so it always includes the
version file. A port's own Makefile that runs make on another port while
it parses (`VAR!= ${MAKE} -C ../other -V X`: www/caddy-custom takes
www/caddy's version, mail/claws-mail-plugins its options from each
plugin, which it also probes with `exists()`) reads that port in a child
make `.MAKE.MAKEFILES` never names; such an entry, some seventeen ports,
is carried over no tree change (`cache::runs_make_at_parse`). A `!=`
running anything but make reads no other port. Uncommitted edits to the
tree were already invisible to the key.

Rejected: synth's directory mtimes, which miss an edit made in place and
cannot tell what a port reads outside its directory; fingerprinting every
file make read at each lookup, which stats some twenty files per port on
every warm run, and on NFS pays a round trip for each; and a directory per
dependency rather than a file, which re-queried every python port for any
change to python's port.

### An option edit to make.conf keeps the entries it cannot reach

Any change to the make.conf hash used to start an empty generation: on an
879-port closure, a 31-second cold scan for `OPTIONS_UNSET+=DOCS`, the
very edit optique's make.conf verdicts lead to. A generation's header now
holds the staged make.conf text and a signature of everything else folded
into its hash (`Settings::conf_rest`: a local build's overlay lines, the
installed packages, the invoking user, the framework's host values). When
a new generation opens under the same tree, commit and signature, and the
latest earlier one's text differs only in option policy, it starts with
the entries that change cannot reach (`cache::makeconf`):

- Option policy is a comment, a blank, or a top-level assignment to
  `OPTIONS_SET`, `OPTIONS_UNSET`, their `_FORCE` forms, `WITH`, `WITHOUT`
  or a per-port `<OPTIONS_NAME>_SET` and the like, each changed line
  counted as a multiset of lines with their conditional depth. A changed
  variable must be assigned under no conditional anywhere, read by no
  line, and the make.conf must include no file: the value make gives it
  outside any port is then the one every port sees. A changed line
  expanding anything (`$`) is no option edit: the probe outside any port
  cannot expand it as a port would (`${PORTNAME}` reads empty there).
- make is asked those values under the old text and the new (two `make
  -V`s outside any port, `makerunner::make_conf_values`). The words in one
  value and not the other reach a port when one matches any of its
  options as a pattern (as make matches an UNSET entry, and so a SET one
  naming the option); a per-port knob reaches the port of that
  OPTIONS_NAME (`PortOptions::name`, kept apart from the options-dir key).
- Every entry dumps the raw value of the global variables, so a kept one
  is patched to the new value, and one whose dump showed another value
  than make.conf's got it elsewhere, and goes.

`OPTIONS_UNSET+=DOCS` then re-queried 165 ports in 7.7 seconds, `NLS`
instead 178 and `X11` 61 in 4.0, the JSON byte-identical to a
`--no-cache` scan each time. `--no-carry-over` starts every new generation
empty, carrying nothing over a tree update or such an edit. Rejected:
judging the edit from the text alone, which cannot see a value an
assignment with `=` erases from an earlier line; ignoring a per-port
knob's name, which a port's dump shows only under that name; and folding
every make.conf change through, which only an evaluation of make.conf for
each port could make sound.

## The framework's host values are probed once per run

Every query used to pay for bsd.port.mk working out the host again: about
ten forks (`uname` three times, `awk` over param.h, `pkg-static` twice,
`sysctl`, `nproc`, perl) before the port's own Makefile counted. bsd.port.mk
already spares its own sub-makes this: the values it lists in
`_EXPORTED_VARS` ("These won't change") go to them on `.MAKEFLAGS`. synth
does the same for its scan (`set_port_cache_variables`, from a fixed
list). So once per run, after the make.conf is settled, optique runs a probe
port that uses nothing (`FRAMEWORK_PROBE` in `src/query/wrapper.rs`) under
the queries' own tree, make.conf and environment, and gives every query
the non-empty `_EXPORTED_VARS` it reports on the command line, where they
win over make.conf just as on a sub-make's `.MAKEFLAGS`
(`framework_values` in `src/query/makerunner.rs`). The probe is a slave
port with `GNU_CONFIGURE`, so the two values bsd.port.mk probes only for
such ports are in the set. An IGNOREd probe gives nothing: its
`_PKG_CHECKED` would spare every query the pkg(8) version check that
IGNOREs it. A failed probe gives nothing either, and the queries probe for
themselves as before. On 841 ports a cold scan went from 42 to 36.7
seconds, with the JSON byte-identical on local, poudriere (an 11.2 jail
too) and synth builds. The probe costs one make, about 50 ms, on a warm run
too.

The values are folded into the make.conf hash. They are the host's
release, arch and CPU count, which no key held before: a local or synth
build on a host upgraded under an unchanged make.conf kept its generation
unless pkg's database happened to change too.

`--no-framework-probe` leaves the probe out: nothing is given or folded
into the hash, each query working the values out itself in a generation
of its own, the answers the same; it rules the probe out when one looks
wrong.

Rejected: synth's fixed list, which drifts from the framework where
`_EXPORTED_VARS` cannot; keeping the values on disk between runs, which
would need a key for the very host state they describe; probing at the
tree's top, as synth does, where bsd.port.subdir.mk answers and the pkg(8)
check never runs.

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
a line for every option, and make building with the same options from it as
from the file it would write: before bsd.port.mk drops the members of a
conditional group whose option is off, whose helpers ran with them,
`session::built_with`), so a pkgname-only change does not force a rewrite,
as with `make config-conditional` (`src/apply/mod.rs`). Identical is judged by
running both files through the framework's own layering, IMPLIES last,
never from the port's queried `effective`: in the TUI a refresh has queried
make with the staged edits already in place. A file missing a line is not
identical even when make reads it the same: such an option follows the
defaults instead of recording a choice, and `make config-conditional`
counts it as new, so it is warned about and completed. Quitting without applying
therefore always leaves the options dir as it was.

A port still in conflict is not written on a plain `y`: a file recording
options that violate their constraints makes the build's `check-config`
fail on it. The TUI asks instead for `Y`, writing it
anyway, or `s`, writing the rest and leaving the conflicting files as
they are on disk with their edits staged and kept in the draft. It does
not refuse outright as `decide` does: some conflicts come from make.conf,
which the interface cannot edit, and refusing would block every apply
until make.conf changed.

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

Reading once leaves a gap for a port with no cache entry yet: its file is
first read when its answer is stored, after make read it, and a file
rewritten in between (`make config`, another optique applying) would pair
the new bytes with the old answer. An answer is not stored when a file
its query read has a ctime at or after the query's start: a write or a
rename into place always moves it, which an mtime `cp -p` sets back does
not. Each entry goes out in one write(2), whole under `O_APPEND` though
two optiques share the generation, and a line cut short is ended before
the next is appended.

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

## An options file optique cannot model is left to make

optique reads an options file as the makefile it is but models only the
SET and UNSET lists and the bookkeeping `make config` writes
(`src/optionsfile.rs`). A file with any other line, in it or in its
`options.local` (another variable, `!=`, a directive, a `$` expansion, a
line that is no assignment), an `options.local` assigning a list afresh
or with `?=`, or an `OPTIONS_FILE` outside the options dir is unmanaged
(`session::unmanaged_file`, `session::moved`): never written or removed, by `clean` either,
its port shown with make's own `PORT_OPTIONS`, every option locked, and
no make.conf verdict, since the layers make dumps after reading the file
may hold its own assignments. Rejected: ignoring such a line with a
warning, as `!=` once was, which let a write drop it and judged the port
from a reading make does not share.

An `options.local` `?=` to a list assigns only while the options file
leaves that list unassigned, and a file optique writes with every option
on has no UNSET line: `OPTIONS_FILE_UNSET?=C` is inert until such a write,
then turns C off, so optique would report C on and write a file make
reads otherwise. So the `?=` leaves the file unmanaged whatever the file
holds now, its line named. Rejected: counting it as a reset only when it
assigns against the current file, which judged the write by a reading
the write itself changes.

Which file a port reads is where make reads it. An `OPTIONS_FILE` one
entry deep in the options dir, set by make.conf or by the port's own
Makefile, is an options file like any other: `<entry>/options` is that
entry's, `<entry>/<file>` is keyed by that path (`optfiles::key_file`), read
with its `.local`, and written, judged and owned like the others
(`makerunner::adopt_options_file`). Eight ports in the tree set it
themselves: devel/git, mail/postfix, net-p2p/transmission-components and
www/py-autobahn give each flavor but the default a file of its own
(`devel_git/lite-options`), mail/dovecot24 and multimedia/avidemux name
another entry. The key is kept relative, so that the cache holds one answer
whether the query read the options dir or the TUI's staging copy.
Rejected: leaving every port whose `OPTIONS_FILE` is not
`<OPTIONS_NAME>/options` unmanaged, on the belief that only make.conf moves
it, which locked those eight ports' flavors and blamed make.conf for it;
and naming every write's temporary file `.options.optique.tmp`, which two
files of one entry written in one pass shared, the second rename failing
after the first had put the wrong content in place.

`OPTIONS_FILE` is moved per port, a make.conf `.if ${FLAVOR:U} == nox`
moving it, outside the options dir, for one flavor. Such a flavor reads none of the shared file, so
it is left out of that file's readers everywhere (`session::moved`): it
owns nothing, takes no part in the write, in `-m` or `clean --redundant`
redundancy or the divergence warning, and shows its own `PORT_OPTIONS`,
its edits refused. The flavors still reading the file keep it managed.
Rejected: leaving the whole shared file unmanaged when any flavor moved
it, which the session judged only for the port and its owner and apply
for every candidate, so `scan` said unconfigured while `sync` and
`decide` wrote nothing, and which flavor the list named first decided
it.

The queries' own command line hides two make.conf settings from them:
`PORT_DBDIR`, which optique passes there and a build does not, and
`OPTIONS_FILE` for any entry no queried port reads. So once per run, after
the make.conf is staged, one make with no port and neither on its command
line (`makerunner::options_place`) asks what that make.conf alone sets:
outside bsd.port.mk nothing else sets either, and the framework probe
cannot answer, as it passes its own `PORT_DBDIR`. It reads no Makefile
but runs in `ports-mgmt/pkg`, which every port depends on, so that a
make.conf testing `.CURDIR` (`.if ${.CURDIR:M*/ports/*}`) answers as for a
build; run from `/`, such a setting went unseen. For a local build it is
given the environment's `PORT_DBDIR`, which a make.conf `?=` defers to in
the user's own make, as it did when that make named the options dir: the
probe's bare environment otherwise reported a mismatch that was none. A
builder's environment is not the user's, and is not given. A `PORT_DBDIR` other
than where the build reads options (the options dir on the host, the
jail's /var/db/ports, synth's /options) is an error: the whole build reads
an empty directory while optique edits files nobody reads. scan counts it
but still reports, the rest being valid; the other commands say it and go
on. A moved `OPTIONS_FILE` breaks the one assumption `clean` lives by,
that an entry's name is its port's: under per-flavor files
`editors_emacs-nox` is `editors/emacs@nox`'s file, not a vanished port's.
So `clean` then removes nothing as vanished or unused, and the same when
the probe failed; `--redundant` still judges managed live ports. Under
`--unused` and `--redundant`, the entry a queried port reads is also kept,
whatever its name says, and so is the entry a moved file lies deeper in:
that needs no make.conf value and covers a move made per port, which a
probe outside any port cannot see. Rejected: guessing the
names a moved `OPTIONS_FILE` yields (per-flavor suffixes and the like),
which works only for the spellings guessed and deletes a file make reads
for any other.

An `options.local` locks only what its UNSET entries match:
bsd.options.mk applies every SET entry, the file's and options.local's,
before any UNSET one, so a SET there is a default the file's UNSET
overrides, and locking it refused edits make would have honoured.

## Flavors sharing one options file: the default flavor owns it

Flavors of one origin can share an `OPTIONS_NAME` and so an options file, but
compute different option sets for it (`editors/emacs@nox` excludes options
from `@full`'s view; `devel/git@lite` would, but devel/git gives it a file
of its own, "An options file optique cannot model is left to make"). The file is owned by the default flavor, written and
judged from its view — exactly what `make config` on the plain origin
produces — and a divergent flavor's difference is reported as a warning,
not an error (`src/apply/mod.rs`, `src/session/mod.rs`).

Without the default flavor in the closure, one rule names the owner for
the session's state and for the write alike (`session::outranks`): the
flavor with the most options, the first met on ties. Flavors mostly
exclude options, so the widest view covers the others'
(`editors/emacs@pgtk`'s holds `@nox`'s), and an edit to an option only it
has reaches the shared staged set. Not always: multimedia/audacious-plugins
@gtk3 adds `AOSD`, which the default `@qt6` lacks. With the default flavor
as owner such an option is never written, so it reads at that flavor's own
outcome and an edit to it is refused (`Session::unstored`), rather than
accepted and dropped. Any other owner writes every flavor's options, so
the shared state holds them all, each at its flavor's value, and is built
again when a refresh changes the file's owner or readers
(`Session::rebase_new_owners`), the staged edits carried over as edits. Rejected: letting `apply` fall back to the first flavor it
meets while the session followed the widest, which judged the file from
one view and wrote it from another. A flavor whose
`OPTIONS_FILE` names another file reads none of the file and is neither
owner nor writer ("An options file optique cannot model is left to make").

Such an owner writes from its own view but drops nothing it does not see:
it cannot tell an option it lacks (the default flavor's) from one the port
removed, and dropping the first loses the user's choice for good, while
keeping the second is harmless (make ignores it, the default flavor's next
write drops it). So a file whose list is a superset of that flavor's is up
to date when everything each present flavor has is unchanged, and a write
takes the options of the other present flavors with their values and keeps
the rest of the file's list entries and lines as they were. Rejected:
querying the default flavor to tell the two apart, an extra make query per
such origin for a case the rule settles.

Removing the file is the exception: it decides every flavor reading it, and
a flavor can set its own defaults (`www/sogo@pgsql` adds `PGSQL`), so
`--minimal` removes it only when it repeats defaults + make.conf for every
flavor in the closure, and `clean --redundant` queries every flavor in
`FLAVORS`, one make query each, keeping the file when one fails (`src/cmd/clean.rs`,
`find_redundant`). Rejected: judging only the default flavor, which deleted a
file `@pgsql` still read.

Rejected: treating the divergence as a conflict that aborts planning, which
stopped every plan touching such an origin.

## make.conf decisions are judged by outcome, not by mention

A make.conf-decided option is one where the actual outcome, not mere
mention in a `_SET`/`_UNSET` list, settles the value: `nofile_effective`
applies defaults, then one `SET`/`UNSET` pass per layer in
`bsd.options.mk`'s own order, each scope's UNSET before the next scope's SET
so a per-port knob overrides the global one, closes the `IMPLIES` chain, and only
then asks whether the option ended up on (`src/session/mod.rs`). A group
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

## Overlays: the framework's lookup, in poudriere's order

`-O name` is read from `poudriere.d/ports/<name>/mnt`, as `poudriere bulk
-O` reads it (`--overlay-dir` is the literal counterpart, as `-P` is of
`-p`), and reaches make as `OVERLAYS+=` lines where poudriere writes them
in the jail's make.conf (`jail_start` in its `common.sh`): after
`USE_PACKAGE_DEPENDS` and `BATCH`, before `PACKAGE_BUILDING`, the jail's
system lines and the poudriere.d layers. The overlay list is then read
back from make (`-V OVERLAYS`), so a layer, `/etc/make.conf` or, for a
local build, the environment counts as for the build. A port is taken
from the first overlay holding `<origin>/Makefile`, else from the main
tree: the framework's rule for dependencies (`Mk/Scripts/do-depends.sh`)
and poudriere's (`_lookup_portdir`), so the scanner, clean's vanished
ports and MOVED's existence check all use it (`src/query/makerunner.rs`,
`locate`). MOVED is every tree's in order, first entry winning, its chains
collapsed in file order, as poudriere's `parse_MOVED.awk` does
(`src/moved.rs`). Only the listed origins are looked up, as poudriere
applies it to the list alone, each as an originspec (`origin@flavor`'s
own entry, else the bare origin's with the flavor re-attached), and only
where the build would: poudriere looks up an origin still in the tree
too (its existence-first defect, which changes what gets built, so
conformed to), a local build and synth one in no tree only
(`src/roots.rs`, `apply_moved`). A listed origin MOVED moves is flagged,
not followed: neither it nor its target is scanned, and `scan` counts it
as pending, since the list names a port that is gone and needs fixing;
`--follow-moved` scans the target in its place, as poudriere builds it,
and says the list still names the old origin (never under `-s`: synth
reads no MOVED, and would not build the target). MOVED is read on every
run, never cached, which the cache's carry-over relies on. Rejected:
following silently, as poudriere does and optique did, which configured
the right port but hid a stale list; and never following, which stopped
optique from predicting what poudriere builds. A listed `origin~sub` is
looked up whole, `~sub` included, as `_check_moved` is given it: no move
without a flavor, the bare origin's entry alone with one, which changes
which origin is built and so is conformed to. The message names the
existence-first defect only for a listed originspec buildable as it
stands (bare, `@all`, `@-`): a flavor-level entry is how a flavor is
retired. A followed origin's old name is no alias in the closure: a
`decide` plan naming it reaches the target (`src/decide.rs`), a dependency
on it stays an error. Rejected: an alias for every lookup, which led a
dependency on the old origin, never walked, to the target, and reported a
loop through it that poudriere never sees. A dependency on an origin
no tree holds is an error
on the depending port, naming what MOVED says of it, since poudriere's
`deps_sanity` fails the bulk on it and synth fails the port
(`src/query/scanner.rs`, `register`); the dependencies of an IGNOREd or
blacklisted port and test-only edges, which `deps_sanity` never looks at,
are not checked. Rejected: following MOVED for dependencies too, as
optique did, which configured the port a dependency had moved to and
passed a closure poudriere refuses to build. The cache keys on every tree: an overlay
on the count and newest mtime of its files, never on its git HEAD, since
it is a few ports edited in place before any commit (keying on HEAD served
a stale answer for a port edited in a git overlay).

A local build's environment `OVERLAYS` is made absolute and written into
its make.conf after `--overlay-dir` (the real make.conf included first,
still read in place, then `OVERLAYS:=` putting both in front of what it
left, so a make.conf assigning `OVERLAYS=` cannot drop them), and kept
out of every query's environment: left there, bmake's `+=` put it before
`--overlay-dir`, and a relative path was read from the port's
directory.

Rejected: `OVERLAYS=` on make's command line (it replaces the make.conf's
`+=` list, so a layer's overlay would vanish); requiring `Mk/` in an
overlay (neither poudriere nor the framework does, and an overlay of a
single port has none); `-O` taking a path (the argument against `-p` taking
one, above); following `poudriere options`, which has no overlays at all.

## Machine interface: one JSON object, all-or-nothing, replayable

Every subcommand can answer on stdout with one JSON object per invocation;
everything else — progress, banner, warnings — goes to stderr, so stdout is
always safe to pipe. `decide` applies a plan of ports and option values
under the same rules the TUI enforces, all-or-nothing: a plan that cannot be
honoured in full changes nothing, so no caller has to work out which half
landed. Exit status answers one question: is anything still left to do.

`tui --drive` speaks keystrokes to the real TUI and is a debugging tool,
not a second way to drive optique from a script.

## TUI keys: one meaning per key, vi's where it has one

`u` undoes the last change, whatever port it was on, and `U` takes the
selected port back to its saved state, from either pane: vi's own `u`/`U`
pair, the frequent action on the plain key. `v` shows the port's full
description and `V` toggles its panel above the options; `i` is the
highlighted option's, so on the port list it only says where it works.
Rejected for the description: `o` for overview; `D`, next to `d`, which
resets the port to its defaults; `e`, which reads as edit.

vi's `h`/`l` are gone, `l` from every place it moved right: `h` is the
port's pkg-help notes, in both panes, so the pair could never be whole.
Entering the option pane is `Enter`, `Tab` or `→`, and once there `Enter`
does nothing: `Space` alone toggles, so a key pressed once too often
cannot flip an option.

`Ctrl-C` always quits, from any popup, prompt or the filter, asking first
only when edits are staged; a second `Ctrl-C` at that prompt keeps them as
a draft, the way out that loses nothing. A Ctrl-letter is never read as
its plain letter: `Ctrl-F`/`Ctrl-B` page, the others are no key.

## TUI port list: the state, then the edit

An edit no longer hides a port's state: each row shows the state (`✗ ? !
≠ ≈ ✓`) and, in a column of its own, `*` for staged edits, and only the
state orders the list and makes a port a problem for `n`/`p`/`t`.
Rejected: "edited" as one more state ranked after conflict, which read an
edited unconfigured port as handled before anything was applied.
