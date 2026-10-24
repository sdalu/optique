# Where optique reads its configuration

optique resolves the options directory, make.conf and the blacklist
exactly as `poudriere(8)` would before a build, as `synth(1)` would
under `-s`, or as `make config` would for a [local build](#local-builds),
and in the same order; see [Commands](commands.md) for the global flags
(`-z`, `-j`, `-p`, `-P`, `-l`, `-s`, `-o`) that steer the resolution. It
writes options files only in the resolved options directory, and only when
you apply in the TUI or run `sync`, `decide` or `clean`; its query cache and
the TUI's drafts live under `~/.cache/optique`.

## The options directory

Given `-j jail`, `-p tree` (default `default`) and `-z set`, optique
tries these candidate names under `/usr/local/etc/poudriere.d/`, in
order, and the first one that exists wins:

```text
<jail>-<tree>-<set>-options
<jail>-<set>-options
<jail>-<tree>-options
<tree>-<set>-options
<set>-options
<tree>-options
<jail>-options
options
```

If none exists, applying creates the one `poudriere options` would have
used for the same flags: `[<jail>-][<tree>-][<set>-]options`, with the
tree component only when `-p` was given explicitly.

`-o dir` bypasses this resolution entirely and uses `dir` verbatim.

The existence of `/usr/local/etc/poudriere.d` is what selects poudriere.
Without it, or under `-l`, optique configures a
[local build](#local-builds) instead, and giving `-j`, `-z` or `-p`
without it is an error.

`POUDRIERE_ETC`, set and non-empty, moves poudriere's configuration as
poudriere itself does: `poudriere.conf` and `poudriere.d` are read from
that directory, which must exist, in place of `/usr/local/etc` in every
path on this page, and the startup banner notes it. Without a
`poudriere.d` there, the build is a local one. It is not read under `-l`
or `-s`.

## The ports tree

`-p tree` (default `default`) is read from
`/usr/local/etc/poudriere.d/ports/<tree>/mnt`. A tree poudriere does not
know is an error, so a typo is not quietly answered from another tree;
only the implicit `default` falls back to `/usr/ports` when poudriere.d
names none. `-P dir` bypasses the lookup and uses `dir`, which must be a
ports tree; it cannot be combined with `-p`.

## make.conf layers

make.conf is layered in poudriere's inclusion order, least to most
specific, with every existing file merged — not just the first match:

```text
make.conf
<set>-make.conf
<tree>-make.conf
<jail>-make.conf
<tree>-<set>-make.conf
<jail>-<tree>-make.conf
<jail>-<set>-make.conf
<jail>-<tree>-<set>-make.conf
```

After them, as poudriere appends them, come the hook plugins'
`hooks/plugins/*/make.conf` files, in name order; the startup banner
lists them among the `make.conf:` sources.

Port queries therefore see exactly what a poudriere build would see,
including `DEFAULT_VERSIONS` (which can change a port's default
options, its option list and its dependencies), `OPTIONS_SET`,
`OPTIONS_UNSET`, the per-port `<PORT>_SET`/`<PORT>_UNSET`, the `*_FORCE`
knobs, and `WITH`/`WITHOUT`, which bsd.options.mk applies after them and
which pin an option as they do.
As in `bsd.options.mk`, an UNSET entry, in make.conf or an options file,
is a make pattern (`*`, `?`, `[...]`) that turns off every option it
matches: `OPTIONS_UNSET=A*` turns off A and A2, and decides both. A SET
or `WITH` entry turns on, and decides, only the option it names. make
also counts every option a SET glob matches as decided, though it turns
none on; that upstream defect is not followed, so `OPTIONS_SET=A*` leaves
A2 undecided.

The layers are concatenated into one staged file, as poudriere itself
concatenates them into the jail's make.conf, so an `.include` with a
relative path does not find its file here any more than it would in the
jail (synth's profile make.conf is handled the same way, as synth appends
it to each builder's).

The query cache is keyed on the text of the make.conf files and, per
answer, on each file they `.include` that make read for that port, so an
edit to an included file is seen at the next run. A file a `.sinclude`
skipped because it did not exist is not: after creating one, run with
`--clear-cache` once ([the cache](how-it-works.md#what-it-does-not-see)).

## A make.conf moving the options

Every query is given the options directory as `PORT_DBDIR` on make's
command line, where it wins over make.conf; a build is given none. So
once per run, with the make.conf settled, optique asks make, reading no
port's Makefile and in the queries' own environment, what that make.conf
alone sets of `PORT_DBDIR`, `OPTIONS_FILE` and `OPTIONS_NAME` (`make -f /dev/null -V
...`; outside bsd.port.mk nothing else sets them). make runs from a
port's directory, `ports-mgmt/pkg` (else the tree's top), so that a
make.conf testing `.CURDIR` (`.if ${.CURDIR:M*/ports/*}`) answers as it
does for a build. For a local build make is given the environment's
`PORT_DBDIR` too, which a make.conf `PORT_DBDIR?=` defers to, as in the
user's own make; a builder's environment is not the user's, and is not
given.

A `PORT_DBDIR` other than the directory the build reads options from —
the options directory itself for a local build, the jail's
`/var/db/ports` where poudriere puts it, the scan slave's `/options`
where synth mounts it — sends the whole build to a directory none of the
files optique reports and edits is in: every port is built from whatever
it holds. The banner says so, even under `-q`:

```text
error: make.conf sets PORT_DBDIR=/var/db/myports: the build reads that directory, not /usr/local/etc/poudriere.d/options
```

`scan` counts it as an error (in its JSON `errors`, subject `make.conf`),
so it exits 1; `sync`, `decide` and `clean` go on, writing to the options
directory being harmless, only useless. A local build without `-o` takes
its options directory from that same `PORT_DBDIR`, so it never trips
this; with `-o` naming another directory it does.

An `OPTIONS_FILE` set there, or an `OPTIONS_NAME` other than
bsd.options.mk's own `${PKGORIGIN:S/\//_/}`, moves options files away from
the names `clean` maps back to ports, so `clean` removes none as vanished
or unused ([commands.md](commands.md#clean)). Per port, `scan` and `sync` already
see a moved `OPTIONS_FILE` and leave that port to make
([commands.md](commands.md#sync)). When make cannot be asked, the banner
notes it and `clean` acts as if `OPTIONS_FILE` were set.

## The jail environment

The merged layers above are wrapped in the environment of a poudriere
jail, not the host's. They come after the lines poudriere puts at the
top of every jail's make.conf — `PACKAGE_BUILDING`,
`PACKAGE_BUILDING_FLAVORS`, `BATCH`, `USE_PACKAGE_DEPENDS` — and are
followed by `LOCALBASE` set to `/nonexistent`. A jail has nothing
installed when poudriere reads a port, while the host running optique
may; without this, a port Makefile that probes installed files would
answer for the host instead, as www/apache24 does when it marks LDAP
`IGNORE` because the host's apr-util was built without it.

When `NO_PACKAGE_BUILDING` is set in poudriere.conf — or in the
poudriere.d files poudriere reads after it, for the jail, tree and set
given (a tree's file only when `-p` names it) — `PACKAGE_BUILDING` and
`PACKAGE_BUILDING_FLAVORS` are left out, as poudriere itself leaves them
out. Only plain assignment lines are read; a value set by shell logic in
those files is not seen. The startup banner's
`environment:` line says which applies: a plain
`poudriere jail (host packages under /usr/local not seen)`, or the same
line with `; NO_PACKAGE_BUILDING set` appended.

`FLAVOR_DEFAULT_ALL=yes`, read from the same files the same way, makes
a bare origin listed stand for every flavor of its port, as `origin@all`
does, as poudriere builds them; a flavor given (`origin@-` included)
still names that flavor alone. The banner notes it.

When `CCACHE_DIR`, read the same way, names an existing directory,
`WITH_CCACHE_BUILD=yes` and `CCACHE_DIR=/root/.ccache` are appended after
the layers, as poudriere's `setup_ccache` does.

Queries run with `USER=root` and `HOME=/root`, as poudriere's
`jail_start` exports them for its builders. It matters for
`bsd.ccache.mk`: under `WITH_CCACHE_BUILD` with no `CCACHE_DIR`, it keeps
ccache, and the devel/ccache build dependency, for root alone, and
disables it for anyone else.

Given `-j jail`, queries also see the jail's system rather than the
host's, from poudriere.d's `jails/<jail>/arch`, `version` and `mnt`: the
prelude sets `_OSRELEASE`, `OSREL` and `OSVERSION` for the jail
(`OSVERSION` from `__FreeBSD_version` in `<mnt>/usr/include/sys/param.h`,
else derived from the version, 11.2 giving 1102000, with a note in the
banner), plus the cross-build lines `MACHINE`, `MACHINE_ARCH` and `ARCH`
whenever the jail's MACHINE_ARCH differs from the host's — including an
armv6/armv7 jail on an arm64 host the kernel can run directly, where
poudriere itself skips them because it then runs the jail's own 32-bit
`make`; optique always runs the host's, so the query still needs the
jail's MACHINE_ARCH. On a 15 host an 11.2 jail's security/ca_root_nss
offers `ETCSYMLINK`, which the port excludes on 15. A jail without an
arch or version file leaves the host's system, with a note.

A [local build](#local-builds) has no jail wrapping at all: queries see
the host as it is, which is then the machine that builds.

## The blacklist

The blacklist chain is read with the same jail/tree/set expansion as
make.conf, and again every existing file contributes entries rather
than only the first match:

```text
blacklist
<set>-blacklist
<tree>-blacklist
<jail>-blacklist
<tree>-<set>-blacklist
<jail>-<tree>-blacklist
<jail>-<set>-blacklist
<jail>-<tree>-<set>-blacklist
```

Under `-j`, when this host's kernel cannot run the jail's architecture
natively (`kern.supported_archs`, an armv6 jail excepted on an
armv7-capable kernel) and poudriere would therefore build it under
qemu, the `qemu-blacklist` layer is loaded last, as poudriere always
loads it then, whatever poudriere.d itself says.

Each line holds one or more whitespace-separated port origins, and `#`
starts a comment running to the end of the line. There are no globs —
entries are matched literally, exactly as poudriere matches them: a
port with no flavors matches only its bare origin; a flavored port
matches `origin`, `origin@all`, or `origin@<its flavor>`, and its
default flavor (the first `make` offers) also matches `origin@-`.
Blacklisted ports are marked in `scan` and in the TUI, never demand
attention, and never trip `scan`'s exit-status gate, whatever their
status — poudriere will not build them, so their options cannot hold a
build up. A dependency loop trips the gate unless every one of its
members is blacklisted (see [Dependency loops](dependency-loops.md)).
A blacklisted port's own dependencies are not walked, as poudriere
gathers none for it, so a port reached only through one is left out of
the closure. An `IGNORE`d port is still walked: its `IGNORE` may depend
on the very options being configured.

## Synth

`-s [profile]` reads the `synth(1)` layout instead, so the same
commands serve a synth host. The profile defaults to the one synth
itself would run with: the environment's `SYNTHPROFILE` when synth.ini
has a section for it that sets `Directory_packages`, then synth.ini's
`profile_selected`, then `LiveSystem`; the banner's note names which
chose it. Because the profile name is
optional, a name that could be read as something else (a subcommand, an
option, or anything containing a slash) must be attached, as
`--synth=name`. `-s` is mutually exclusive with `-z`, `-j`, `-p` and
`-l`.

The ports tree comes from `Directory_portsdir` and the options
directory from `Directory_options`, both read from the profile's
section of `/usr/local/etc/synth/synth.ini` alone, as synth reads them
(`[Global Configuration]` never stands in for a profile). A profile
without `Directory_portsdir` takes the first that exists of `PORTSDIR`,
make's `PORTSDIR`, `/usr/dports` and `/usr/ports`, in that order, as synth
does, a note naming which. make is asked as synth asks it, `make -f
/usr/share/mk/bsd.port.mk -V PORTSDIR` from the directory optique runs
in; that file falls back to a tree found above that directory, else to
`/usr/ports`, so `/usr/dports` is reached only when make fails. A profile
without `Directory_options` takes `/var/db/ports`. A missing synth.ini
reads as empty; one that cannot be read is an error, since synth will not
run without it.

optique reads synth.ini as synth's own reader reads it, defects
included, since that decides which settings synth uses: a section header
is `[name]` with its `[` in the first column, the name running untrimmed
to the first `]` and anything after it ignored, so `[ name ]` names the
section ` name ` and an indented header is none, its lines staying in the
section above; a UTF-8 byte-order mark is part of the first line, so a
header there is none and the lines after it are in no section; keys and
values are trimmed of spaces only, a tab or the CR of a CRLF line end
staying part of them; and a key's first match within its section wins, a
bare or empty one included, so it ends the search there rather than
reading on to a later, real assignment of the same key. Each line synth
reads otherwise than it seems to say is named in a note, saying it is
read as synth reads it. The file is read as bytes, as synth reads it: a
path setting keeps them, so a path in Latin-1 is the directory synth
uses. A section header with no
`]` is no header and is skipped; synth fails to load such a file, which is
left to the user to fix.
make.conf is the single file
`/usr/local/etc/synth/<profile>-make.conf` — the one synth appends to
each builder's make.conf — or none at all when the profile has none (the
host's `/etc/make.conf` is never read), so there is no layering and no
blacklist under `-s`.

synth reads each port in a chroot whose `/usr/local` is empty, so the
queries do too: the profile's file comes after the lines synth puts at
the top of that chroot's make.conf (`SYNTHPROFILE`, `PACKAGE_BUILDING`,
`BATCH`, `USE_PACKAGE_DEPENDS_ONLY`; no `PACKAGE_BUILDING_FLAVORS`;
`WITH_CCACHE_BUILD` and `CCACHE_DIR` when the profile's
`Directory_ccache` exists, so ports gain devel/ccache as a build
dependency as they do in synth) and is followed by
`LOCALBASE=/nonexistent`. Without it a port Makefile
that probes installed files would answer for the host: with php82
installed, `Mk/Uses/php.mk` reads its `php.conf` and every `php84-*`
extension builds against `lang/php82`. The startup banner's
`environment:` line reads
`synth scan slave (host packages under /usr/local not seen)`. Queries run
with `USER=root` and `HOME=/root`, as synth's builders do.

`-o dir` and `-P dir` still override the options directory and the
ports tree.

When no ports are given at all (no origins, no `-f`), synth mode uses
the installed packages as the list, mirroring how synth builds what is
installed: one root per installed package, its origin from
`pkg query -a %o` and its flavor from that package's `flavor`
annotation, so several flavors of one origin installed side by side
(polkit-qt-1 for qt5 and qt6) are each a root. As synth matches them
against its flavor index, a package whose origin is not in the tree, or
a flavored port's package with no flavor annotation or a flavor the port
no longer has, is skipped, all of them named in one note; a port without
flavors is rooted as its bare origin whatever the annotation. This
applies to the TUI, `scan`, `sync` and `clean --unused` (see
[Commands](commands.md)). `--repo name` is repeatable and restricts that
implicit list to packages installed from the named pkg repositories,
e.g. `--repo Synth` to consider only synth-built packages; the names are
listed by `pkg query -a %R`.

A list given is read poudriere's way in every mode. synth's own list
syntax is stricter, taking only its flavor index's keys verbatim; a list
synth refuses is left to the user to fix, and optique says nothing of it.

synth refuses a build when the saved options file of a port without
flavors, `<category>_<name>/options`, names an option the port lacks or
has more or fewer `OPTIONS_FILE_SET`/`OPTIONS_FILE_UNSET` entries than
the port has options, every line counted. Under `-s`, `scan` marks such
a port `[synth refuses the file: reason]` and counts it as needing
attention in its exit status, whatever its status; `sync` rewrites the
file.

synth's check of a cached options file also reads bytes 17 to 20 of every
line without checking its length, and aborts on a shorter one. So what
optique writes, in every mode, never holds one: it writes no
`_OPTIONS_READ` line, which nothing reads, and its `# Options for
<pkgname>` comment ends in `, by optique`. A file optique did not write is
left as it is.

When `Directory_options` is the live `/var/db/ports`, minimal and
redundancy cleanups also affect plain make config workflows.

## Overlays

An overlay is a ports tree laid over the main one, holding only the
ports it adds or replaces (and possibly `Mk/Uses` files or a `MOVED`).
poudriere registers it like any tree, `poudriere ports -c -p name`, and
`poudriere bulk -O name` stacks it (poudriere 3.4.0 or later).
`poudriere options` has no `-O`, so optique is the way to configure an
overlay's ports:

```sh
optique scan -z server -O myoverlay -f list
```

`-O name` (repeatable) is read from
`/usr/local/etc/poudriere.d/ports/<name>/mnt`; `--overlay-dir dir` is the
literal counterpart, as `-P` is of `-p`. Both keep command-line order:
the first shadows the next and the main tree. Neither needs `Mk/`: an
overlay holds a handful of ports. They reach make as `OVERLAYS+=` lines
where poudriere writes them in the jail's make.conf, after its
`USE_PACKAGE_DEPENDS` and `BATCH` lines, before `PACKAGE_BUILDING` and the
poudriere.d layers, with the host's paths; the overlay
list is then what make reports for that make.conf, so an `OVERLAYS` set
by a layer counts too, and the startup banner lists them in order.

A port is taken from the first overlay holding `<origin>/Makefile`, else
from the main tree, as the ports framework looks dependencies up: a port
only an overlay holds is found, as a root or a dependency, and `clean`
counts a port as vanished only when no tree holds it. `scan` ends the row
of a port an overlay answers for with `[overlay name]`, and the TUI's
option pane names it on its bottom border. `MOVED` is the main tree's then each overlay's, read
as one file: a chain may run across them, and an origin listed twice
keeps its first entry, with a warning. The options directory, the
make.conf layers and the blacklist do not depend on overlays.

A local build takes `OVERLAYS` from `/etc/make.conf` or the environment,
as make does; `--overlay-dir` comes before those. `-O` needs poudriere.d
and conflicts with `-l` and `-s`; `--overlay-dir` conflicts with `-s`.

## Local builds

Without `/usr/local/etc/poudriere.d`, or under `-l` whether it exists or
not, optique configures ports for a build on this host, the way
`make config` does:

- the ports tree is `PORTSDIR` and the options directory `PORT_DBDIR`,
  both as `make(1)` sees them, from the environment or `/etc/make.conf`,
  falling back to `/usr/ports` and `/var/db/ports`; the startup banner
  notes a value that came from either variable, and `-P` and `-o`
  override them;
- the one make.conf layer is `/etc/make.conf`, or the file
  `__MAKE_CONF` names in the environment, read in place so that its
  relative `.include`s resolve, and keyed with them
  ([make.conf layers](#makeconf-layers));
- queries see the host's installed packages under `/usr/local`, since
  they are what the build will find (no `LOCALBASE=/nonexistent`);
- queries run as the invoking user, with its `USER` and `HOME`, not as a
  builder's root, and that `USER` is part of the cache key;
- queries do not see optique's environment: a make variable exported in
  the shell (`OPTIONS_SET`, `WITH_DEBUG`, `DEFAULT_VERSIONS`, ...) does
  not reach them, since the cache could not key it; set it in the
  make.conf instead;
- there is no blacklist.

`-l` is the way to configure a local build on a host that also runs
poudriere:

```sh
optique scan -l -f list
```

`-l` is mutually exclusive with `-z`, `-j`, `-p` and `-s`. Poudriere
builds ignore `PORTSDIR` and `PORT_DBDIR`, synth builds too but for a
profile without `Directory_portsdir`, and whatever the build, every query
is given the ports tree and options directory optique resolved, so a
`PORTSDIR` in the environment cannot send `make` to another tree.
Whatever the build, a query runs with an environment of `PATH`, `TMPDIR`
and `PKG_DBDIR` (those set), `USER` and `HOME` as above (root's for a
poudriere or synth build, the invoking user's for a local one), and no
other variable.
