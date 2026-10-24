# The ports framework

A plain ports-tree build is `make` run in a port's directory, with no
poudriere or synth around it. All it knows about options and dependencies
comes from one parse: `sys.mk` and the make.conf it pulls in, the port's
`Makefile`, then the framework under `Mk/`. This page follows that parse,
as the files listed under [Sources](#sources) implement it.

## Finding make.conf

`make` reads the system makefile `sys.mk` before any other file (make(1),
FILES; make.conf(5), DESCRIPTION). In `/usr/share/mk/sys.mk`, outside the
`%POSIX` branch, the "Pull in global settings" block does:

```make
__MAKE_CONF?=/etc/make.conf
.if exists(${__MAKE_CONF})
.include "${__MAKE_CONF}"
.endif
```

`__MAKE_CONF` is assigned with `?=`, so a value from the environment or the
command line picks another file; make.conf(5) gives `/dev/null` as the way
to reset every control to its default. A missing file is skipped. The
same block then `.sinclude`s `local.sys.mk`. make.conf is read before the
port's `Makefile`, so a port's plain assignments override it and its `?=`
defaults do not.

make(1) ("VARIABLES") ranks variables, lowest first: environment, global
(any makefile, make.conf included), command line, target-local. An
environment variable loses to any make.conf or Makefile assignment unless
`make -e` is used; `make VAR=value` beats both. Command-line assignments
are appended to `.MAKEFLAGS`, which make exports to every program it runs,
so they reach sub-makes. `bsd.port.mk` adds its own: in the block guarded
by `!defined(NOPRECIOUSMAKEVARS)` it appends each variable of
`_EXPORTED_VARS` (`ARCH`, `OPSYS`, `_OSRELEASE`, `OSREL`, `OSVERSION`,
`_PKG_CHECKED`, `_SMP_CPUS` and a few more) not already there, so child
makes do not recompute them.

### Where the tree and the databases are

A port `Makefile` ends with `.include <bsd.port.mk>`, which is
`/usr/share/mk/bsd.port.mk`. When `PORTSDIR` is not already defined, it
probes `.`, `..`, `../..` and `../../..` under `.CURDIR` for
`Mk/bsd.port.mk`, falls back to `/usr/ports`, resolves the result with
`:tA`, sets `_WITHOUT_SRCCONF` (so `bsd.own.mk` skips `/etc/src.conf`),
includes `bsd.own.mk`, then `${PORTSDIR}/Mk/bsd.port.mk`.

Defaults, all `?=` and so overridable from make.conf:

| Variable | Default | Set in |
| :-- | :-- | :-- |
| `PORTSDIR` | `/usr/ports` | `bsd.port.mk`, top, before `bsd.commands.mk` |
| `LOCALBASE` | `/usr/local` | same block |
| `PORT_DBDIR` | `/var/db/ports` | `bsd.port.mk`, options section, after `_VARS` |
| `DIALOG4PORTS` | `${LOCALBASE}/bin/portconfig` | `bsd.commands.mk` |
| `PKG_INFO` | `${PKG_BIN} info -g` | `bsd.commands.mk` |
| `PKG_ORIGIN` | `ports-mgmt/pkg` | `bsd.port.mk`, pre-makefile section |

`ARCH`, `OPSYS`, `_OSRELEASE` and `OSVERSION` come from `uname -p`, `-s`,
`-r` and `/usr/include/sys/param.h` unless defined. Once `PKGORIGIN` is
known, the options section applies `${PKGORIGIN:S/\//_/}_VARS` (e.g.
`shells_zsh_VARS`): words `name=value`, `name+=value` or `name@` (undefine).
It runs before `bsd.options.mk`, so it can set option variables per port.

## Naming the options file

`bsd.options.mk` begins with:

```make
OPTIONS_NAME?=	${PKGORIGIN:S/\//_/}
OPTIONS_FILE?=	${PORT_DBDIR}/${OPTIONS_NAME}/options
```

`PKGORIGIN` defaults, in the options section of `bsd.port.mk`, to
`${PKGCATEGORY}/${PORTDIRNAME}`: the first word of `CATEGORIES` and the
port's directory name. Neither flavour nor package name enters it, so all
flavours of a port share one options file and one set of
`${OPTIONS_NAME}_*` variables. A port may set `OPTIONS_NAME` itself.

The "options files (from dialog)" block of `bsd.options.mk` reads:

```text
${PORT_DBDIR}/
└── ${OPTIONS_NAME}/          e.g. /var/db/ports/shells_zsh/
    ├── options               written by 'make config'; .include'd if it
    │                         exists and the target is not rmconfig
    └── options.local         never written by the framework; .sinclude'd
                              always, rmconfig included
```

Both are makefile fragments. The framework uses only the
`OPTIONS_FILE_SET` and `OPTIONS_FILE_UNSET` they leave, but anything else
they assign is visible to the rest of the parse.

`UNIQUENAME` is gone (`CHANGES`, 20150817). What remains is
`_OPTIONS_UNIQUENAME=${PKGNAMEPREFIX}${PORTNAME}`, used only to add a
`WARNING` when `${_OPTIONS_UNIQUENAME}_SET`, `_UNSET`, `_SET_FORCE` or
`_UNSET_FORCE` is defined; those variables have no effect.

## The option list

`bsd.options.mk` turns the port's declarations into the list in order:

1. Each word of `OPTIONS_DEFINE_${ARCH}` not in `OPTIONS_DEFINE` is
   appended to it; for each radio group `R`, `OPTIONS_RADIO_R_${ARCH}` is
   appended to `OPTIONS_RADIO_R` the same way. No other group kind has an
   `_${ARCH}` form.
2. `OPTIONS_DEFAULT_${ARCH}` is appended to `OPTIONS_DEFAULT` if defined,
   else `OPTIONS_DEFAULT_OTHER_ARCHS` is.
3. `_ALL_EXCLUDE` is `OPTIONS_EXCLUDE_${ARCH}`, `OPTIONS_EXCLUDE`,
   `OPTIONS_SLAVE`, `OPTIONS_EXCLUDE_${OPSYS}` and
   `OPTIONS_EXCLUDE_${OPSYS}_${OSREL:R}` (major release).
4. Each option of `OPTIONS_DEFINE`, not itself excluded, whose
   `${opt}_IMPLIES` names an excluded option is excluded too. One pass,
   not transitive, and group members are not examined.
5. Each word of `_ALL_EXCLUDE` is removed with `:N` from `OPTIONS_DEFAULT`,
   `OPTIONS_DEFINE` and every `OPTIONS_<type>_<group>`. `:N` takes a
   pattern, so a word with glob characters removes every match.
6. Groups left empty are dropped from `OPTIONS_SINGLE`, `_RADIO`, `_MULTI`
   and `_GROUP`.
7. `ALL_OPTIONS` is `OPTIONS_DEFINE:O:u`; `OPTIONS_DEFAULT` is sorted and
   uniqued too.
8. `COMPLETE_OPTIONS_LIST` is `ALL_OPTIONS` followed by the members of each
   remaining group, single, radio, multi, group in turn. Later in the file,
   `NO_OPTIONS_SORT` resets `ALL_OPTIONS` to the unsorted `OPTIONS_DEFINE`.

| Kind | Variables | Rule enforced by `pre-check-config` |
| :-- | :-- | :-- |
| single | `OPTIONS_SINGLE`, `OPTIONS_SINGLE_<G>` | exactly one member on |
| radio | `OPTIONS_RADIO`, `OPTIONS_RADIO_<G>` | at most one member on |
| multi | `OPTIONS_MULTI`, `OPTIONS_MULTI_<G>` | at least one member on |
| group | `OPTIONS_GROUP`, `OPTIONS_GROUP_<G>` | none |

A single or multi group whose name `<G>` is also in `ALL_OPTIONS` is
conditional on that option (see [the config targets](#the-config-targets)).

`OPTIONS_SLAVE` lists options a slave port removes from its master's list
and always turns on: excluded in step 3, they never appear in
`COMPLETE_OPTIONS_LIST`, and are appended to `PORT_OPTIONS` at the end.
`_REALLY_ALL_POSSIBLE_OPTIONS`, `COMPLETE_OPTIONS_LIST` plus
`_ALL_EXCLUDE`, is what the helpers iterate over.

## How PORT_OPTIONS is built

`PORT_OPTIONS` lists the options that are on; `NEW_OPTIONS` those no
setting has named. `bsd.options.mk` builds both, from the "Some options are
always enabled by default" loop to the final `:O:u`, in this order, each
step overriding the earlier ones:

1. `DOCS`, `NLS`, `EXAMPLES` and `IPV6` are added if in
   `COMPLETE_OPTIONS_LIST`, whatever `OPTIONS_DEFAULT` says.
2. `_ALL_EXCLUDE` is removed.
3. If `OPTIONS_OVERRIDE` is defined, `PORT_OPTIONS` becomes its value,
   unchecked, `NEW_OPTIONS` is emptied, and steps 4 to 9 are skipped,
   options files included. Otherwise `NEW_OPTIONS` starts as
   `COMPLETE_OPTIONS_LIST`.
4. `OPTIONS_DEFAULT` is added (`NEW_OPTIONS` untouched).
5. `OPTIONS_SET`, then `OPTIONS_UNSET` (make.conf, all ports).
6. `${OPTIONS_NAME}_SET`, then `${OPTIONS_NAME}_UNSET` (make.conf, this
   port).
7. The options file, then `options.local`, are read.
8. `OPTIONS_FILE_SET`, as left by them.
9. `OPTIONS_FILE_UNSET`.
10. `OPTIONS_SET_FORCE`, `OPTIONS_UNSET_FORCE`,
    `${OPTIONS_NAME}_SET_FORCE`, `${OPTIONS_NAME}_UNSET_FORCE`, in that
    order. These run even under `OPTIONS_OVERRIDE`.
11. `WITH`, then `WITHOUT`, for the command line ("Cmdline always win over
    the rest").
12. The `IMPLIES` closure.
13. `OPTIONS_SLAVE` is appended; the list is sorted and uniqued.

The header comment of `bsd.options.mk` and ports(7) (Example 4) agree.

### How the words are matched

Every set step does, per word `opt`, and every unset step does with no
check:

```make
.if !empty(COMPLETE_OPTIONS_LIST:M${opt})      # set
PORT_OPTIONS+=	${opt}
NEW_OPTIONS:=	${NEW_OPTIONS:N${opt}}
.endif
PORT_OPTIONS:=	${PORT_OPTIONS:N${opt}}         # unset
NEW_OPTIONS:=	${NEW_OPTIONS:N${opt}}
```

Consequences, checked with `make -V` on `shells/zsh` in this run:

- A set word naming no option of the port is ignored, and an excluded
  option cannot be set (it is no longer in the list).
- An unset word is a pattern: `OPTIONS_UNSET=E*` turns off every option
  starting with `E`.
- A set word is a pattern only for the test: `OPTIONS_SET=E*` (or
  `WITH=E*`) appends the literal word `E*`, which turns nothing on, while
  `:N` removes every match from `NEW_OPTIONS`.
- `OPTIONS_OVERRIDE=FOO` gives `PORT_OPTIONS` = `FOO`, not an option of
  the port, and an empty `NEW_OPTIONS`.

The options files are `.include`d before step 8, so the `.for` loops of
later steps see anything they assign, `OPTIONS_SET_FORCE` or `WITH`
included (derived from the order of the blocks; not exercised).

`NEW_OPTIONS` ends as the options no set or unset step named, at any
level; `OPTIONS_DEFAULT` and step 1 do not count. A saved options file
names every option the port had when written, so in practice
`NEW_OPTIONS` is the options added since.

### IMPLIES and PREVENTS

`_DEPCHAIN` holds a word `A.B` for each `B` of `${A}_IMPLIES`, `A` ranging
over `COMPLETE_OPTIONS_LIST`. Then, for at most one pass more than there
are options and until nothing changes, every option on adds the right
side of its pairs and the right sides of those options' own pairs. As this
runs after `WITHOUT`, no unset variable can turn an implied option off;
only turning off the implying option does. The right side is not checked
against the list, and `OPTIONS_SLAVE`, appended later, is not followed.

`${opt}_PREVENTS` does not change `PORT_OPTIONS`: the comment in
`bsd.options.mk` defers it to `bsd.port.mk:pre-check-config`, which records
each enabled option whose `PREVENTS` names another enabled one.

## Option helpers

With `PORT_OPTIONS` final, `bsd.options.mk` walks
`_REALLY_ALL_POSSIBLE_OPTIONS` twice, first for `${opt}_SUBPACKAGES` and
`_SUBPACKAGES_OFF`, then for the rest: `${opt}_X` applies to an option
that is on, `${opt}_X_OFF` to one that is off.

| Helper | On | Off |
| :-- | :-- | :-- |
| `${opt}_<FLAG>` (`_OPTIONS_FLAGS`) | `<FLAG>+=` value | `_OFF` form, same |
| `${opt}_<KIND>_DEPENDS` | `<KIND>_DEPENDS+=` value | `_OFF` form, same |
| `${opt}_USE= FOO=a,b` | `USE_FOO+= a b` | `_USE_OFF` |
| `${opt}_VARS= X=v` or `X+=v` | `X= v` or `X+= v` | `_VARS_OFF` |
| `${opt}_CONFIGURE_ENABLE= x` | `--enable-x` | `--disable-x` |
| `${opt}_CONFIGURE_WITH= x` | `--with-x` | `--without-x` |
| `${opt}_CMAKE_BOOL`, `_MESON_TRUE`, ... | `true`, `yes`, `enabled` | the opposite |

`_OPTIONS_FLAGS` includes `USES`, `BROKEN`, `IGNORE`, `CATEGORIES`,
`DISTFILES`, `PLIST_FILES`, `CFLAGS` and more. `_OPTIONS_DEPENDS` is `PKG
FETCH EXTRACT PATCH BUILD_RUN BUILD LIB RUN TEST`, so every dependency
kind has an option form; `${opt}_<KIND>_DEPENDS.p` feeds subpackage `p`.
With `OPTIONS_SUB`, each option adds `OPT=""`/`NO_OPT="@comment "` (or the
reverse) to `PLIST_SUB` and `SUB_LIST`. Each option also queues
`<type>-<stage>-<opt>-on` (or `-off`) targets into `_OPTIONS_<stage>`,
which `bsd.port.mk` merges into the stage sequences.

A port that includes `bsd.port.options.mk` (to test `PORT_OPTIONS` in its
own `.if`s) gets the options section at that point: the file sets
`INOPTIONSMK` and includes `bsd.port.mk`, whose options section runs when
`INOPTIONSMK` is defined. Helpers defined later are not applied;
`bsd.options.mk` records those it saw in `_OPTIONS_HELPERS_SEEN` and
`bsd.sanity.mk` reports any others as a `DEV_ERROR` ("incorrectly set after
bsd.port.options.mk and are ineffective"). `bsd.sanity.mk` also warns that
`WITH_<opt>` and `WITHOUT_<opt>` are unsupported for every option but
`DEBUG`.

The options section runs before the pre-makefile section, so the words an
option adds to `USES` are there when `bsd.port.mk` loads `Uses/*.mk`, and
the dependencies those files add follow. `${opt}_<KIND>_DEPENDS` are
appended directly, and the port's own `.if ${PORT_OPTIONS:MFOO}` blocks
add the rest.

## The config targets

All live in the post-makefile section of `bsd.port.mk`, each under
`.if !target(...)` so a port can replace it.

`pre-check-config` has no commands, but its body is parse-time code, run
on every full parse. It records option names outside `[-_A-Z0-9]` (a
`DEV_WARNING`); for each single group, `OPTIONS_WRONG_SINGLE` when zero or
more than one member is on; for each radio group, `OPTIONS_WRONG_RADIO`
when more than one is; for each multi group, `OPTIONS_WRONG_MULTI` when
none is; and `OPTIONS_WRONG_PREVENTS` pairs. A conditional single or multi
group whose option is off is not checked: its members are removed from
`PORT_OPTIONS` instead.

That pruning happens after the helpers ran. On `mail/faces`
(`OPTIONS_SINGLE=AUDIO`, with `AUDIO` also in `OPTIONS_DEFINE`),
`make WITH=SOX -V PORT_OPTIONS -V RUN_DEPENDS` prints `DOCS NLS` and
`play:audio/sox`: `SOX` is no longer on, but `SOX_RUN_DEPENDS` was added.

`_check-config` prints a message per recorded problem and sets
`_CHECK_CONFIG_ERROR`, on which `check-config` fails. `check-config` is
step 650 of `_SANITY_SEQ`, so every stage that runs `check-sanity` stops on
an invalid configuration.

| Target | What it does |
| :-- | :-- |
| `config` | `pre-config do-config`; prints "Skipping" if `NO_DIALOG` is set |
| `config-conditional` | `make config` if `NEW_OPTIONS` is not empty |
| `config-recursive` | `config-conditional` in the port and `ALL-DEPENDS-FLAVORS-LIST` |
| `showconfig` | `check-config`, then each option as `NAME=on` or `=off` |
| `rmconfig` | removes `OPTIONS_FILE`, tries `rmdir` on its directory |
| `*-recursive` | `showconfig`, `rmconfig` over the same list |
| `sanity-config` | after `_check-config`, offers to re-run `make config` |

`config-recursive` computes its list once, before the loop; choices made
in the dialogs it opens do not extend it.

`pre-config` builds `D4P_ENV` (`PORT_OPTIONS`, `ALL_OPTIONS`, groups and
members, `NEW_OPTIONS`, every `_DESC`, `DIALOG4PORTS`, the `D4P*` layout
variables). `do-config`, when the port has options, creates
`${OPTIONS_FILE:H}` (through `SU_CMD` if `PORT_DBDIR` is not writable and
its parent is owned by root) and runs `Scripts/dialog4ports.sh`. That
script builds and installs `ports-mgmt/portconfig` with `-D NO_DIALOG` if
`DIALOG4PORTS` is missing (only builds it under `INSTALL_AS_USER` or when
`PREFIX` differs from `LOCALBASE`), then runs it, the selection going to a
temporary file. A cancelled dialog leaves the file alone ("Options
unchanged"). Otherwise `do-config` rewrites it whole:

```make
# This file is auto-generated by 'make config'.
# Options for file-roller-3.42.0_3,1
_OPTIONS_READ=file-roller-3.42.0_3,1
_FILE_COMPLETE_OPTIONS_LIST=NAUTILUS 7ZIP ARJ LZO RAR ZIP
OPTIONS_FILE_SET+=NAUTILUS
OPTIONS_FILE_SET+=7ZIP
OPTIONS_FILE_UNSET+=ARJ
OPTIONS_FILE_UNSET+=LZO
OPTIONS_FILE_UNSET+=RAR
OPTIONS_FILE_SET+=ZIP
```

There is one line per option of `COMPLETE_OPTIONS_LIST`, in that order, set
when `grep -qw` finds the name in the dialog's output. `_OPTIONS_READ` and
`_FILE_COMPLETE_OPTIONS_LIST` are written, but nothing under `Mk/` reads
them. The write goes through `SU_CMD` when the directory is neither
writable nor owned by the user; `do-config` ends with `make sanity-config`.

### When the dialog opens on its own

For `extract patch configure build stage install package`, unless
`_OPTIONS_OK` is defined, the stage depends on `config-conditional` and
then re-runs itself with `CONFIG_DONE_${PKGBASE:tu}=1`. `_OPTIONS_OK` is
defined (block "Do preliminary work to detect if we need to run the config
target") when the port has no `OPTIONS_DEFINE`, `_SINGLE`, `_MULTI`,
`_GROUP` or `_RADIO`, or when `CONFIG_DONE_<PKGBASE>`, `PACKAGE_BUILDING`
or `BATCH` is defined.

| Knob | Effect |
| :-- | :-- |
| `BATCH` | `_OPTIONS_OK`; `IGNORE` on `IS_INTERACTIVE` ports |
| `PACKAGE_BUILDING` | `_OPTIONS_OK`; the pkg(8) version check is skipped |
| `NO_DIALOG` | `config` does nothing, so `config-conditional` opens nothing |

None changes `PORT_OPTIONS`: an unconfigured port builds with defaults.

## Dependencies

### Kinds and syntax

`bsd.port.mk` documents `EXTRACT_`, `PATCH_`, `FETCH_`, `BUILD_`, `RUN_`,
`LIB_` and `TEST_DEPENDS`; `PKG_DEPENDS` and `BUILD_RUN_DEPENDS` are
handled in the code. `Scripts/do-depends.sh` parses each entry as:

```text
pattern:origin[@flavor][:target]          origin may also be origin~subpkg
```

- In `LIB_DEPENDS` the pattern must be `lib*.so*`, found by
  `Scripts/find-lib.sh`. Elsewhere a pattern with `<`, `>` or `=` is a
  package query (`PKG_INFO`), `/nonexistent` never matches, an absolute
  path is tested for existence, anything else is looked up in `PATH`.
- `@flavor` exports `FLAVOR` to the sub-make; `~subpkg` names a
  subpackage. One `case` tests the flavour form first, so an origin with
  both is split at the `@` only. An `@` or `~` followed by `/` is part of
  the path.
- A relative origin is looked up in each of `OVERLAYS`, then `PORTSDIR`;
  the first with a `Makefile` wins.
- `target` replaces `DEPENDS_TARGET` (default `install`), `fetch` becoming
  `checksum`. `DEPENDS_PRECLEAN` and `DEPENDS_CLEAN` add `clean`, but only
  when a target field is given.

The `depends` target runs `pkg-`, `extract-`, `patch-`, `lib-`, `fetch-`,
`build-` and `run-depends` (not `test-depends`), each calling
`do-depends.sh`. It tests the host: a dependency met by an installed file
or package is not built. `STRICT_DEPENDS` makes a missing one an error.

### Assembly

In "Dependency checking" (post-makefile section):

1. For each package of `_PKGS` (main and subpackages), `BUILD_RUN_DEPENDS`
   is appended to `BUILD_DEPENDS` and `RUN_DEPENDS` (with the `.<sp>`
   suffix for subpackages).
2. Inside `.if !target(depends)`, `<KIND>_DEPENDS_ALL` collects
   `<KIND>_DEPENDS` and every `<KIND>_DEPENDS.<sp>`, for `PKG EXTRACT PATCH
   FETCH BUILD LIB RUN TEST`.
3. `_UNIFIED_DEPENDS` is the `_ALL` lists of `PKG EXTRACT PATCH FETCH BUILD
   LIB RUN`; `TEST_DEPENDS` is left out.

Entries come from the port's `Makefile`; the option helpers;
`${FLAVOR}_<KIND>_DEPENDS` (pre-makefile section,
`_FLAVOR_HELPERS_APPEND`); `Uses/*.mk`, loaded in the pre-makefile section
from `OVERLAYS` first and again in the post-makefile section for those in
`_USES_POST` (`Uses/gmake.mk` adds `gmake>=4.4.1:devel/gmake` to
`BUILD_DEPENDS`; `Uses/pkgconfig.mk` adds `devel/pkgconf` to build, run
or both by argument); and `PKG_DEPENDS`, which defaults to
`${LOCALBASE}/sbin/pkg:${PKG_ORIGIN}` unless the port defines it or
`CLEAN_FETCH_ENV` is set.

### Flavours

`FLAVOR` comes from the environment or a dependency's `@flavor`.
`bsd.port.mk` stops with `.error` if it is passed empty on the command
line, and drops it from `.MAKEOVERRIDES` so it does not leak to child
makes. Empty, it becomes the first word of `FLAVORS`; one not in `FLAVORS`
sets `IGNORE`. `${FLAVOR}_<VAR>` then overrides `DESCR PLIST PKGNAMEPREFIX
PKGNAMESUFFIX` and appends to `CONFLICTS*` and every `*_DEPENDS` kind.
When `FLAVOR` is not given, the options section has already run with it
empty by then.

### Lists and closures

`Scripts/depends-list.sh`, wrapped as `DEPENDS-LIST`, prints origins.

| Target / variable | Input | Recursive |
| :-- | :-- | :-- |
| `all-depends-list` (`ALL-DEPENDS-LIST`) | `_UNIFIED_DEPENDS` | yes |
| `ALL-DEPENDS-FLAVORS-LIST` | `_UNIFIED_DEPENDS` | yes, with `@flavor` |
| `build-depends-list` | `PKG EXTRACT PATCH FETCH BUILD LIB` `_ALL` | no |
| `run-depends-list` | `LIB RUN` `_ALL` | no |
| `test-depends-list` | `TEST_DEPENDS_ALL` | no |
| `package-depends-list` | `LIB RUN` `_ALL`, with `CHILD_DEPENDS` | yes |

Recursing, `depends-list.sh` runs `make -C <dir> -V _UNIFIED_DEPENDS` in
each dependency, `FLAVOR` exported when the entry had one, and follows
that. Every level is the build-and-run union, so `all-depends-list` is the
full closure; each dependency is evaluated by its own parse, under its own
options file. The run-only closure is `package-depends-list`, recursing
with `CHILD_DEPENDS=yes` over `LIB_DEPENDS` and `RUN_DEPENDS`.

`depends-list.sh` skips an entry already printed, keyed on what it prints:
the directory, or `dir@flavor` with `-f`. Without `-f` (`all-depends-list`
unless `DEPENDS_SHOW_FLAVOR` is set), two flavours of one port count as one
and only the first met is recursed into. A missing directory is reported
on stderr ("dependency list incomplete") and skipped.

## Querying with make -V

`make -V VAR` prints the value "extracted from the global scope after all
makefiles have been read" (make(1)), fully expanded since `sys.mk` and
`bsd.port.mk` set `.MAKE.EXPAND_VARIABLES= yes`. The whole parse runs:
make.conf, options files, `!=` shell assignments and `pre-check-config`.

| Variable | Holds |
| :-- | :-- |
| `OPTIONS_NAME`, `OPTIONS_FILE` | where the options are saved |
| `ALL_OPTIONS`, `COMPLETE_OPTIONS_LIST` | plain options; plus group members |
| `OPTIONS_<type>`, `OPTIONS_<type>_<G>`, `OPTIONS_DEFAULT` | after exclusion |
| `PORT_OPTIONS`, `NEW_OPTIONS` | on (after pruning); not yet named |
| `SELECTED_OPTIONS`, `DESELECTED_OPTIONS` | on and off, sorted |
| `<opt>_DESC`, `<opt>_IMPLIES`, `<opt>_PREVENTS` | per option |
| `<KIND>_DEPENDS_ALL`, `_UNIFIED_DEPENDS` | dependency entries |
| `FLAVORS`, `FLAVOR`, `PKGNAME`, `PKGORIGIN`, `IGNORE`, `BROKEN` | identity, refusal |

Pitfalls:

- `bsd.options.desc.mk`, which holds the stock `_DESC` of common options,
  is included only in the `showconfig` block, under `make(*config*)` or a
  `-V` of some `*_DESC` variable in `.MAKEFLAGS`. A query naming no `_DESC`
  sees only the port's own descriptions.
- `<KIND>_DEPENDS_ALL`, and so `_UNIFIED_DEPENDS`, are built inside
  `.if !target(depends)`: a port with its own `depends` target has none.
  `BUILD_DEPENDS` alone misses subpackage entries.
- `PORT_OPTIONS` is the pruned list, while the helpers saw the unpruned one
  (the `mail/faces` case).
- The host leaks in: `ARCH`, `OPSYS`, `OSVERSION` from `uname` and
  `param.h` unless set; `PORT_DBDIR` and `LOCALBASE` default to the host's;
  `do-depends.sh` and `package-depends-list` consult installed packages.
  Only parse-time values are visible to `-V`.
- A port's `.if ${PORT_OPTIONS:M...}` is evaluated where the parser meets
  it: before `bsd.port.options.mk` (or `bsd.port.mk`) it sees an empty
  list, and `-V` cannot recover what it skipped.
- `-V` builds no target, so it does not fail on `IGNORE` or an invalid
  configuration; those stop a build through `check-sanity` only and must
  be read as variables.

## Sources

- Ports tree at git `4df6b05e0803ae455de700ec72fbd9b5042852b4`
  (2026-10-22; `Mk/` last changed at `fc35060efaba`), plus one local
  commit adding `ports-mgmt/optique`, outside `Mk/`.
  - `Mk/bsd.options.mk` (whole); `Mk/bsd.port.mk`: defaults, options,
    pre-makefile (`USES`, flavours) and post-makefile sections
    (`PKG_DEPENDS`, `_OPTIONS_OK`, dependency checking and lists, config
    targets, `_SANITY_SEQ`, stage targets)
  - `Mk/bsd.port.options.mk`, `Mk/bsd.sanity.mk`, `Mk/bsd.commands.mk`,
    `Mk/bsd.options.desc.mk` (header)
  - `Mk/Scripts/do-depends.sh`, `depends-list.sh`, `dialog4ports.sh`,
    `functions.sh` (`export_ports_env`); `Mk/Uses/gmake.mk`, `pkgconfig.mk`
  - `CHANGES` (20150817); `make -V` runs on `shells/zsh` and `mail/faces`
- FreeBSD 15.1-RELEASE-p3: `/usr/share/mk/sys.mk`, `bsd.port.mk`,
  `bsd.own.mk`, `bsd.opts.mk` (`_WITHOUT_SRCCONF` only).
- Manual pages: make(1), make.conf(5), ports(7).
