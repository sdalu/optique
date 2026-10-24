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
[dependency loops](dependency-loops.md)). A query takes about 0.2–0.6 s, and
up to 16 run in parallel (`-J` changes that).

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

The listed origins follow the tree's `MOVED` file as poudriere follows it:
only an origin no longer in the tree, since a name moved away can be
reused (math/mumps moved to math/mumps-edf, then math/mumps4 to
math/mumps), its chains collapsed in file order as poudriere's
`parse_MOVED.awk` collapses them, a chain stopping at the first origin that
exists. A listed origin that is gone, or whose entries cycle without
reaching one (poudriere loops forever on that, an upstream defect), is an
error. Dependencies do not follow `MOVED`: a dependency on an origin in no
tree, moved, expired or never there, is an error on the depending port,
naming what `MOVED` says of it, as poudriere refuses the bulk on it; the
dependencies of an `IGNORE`d or blacklisted port, and test-only edges, are
not checked, as poudriere does not check them. These errors, and a port
whose query fails, count in `scan`'s exit status.

## The cache

Answers are cached under `~/.cache/optique/`, keyed on the ports tree's git
HEAD (on a tree without git, the mtimes of `Mk/bsd.port.mk`, `MOVED` and
`UPDATING`) and every overlay's (the count and newest mtime of its files,
even in a git checkout, since overlay ports are edited before any commit),
the hash of the layered make.conf, and the content of the
port's options file and of the `options.local` beside it, which make reads
after it. A local build also keys on pkg's database
(`local.sqlite`), since its queries see the installed packages: a package
install or delete starts a new generation; and on the invoking `USER`, its
queries running as that user. Every build also keys on the values
bsd.port.mk works out about the host (release, arch, CPU count), probed
once per run and then given to every query, so an upgraded host starts a
new generation. Files a make.conf `.include`s are not part of that hash:
after editing one, run once with `--clear-cache`. A warm run over a thousand ports takes well under a second;
only ports whose inputs changed are queried again. When the tree moves to
another git commit, the new generation starts with the entries of the
previous one that the commits in between leave alone: an entry goes when
a file make read for the port changed, or anything in the port's own
directory (or its master's), so a daily update re-queries a handful of
ports rather than all of them. The banner says how many entries were
carried over.

Each options file is read once per run, whoever needs it: the cache check,
the session, the TUI's staging copy and the write planning all share that
read. An options file that exists but cannot be read is an error for its
port, never mistaken for a missing one.

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
