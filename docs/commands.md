# Commands

optique has five subcommands: `tui`, `scan`, `sync`, `decide` and `clean`.
All five start from the same closure scan — the ports named on the command
line or in a pkglist, resolved with their full dependency tree — and differ
only in what they do with it: `tui` edits it interactively, `scan` reports
its status, `sync` and `decide` write options files headlessly, and `clean`
garbage-collects the options directory. `tui` is the default: give it port
origins with no subcommand and it opens the interface (see [tui.md](tui.md)).

## Choosing the ports

Ports are named as positional `category/name[@flavor]` arguments, read from
one or more `-f`/`--file` pkglists (one origin per line, read as poudriere
reads it: a line is a comment, skipped whole, only when its first word
starts with `#`, and anything past the first word on any other line,
comment or not, is dropped with it — the poudriere pkglist format), or
both at once; `-f` is repeatable. A stray leading or trailing `/` is
stripped from an origin, in a pkglist line or on the command line alike,
and a `~subpackage` is dropped, as poudriere splits it off before the
flavor: `devel/bar@py311~lib` roots `devel/bar@py311`, every subpackage
coming from the one port build and its one options file. `MOVED` still
reads it whole (below).
As poudriere reads them, `origin@all` names every flavor of the port (its
`FLAVORS`, queried first; the bare origin for a port without any) and
`origin@-` its default flavor, the bare origin, whatever the build.

`MOVED` is read for each listed originspec first, before `@all` expands,
as poudriere's own listing reads it: the entry of `origin@flavor` as
written, else the bare origin's, its target taking the listed flavor
unless it names one (one naming another flavor is an error, as it stops
poudriere). A listed originspec naming a subpackage is looked up whole,
`~sub` included, as poudriere's `check_moved` is given it, and `MOVED`
names no subpackage: `cat/old~sub` is never moved, and `cat/old@fl~sub`
only by the bare origin's entry, never by `cat/old@fl`'s. Under poudriere
every listed origin is looked up, one still in the tree included
(`math/mumps`, moved to `math/mumps-edf`, an upstream defect the message
names for a bare origin, `@all` or `@-`; an entry for one flavor, as
`audio/kid3@qt5` moved to `audio/kid3@qt6`, retires that flavor and reads
as a plain move); a local build and synth take an origin in the tree as it
is. A listed origin `MOVED` moves is not followed by default: neither it
nor its target is scanned, and it is reported as pending, the list needing a fix —

```
moved: listed cat/old moved to cat/new (renamed): fix the list, or --follow-moved to follow it
```

— which holds `scan`'s gate (exit 1, a `moved` entry in its JSON) and
stops `clean --unused` from pruning. `--follow-moved` scans and configures
the target instead, as poudriere builds it (`origin@all` and, under
`FLAVOR_DEFAULT_ALL`, a bare `origin` then query the FLAVORS of whatever
it last moved to), with a note that the list still names the old origin;
the gate does not count it. A `decide` plan naming the old origin
configures the target, but a dependency on the old origin stays an error
and never leads to the target, so it closes no loop through it. Under
`-s` it is never followed, synth reading no `MOVED`. A listed origin
`MOVED` removes, or whose entries cycle, is an error. A dependency never
follows `MOVED`: one on a moved or removed origin is an error on the port
that depends on it, `MOVED` read for its note as poudriere's `deps_sanity`
reads it, so `cat/old~sub` gets none (see
[how-it-works.md](how-it-works.md#querying-a-port)).

Across arguments and files, duplicates are dropped and first-seen order is
kept. With neither, every subcommand but `clean` and `decide` (which
then takes the plan's own ports) refuses — "no port origins given (arguments or -f pkglist)"
 once a subcommand is named, or the equivalent message
for the bare default-TUI invocation — and plain
`clean` (without `--unused`) needs no list at all — see [`clean`](#clean).

Under `-s`/`--synth` (see [configuration.md](configuration.md#synth)), an
empty list is not an error: `tui`, `scan`, `sync` and `clean --unused` fall
back to the packages currently installed on the host, matching synth's own
"build what's installed": one root per installed package (`pkg query -a
%o`, with the flavor from that package's `flavor` annotation, so flavors
installed side by side are each a root). `--repo NAME` (repeatable)
narrows that implicit list to packages installed from the named pkg
repositories, and is rejected when any ports *are* given, and with
`decide` and plain `clean`, since it only means something for the
implicit list. A list given is still read poudriere's way; synth's own
list syntax is stricter (only its flavor index's keys, verbatim), and a
list it refuses is the user's to fix, not optique's to report.

## `tui` (the default)

```sh
optique -z workstation www/nginx mail/dovecot
optique tui -z workstation -f pkglist
```

Opens the full-screen interface over the closure; see [tui.md](tui.md) for
the screen, the keys and the status markers. It needs a real terminal —
both standard input and standard output must be ttys — and refuses to start
otherwise, unless `--drive` is given (the headless
driver is documented in [tui.md](tui.md#driving-the-tui-headless)).
`--dry-run`/`-n` has no effect here: a note is printed instead, since the
apply dialog already previews every change before writing
.

## `scan`

```sh
optique scan -z workstation -f pkglist
optique scan -z workstation --json --options -f pkglist
```

Non-interactive status report over the closure, meant as a build gate: each
port is `ok`, `unconfigured` (no saved options file), `stale` (the option
list changed since the file was written) or `conflict` (the options make
computes from the saved ones, `*_FORCE` included, violate a group rule or
`PREVENTS`, which `make check-config` checks for an `OPTIONS_SLAVE` option
too; a `PREVENTS` word is a pattern, as make matches it, and what an option
`IMPLIES` goes in whether the port lists it or not). A flagged port also lists what
`make.conf` leaves `undecided`, or is marked `[mc-covered ≈]` when `make.conf`
settles every option by itself. An unconfigured or stale `mc-covered` port
does not count against the exit gate (a conflict still does, and so does,
under `-s`, a file synth would refuse); blacklisted ports never count. `--json` prints one
object on stdout instead of the table, and `--options` adds each port's full
per-option detail — see [machine-interface.md](machine-interface.md) for
both shapes.

## `sync`

```sh
optique sync -z workstation --dry-run -f pkglist
optique sync -z workstation -f pkglist
```

Headless refresh of the options files — the fast replacement for
`poudriere options -C`: for every port it keeps the saved choices that still
apply, adopts the effective value (default plus `make.conf`) for options the
file does not know about yet, and drops options the port no longer has
. A file whose complete option list and enabled set
would not change is left untouched even if the recorded package name is
stale, matching `make config-conditional`. A file naming an option with no
`OPTIONS_FILE_SET` or `OPTIONS_FILE_UNSET` line — `make config` never writes
one; a hand edit or a write cut short does — is warned about by every
command that reads it, and `sync` completes it: make reads such an option at
its defaults, follows them if they change, and `make config-conditional` asks
about it again. A hand-written file is read as the makefile it is: several
options to a line, spaces around `+=` or `=`, and `=` assigning a list
afresh; `sync` writes it back as `make config` does, but for the unread
`_OPTIONS_READ` line, left out, and a pkgname comment long enough not to
crash synth (see [configuration.md](configuration.md#synth)). An
`options.local` beside the file is read after it, as make does: its SET
and UNSET lines join the file's, every SET before any UNSET, and the
result is the saved choice every command shows. `sync` never writes it,
so an option one of its UNSET entries matches stays off, and the TUI and
`decide` refuse to change it; a SET entry there locks nothing, the file's
UNSET overriding it. Every command that reads one notes which options it
sets or unsets.

optique leaves to make, never writing or removing it (`clean` included),
an options file it cannot model: one whose `options.local` assigns a SET
or UNSET list afresh, dropping the file's lines; one where either file has
a line it cannot model — an assignment to anything but the SET and UNSET
lists and the bookkeeping `make config` writes, a `!=`, a directive, a `$`
expansion, or a line that is no assignment; and one `make.conf` moves away
by setting `OPTIONS_FILE`. A warning names the file and line, or the
moved path. The port shows make's own outcome, every option locked with no
make.conf verdict, is never unconfigured or stale, and takes no edit.

`--dry-run`/`-n` reports what
would be written and removed without touching anything; `--json` answers in
the same shape as `decide` (see [machine-interface.md](machine-interface.md))
instead of the per-file listing.

## `decide`

```sh
echo '{"www/nginx": {"LUA": true}}' | optique decide -z ws -n
echo '{"www/nginx": {"LUA": true}}' | optique decide -z ws
```

Sets option values from a JSON plan read on stdin — ports to booleans per
option name — for scripts and agents that decide without a terminal
. The plan goes through the same group, `IMPLIES`
and `*_FORCE` rules the TUI enforces, writes only the named ports' files, and
writes nothing unless the whole plan can be honoured. `-n`/`--dry-run`
previews the report without writing. See
[machine-interface.md](machine-interface.md) for the plan format and the
report shape.

## `clean`

```sh
optique clean -z workstation --dry-run
optique clean -z workstation --redundant
optique clean -z workstation --unused -f pkglist
optique clean -z workstation --unused --redundant --dry-run -f pkglist
```

Garbage-collects the resolved options directory. By default it walks every
entry and removes the ones whose port has vanished from the tree, resolving
`MOVED` first so renames and plain removals are told apart. Plain `clean`
takes no package list; giving origins or `-f` without `--unused` is an error,
since they would otherwise be silently ignored, and `--unused` without a
list is an error the other way round:

- `-r`/`--redundant` — also remove files whose recorded configuration is
  exactly what defaults plus the `make.conf` overlay would produce anyway,
  so removing them changes nothing on a later `sync`.
- `-u`/`--unused` — also remove entries for ports outside the dependency
  closure of the given package list, which is then required. If the scan
  of that closure has any error — a port that fails to query, a listed
  origin gone, a dependency on an origin in no tree — the closure is
  incomplete and `clean` refuses to prune anything at all, rather than risk
  deleting live configuration: `N error(s): the dependency closure is
  incomplete, refusing to prune entries`. So it does while the list names
  an origin `MOVED` moves and that is not followed, whose target's
  options would otherwise go as unused.
- `--json` — report on stdout as one JSON object instead of the per-entry
  listing; see [machine-interface.md](machine-interface.md).

An options file optique leaves to make ([`sync`](#sync)) is never
removed, whichever option asks; a warning names it as kept.

An entry is mapped back to its port by its name, which make derives from
`OPTIONS_FILE`. A make.conf setting `OPTIONS_FILE` (to anything but
bsd.options.mk's own `${PORT_DBDIR}/${OPTIONS_NAME}/options`) breaks the
mapping: with per-flavor files, `editors_emacs-nox` is the file of
`editors/emacs@nox`, not of a vanished `editors/emacs-nox`. Then no entry
is removed as vanished or unused, with one warning:

```text
warning: make.conf sets OPTIONS_FILE (<value>): optique cannot tell which options-dir entries make reads, so none is removed as vanished or unused
```

and the same, beginning `make could not say whether make.conf sets
OPTIONS_FILE`, when make could not be asked. `--redundant` still judges
the entries of live ports optique manages (a port whose `OPTIONS_FILE`
moved is not one). Under `--unused` or `--redundant`, an entry the moved
`OPTIONS_FILE` of a queried port lies in is kept whatever its name says,
which covers a make.conf moving it for some ports only:
`misc_flav-nox: make reads misc/flav@nox's options from it (make.conf
OPTIONS_FILE); kept`.

A make.conf `PORT_DBDIR` sending the build elsewhere
([configuration.md](configuration.md#a-makeconf-moving-the-options))
is said in the banner, and `clean` goes on: `--unused` does not count it
among the errors that stop it pruning.

`--minimal`/`-m` has no effect on `clean`; a note says so when it is given
without `--redundant` — see
[Minimal mode](#minimal-mode).

## Global options

These flags follow the action (`optique scan -z ws -f list`); with no
action they open the TUI (`optique -z ws www/nginx`). An option before an
action is a usage error that shows the command as it should read: the
action comes first, as for `poudriere` and `pkg`, so that every option of
a run is read at one level.

| Flag                  | Argument    | Meaning                                |
| :-------------------- | :---------- | :------------------------------------- |
| `-z`, `--set`         | `SET`       | poudriere set name                     |
| `-j`, `--jail`        | `JAIL`      | poudriere jail name                    |
| `-p`, `--ports-tree`  | `TREE`      | poudriere tree (default `default`)     |
| `-P`, `--portsdir`    | `DIR`       | explicit ports tree                    |
| `-O`, `--overlay`     | `NAME`      | poudriere overlay (repeatable)         |
| `--overlay-dir`       | `DIR`       | explicit overlay (repeatable)          |
| `-l`, `--local`       | —           | local build, poudriere.d ignored       |
| `-s`, `--synth`       | `[PROFILE]` | synth's layout, not poudriere's        |
| `--repo`              | `NAME`      | synth: restrict implicit list to repo  |
| `-o`, `--options-dir` | `DIR`       | explicit options dir                   |
| `-f`, `--file`        | `PKGLIST`   | pkglist of origins (repeatable)        |
| `--follow-moved`      | —           | follow `MOVED` for a listed origin     |
| `-J`, `--jobs`        | `JOBS`      | parallel `make` jobs (min(16, ncpu))   |
| `--no-cache`          | —           | skip the persistent query cache        |
| `--clear-cache`       | —           | delete the query cache first           |
| `-n`, `--dry-run`     | —           | preview `sync`/`decide`/`clean` writes |
| `-m`, `--minimal`     | —           | persist only deviations                |
| `-v`, `--verbose`     | —           | more per-subcommand detail             |
| `-q`, `--quiet`       | —           | drop banner and per-item stdout rows   |
| `--color`             | `WHEN`      | `auto`, `always` or `never`            |
| `-h`, `--help`        | —           | print usage and exit                   |
| `-V`, `--version`     | —           | print the version and exit             |

`-O` conflicts with `-l` and `-s`, `--overlay-dir` with `-s` (see
[overlays](configuration.md#overlays)). `-s` and `-l` conflict with
`-z`/`-j`/`-p` and with each other, and `-p`
with `-P`; see [configuration.md](configuration.md#synth) for the synth
layout, [local builds](configuration.md#local-builds) for `-l`, and [Choosing the ports](#choosing-the-ports) for
`--repo` and `-f`. `--verbose` reports each subcommand's own extra detail
(effective options and warnings for `scan`, final state for `sync`,
kept-reasons for `clean`); `--quiet` is ignored by `scan --json`. `--color`
tints the `scan` marker column and the informational stderr output, judging
stdout and stderr ttyness separately.

## Exit status

| Code | `scan`                | `decide`                | the others |
| :--- | :-------------------- | :---------------------- | :--------- |
| 0    | nothing pending       | the plan was honoured   | success    |
| 1    | a decision is pending | not honoured, no writes | —          |
| 2    | bad usage             | bad usage               | bad usage  |
| 3    | an error              | an error                | an error   |

An error never exits 1, so a gate cannot take a failed run for a pending
decision.

A `scan` decision is pending when a port is unconfigured or stale with
undecided options, has a conflict, is caught in a dependency loop that is
not fully blacklisted, when the list names an origin `MOVED` moves and
that is not followed (`N listed moved (see above)` on the summary line),
or there is an error: a make.conf `PORT_DBDIR` the build reads instead of
the options directory
([configuration.md](configuration.md#a-makeconf-moving-the-options)), a
port whose query failed or
a listed origin gone from the tree (neither checked, nor what it depends
on), or a dependency on an origin in no tree. The summary line counts
those as `N errors (see above)`, each named on stderr. A moved origin
followed under `--follow-moved` does not count. `decide`'s status answers
for its plan alone: a moved origin is reported in its JSON `moved`, and a
plan naming one not followed finds no port for it (`unknown`).
An unconfigured or stale port make.conf fully decides (`[mc-covered ≈]`)
does not count, nor does any blacklisted port; under `-s`, a file synth
would refuse counts whatever the port's status. Per-file failures during `sync` or `clean` are reported one by
one and counted in the summary without failing the run. A file `sync`
cannot write stops it from writing or removing any other: every file is
written aside first and renamed into place only once all of them were.

For `scan` and `decide`, the exit status answers "is there anything left
to do" rather than plain success or failure, which is what makes `scan` a
cron gate: `optique scan -z ws -q -f list || mail-me`.

## Minimal mode

`-m`/`--minimal` is a global flag that changes what `sync` and TUI apply
consider worth persisting:

- On `sync` and apply, a port whose final configuration equals what defaults
  plus `make.conf` already produce gets no options file at all; an existing
  file that is already identical is removed rather than rewritten, so only
  deviations end up on disk.
- Bare defaults then count as decided: `scan`'s exit gate and the TUI's
  attention views only flag deviations and conflicts, not every option a
  port happens to have. A port the defaults and make.conf settle reads
  `ok (no file: defaults + make.conf)` in `scan` (or names its stale
  file's changes) and wears `≈` in the TUI, instead of `?` or `!`; the
  JSON keeps its factual `status` (`unconfigured`, `stale`) with
  `mc_covered` true.

`clean`'s counterpart is `-r`/`--redundant`, not `--minimal`: giving
`--minimal` to `clean` without `--redundant` has no effect, and a note on
stderr says so.
