# Benchmark: optique against `poudriere options -C`

Both tools answer the same question for a package list: which ports of its
dependency closure have options, and what should their options files say.
`poudriere options -C` walks the closure one `make` at a time, once per
listed origin, and opens a dialog for each port with new options; optique
queries the closure once, in parallel, caches every answer, and opens one
screen for all of them (or, with `sync`, writes the defaults without
asking). This page measures both on two closures, and checks that they
write the same files.

## Results

Host: Intel Core Ultra 7 165U (14 threads, mostly efficiency cores),
FreeBSD 15.1, a desktop in use while it ran. `/usr/ports` at
`2354a0a5d8ed` (2026-10-25), optique at `cc412b9`. Both tools read the same
empty make.conf; times are wall clock, from
[`scripts/bench.sh`](../scripts/bench.sh).

### A workstation: 578 origins, six package lists

The lists a poudriere host builds for a workstation (basics, embedded
development, games, retro gaming, ruby, desktop): 578 origins, a closure of
3,417 ports, for which optique writes 1,181 options files.

| Task | `poudriere options -C` | optique |
| :-- | --: | --: |
| Configure from an empty options dir | 2,793 s (47 min) **+ 1,162 dialogs** (1) | 87.6 s (`sync`, no dialog) |
| Check again, everything configured | 2,697 s (45 min) (1) | 90.3 s cold, **0.4 s** warm |
| After `OPTIONS_UNSET+=DOCS` in make.conf | 2,697 s, and no word of what changed (2) | **20.8 s** (669 ports queried) |
| After 460 commits (1,802 paths) | 2,697 s | **4.1 s** (59 ports queried) |
| After 1,277 commits (6,717 paths) | 2,697 s | **9.0 s** (219 ports queried) |
| Check, cold, one job (`-J 1`) | | 678.3 s |
| Check, cold, without the framework probe | | 96.5 s |

(1) A lower bound: two origins' walks stopped early (see
[How it was measured](#how-it-was-measured)); a complete run takes about
twenty more dialogs and some seconds more.
(2) `config-conditional` only looks for options a port has that its file
does not record; a make.conf edit changes no file, so poudriere walks the
same way and opens nothing.

### A large closure: www/firefox, editors/libreoffice, x11/kde

3 origins, a closure of 876 ports, 327 of them with options.

| Task | `poudriere options -C` | optique |
| :-- | --: | --: |
| Configure from an empty options dir | 71.4 s **+ 327 dialogs** | 31.6 s (`sync`, no dialog) |
| Check again, everything configured | 48.1 s | 32.4 s cold, **0.1 s** warm |
| After `OPTIONS_UNSET+=DOCS` in make.conf | 48.1 s | **6.4 s** (165 ports queried) |
| After 460 commits | 48.1 s | **1.3 s** (15 ports queried) |
| After 1,277 commits | 48.1 s | **5.0 s** (51 ports queried) |
| Check, cold, one job (`-J 1`) | | 188.6 s |
| Check, cold, without the framework probe | | 35.5 s |

## Reading the numbers

- **poudriere's cost grows with the number of origins listed; optique's
  with the closure.** `config-recursive` runs once per listed origin and
  lists its closure afresh, so 578 origins sharing most of their
  dependencies walk them 578 times. optique walks the closure once: with a
  single job it checks the workstation 4.0 times faster than poudriere,
  with 14 jobs 30 times faster, and warm in under half a second.
- **For a single origin, a query costs more in optique.** Its wrapper reads
  far more of each port (descriptions, `IMPLIES`/`PREVENTS`, each option's
  dependencies, make.conf's verdicts) than `config-conditional` needs: on
  the 3-origin closure, one job is 3.9 times slower than poudriere, and 14
  jobs, on this mostly efficiency-core CPU, only 1.5 times faster cold.
  There the cache does the rest.
- **The cache follows what changed.** A tree update re-queries the ports
  its commits touch; a make.conf option edit, the ports it can reach. See
  [the cache](how-it-works.md#the-cache).
- **The framework probe** (bsd.port.mk's host values, worked out once per
  run instead of in every query) saves 6 to 9% of a cold scan.
- **The dialogs are not in the machine time.** The benchmark answers each
  at once; a person answers 1,162 screens one at a time, in dependency
  order, with no overview and no way back. That, more than the seconds, is
  what optique replaces.
- **Not measured:** `config-recursive` lists the closure once, from the
  options before any dialog, so an option turned on that adds a dependency
  is not followed until the next run; optique re-queries the closure after
  each change.

## The same files

From an empty options dir, both write an options file for the same ports
with the same options on and off, except where they differ on purpose:

| Port | optique | `poudriere options` | Why |
| :-- | :-- | :-- | :-- |
| `math/openblas` | `DYNAMIC_ARCH` on | off | optique sets `PACKAGE_BUILDING`, as a poudriere bulk build does unless `NO_PACKAGE_BUILDING`; `poudriere options` does not, so it offers defaults the build will not use |
| `multimedia/libdvdread` | `DVDCSS_DLOPEN` off, so `multimedia/libdvdcss` leaves the closure | on, and `libdvdcss` gets a file | the same |
| `lang/gnat12` | `ASSETS` on | off | the same |
| `editors/emacs` | `NATIVECOMP` on, so `lang/gcc13` joins the closure | off | `@full` and `@nox` share one file: optique gives it to the default flavor, poudriere's last dialog (`@nox`) overwrites the first ([why](../DESIGN.md#flavors-sharing-one-options-file-the-default-flavor-owns-it)) |

On the large closure that is the whole difference: 324 of poudriere's 327
files are identical to optique's, the other three being `openblas`,
`libdvdread` and the `libdvdcss` optique has no reason to write. On the
workstation, the four rows above, and the 20 ports the two stopped walks
missed (`textproc/ripgrep` and what lies behind it). optique also wrote
one for `graphics/opencolorio`, whose only option its own Makefile forces
off; since `1e60996` it writes none for a port make.conf decides entirely,
as `config-conditional` does.

## How it was measured

poudriere need not be installed: [`scripts/bench.sh`](../scripts/bench.sh)
runs what `poudriere options -C -z <set> <origin>...` runs, read from its
`options.sh`, `setup_makeconf` and `setup_ports_env`:

- the listed origins are read as `_listed_ports` reads them (first word,
  `#` lines skipped, duplicates dropped) and resolved through `MOVED`;
- for each, `make config-conditional`, then `make config-recursive` with
  `LOCALBASE=/nonexistent`, both with `PORT_DBDIR` and `PORTSDIR` on the
  command line;
- under a make.conf holding the set's layer (empty) and what
  `Mk/Scripts/ports_env.sh` exports, with `UID=0` and `GID=0`, so that
  poudriere's queries too skip the framework's per-port probes;
- `dialog4ports` is a stub that keeps the options it is shown (the
  defaults, for a new port), as a user pressing Enter would, and logs each
  call to count the dialogs.

optique gets the same list and the same empty layer, through a throwaway
`POUDRIERE_ETC`, as a poudriere build (`-z bench -P /usr/ports`). Nothing of
the host's configuration is read or written. The make.conf rows add the
line to that layer after a warm scan; the tree-update rows warm the cache
at an older commit of a shared clone, check out HEAD, and time the next
scan.

The workstation's poudriere rows were measured before the script gave
make a `TMPDIR` of its own. `Mk/Uses/cargo.mk` moves `TMPDIR`, where bmake
writes its command scripts, into the port's `WRKDIR`; this tree held a
`textproc/ripgrep/work` a build as root had left, so `make config` failed
there for the benchmark, which runs as a user (poudriere runs as root), and
ended the walk of the two origins that reach it, `editors/vscode` and
`misc/openclaw`. Run again with the fix, `misc/openclaw`'s walk opens the
17 dialogs it had missed. The script now reports any walk that fails.

To reproduce, from the source tree after `make build`:

```sh
scripts/bench.sh www/firefox editors/libreoffice x11/kde
scripts/bench.sh -f list1 -f list2 ...
# with the one-job scan, two tree updates (needs ~1.5 GB in TMPDIR) and
# the work dir kept for comparing the files:
TMPDIR=/var/tmp scripts/bench.sh -1 -k -u "<older-rev> <older-rev>" -f list
```

Every number moves with the ports tree, the CPU, the load and the lists.
