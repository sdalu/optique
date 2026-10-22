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
one or more `-f`/`--file` pkglists (one origin per line, `#` starting a
comment, the poudriere pkglist format), or both at once; `-f` is repeatable.
Across arguments and files, duplicates are dropped and first-seen order is
kept. With neither, every subcommand but `clean`
refuses — "no port origins given (arguments or -f pkglist)"
 once a subcommand is named, or the equivalent message
for the bare default-TUI invocation — and plain
`clean` (without `--unused`) needs no list at all — see [`clean`](#clean).

Under `-s`/`--synth` (see [configuration.md](configuration.md#synth)), an
empty list is not an error: `tui`, `scan`, `sync` and `clean --unused` fall
back to the packages currently installed on the host, matching synth's own
"build what's installed" (`pkg query -a %o`, with flavors from the pkg
`flavor` annotation). `--repo NAME` (repeatable)
narrows that implicit list to packages installed from the named pkg
repositories, and is rejected when any ports *are* given, since it only
means something for the implicit list.

## `tui` (the default)

```sh
optique -z workstation www/nginx mail/dovecot
optique -z workstation tui -f pkglist
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
optique -z workstation scan -f pkglist
optique -z workstation scan --json --options -f pkglist
```

Non-interactive status report over the closure, meant as a build gate: each
port is `ok`, `unconfigured` (no saved options file), `stale` (the option
list changed since the file was written) or `conflict` (the saved options
violate a group rule or `PREVENTS`). A flagged port also lists what
`make.conf` leaves `undecided`, or is marked `[mc-covered ≈]` when `make.conf`
settles every option by itself; `mc-covered` and blacklisted ports never
count against the exit gate. `--json` prints one
object on stdout instead of the table, and `--options` adds each port's full
per-option detail — see [machine-interface.md](machine-interface.md) for
both shapes.

## `sync`

```sh
optique -z workstation sync --dry-run -f pkglist
optique -z workstation sync -f pkglist
```

Headless refresh of the options files — the fast replacement for
`poudriere options -C`: for every port it keeps the saved choices that still
apply, adopts the effective value (default plus `make.conf`) for options the
file does not know about yet, and drops options the port no longer has
. A file whose complete option list and enabled set
would not change is left untouched even if the recorded package name is
stale, matching `make config-conditional`. `--dry-run`/`-n` reports what
would be written and removed without touching anything; `--json` answers in
the same shape as `decide` (see [machine-interface.md](machine-interface.md))
instead of the per-file listing.

## `decide`

```sh
echo '{"www/nginx": {"LUA": true}}' | optique -z ws decide -n
echo '{"www/nginx": {"LUA": true}}' | optique -z ws decide
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
optique -z workstation clean --dry-run
optique -z workstation clean --redundant
optique -z workstation clean --unused -f pkglist
optique -z workstation clean --unused --redundant --dry-run -f pkglist
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
  closure of the given package list, which is then required. If any port in
  that closure fails to query, the closure is incomplete and `clean` refuses
  to prune anything at all, rather than risk deleting live configuration
 .
- `--json` — report on stdout as one JSON object instead of the per-entry
  listing; see [machine-interface.md](machine-interface.md).

`--minimal`/`-m` has no effect on `clean`; a note says so when it is given
without `--redundant` — see
[Minimal mode](#minimal-mode).

## Global options

These flags are accepted before or after the subcommand name
.

| Flag                  | Argument    | Meaning                                |
| :-------------------- | :---------- | :------------------------------------- |
| `-z`, `--set`         | `SET`       | poudriere set name                     |
| `-j`, `--jail`        | `JAIL`      | poudriere jail name                    |
| `-p`, `--ports-tree`  | `TREE`      | poudriere tree (default `default`)     |
| `-P`, `--portsdir`    | `DIR`       | explicit ports tree                    |
| `-l`, `--local`       | —           | local build, poudriere.d ignored       |
| `-s`, `--synth`       | `[PROFILE]` | synth's layout, not poudriere's        |
| `--repo`              | `NAME`      | synth: restrict implicit list to repo  |
| `-o`, `--options-dir` | `DIR`       | explicit options dir                   |
| `-f`, `--file`        | `PKGLIST`   | pkglist of origins (repeatable)        |
| `-J`, `--jobs`        | `JOBS`      | parallel `make` jobs (min(16, ncpu))   |
| `--no-cache`          | —           | skip the persistent query cache        |
| `--clear-cache`       | —           | delete the query cache first           |
| `-n`, `--dry-run`     | —           | report changes for `sync`/`clean` only |
| `-m`, `--minimal`     | —           | persist only deviations                |
| `-v`, `--verbose`     | —           | more per-subcommand detail             |
| `-q`, `--quiet`       | —           | drop banner and per-item stdout rows   |
| `--color`             | `WHEN`      | `auto`, `always` or `never`            |
| `-h`, `--help`        | —           | print usage and exit                   |
| `-V`, `--version`     | —           | print the version and exit             |

`-s` and `-l` conflict with `-z`/`-j`/`-p` and with each other, and `-p`
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
not fully blacklisted, or could not be checked because its query failed.
Ports make.conf fully decides (`[mc-covered ≈]`) and blacklisted ports
never count. Per-file failures during `sync` or `clean` are reported one by
one and counted in the summary without failing the run.

For `scan` and `decide`, the exit status answers "is there anything left
to do" rather than plain success or failure, which is what makes `scan` a
cron gate: `optique -z ws -q scan -f list || mail-me`.

## Minimal mode

`-m`/`--minimal` is a global flag that changes what `sync` and TUI apply
consider worth persisting:

- On `sync` and apply, a port whose final configuration equals what defaults
  plus `make.conf` already produce gets no options file at all; an existing
  file that is already identical is removed rather than rewritten, so only
  deviations end up on disk.
- Bare defaults then count as decided: `scan`'s exit gate and the TUI's
  attention views only flag deviations and conflicts, not every option a
  port happens to have.

`clean`'s counterpart is `-r`/`--redundant`, not `--minimal`: giving
`--minimal` to `clean` without `--redundant` has no effect, and a note on
stderr says so.
