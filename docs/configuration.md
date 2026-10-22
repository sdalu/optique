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

Port queries therefore see exactly what a poudriere build would see,
including `DEFAULT_VERSIONS` (which can change a port's default
options, its option list and its dependencies), `OPTIONS_SET`,
`OPTIONS_UNSET`, the per-port `<PORT>_SET`/`<PORT>_UNSET`, and `*_FORCE`
knobs.

The layers are concatenated into one staged file, as poudriere itself
concatenates them into the jail's make.conf, so an `.include` with a
relative path does not find its file here any more than it would in the
jail (synth's profile make.conf is handled the same way, as synth appends
it to each builder's).

**Limitation:** the query cache is keyed on the text of the make.conf
files, not of the files they `.include`. After editing an included file,
run with `--clear-cache` once, or its change is not seen until something
else invalidates the cached answers.

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

Each line holds one port origin, `#` starts a comment, and `*` globs
are allowed (`www/nginx*`). Blacklisted ports are marked in `scan` and
in the TUI, never demand attention, and never trip `scan`'s exit-status
gate, whatever their status — poudriere will not build them, so their
options cannot hold a build up. A dependency loop trips the gate unless
every one of its members is blacklisted (see
[Dependency loops](dependency-loops.md)).

## Synth

`-s [profile]` reads the `synth(1)` layout instead, so the same
commands serve a synth host. The profile defaults to synth.ini's
`profile_selected`, then `LiveSystem`. Because the profile name is
optional, a name that could be read as something else (a subcommand, an
option, or anything containing a slash) must be attached, as
`--synth=name`. `-s` is mutually exclusive with `-z`, `-j`, `-p` and
`-l`.

The ports tree comes from `Directory_portsdir` and the options
directory from `Directory_options`, both read from the profile's
section of `/usr/local/etc/synth/synth.ini`, falling back to that
file's `[Global Configuration]` section and then to `/usr/ports` and
`/var/db/ports`. make.conf is the single file
`/usr/local/etc/synth/<profile>-make.conf` — the one synth appends to
each builder's make.conf — so there is no layering and no blacklist
under `-s`. There is no jail environment either: synth builds on the host
it configures, so the host's installed packages are part of what it
builds against.

`-o dir` and `-P dir` still override the options directory and the
ports tree.

When no ports are given at all (no origins, no `-f`), synth mode uses
the installed packages as the list, mirroring how synth builds what is
installed: the origins come from `pkg query -a %o`, with flavors from
the pkg `flavor` annotation. This applies to the TUI, `scan`, `sync`
and `clean --unused` (see [Commands](commands.md)). `--repo name` is
repeatable and restricts that implicit list to packages installed from
the named pkg repositories, e.g. `--repo Synth` to consider only
synth-built packages; the names are listed by `pkg query -a %R`.

When `Directory_options` is the live `/var/db/ports`, minimal and
redundancy cleanups also affect plain make config workflows.

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
  relative `.include`s resolve (the cache limitation under
  [make.conf layers](#makeconf-layers) still applies);
- queries see the host's installed packages under `/usr/local`, since
  they are what the build will find (no `LOCALBASE=/nonexistent`);
- there is no blacklist.

`-l` is the way to configure a local build on a host that also runs
poudriere:

```sh
optique -l scan -f list
```

`-l` is mutually exclusive with `-z`, `-j`, `-p` and `-s`. Poudriere and
synth builds ignore `PORTSDIR` and `PORT_DBDIR`, and whatever the build,
every query is given the ports tree and options directory optique
resolved, so a `PORTSDIR` in the environment cannot send `make` to
another tree.
