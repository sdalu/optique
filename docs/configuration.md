# Where optique reads its configuration

optique resolves the options directory, make.conf and the blacklist
exactly as `poudriere(8)` would before a build, or as `synth(1)` would
under `-s`, and in the same order; see [Commands](commands.md) for the
global flags (`-z`, `-j`, `-p`, `-s`, `-o`) that steer the resolution. It
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

Without a poudriere.d at all, and without `-j`/`-z`, the options
directory falls back to plain `/var/db/ports` — see
[Without poudriere](#without-poudriere) below. Giving `-j` or `-z` when
`/usr/local/etc/poudriere.d` does not exist is an error.

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

Without a poudriere.d (plain `/var/db/ports`), there is no jail
wrapping at all: queries see the host as it is, which is then the
machine that builds — see [Without poudriere](#without-poudriere).

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
`--synth=name`. `-s` is mutually exclusive with `-z` and `-j`.

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

`-o dir` still overrides the options directory. `-p` names a poudriere
tree and is therefore ignored under `-s`, which the startup banner
notes.

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

## Without poudriere

When `/usr/local/etc/poudriere.d` does not exist and neither `-j` nor
`-z` is given, optique falls back to plain `/var/db/ports` as the
options directory, with no jail wrapping and no blacklist: queries see
the host's own environment, which is then the machine that builds.
`-p`/`--ports-tree` still falls back to `/usr/ports` when no poudriere
tree points elsewhere. `-o dir` overrides the options directory in this
case as in every other.
