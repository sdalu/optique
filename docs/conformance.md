# Conformance

optique does not reimplement the ports framework: every port is evaluated
by `make` itself (see [How it works](how-it-works.md)). What it does model
is the part around those queries: which environment `make` runs in, which
files poudriere or synth would read, which ports a list names, and the
framework's options rules where optique must reason about a file before
writing it (what is new, stale, decided, redundant, in conflict). This page
records how that part was checked against the three upstreams, what
matches, what differs on purpose, and what is known not to.

Checked against, in October 2026:

| Upstream | Revision |
| :-- | :-- |
| ports framework | `/usr/ports/Mk` of the FreeBSD ports tree (`bsd.options.mk`, `bsd.port.mk`) |
| poudriere | git `3b268b38` (2026-10-19), `src/share/poudriere/common.sh`, `options.sh`, `bulk.sh` |
| synth | 3.15 (`ports-mgmt/synth`), with its INI reader ini-files 11 (`misc/ini_file_manager`) |

Upstream line numbers drift; the functions named below are the stable
reference. Verdicts: **matches**; **differs on purpose**, with the
reason in [DESIGN.md](../DESIGN.md); **not modelled**, with why it does
not change a result; **known gap**, with what it can change.

## The ports framework

| Behaviour | Upstream | Verdict |
| :-- | :-- | :-- |
| Option list: `OPTIONS_DEFINE`, groups, `_${ARCH}`, `OPTIONS_EXCLUDE` (`_${ARCH}`, `_${OPSYS}`, `_${OPSYS}_${OSREL}`) | `bsd.options.mk` | matches: taken from `make` as computed (`COMPLETE_OPTIONS_LIST`) |
| Defaults: `OPTIONS_DEFAULT` (`_${ARCH}`), DOCS/NLS/EXAMPLES/IPV6 | `bsd.options.mk` | matches |
| Layer order: `OPTIONS_SET`/`UNSET`, `${OPTIONS_NAME}_SET`/`_UNSET`, the options file, the four `*_FORCE`, each scope's UNSET after its SET | `bsd.options.mk` | matches |
| `options.local` read after the options file, its lines joining the file's | `bsd.options.mk` (`.sinclude ${OPTIONS_FILE}.local`) | matches; shown as a note, never written |
| UNSET entries are make patterns (`${PORT_OPTIONS:N${opt}}`); a SET entry is added only when `COMPLETE_OPTIONS_LIST:M${opt}` matches | `bsd.options.mk` | matches (bmake's `Str_Match`) |
| `WITH`/`WITHOUT` in make.conf, after FORCE, before IMPLIES | `bsd.options.mk` | matches; locked like FORCE, JSON scope `with` |
| IMPLIES, transitive, after FORCE | `bsd.options.mk` | matches |
| `OPTIONS_SLAVE`, added to `PORT_OPTIONS` last | `bsd.options.mk` | matches; its PREVENTS are checked, its IMPLIES are not (the chain is built from the option list alone, as in the framework) |
| Conditional SINGLE/MULTI groups (named after an option) checked only while that option is on, their members dropped otherwise | `bsd.port.mk` pre-check-config | matches |
| RADIO and PREVENTS always checked; SINGLE never picks a member by itself | `bsd.port.mk` pre-check-config | matches |
| New options: no SET/UNSET line anywhere (`NEW_OPTIONS`); `_FILE_COMPLETE_OPTIONS_LIST` is written, never read | `bsd.options.mk`, `bsd.port.mk` do-config | matches for "new"; "undecided" is judged by outcome — **differs on purpose** ("make.conf decisions are judged by outcome") |
| The options file is a makefile: several words per line, spaces around `+=`, `=` resets | `bsd.options.mk` (`.include`) | matches |
| What `make config` writes: header, `_OPTIONS_READ`, list, one SET/UNSET line per option | `bsd.port.mk` do-config | matches |
| `rmconfig` keeps a directory holding `options.local` | `bsd.port.mk` | matches (`clean`) |
| `OPTIONS_NAME` ignores the flavor: flavors share one file | `bsd.options.mk` | matches; who writes it — **differs on purpose** ("Flavors sharing one options file") |
| `PORT_DBDIR`, `PORTSDIR` | `bsd.port.mk` | matches: both passed explicitly on every query |
| `OPTIONS_OVERRIDE`, defined even empty (replaces defaults, make.conf and the file; the `*_FORCE` knobs, `WITH`/`WITHOUT`, `IMPLIES` and `OPTIONS_SLAVE` still apply on top) | `bsd.options.mk:300-304` | matches: dumped only `.if defined`; the port is fully decided (`undecided` empty, never stale/unconfigured), its options file untouched by `sync`/`decide`/apply, and `scan --json --options` reports every option `scope: "override"`, locked |

## poudriere

| Behaviour | Upstream | Verdict |
| :-- | :-- | :-- |
| Options directory: `[jail-][tree-][set-]options` candidates, first existing wins; the one written when none exists | `use_options`, `options.sh` | matches; where a less specific directory exists, optique writes the one bulk reads, where `poudriere options` would create a new one — documented in the man page |
| `-o name` under poudriere.d vs a path | `options.sh` | differs: optique's `-o` is a path (man page) |
| make.conf layers, order, realpath dedupe | `setup_makeconf`, `append_make` | matches |
| Hook plugins' `make.conf` after the layers | `setup_makeconf` | matches |
| Jail prelude: `USE_PACKAGE_DEPENDS`, `BATCH`, `PACKAGE_BUILDING(_FLAVORS)`, `OVERLAYS+=` | `setup_makeconf` | matches |
| `NO_PACKAGE_BUILDING`, `FLAVOR_DEFAULT_ALL`, read from poudriere.conf's layers | `include_poudriere_confs` | matches for plain assignments; a value set by shell logic is missed — **differs on purpose** |
| The jail's system: cross-build `MACHINE`/`MACHINE_ARCH`/`ARCH`, the jail's `OSVERSION`/`OSREL` | `setup_makeconf`, `setup_ports_env`, `need_cross_build` | `OSVERSION`/`OSREL` match under `-j` (from `jails/<j>/arch`, `version`, `mnt`; `OSVERSION` derived from the version, with a note, when the jail's `param.h` is not there); the cross-build lines are written whenever the jail's MACHINE_ARCH differs from the host's, ignoring `kern.supported_archs` — **differs on purpose**: optique always runs the host's own `make`, never the jail's 32-bit one, so an armv6/armv7 jail on an arm64 host still needs them |
| An empty `/usr/local` | the jail itself | `LOCALBASE=/nonexistent` — **differs on purpose** ("Queries see a jail, not the host") |
| `FORCE_PACKAGE`, stripped `DEVELOPER=` lines | `setup_makeconf` | not modelled: no option or dependency effect |
| Blacklist: layers, words, literal match, `@flavor`/`@all`/`@-` | `load_blacklist`, `gather_port_vars_port` | matches |
| A blacklisted port's dependencies are not gathered | `gather_port_vars_port` | matches |
| An IGNORE'd port's dependencies are not gathered | `gather_port_vars_port` | **differs on purpose**: optique walks them, IGNORE often depending on the options being configured |
| pkglist: first word, `#` comments, a stray `/` trimmed | list reading in `common.sh` | matches in `-f` lists; **known gap**: on the command line a trailing `/` is refused |
| `origin@all`, `origin@-`, `FLAVOR_DEFAULT_ALL=yes` | `common.sh` flavor expansion | matches, MOVED applied to the originspec first (`_check_moved`) so `@all`/a bare origin under FLAVOR_DEFAULT_ALL queries the FLAVORS of wherever it moved to, not the old origin's missing directory |
| `origin~subpackage` | `common.sh` | **known gap**: refused, or read as part of the flavor after `@` |
| `-a` (every port) | `bulk.sh` | not modelled: a full list is accepted but slow, and only default flavors are rooted |
| MOVED: renamed, chained, removed, expired entries | `parse_MOVED.awk`, `check_moved` | matches |
| MOVED: a flavored entry (`origin@flavor\|new`); a listed origin that still exists but has an entry | `check_moved` | **known gap**: optique looks up the bare origin, and stops at one that exists |
| Options changed since the package: pkg annotation vs `SELECTED_OPTIONS` | `common.sh` | consistent: optique rewrites a file only when the enabled set changes |
| Overlays: `-O` order, `OVERLAYS+=` | `bulk.sh`, `common.sh` | matches |
| `poudriere options` runs `make config` on the host, no jail prelude | `options.sh` | **differs on purpose**: optique asks what the jail's build will see |
| `poudriere -e etcdir`; the old `<mnt>/ports` tree layout | `common.sh` | not modelled; the second fails loudly |

## synth

| Behaviour | Upstream | Verdict |
| :-- | :-- | :-- |
| Profile: `-s name`, else `$SYNTHPROFILE` (when its section has `Directory_packages`), else `profile_selected`, else LiveSystem | `parameters.adb` | matches |
| Profile keys from the profile's own section, never `[Global Configuration]` | `parameters.adb` | matches |
| INI syntax: `;`/`#` comments, case-sensitive keys and sections, first match wins, no quoting | ini-files `config.adb` | matches; **known gap** on hand edits: an empty or bare key does not end the search as in synth, and indented headers and tab-indented keys are accepted where synth would not |
| `Directory_options` (default `/var/db/ports`) as `PORT_DBDIR`, `options.local` included | `parameters.adb`, `replicant.adb` | matches |
| Missing `Directory_portsdir`: synth tries `$PORTSDIR`, `make -V PORTSDIR`, then `/usr/dports`, `/usr/ports` | `parameters.adb` | not modelled (`/usr/ports`): synth writes every key back into synth.ini on first use |
| Scan make.conf: `SYNTHPROFILE`, `USE_PACKAGE_DEPENDS_ONLY`, `PACKAGE_BUILDING`, `BATCH`, the ccache lines when `Directory_ccache` exists, then `<profile>-make.conf` | `replicant.adb` `create_make_conf` | matches |
| `varcache.conf` (`OSVERSION`, `ARCH`, … from `Directory_system`) | `replicant-platform.adb` | not copied: the host computes the same while `Directory_system` is `/`; **known gap** otherwise |
| An empty `/usr/local` in the scan chroot | `replicant.adb` | `LOCALBASE=/nonexistent` — **differs on purpose** |
| Scan environment `env -i PATH=… USER=root LANG=C` | `portscan.adb` | not modelled: queries get `PATH`, `HOME`, `TMPDIR`, `PKG_DBDIR`; nothing in the options or dependency paths reads the others |
| No blacklist | — | matches |
| Implicit list: every installed package, its flavor from the `flavor` annotation, kept only where synth's flavor index matches | `portscan-pilot.adb` | matches |
| A build refused when an options file's entries differ in number from the port's options, or name one it lacks | `portscan-packages.adb` | matches: such a port trips `scan`'s gate under `-s` |
| Prebuilt packages: their options compared with the port's selected options | `portscan-packages.adb` | consistent: synth compares outcomes, so a minimal or redundant file causes no rebuild |

## How it is checked

Two kinds of tests pin the table:

- Unit tests model the framework's rules on hand-built port data: the
  layering, patterns, groups, flavors sharing a file, the file reader, the
  poudriere.d and synth.ini readers.
- Fixture tests run the binary over a handful of minimal ports laid on the
  host's real `/usr/ports/Mk` (`tests/ports`, `tests/common`), so `make`
  decides and optique must agree with it. `make tests-live` runs them:
  - `tests/fixture.rs`: a conditional group (`misc/cond`, after
    mail/faces), an `OPTIONS_SLAVE` flavor (`misc/slavey`, after
    databases/sqlite3@icu), three flavors sharing a file (`misc/flav`,
    after editors/emacs), a forced group member (`misc/forced`), a
    Latin-1 make.conf;
  - `tests/fixture_model.rs`: new options counted by line, a hand-written
    file, `options.local`, UNSET patterns (`misc/model-globs`), an
    `OPTIONS_SLAVE` option's PREVENTS (`misc/model-slave-prevents`),
    `WITH`/`WITHOUT`;
  - `tests/fixture_lists.rs`: `@all` and `@-`.

poudriere and synth themselves were not run: their side of each row comes
from reading their source, and optique's side was driven through fake
poudriere.d and synth.ini trees.

## Not checked

- synth's pkgsrc mode, and its NetBSD, Linux and SunOS profiles.
- A run against a real poudriere jail or a real synth build.
- MOVED entries in overlays; synth.ini lines longer than its 1,000-character
  limit.
