# poudriere

How upstream poudriere sets up the environment in which options and
dependencies are evaluated, which options directory a build reads, how
`bulk` and `testport` build the dependency graph, and when an
incremental build deletes a package, at the revision under
[Sources](#sources). Functions live in `src/share/poudriere/common.sh`
unless another file is named.

Where the files a build reads end up:

```text
host ${POUDRIERED}          reference jail          each builder
┌────────────────────┐      ┌─────────────────┐     ┌──────────────────┐
│ make.conf          │ cat  │                 │     │                  │
│ ... 8 layers, in   ├─────▸│ /etc/make.conf  ├────▸│ clone of the     │
│ the order below    │      │                 │     │ reference jail   │
├────────────────────┤      ├─────────────────┤     ├──────────────────┤
│ first existing     │ copy │ /var/db/ports   │ ro  │ /var/db/ports    │
│ <cand>-options     ├─────▸│ (a copy)        ├────▸│ (null mount)     │
└────────────────────┘      └─────────────────┘     └──────────────────┘
```

Contents:

- [Configuration files](#configuration-files)
- [make.conf for a build](#makeconf-for-a-build)
- [Options directories](#options-directories)
- [Environment of the metadata queries](#environment-of-the-metadata-queries)
- [Ports to build](#ports-to-build)
- [Gathering metadata](#gathering-metadata)
- [The build queue](#the-build-queue)
- [Incremental rebuild](#incremental-rebuild)
- [Likely defects](#likely-defects)
- [Tests on options and dependencies](#tests-on-options-and-dependencies)
- [Sources](#sources)

## Configuration files

`POUDRIERE_ETC` is the configuration root: `-e etcdir` in
`src/bin/poudriere.in`, else `${SCRIPTPREFIX}/../../etc` (`PREFIX/etc`
when installed), made absolute against the starting directory. A
`POUDRIERE_ETC` already in the environment does the same as `-e`:
`poudriere.in` passes any non-empty value through the `env -i` that starts
the sub-command, without the check `-e` makes that it is a directory.
`POUDRIERED` is `${POUDRIERE_ETC}/poudriere.d`; `HOOKDIR` is
`${POUDRIERED}/hooks`.

`include_poudriere_confs` sources the shell configuration while
`common.sh` is itself being sourced, before the sub-command has parsed
its own arguments. It therefore scans the whole argument vector for
`-j`, `-p` and `-z` on its own, and sees only what was typed: a build
run without `-p` has no tree name at this point, even though `bulk`,
`testport` and `options` later default `PTNAME` to `default`. Files
are sourced in this order, later ones overriding earlier ones:

1. `POUDRIERE_ETC/poudriere.conf`
2. `POUDRIERED/poudriere.conf`, skipped when its real path is that of
   the first file
3. `POUDRIERED/<set>-poudriere.conf`
4. `POUDRIERED/<tree>-poudriere.conf`
5. `POUDRIERED/<jail>-poudriere.conf`
6. `POUDRIERED/<tree>-<set>-poudriere.conf`
7. `POUDRIERED/<jail>-<tree>-poudriere.conf`
8. `POUDRIERED/<jail>-<set>-poudriere.conf`
9. `POUDRIERED/<jail>-<tree>-<set>-poudriere.conf`

Each combined name is tried only when all of its parts were given. At
least one of the first two files must be readable, or poudriere stops
with "Unable to find a readable poudriere.conf". `poudriere.8` lists
the same order.

`load_blacklist` reads `blacklist` and `<name>-blacklist` from
`poudriere.d` for the same names as the make.conf layers below, plus
`qemu-blacklist` when emulating, and accumulates every origin listed
into `BLACKLIST`.

## make.conf for a build

`jail_start` builds the reference jail's `/etc/make.conf` by appending
to the file cloned from the jail's own tree. The pieces arrive in this
order:

1. Whatever the jail's own `/etc/make.conf` already held.
2. When emulating with QEMU, `.sinclude "/etc/make.nxb.conf"`.
3. The prelude written by `jail_start`:

   ```make
   USE_PACKAGE_DEPENDS=yes
   BATCH=yes
   WRKDIRPREFIX=/wrkdirs
   PORTSDIR=/usr/ports
   PACKAGES=/packages
   DISTDIR=/distfiles
   OVERLAYS+=/overlays/<overlay>     # one line per -O overlay
   FORCE_PACKAGE=yes                 # unless NO_FORCE_PACKAGE is set
   PACKAGE_BUILDING=yes              # unless NO_PACKAGE_BUILDING is set
   PACKAGE_BUILDING_FLAVORS=yes      # same condition
   ```

   `/overlays` is the default `OVERLAYSDIR`. `PACKAGE_BUILDING=yes` is
   also exported into poudriere's own environment.
4. `setup_makeconf`, below: cross-build lines, the layered
   `poudriere.d` files and the plugin files.
5. `setup_ccache`: `WITH_CCACHE_BUILD=yes` and `CCACHE_DIR` when
   `CCACHE_DIR` is a directory; `NO_CCACHE_DEPEND=1` and
   `CCACHE_WRAPPER_PATH` when a static host ccache is used (its comment:
   so that the framework does not add a ccache `BUILD_DEPENDS`).
6. The `jail start` hook runs.
7. `setup_ports_env`: when `/usr/ports/Mk/Scripts/ports_env.sh` exists,
   it is run in the jail and its `export` lines are stored, stripped of
   `export` and quotes, in `/etc/make.conf.ports_env`; make.conf then
   gets a `#### Misc Poudriere ####` block with
   `.include "/etc/make.conf.ports_env"`, `GID=0` and `UID=0`.

The same sequence against the queries that read it:

```text
jail_start                             /etc/make.conf of the reference jail
 ├─ jail's own make.conf               ┐
 ├─ .sinclude make.nxb.conf (QEMU)     │
 ├─ prelude: BATCH, PACKAGE_BUILDING…  │ seen by every
 ├─ setup_makeconf: *-make.conf layers │ metadata query
 ├─ setup_ccache                       │
 └─ setup_ports_env, UID=0, GID=0      ┘
prepare_ports
 ├─ gather_port_vars ────────────────▸ one make -V run per originspec
 ├─ incremental checks, deletions
 └─ DISABLE_MAKE_JOBS=poudriere,       not seen by any query
    UID/GID dropped (BUILD_AS_NON_ROOT)
```

All of this exists before the ports are gathered. Two edits come only
at the end of `prepare_ports`, after the dependency computation and the
incremental checks: `DISABLE_MAKE_JOBS=poudriere` is appended unless
`ALLOW_MAKE_JOBS=yes`, and with `BUILD_AS_NON_ROOT=yes` the `UID=0` and
`GID=0` lines are deleted again. Neither is seen by the metadata
queries.

### The layered files

`setup_makeconf` first handles the architecture. When `need_cross_build`
holds for the host and jail architectures (i386 on amd64, powerpc on
powerpc64, or anything needing emulation), it writes
`MACHINE=`, `MACHINE_ARCH=` and `ARCH=${MACHINE_ARCH}`, and
`export_cross_env` exports `UNAME_r`, `UNAME_v`, `UNAME_m`, `UNAME_p`
and `OSVERSION` (read from the jail's `sys/param.h`). It then calls
`append_make` for each name below, in this order; `-` stands for plain
`make.conf`, any other name for `<name>-make.conf`, all in `POUDRIERED`:

1. `make.conf`
2. `<set>-make.conf`
3. `<tree>-make.conf`
4. `<jail>-make.conf`
5. `<tree>-<set>-make.conf`
6. `<jail>-<tree>-make.conf`
7. `<jail>-<set>-make.conf`
8. `<jail>-<tree>-<set>-make.conf`
9. `hooks/plugins/<plugin>/make.conf`, for every plugin directory

The names with a set are tried only when a set was given;
`<jail>-<tree>` is always tried. Here the tree name is `PTNAME` after
defaulting, so `default-make.conf` and `<jail>-default-make.conf` are
read by a build run without `-p`, unlike `default-poudriere.conf`.
`append_make` skips a missing file, and skips a file whose real path
already appears in a `#### <realpath> ####` header in the destination,
so a symlinked layer is included once. Each included file is preceded
by that header (followed by the name used, when it differs from the real
path) and copied verbatim; `.include` lines in it are left for make.

Finally lines starting `DEVELOPER=` are deleted, with a warning.

## Options directories

### Which directory a build reads

`do_portbuild_mounts` gives the reference jail its options. It tries
the candidates below in order and `use_options` copies the first that
exists, with `do_clone`, into the jail's `/var/db/ports`:

1. `<jail>-<tree>-<set>-options` (the `MASTERNAME`; `<jail>-<tree>`
   when no set is given)
2. `<jail>-<set>-options`
3. `<jail>-<tree>-options`
4. `<tree>-<set>-options`
5. `<set>-options`
6. `<tree>-options`
7. `<jail>-options`
8. `options`

Candidates 2, 4 and 5 are tried only with a set. Only one directory is
used; they are not merged. With `TMPFS_DATA` or `TMPFS_ALL` the jail's
`/var/db/ports` is a tmpfs first. Builder jails do not copy anything:
they null-mount the reference jail's `/var/db/ports` read-only.

The manual (`poudriere.8`, "Custom build options") gives the same order
but says the directory is null-mounted into the jail; the code copies
it into the reference jail and null-mounts only that copy into the
builders.

poudriere passes no `PORT_DBDIR` to the metadata queries or builds of
`bulk`; they rely on the framework's default, `/var/db/ports` (not
verified in these sources), where the directory is put.

### poudriere options

`options.sh` runs entirely on the host. It accepts `-a arch`, `-c`,
`-C`, `-j jail`, `-f file`, `-o name`, `-p tree`, `-n`, `-r`, `-s` and
`-z set`; it has no `-O`.

It writes `POUDRIERED/[<jail>-][<tree>-][<set>-]options`, each part
present only when given. The tree part comes from `PTNAME_TMP`, set
only by an explicit `-p`: without `-p` the tree is left out, and
`-p default` writes `<jail>-default-options`. `-o name` replaces the
whole name: the directory is `POUDRIERED/name`, with no `-options`
suffix, whereas a build only looks for names ending in `-options`.
Without `-o`, when `<jail>-<set>-options` exists and the new directory
does not, it warns that those options will no longer be used and asks
for confirmation on a terminal. The directory is created if missing.

The command is chosen by the flags, which are mutually exclusive:

| Flag       | Target               | Recursive target       | Dialog (1) |
| ---------- | -------------------- | ---------------------- | ---------- |
| none, `-C` | `config-conditional` | `config-recursive`     | yes        |
| `-c`       | `config`             | `config-recursive`     | yes        |
| `-r`       | `rmconfig`           | `rmconfig-recursive`   | no         |
| `-s`       | `showconfig`         | `showconfig-recursive` | no         |

(1) Needs `portconfig`, or else `dialog4ports`.

`-j` and `-a` are checked for compatibility; either gives the
architecture for `setup_makeconf`.

The make.conf is a temporary file built by `setup_makeconf` for the
given jail, tree (`PTNAME`, `default` when `-p` is absent) and set,
followed by `setup_ports_env` run on the host; it is exported as
`__MAKE_CONF`. It holds none of the jail prelude: no `BATCH`,
`PACKAGE_BUILDING`, `PORTSDIR` or `OVERLAYS` lines. Without `-j`, the
jail part of each name is empty, so names like `-default-make.conf`
are looked up. `fetch_global_port_vars` then runs on the host and
exports `LOCALBASE` and `PREFIX` as `bsd.port.mk` evaluates them under
that make.conf.

Ports come from the command line or from `-f` files through
`listed_ports`, which applies `MOVED` and requires each origin to exist.
For each, `options.sh` runs, in the port's directory of the main tree
(overlays are not consulted):

```sh
env ${flavor:+FLAVOR=$flavor} make PORT_DBDIR=$PORT_DBDIR \
    PORTSDIR=$PORTSDIR -C $PORTSDIR/$origin $COMMAND
```

and then, unless `-n` was given:

```sh
env ${flavor:+FLAVOR=$flavor} make PORT_DBDIR=$PORT_DBDIR \
    PORTSDIR=$PORTSDIR PKG_BIN=$(which pkg-static) \
    DIALOG4PORTS=$(which $d4p) LOCALBASE=/nonexistent \
    -C $PORTSDIR/$origin $RECURSE_COMMAND
```

Only the recursive run is given `DIALOG4PORTS`, `PKG_BIN` and
`LOCALBASE=/nonexistent`; the first run sees the exported `LOCALBASE`.

`testport -c` instead runs `make config` for the tested port only,
with a `setup_makeconf` make.conf and `PORT_DBDIR` set to the reference
jail's `/var/db/ports`: the copy made at jail start, so the choices are
not written back to `poudriere.d`.

## Environment of the metadata queries

Metadata is read by `port_var_fetch`, which runs one
`/usr/bin/make -C <portdir>` with a `-V` for each wanted expression and
any `NAME=value` assignments on the command line. During a build
`MASTERNAME` is set, so the command goes through `injail`, which is
`jexec -U ${JUSER:-root}` into the jail, without `-l`. The port directory comes
from `_lookup_portdir`: the first overlay, in `-O` order, whose
`/overlays/<o>/<origin>/Makefile` is readable, else `/usr/ports/<origin>`.
A flavor is passed as `FLAVOR=<flavor>` on the command line.

What the query sees:

- The jail's `/etc/make.conf` above; builds do not set `__MAKE_CONF`.
- poudriere's exported environment, including `PACKAGE_BUILDING=yes`,
  `HOME=/root`, `USER=root`, `MAKE_OBJDIR_CHECK_WRITABLE=0`, and the
  `LOCALBASE` and `PREFIX` that `fetch_global_port_vars` exports (taken
  from `bsd.port.mk` when not already set), and, for a cross build, the
  `UNAME_*` and `OSVERSION` from `export_cross_env`. `HOME` and `USER`
  are exported by `jail_start`, and `injail` does not clear the
  environment, so the framework sees `USER=root`: with
  `WITH_CCACHE_BUILD` and no `CCACHE_DIR`, `bsd.ccache.mk` falls back to
  `/root/.ccache` instead of disabling ccache.
- `TRYBROKEN=yes` in the environment after `bulk -T`. For `testport -T`
  it is passed only to the tested port's own query (as the per-port
  `port_flags`), so a broken dependency stays ignored.

`update_version_env` also puts `UNAME_r`, `UNAME_v`, `OSVERSION` (and
for a cross build `UNAME_m`, `UNAME_p`) in the jail's `login.conf`
`setenv`; whether `jexec` without `-l` applies it is not verified.

## Ports to build

`read_packages_from_params` takes either origins on the command line,
`-f` files, or `-a`; command-line origins cannot be combined with
either of the others. `_listed_ports` then produces the list:

- With `-a`, every `SUBDIR +=` entry of every category listed in the
  top `Makefile` of the tree and of each overlay, skipping categories
  without a `Makefile` and entries without a directory; the list is
  sorted and made unique when overlays are present. A flavored origin
  with `-a` is an error.
- Otherwise the first word of each line of each `-f` file, skipping
  blank lines and lines starting with `#`, plus the command-line
  origins, with a leading and a trailing `/` trimmed, sorted and made
  unique.

Each entry is an originspec, `origin[@flavor][~subpackage]`.
`_listed_ports` refuses a flavor on a tree without `FLAVORS`, follows
`MOVED` (`EXPIRED` is an error), and requires the origin to exist in an
overlay or the tree; any failure is fatal.

### MOVED

`MOVED` is applied to listed origins only; a dependency on a moved origin
is not rewritten (see `deps_sanity` below). `awk/parse_MOVED.awk` reads
the file once and collapses chains in file order: a later `B|C` repoints
an earlier `A|B` to `A|C` (`update_forwards`). `check_moved` then follows
the collapsed entry: it returns the target if that origin exists, else
looks the target up again, and so on. A line with an empty old origin
(`|B|…`) deletes `B`'s earlier entry. Two consequences:

- `A` goes to `C` even when `B` exists again; `B` is never tested.
- A cycle `a|b`, `b|a` collapses to `a|a` and `b|a`; when `a` does not
  exist, `check_moved` loops forever.

`@-` (`FLAVOR_DEFAULT`) names the default flavor and `@all`
(`FLAVOR_ALL`) every flavor. `FLAVOR_DEFAULT_ALL=yes` makes every listed
origin behave as `@all`, and `-a` builds every flavor
(`build_all_flavors`).

## Gathering metadata

`gather_port_vars` drives the walk with four directory queues:

| Queue    | Holds                                                        |
| -------- | ------------------------------------------------------------ |
| `gqueue` | originspecs to query (`gather_port_vars_port`)               |
| `dqueue` | ports already queried whose dependencies must be queued      |
| `mqueue` | default-flavor lookups needed only to learn a port's flavors |
| `fqueue` | non-default flavors, held until their default was queried    |

A listed origin goes into `gqueue` with the reverse dependency
`listed`. A listed flavored origin goes into `fqueue`, and its plain
origin into `gqueue` as a `metadata` lookup unless it is listed itself.
With `-a` every listed origin is queried directly and no dependency
is queued, since the category walk reaches every default origin and
`build_all_flavors` queues every other flavor in `fqueue`. The loop runs
`dqueue`, then `gqueue`, then promotes `mqueue`, then `fqueue`, until
all four are empty. Each originspec is queried once.

The queues and what moves between them:

```text
                         ┌──────────┐  make -V   ┌──────────┐
listed origin ──────────▸│  gqueue  ├───────────▸│  dqueue  │
plain of listed @flavor ▸│          │  IGNORE:   └────┬─────┘
                         └──────────┘  no deps        │ each _PDEPS
                            ▴  ▴  ▴                   │ entry
                            │  │  └───── plain dep ───┤
                            │  │  ┌──────────┐        │
                            │  └──┤  mqueue  │◂───────┤ plain of @flavor dep
                            │     └──────────┘        │
                            │     ┌──────────┐        │
                            └─────┤  fqueue  │◂───────┘ @flavor dep
listed origin@flavor ────────────▸│          │
                                  └──────────┘
```

`mqueue` moves into `gqueue` only once `gqueue` and `dqueue` are both
empty, and `fqueue` only after that.

### The single make run per port

`deps_fetch_vars` asks for everything in one `port_var_fetch`:

| Expression (`*_ALL` forms with `SUBPACKAGES`) | Kept as                      |
| :-------------------------------------------- | :--------------------------- |
| `PKGNAME` (`PKGNAME.<subpkg>`)                | the package name             |
| `FLAVOR`, `FLAVORS`                           | with the `FLAVORS` feature   |
| `_DEPEND_SPECIALS`, `PKGCATEGORY`, `PREFIX`   | specials, category check     |
| `IGNORE`, `FORBIDDEN`, `NO_ARCH:Dyes`         | ignore reason and flags      |
| `DIST_SUBDIR`, `ALLFILES`                     | distfiles                    |
| `NEED_ROOT`                                   | with `BUILD_AS_NON_ROOT=yes` |
| `LIB_DEPENDS`, `RUN_DEPENDS`                  | raw, `CHECK_CHANGED_DEPS`    |
| sorted `+OPT`/`-OPT` list                     | `CHECK_CHANGED_OPTIONS`      |
| `_BDEPS`, `_RDEPS`, `_PDEPS` as origins       | build, run, all deps         |

The option list is computed on the command line:

```make
_PRETTY_OPTS='${SELECTED_OPTIONS:@opt@${opt}+@} ${DESELECTED_OPTIONS:@opt@${opt}-@}'
-V '${_PRETTY_OPTS:O:C/(.*)([+-])$/\2\1/}'
```

that is, every option name suffixed with `+` or `-`, sorted, then
rewritten as `+NAME` or `-NAME`. It is only asked for when
`CHECK_CHANGED_OPTIONS` is not `no` and the tree has the
`SELECTED_OPTIONS` feature, which `fetch_global_port_vars` adds when
`Mk/bsd.options.mk` mentions `SELECTED_OPTIONS`. The raw dependency
variables are asked for when `CHECK_CHANGED_DEPS` is not `no`.

`_BDEPS` joins `PKG_DEPENDS`, `EXTRACT_DEPENDS`, `PATCH_DEPENDS`,
`FETCH_DEPENDS`, `BUILD_DEPENDS` and `LIB_DEPENDS`; `_RDEPS` joins
`LIB_DEPENDS` and `RUN_DEPENDS`; `_PDEPS` all seven (the `*_ALL` forms
with `SUBPACKAGES`). Each is reduced with

```make
:C,([^:]*):([^:]*):?.*,\2,:C,^${PORTSDIR}/,,:O:u
```

that is, to the second colon field (the origin, with any `@flavor`), stripped of
`${PORTSDIR}/`, sorted and unique. `TEST_DEPENDS` is never read, by
`bulk -t` and `testport` included.

### Checks on the result

`deps_fetch_vars` rejects an empty or invalid `PKGNAME`, and a
`PKGCATEGORY` that is not the origin's own category. A blacklisted port
gets `IGNORE` set to `Blacklisted` when it has none: a non-flavored
origin matches its plain name; a default flavor matches the plain name,
`@-`, `@all` or `@<flavor>`; another flavor matches the plain name,
`@all` or `@<flavor>`. Two originspecs yielding one `PKGNAME` are an
error, except a flavor that resolves to the default flavor's package,
which is dropped as superfluous. An invalid flavor (one not in
`FLAVORS`) is fatal in `gather_port_vars_port`.

`gather_port_vars_port` records `pkgname originspec rdep ignore` in
`all_pkgs`. When a listed default origin has flavors and all flavors are
wanted, every other flavor is queued in `fqueue`. Dependencies are not
followed for a port with an `IGNORE` reason, so an ignored port's
dependencies are queried only when something else needs them
(`test/bulk-ignore-listed.sh`, `test/bulk-ignore-one-dep-but-not-other.sh`).
Otherwise `deps_sanity` refuses a port depending on itself, on an origin
ending in `/`, on a nonexistent origin (naming its `MOVED` entry if
any), or, with `FLAVORS`, on an originspec with an empty flavor. A
refusal is fatal to the whole run (`set_pipe_fatal_error`); a port with
an `IGNORE` reason never reaches `deps_sanity`, so its bad dependencies
abort nothing. Otherwise the port enters `dqueue`.

`gather_port_vars_process_depqueue` takes each entry of the `_PDEPS`
list. Without `-a`, a plain dependency goes to `gqueue`; a flavored one
puts its plain origin in `mqueue` and the flavored originspec in
`fqueue`. A dependency already queried is not queued again.

### IGNORE and BROKEN

Only `IGNORE` is read; `BROKEN` acts through the framework, which turns
it into `IGNORE` unless `TRYBROKEN` is set (not in these sources).
`test/bulk-build-broken.sh` and `test/testport-build-broken.sh` pin that
a `BROKEN` port is ignored, and built with `-T`.

`compute_needed` keeps the `all_pkgs` lines with fewer than four fields
in `all_pkgs_not_ignored`, so a line carrying an ignore reason drops out
of the queue. `trim_ignored` reports each ignored package and calls
`clean_pool`, which marks every port depending on it as skipped, except
a listed non-default flavor of an `@all` request, which is marked
ignored instead.

## The build queue

`generate_queue_pkg` creates a `build` and a `run` job for every
non-ignored package, with `run:P` depending on `build:P`. For each
origin in the build list it adds `build:P → run:D`; for each origin in
the run list, both `build:P → run:D` and `run:P → run:D`. So
`LIB_DEPENDS` and `RUN_DEPENDS` must be installable before a port
builds, and run dependencies are transitive for installation. A
dependency whose `PKGNAME` is unknown is fatal.

```text
D in P's build list (PKG EXTRACT PATCH FETCH BUILD LIB_DEPENDS):

  build:D ──▸ run:D ──▸ build:P ──▸ run:P

D in P's run list (LIB_DEPENDS, RUN_DEPENDS):

  build:D ──▸ run:D ──┬─▸ build:P ──▸ run:P
                      └──────────────────▴

  A ──▸ B: job A must finish before job B starts
```

With `CHECK_CHANGED_DEPS` on, it also checks that the package name in
each versioned `RUN_DEPENDS` entry (`name>=ver:origin`) is that of the
origin's real `PKGNAME`; a mismatch is a warning, or fatal with
`BAD_PKGNAME_DEPS_ARE_FATAL=yes` (`test/bulk-bad-dep-pkgname.sh`).

After the incremental checks below, `pkgqueue_unqueue_existing_packages`
removes the build job of every package that still exists, and, when
`TRIM_ORPHANED_BUILD_DEPS=yes` (the default) and not with `-a`,
`pkgqueue_trim_orphaned_build_deps` drops build jobs not reachable
through run dependencies from the listed packages and `ports-mgmt/pkg`
(`test/bulk-build-inc-trim_orphaned_build_deps*.sh`).

## Incremental rebuild

`prepare_ports` first deletes everything when the jail's version differs
from `PACKAGES/.jailversion`, or with `-c`; with `-C`, it deletes the
listed packages that are not ignored (with `-a`, `-C` becomes `-c`).
`delete_old_pkgs` then runs `delete_old_pkg` on every file in
`PACKAGES/All`. Whether a package nobody asked for is deleted follows
`DELETE_UNQUEUED_PACKAGES`: `always`; `yes` only for `-a`, or for `-f`
when not testing (not `testport`, `bulk -t` or `-C`); `no` (the default)
never.

`_delete_old_pkg` deletes a package, stopping at the first reason, when:

1. its package base matches `FORCE_REBUILD_PACKAGES`;
2. with `DELETE_UNKNOWN_FILES=yes`, it is a dead symlink or not a
   package file;
3. the port is `FORBIDDEN`;
4. its origin cannot be read;
5. neither its package base nor its originspec is needed and not
   ignored (only when unqueued packages may be deleted; otherwise it is
   left alone);
6. its origin is in `MOVED`, or no longer exists;
7. its originspec was not queried this run (a stale origin sharing the
   package base);
8. the package base changed;
9. the version changed;
10. the ABI changed (`*` for the architecture when `NO_ARCH`);
11. the flavor recorded in the package differs from the port's, or is
    no longer in `FLAVORS`;
12. its dependency list cannot be read;
13. `CHECK_CHANGED_DEPS` finds a new dependency;
14. `CHECK_CHANGED_OPTIONS` finds different options;
15. a recorded dependency's package base is not one the dependency's
    origin produces (`origin_has_pkgbase`).

### Changed dependencies

With `CHECK_CHANGED_DEPS` not `no` (default `yes`), the raw
`RUN_DEPENDS`, plus `LIB_DEPENDS` on a tree without the
`AUTO_LIB_DEPENDS` feature, are tested entry by entry. An entry already
satisfied by the jail's base system is skipped: a library found in
`/lib` or `/usr/lib`, an absolute path that exists in the jail, or a
command `which` finds. The origins of the rest must appear among the
origins recorded in the package (after `MOVED`), or, for a flavored
dependency, its package base among the recorded package names.
Otherwise the package is deleted with "new dependency".

### Changed options

With `CHECK_CHANGED_OPTIONS` not `no` (default `verbose`), the options
recorded in the package are read by `pkg_get_options`
(`include/pkg.sh`): `pkg query -F <file> '%Ok %Ov'`, sorted, each line
turned into `+KEY` for `on`/`true` and `-KEY` for `off`/`false`, joined
by spaces. The current options are the `+OPT`/`-OPT` list from the gathering
query (from `make pretty-print-config` in the jail on a tree without
`SELECTED_OPTIONS`). The strings must be equal, or the package is
deleted with "changed options"; `verbose` prints both (`Pkg:`, `New:`).
These are pkg's recorded options, not an annotation. No test in `test/`
sets `CHECK_CHANGED_OPTIONS`.

### Recursive deletion

Unless `-S` (`SKIP_RECURSIVE_REBUILD`), `sanity_check_pkgs` then
repeats until nothing changes: a needed package is deleted when a
dependency's package file is missing (with `PKG_NO_VERSION_FOR_DEPS` on,
only if that dependency is not queued), or when it has unversioned
dependencies and `PKG_NO_VERSION_FOR_DEPS=no`.
`test/bulk-build-inc-delete_old_pkg-changed-dep-pkgname.sh` renames a
dependency's package through a make.conf option (`misc_foo_SET= RENAME`)
and expects its dependent rebuilt by the package-base check (15).

## Likely defects

Behaviour that reads as a defect rather than a design choice: poudriere
loops, gives a result its own code intends otherwise, treats two
equivalent inputs differently, or contradicts its manual. Each item is
described where the link points.

Items marked **radical** change what gets built (options, dependencies,
which origin), or make poudriere fail on a file a configuring tool writes
(an options file); a tool predicting what poudriere will do follows them,
and says so, until they are fixed upstream. An item where poudriere fails on
its own input (a malformed configuration or list the user wrote) is not
marked: that input is the user's to fix. Unmarked defects are not to be
reproduced.

**Loops and wrong results**

- A `MOVED` cycle such as `a|b`, `b|a` collapses to `a|a`, and
  `check_moved` loops forever when `a` does not exist: poudriere failing on
  its own input, the tree's MOVED ([MOVED](#moved)).
- **Radical.** `parse_MOVED.awk` collapses chains in file order, so `A|B`
  followed by `B|C` sends `A` to `C` even when `B` exists again;
  `check_moved` never tests `B` ([MOVED](#moved)).
- A line with an empty old origin (`|cat/foo|…`) deletes `cat/foo`'s
  entry but leaves its record among those leading to its target: after
  `cat/foo|cat/bar`, `|cat/foo|…`, `cat/bar|cat/baz`, the awk prints
  ` cat/baz` with no source, `load_moved` stores `cat/baz` as moved to
  nothing, and a listed `cat/baz` aborts the run in
  `test_port_origin_exist ""` ([MOVED](#moved)).
- In `_delete_old_pkg`, the `originspec_decode` after `return 0` on a
  `MOVED` origin can never run.

**Inconsistent treatment**

- **Radical.** `include_poudriere_confs` scans the arguments before the
  sub-command defaults the tree to `default`, so a build without `-p` reads
  neither `default-poudriere.conf` nor `<jail>-default-poudriere.conf`,
  while `setup_makeconf`, called later, does read `default-make.conf` and
  `<jail>-default-make.conf` ([Configuration files](#configuration-files),
  [The layered files](#the-layered-files)).
- `poudriere options` gives `LOCALBASE=/nonexistent`, `DIALOG4PORTS` and
  `PKG_BIN` to its recursive `make` run only; the run on the named port
  itself sees the exported `LOCALBASE`
  ([poudriere options](#poudriere-options)).
- `poudriere options` without `-j` looks up layer names with an empty jail
  part, such as `-default-make.conf`, and runs make in the main tree only,
  so a port that exists only in an overlay cannot be configured
  ([poudriere options](#poudriere-options)).
- `test/common.bulk.sh` passes `-O` to `poudriere options`, whose
  `getopts` string has no `O` ([Tests](#tests-on-options-and-dependencies)).

**Documentation**

- `poudriere.8` says the options directory is null-mounted into the jail;
  the code copies it into the reference jail and null-mounts that copy
  into the builders
  ([Which directory a build reads](#which-directory-a-build-reads)).
- `poudriere-options.8` says `-o` expands to `$POUDRIERE_ETC/name`; the
  code writes `$POUDRIERE_ETC/poudriere.d/name`, with no `-options`
  suffix, a name no build looks for unless the user adds the suffix
  ([poudriere options](#poudriere-options)).

**Questionable rather than wrong**

- `testport -c` writes the choices into the reference jail's copy of the
  options directory, so they are used for that test and then lost, never
  saved to `poudriere.d`.
- `POUDRIERE_ETC` taken from the environment skips the check that `-e`
  makes that the path is a directory.

## Tests on options and dependencies

| Test file (in `test/`)                    | What it pins                    |
| :---------------------------------------- | :------------------------------ |
| `options-smoke.sh`, `options-overlays.sh` | `options -s -n` passes          |
| `options-badorigin.sh`                    | ... and fails on a bad origin   |
| `port_var_fetch.sh`                       | assignments, origin reduction   |
| `originspec.sh`                           | originspec encode and decode    |
| `bulk-build-ignored.sh`                   | `IGNORE` holds even with `-T`   |
| `bulk-ignore-*.sh`                        | ignored ports, their dependents |
| `bulk-skipped-and-needed-dep.sh`          | skipped dependents              |
| `bulk-flavor-*.sh`                        | `@flavor`, `@all`, metadata     |
| `bulk-flavor-nonexistent.sh`              | a bad flavor fails              |
| `bulk-build-specific-bad-flavor.sh`       | same                            |
| `bulk-MOVED-*.sh`                         | `MOVED` on listed origins       |
| `bulk-overlay-*.sh`                       | which tree's copy is used       |
| `bulk-no-overlay-specific.sh`             | same                            |
| `bulk-build-inc-*.sh`                     | incremental deletion, trimming  |
| `bulk-build-FORCE_REBUILD_PACKAGES.sh`    | forced rebuild by package base  |

`set_make_conf` and `set_poudriere_conf` in `test/common.bulk.sh` write
`<jail>-<tree>-<set>-make.conf` and `-poudriere.conf`. Its `do_options`
passes `-O` for configured overlays, which `options.sh` does not accept;
how `options-overlays.sh` passes is not verified.

## Sources

poudriere git `3b268b38` (2026-10-19). Files read, relative to the
upstream tree:

- `src/bin/poudriere.in`, `src/etc/poudriere.conf.sample`
- `src/share/poudriere/common.sh` (every function named on this page,
  and its defaults), `bulk.sh`, `testport.sh`, `options.sh`,
  `include/pkg.sh`, `include/pkgqueue.sh`
- `src/man/poudriere.8`, `poudriere-options.8`, `poudriere-bulk.8`
- `test/common.bulk.sh` and the test files named above
