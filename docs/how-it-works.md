# How optique works

optique asks the ports framework itself what each port is: one `make` run per
port, many at once, every answer cached until something it depends on
changes. Nothing is written before you apply, and then only the options
files that change. The reasons behind each choice, and what was rejected,
are in [DESIGN.md](../DESIGN.md).

```text
    origins · -f pkglist
          │
          ▾
┌────────────────────┐                ┌──────────────────────────┐
│ scan the closure   ├── lookup ─────▸│ query cache              │
│ breadth-first,     │◂── hit ────────┤ ~/.cache/optique         │
│ edge by edge       │                │ tree · make.conf · file  │
└─────────┬──────────┘                └──────────────────────────┘
          │ miss
          ▾
┌────────────────────────────────────────────────────────────────┐
│ make -f /dev/stdin optique-config      up to 16 at once        │
│   PORT_DBDIR   the options dir                                 │
│   __MAKE_CONF  the build's make.conf: poudriere's jail prelude │
│                and layers, or synth's header and profile;      │
│                LOCALBASE=/nonexistent (host make.conf for -l)  │
└─────────┬──────────────────────────────────────────────────────┘
          │ .info OPTIQUE|… lines: options, deps, BROKEN/IGNORE
          ▾
┌────────────────────┐   ┌───────────────────────────────────────┐
│ session            ├──▸│ scan · sync · decide · clean          │
│ saved, staged and  │   │ reports, JSON, exit status            │
│ baseline state per │   └───────────────────────────────────────┘
│ options file       │   ┌───────────────────────────────────────┐
│                    ├◂─▸│ TUI: edits → staging dir →            │
│                    │   │ background re-queries                 │
└────────────────────┘   └───────────────────────────────────────┘
```

## Querying a port

Each port is resolved by piping a small wrapper makefile to
`make -f /dev/stdin optique-config` in the port's directory: the first
[overlay](configuration.md#overlays) holding it, else the ports tree. The wrapper
includes the port's own `Makefile` and prints, as parse-time `.info` lines,
everything optique needs: the options, groups and descriptions,
`IMPLIES`/`PREVENTS`, `BROKEN`/`IGNORE`, the make.conf layers' verdicts, and
the dependency list (twice: the framework's own `_UNIFIED_DEPENDS`, and the
lists poudriere builds without `-t`, so that test-only edges can be told
apart on a tree old enough to still fold `TEST_DEPENDS` in; see
[dependency loops](dependency-loops.md)). A dependency on `origin~subpkg`
is one on `origin`, as the framework's `do-depends.sh` reads it: every
subpackage comes from the one port build and its one options file. A query takes about 0.2–0.6 s, and
up to 16 run in parallel (`-J` changes that). What bsd.port.mk would work
out about the host in every query (release, arch, CPU count, pkg's
version) is probed once per run and given to each on its command line.

Every query runs under the make.conf the build would use. For poudriere,
that is the one it would use for the same jail, tree and set, wrapped in a
poudriere jail's environment: what poudriere puts at the top of every
jail's make.conf, and `LOCALBASE=/nonexistent` so that the packages
installed on the host do not answer the Makefiles' probes. For synth, it
is the header synth writes for its scan slave, the profile's make.conf,
and the same `LOCALBASE` line, synth's chroot having nothing installed
either. A local build (`-l`) reads the host's own make.conf as is, and its
queries see the installed packages, as its build would. A poudriere or
synth query runs with `USER=root` and `HOME=/root`, as their builders do,
a local build's with the invoking user's own: `bsd.ccache.mk` keeps ccache,
and its `devel/ccache` dependency, for root alone. See
[configuration](configuration.md) for the order the files are read in.

The listed origins are looked up in the tree's `MOVED` file before the
walk, its chains collapsed in file order as poudriere's `parse_MOVED.awk`
collapses them, each as an originspec, as poudriere's `check_moved` looks
it up: `origin@flavor`'s own entry, else the bare origin's, its target
taking the listed flavor (an entry naming another flavor stops poudriere,
and is an error), a chain stopping at the first target in a tree. Which
origins are looked up follows the build. poudriere looks up every listed
one before testing whether it is in the tree, so an entry moves a name the
tree has since reused (math/mumps moved to math/mumps-edf, then math/mumps4
to math/mumps): an upstream defect that changes what gets built, conformed
to. `make config` knows no `MOVED`, and synth reads none: a local build and
synth take an origin in the tree as it is. A listed origin `MOVED` moves is
flagged rather than followed: left out of the walk with its target, and
pending in `scan`'s exit status, since the list needs a fix;
`--follow-moved` walks the target in its place, as poudriere builds it
(never under `-s`: synth would not). A listed origin that is gone, or
whose entries cycle without reaching one (poudriere loops forever on that,
an upstream defect), is an error. This is read on every run, never cached:
the cache's carry-over takes `MOVED` to be read each time. Dependencies do
not follow `MOVED`: a dependency on an origin in no
tree, moved, expired or never there, is an error on the depending port,
naming what `MOVED` says of it, as poudriere refuses the bulk on it; the
dependencies of an `IGNORE`d or blacklisted port, and test-only edges, are
not checked, as poudriere does not check them. These errors, and a port
whose query fails, count in `scan`'s exit status.

## The cache

Answers are cached under `~/.cache/optique/`, so a later run queries only
the ports whose inputs changed: a warm run over a thousand ports takes well
under a second ([benchmark](benchmark.md)). The man page's "Query cache" subsection is the reference;
the reasons, the measurements and what was rejected are in
[DESIGN.md](../DESIGN.md#persistent-cache-keyed-on-what-actually-changed).

### What keys a generation

Answers are grouped in generations, one JSON Lines file each, named
`v<schema>-<tree>-<make.conf hash>.jsonl`. A generation is one state of:

- the ports tree: its git HEAD, read from `.git` without running git, or
  on a tree without git the mtimes of `Mk/bsd.port.mk`, `MOVED` and
  `UPDATING`;
- every overlay: the count and newest mtime of its files, even in a git
  checkout, since overlay ports are edited before any commit;
- the text of the staged make.conf, layers and all;
- the values bsd.port.mk works out about the host (release, arch, CPU
  count: its `_EXPORTED_VARS`). They are read once per run from a probe
  port that uses nothing, then given to every query on its command line,
  as the framework gives them to its own sub-makes; an upgraded host is a
  new generation;
- for a local build, pkg's database (size and mtime of `local.sqlite`),
  since its queries see the installed packages, and the invoking `USER`,
  its queries running as that user.

The current generation and the most recent other one are kept; older ones
are removed when a generation is opened, and so are those an older optique
wrote in another format.

### When an entry answers

Within a generation, an entry answers for its port while the port's
options file and the `options.local` beside it, which make reads after it,
hold the bytes the query saw.

Each options file is read once per run, whoever needs it: the cache check,
the session, the TUI's staging copy and the write planning all share that
read. An options file that exists but cannot be read is an error for its
port, never mistaken for a missing one.

### After a tree update

When the tree moves to another git commit, the new generation starts with
the entries of the latest one made under the same make.conf and overlays
that the commits in between leave alone. optique runs `git diff-tree`
between the two commits; each entry keeps the list of the tree's files
make read for it (`.MAKE.MAKEFILES`), and goes when:

- one of those files changed (`Mk/bsd.port.mk`, a `Mk/Uses/*.mk` it
  uses, another port's `Makefile.version` it includes);
- anything changed in the port's own directory or, for a slave, its
  master's: `pkg-help`, `Makefile.local` and `files/` are probed there,
  not included;
- a file of its category changed other than the category's `Makefile`
  (`../Makefile.inc` is probed).

A new `Mk/*.mk`, or a top-level file other than `MOVED`, `UPDATING`,
`CHANGES`, `UIDs`, `GIDs` and the like, takes every entry. A `Mk/` script
make runs while it parses (`Uses/cargo.mk`'s awk scripts, whose crates add
build dependencies) counts as a change to the makefile running it; the
other scripts, `Tools/`, `Templates/` and `Keywords/` take none, since
only targets read them.
On a thousand-port closure a day's update re-queried 3 ports and a week's
44. The banner says how many entries were carried over. Without git, or
when git cannot list the changes (the old commit gone), nothing is carried
over.

### What it does not see

- Files a make.conf `.include`s: after editing one, run once with
  `--clear-cache`. A tree update does not clear their stale answers any
  more, since it keeps whatever its changes leave alone.
- A port edited in a git tree and not committed: the tree is known by its
  commit. Edit ports in an [overlay](configuration.md#overlays), keyed by
  its files' mtimes, or run with `--no-cache` while editing.
- A `lang/zig<N>` or `devel/electron<N>` port added after a port using it
  was found IGNOREd for its absence: that IGNORE is carried over until the
  port or the `.mk` changes.

### Turning it off

`--no-cache` neither reads nor writes the cache; `--clear-cache` deletes
every generation first. There is no environment variable for either.

## The session and the TUI

The scan's answers become a session: for every options file, the saved
choices, the baseline `sync` would write, and what is staged now. `scan`,
`sync`, `decide` and `clean` report from it; the [TUI](tui.md) edits it.
An options file optique cannot model (a line it does not read, an
`options.local` resetting a list, or an `OPTIONS_FILE` make.conf moves) is
left to make: its port shows make's own outcome, every option locked, and
nothing writes or removes the file.

TUI edits never touch the real options directory. They are written to a
private staging copy, and the ports they affect are queried again in the
background against it, so a toggle that pulls in a dependency shows it
within a second or two. Only apply writes the real files, each one
atomically (temporary file, `fsync`, rename), and only those that change.
