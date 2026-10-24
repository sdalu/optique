# Benchmark: optique against `poudriere options -C`

Both tools answer the same question for a package list: which ports of its
dependency closure have options, and what should their options files say.
`poudriere options -C` walks the closure one `make` at a time and opens a
dialog for each port with new options; optique queries the closure in
parallel, caches every answer, and opens one screen for all of them (or,
with `sync`, writes the defaults without asking). This page measures both
on two closures, and checks that they write the same files.

## Results

Host: Intel Core Ultra 7 165U (14 threads, mostly efficiency cores),
FreeBSD 15.1, `/usr/ports` at `faf90045ac98` (2026-10-22). Both tools read
the same empty make.conf; times are wall clock, from
[`scripts/bench.sh`](../scripts/bench.sh).

### A large closure: www/firefox, editors/libreoffice, x11/kde

876 ports, 327 of them with options.

| Task | `poudriere options -C` | optique |
| :-- | --: | --: |
| Configure from an empty options dir | 73.4 s **+ 327 dialogs** | 27.1 s (`sync`, no dialog) |
| Check again, everything configured | 48.8 s | 27.1 s cold, **0.2 s** warm |
| Check after a day of commits (182 commits, 1,024 paths) | 48.8 s | **1.0 s** (3 ports queried) |
| Check after a week of commits (999 commits, 6,078 paths) | 48.8 s | **2.3 s** (43 ports queried) |
| Check, cold, one job (`-J 1`) | | 174.6 s |

### A workstation package list

34 origins (three of them moved since the list was written: optique and
poudriere both follow `MOVED`), 421 ports, 143 with options.

| Task | `poudriere options -C` | optique |
| :-- | --: | --: |
| Configure from an empty options dir | 50.4 s **+ 143 dialogs** | 4.9 s (`sync`, no dialog) |
| Check again, everything configured | 40.2 s | 5.2 s cold, **0.1 s** warm |
| Check after a day of commits | 40.2 s | **0.2 s** (2 ports queried) |
| Check after a week of commits | 40.2 s | **1.1 s** (13 ports queried) |
| Check, cold, one job (`-J 1`) | | 31.4 s |

<details>
<summary>The list</summary>

```text
shells/bash shells/bash-completion sysutils/beadm ftp/curl sysutils/ddrescue
sysutils/devcpu-data ports-mgmt/dialog4ports sysutils/dmidecode
sysutils/exfat-utils audio/flac sysutils/fusefs-ntfs devel/git audio/lame
ftp/lftp sysutils/lsof audio/mp3gain security/nmap java/openjdk8
audio/opus-tools ports-mgmt/pkg ports-mgmt/poudriere sysutils/pstree net/rsync
net-p2p/rtorrent sysutils/screen x11/slim sysutils/smartmontools
ports-mgmt/synth sysutils/tree audio/vorbis-tools audio/vorbisgain ftp/wget
x11/xorg shells/zsh
```

</details>

A second run of each gave times within 10% of these.

## The same files

Starting from an empty options dir, both write an options file for the same
ports, with the same options on and off, except where the build environment
differs on purpose. optique sets `PACKAGE_BUILDING`, as a poudriere bulk
build does unless `NO_PACKAGE_BUILDING` is set; `poudriere options` does
not, so the defaults it offers are not always the ones the build will use:

| Port | Under `PACKAGE_BUILDING` (the build, optique) | Without it (`poudriere options`) |
| :-- | :-- | :-- |
| `math/openblas` | `DYNAMIC_ARCH` on | off |
| `multimedia/libdvdread` | `DVDCSS_DLOPEN` off, so `multimedia/libdvdcss` leaves the closure | on, and `libdvdcss` gets a file |
| `lang/gnat12` | `ASSETS` on | off |

That is the whole difference. On the large closure 324 of poudriere's 327
files are identical to optique's, the other three being `openblas`,
`libdvdread` and the `libdvdcss` optique has no reason to write; on the
workstation list 142 of 143 are, `gnat12` aside. See
[configuration](configuration.md) for the environment optique gives a
query.

## Reading the numbers

- **A single optique query costs more than poudriere's.** optique's wrapper
  reads far more of each port (descriptions, `IMPLIES`/`PREVENTS`, each
  option's dependencies, make.conf's verdicts) than `config-conditional`
  needs. With one job it is 3.6 times slower on the large closure.
- **The cold speedup is parallelism, the rest is the cache.** With 14 jobs
  the cold check is 1.8 times faster than poudriere on the large closure,
  on a CPU that is mostly efficiency cores. Everything beyond that comes
  from the cache, kept across runs and, for a git tree, across updates
  (see [the cache](how-it-works.md#the-cache)).
- **poudriere repeats the shared part of the closure for each origin.**
  `config-recursive` runs once per listed origin, and each lists its own
  closure afresh, so 34 origins sharing most of their dependencies walk
  them 34 times: the workstation list's 421 ports take poudriere 40 s, the
  large closure's 876 ports 49 s. optique walks the closure once, which is
  why its one-job check of the workstation list (31 s) still beats
  poudriere there.
- **The dialogs are not in the machine time.** The stub answers each dialog
  at once; a person answers 327 (or 143) screens one at a time, in
  dependency order, with no overview and no way back. That, more than the
  seconds, is what optique replaces.
- **Not measured:** `config-recursive` lists the closure once, from the
  options before any dialog, so an option turned on that adds a dependency
  is not followed until the next run; optique re-queries the closure after
  each change.

## How it was measured

poudriere need not be installed: [`scripts/bench.sh`](../scripts/bench.sh)
runs what `poudriere options -C -z <set> <origin>...` runs, read from its
`options.sh`, `setup_makeconf` and `setup_ports_env`:

- each listed origin is resolved through `MOVED`, as `listed_ports` does;
- for each, `make config-conditional`, then `make config-recursive` with
  `LOCALBASE=/nonexistent`, both with the port's `PORT_DBDIR` and
  `PORTSDIR` on the command line;
- under a make.conf holding the set's layer (empty) and what
  `Mk/Scripts/ports_env.sh` exports, with `UID=0` and `GID=0`, so that
  poudriere's queries too skip the framework's per-port probes;
- `dialog4ports` is a stub that keeps the options it is shown (the
  defaults, for a new port), as a user pressing Enter would, and logs each
  call to count the dialogs.

optique gets the same list and the same empty layer, through a throwaway
`POUDRIERE_ETC`, as a poudriere build (`-z bench -P /usr/ports`). Nothing of
the host's configuration is read or written. The tree-update rows warm the
cache at an older commit of a shared clone, check that commit out to HEAD,
and time the next scan.

To reproduce, from the source tree after `make build`:

```sh
scripts/bench.sh www/firefox editors/libreoffice x11/kde
scripts/bench.sh -f /usr/local/etc/poudriere.d/workstation.pkglist
# with the one-job scan and two tree updates (needs ~1.5 GB in TMPDIR):
TMPDIR=/var/tmp scripts/bench.sh -1 -u "<older-rev> <older-rev>" -f list
```

Every number moves with the ports tree, the CPU and the list.
