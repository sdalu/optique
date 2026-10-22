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
│   __MAKE_CONF  jail prelude · poudriere.d layers ·             │
│                LOCALBASE=/nonexistent                          │
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
`make -f /dev/stdin optique-config` in the port's directory. The wrapper
includes the port's own `Makefile` and prints, as parse-time `.info` lines,
everything optique needs: the options, groups and descriptions,
`IMPLIES`/`PREVENTS`, `BROKEN`/`IGNORE`, the make.conf layers' verdicts, and
the dependency list (twice, with and without `TEST_DEPENDS`, so that
test-only edges can be told apart; see
[dependency loops](dependency-loops.md)). A query takes about 0.2–0.6 s, and
up to 16 run in parallel (`-J` changes that).

Every query runs under the make.conf poudriere would use for the same jail,
tree and set, wrapped in a poudriere jail's environment: what poudriere puts
at the top of every jail's make.conf, and `LOCALBASE=/nonexistent` so that
the packages installed on the host do not answer the Makefiles' probes. See
[configuration](configuration.md) for the order the files are read in.

## The cache

Answers are cached under `~/.cache/optique/`, keyed on the ports tree's git
HEAD, the hash of the layered make.conf, and the content of the port's
options file. Files a make.conf `.include`s are not part of that hash:
after editing one, run once with `--clear-cache`. A warm run over a thousand ports takes well under a second;
only ports whose inputs changed are queried again. Renamed ports are
followed through the tree's `MOVED` file; a port that is gone, or whose
query fails, is reported as an error, and `scan` counts it in its exit
status.

Each options file is read once per run, whoever needs it: the cache check,
the session, the TUI's staging copy and the write planning all share that
read. An options file that exists but cannot be read is an error for its
port, never mistaken for a missing one.

## The session and the TUI

The scan's answers become a session: for every options file, the saved
choices, the baseline `sync` would write, and what is staged now. `scan`,
`sync`, `decide` and `clean` report from it; the [TUI](tui.md) edits it.

TUI edits never touch the real options directory. They are written to a
private staging copy, and the ports they affect are queried again in the
background against it, so a toggle that pulls in a dependency shows it
within a second or two. Only apply writes the real files, each one
atomically (temporary file, `fsync`, rename), and only those that change.
