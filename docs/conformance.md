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
reason in [DESIGN.md](../DESIGN.md), or because the upstream behaviour
is a bug optique does not replicate; **not modelled**, with why it does
not change a result; **known gap**, with what it can change.

## The ports framework

| Behaviour | Upstream | Verdict |
| :-- | :-- | :-- |
| Option list: `OPTIONS_DEFINE`, groups, `_${ARCH}`, `OPTIONS_EXCLUDE` (`_${ARCH}`, `_${OPSYS}`, `_${OPSYS}_${OSREL}`) | `bsd.options.mk` | matches: taken from `make` as computed (`COMPLETE_OPTIONS_LIST`) |
| Defaults: `OPTIONS_DEFAULT` (`_${ARCH}`), DOCS/NLS/EXAMPLES/IPV6 | `bsd.options.mk` | matches |
| Layer order: `OPTIONS_SET`/`UNSET`, `${OPTIONS_NAME}_SET`/`_UNSET`, the options file, the four `*_FORCE`, each scope's UNSET after its SET | `bsd.options.mk` | matches |
| `options.local` read after the options file, its lines joining the file's | `bsd.options.mk` (`.sinclude ${OPTIONS_FILE}.local`) | matches; shown as a note, never written; read with or without the options file, so `-m` and `clean --redundant` judge a file redundant only when options.local alone gives the same outcome; an option one of its entries decides is locked (`locked_by: "options.local"`), its edits refused; one assigning a SET or UNSET list afresh (`=`, `:=`, an effective `?=`) makes the file unmanageable: warned about once, never written or removed, never stale, edits refused |
| UNSET entries are make patterns (`${PORT_OPTIONS:N${opt}}`); a SET entry is added, word for word, only when it matches as `COMPLETE_OPTIONS_LIST:M${opt}` | `bsd.options.mk` | matches (bmake's `Str_Match`) |
| `WITH`/`WITHOUT` in make.conf, after FORCE, before IMPLIES | `bsd.options.mk` | matches; locked like FORCE, JSON scope `with` |
| IMPLIES, transitive, after FORCE | `bsd.options.mk` | matches |
| `OPTIONS_SLAVE`, added to `PORT_OPTIONS` last | `bsd.options.mk` | matches; its PREVENTS are checked, its IMPLIES are not (the chain is built from the option list alone, as in the framework) |
| Conditional SINGLE/MULTI groups (named in `ALL_OPTIONS`) checked only while that option is on, their members dropped otherwise | `bsd.port.mk` pre-check-config | matches: `ALL_OPTIONS` is dumped and tested as make tests it |
| RADIO and PREVENTS always checked; SINGLE never picks a member by itself | `bsd.port.mk` pre-check-config | matches |
| New options: no SET/UNSET line anywhere (`NEW_OPTIONS`); `_FILE_COMPLETE_OPTIONS_LIST` is written, never read | `bsd.options.mk`, `bsd.port.mk` do-config | matches for "new"; "undecided" is judged by outcome — **differs on purpose** ("make.conf decisions are judged by outcome") |
| The options file is a makefile: several words per line, spaces around `+=`, `=` and `:=` reset, `?=` assigns only a list no earlier line (file, then `options.local`) assigned, a trailing backslash continues the line, `#` starts a comment and `\#` is a literal `#` | `bsd.options.mk` (`.include`) | matches; a `!=` line (a shell command) is not run — **differs on purpose**: reading an options file never runs a command it holds; the line is ignored, with a warning naming the file and line |
| What `make config` writes: header, `_OPTIONS_READ`, list, one SET/UNSET line per option | `bsd.port.mk` do-config | matches |
| `rmconfig` keeps a directory holding `options.local` | `bsd.port.mk` | matches (`clean`) |
| `OPTIONS_NAME` ignores the flavor: flavors share one file | `bsd.options.mk` | matches; who writes it — **differs on purpose** ("Flavors sharing one options file") |
| `PORT_DBDIR`, `PORTSDIR` | `bsd.port.mk` | matches: both passed explicitly on every query |
| `OPTIONS_OVERRIDE`, defined even empty (replaces defaults, make.conf and the file; the `*_FORCE` knobs, `WITH`/`WITHOUT`, `IMPLIES` and `OPTIONS_SLAVE` still apply on top) | `bsd.options.mk:300-304` | matches: dumped only `.if defined`; the port is fully decided (`undecided` empty, never stale/unconfigured), its options file untouched by `sync`/`decide`/apply, and `scan --json --options` reports every option `scope: "override"`, locked, except one a `*_FORCE` knob or `WITH`/`WITHOUT` pins, which reports that pin |

## poudriere

| Behaviour | Upstream | Verdict |
| :-- | :-- | :-- |
| Options directory: `[jail-][tree-][set-]options` candidates, first existing wins; the one written when none exists | `use_options`, `options.sh` | matches; where a less specific directory exists, optique writes the one bulk reads, where `poudriere options` would create a new one — documented in the man page |
| `-o name` under poudriere.d vs a path | `options.sh` | **differs on purpose**: optique's `-o` is the directory itself, used verbatim, as `-P` is the tree; `-z`/`-j`/`-p` already select poudriere.d's candidates (man page) |
| make.conf layers, order, realpath dedupe | `setup_makeconf`, `append_make` | matches |
| Hook plugins' `make.conf` after the layers | `setup_makeconf` | matches |
| ccache: `WITH_CCACHE_BUILD=yes`, `CCACHE_DIR=/root/.ccache` after the layers when poudriere.conf's `CCACHE_DIR` is a directory | `setup_ccache` | matches |
| Jail prelude: `USE_PACKAGE_DEPENDS`, `BATCH`, `OVERLAYS+=`, `PACKAGE_BUILDING(_FLAVORS)`, in that order | `setup_makeconf` | matches |
| `NO_PACKAGE_BUILDING`, `FLAVOR_DEFAULT_ALL`, read from poudriere.conf's layers | `include_poudriere_confs` | matches for plain assignments; a value set by shell logic is missed — **differs on purpose** |
| The jail's system: cross-build `MACHINE`/`MACHINE_ARCH`/`ARCH`, the jail's `OSVERSION`/`OSREL` | `setup_makeconf`, `setup_ports_env`, `need_cross_build` | `OSVERSION`/`OSREL` match under `-j` (from `jails/<j>/arch`, `version`, `mnt`; `OSVERSION` derived from the version, with a note, when the jail's `param.h` is not there); the cross-build lines are written whenever the jail's MACHINE_ARCH differs from the host's, ignoring `kern.supported_archs` — **differs on purpose**: optique always runs the host's own `make`, never the jail's 32-bit one, so an armv6/armv7 jail on an arm64 host still needs them |
| An empty `/usr/local` | the jail itself | `LOCALBASE=/nonexistent` — **differs on purpose** ("Queries see a jail, not the host") |
| `FORCE_PACKAGE`, stripped `DEVELOPER=` lines | `setup_makeconf` | not modelled: no option or dependency effect |
| Blacklist: layers, words, literal match, `@flavor`/`@all`/`@-`; `qemu-blacklist` for a jail poudriere would emulate | `load_blacklist`, `need_emulation`, `gather_port_vars_port` | matches |
| A blacklisted port's dependencies are not gathered | `gather_port_vars_port` | matches |
| An IGNORE'd port's dependencies are not gathered | `gather_port_vars_port` | **differs on purpose**: optique walks them, IGNORE often depending on the options being configured |
| Origins: first word of a list line, a line whose first word starts with `#` skipped, one leading and one trailing `/` trimmed, in lists and on the command line | list and argument reading in `common.sh` | matches |
| `origin@all`, `origin@-`, `FLAVOR_DEFAULT_ALL=yes` | `common.sh` flavor expansion | matches, MOVED applied to the originspec first (`_check_moved`) so `@all`/a bare origin under FLAVOR_DEFAULT_ALL queries the FLAVORS of wherever it moved to, not the old origin's missing directory |
| `origin~subpackage` | `common.sh` | **known gap**: refused, or read as part of the flavor after `@` |
| `-a` (every port) | `bulk.sh` | **known gap**: no `-a`; a full list is accepted but slow, and only its default flavors are rooted where `bulk -a` builds every flavor |
| MOVED: renamed, chained, removed, expired entries | `parse_MOVED.awk`, `check_moved` | matches |
| MOVED: a flavored entry (`origin@flavor\|new`); a listed origin that still exists but has an entry | `check_moved` | **known gap**: optique looks up the bare origin, and stops at one that exists |
| Options changed since the package: pkg annotation vs `SELECTED_OPTIONS` | `common.sh` | consistent: optique rewrites a file when the outcome or its option list changes; neither changes what poudriere compares, so no spurious rebuild |
| Overlays: `-O` order, `OVERLAYS+=` | `bulk.sh`, `common.sh` | matches |
| `poudriere options` runs `make config` on the host, no jail prelude | `options.sh` | **differs on purpose**: optique asks what the jail's build will see |
| `poudriere -e etcdir` (another poudriere.conf and poudriere.d) | `common.sh` | **known gap**: optique always reads `/usr/local/etc`'s, so under `-e` it reads other layers and options directories than the build |
| The old `<mnt>/ports` tree layout | `common.sh` | not modelled; fails loudly |

## synth

| Behaviour | Upstream | Verdict |
| :-- | :-- | :-- |
| Profile: `-s name`, else `$SYNTHPROFILE` (when its section has `Directory_packages`), else `profile_selected`, else LiveSystem | `parameters.adb` | matches |
| Profile keys from the profile's own section, never `[Global Configuration]` | `parameters.adb` | matches |
| INI syntax: `;`/`#` comments, case-sensitive keys and sections, the first match (a bare or empty one included) ends the search, no quoting | ini-files `config.adb` | matches |
| synth's INI reader keeps a tab around a key or value, keeps the spaces of `[ name ]`, and takes no indented `[name]` for a header | ini-files `config.adb` | **differs on purpose**: a reader bug not replicated; optique reads such lines as meant and names each in a note, since synth reads them otherwise |
| `Directory_options` (default `/var/db/ports`) as `PORT_DBDIR`, `options.local` included | `parameters.adb`, `replicant.adb` | matches |
| Missing `Directory_portsdir`: synth tries `$PORTSDIR`, `make -V PORTSDIR`, then `/usr/dports`, `/usr/ports` | `parameters.adb` | not modelled (`/usr/ports`): synth writes every key back into synth.ini on first use |
| Scan make.conf: `SYNTHPROFILE`, `USE_PACKAGE_DEPENDS_ONLY`, `PACKAGE_BUILDING`, `BATCH`, the ccache lines when `Directory_ccache` exists, then `<profile>-make.conf` | `replicant.adb` `create_make_conf` | matches; synth's test mode adds `DEVELOPER=1` (not modelled: `-s` configures builds) |
| An unreadable `<profile>-make.conf` | `replicant.adb` | **differs on purpose**: synth skips it silently, building without it; optique stops with an error rather than configure against settings the user did not mean to drop |
| `varcache.conf` (`OSVERSION`, `ARCH`, … from `Directory_system`) | `replicant-platform.adb` | not copied: while `Directory_system` is `/` the host gives the same `OSVERSION`, `ARCH` and `OPSYS`; the rest (`_OSRELEASE=…-SYNTH`, `UID=0`, `PYTHONBASE`, `_SMP_CPUS`) changes no option or dependency; **known gap** when `Directory_system` is not `/` |
| An empty `/usr/local` in the scan chroot | `replicant.adb` | `LOCALBASE=/nonexistent` — **differs on purpose** |
| Scan environment `env -i PATH=… TERM=dumb USER=root HOME=/root LANG=C` | `portscan.adb` | not modelled: queries get `PATH`, `HOME`, `TMPDIR`, `PKG_DBDIR`; nothing in the options or dependency paths reads the others |
| No blacklist | — | matches |
| Implicit list: every installed package, its flavor from the `flavor` annotation, kept only where synth's index (the categories' `SUBDIR` lines, each port's flavors) has it | `portscan-pilot.adb`, `portscan.adb` `prescan_ports_tree` | matches |
| A build refused when an options file's entries differ in number from the port's options, or name one it lacks | `portscan-packages.adb` | matches: such a port trips `scan`'s gate under `-s`, with the reason |
| synth aborts on an options-file line shorter than 18 bytes, or of 18–19 without `+=` at 17–18 (an unhandled Ada range error, by reading) | `portscan-packages.adb` `passed_options_cache_check` | **differs on purpose**: a crash, not replicated; optique writes no such line, and does not refuse a hand-written file for one |
| Prebuilt packages: their options compared with the port's selected options | `portscan-packages.adb` | consistent: synth compares outcomes, so a minimal or redundant file causes no rebuild |

## How it is checked

Two kinds of tests pin the table:

- Unit tests model the framework's rules on hand-built port data: the
  layering, patterns, groups, flavors sharing a file, the file reader, the
  poudriere.d and synth.ini readers. They alone pin the poudriere and synth
  rows, driven through fake poudriere.d and synth trees: no fixture test
  runs a poudriere (`-j`) or synth (`-s`) build.
- Fixture tests run the binary over a handful of minimal ports laid on the
  host's real `/usr/ports/Mk` (`tests/ports`, `tests/common`), so `make`
  decides and optique must agree with it. `make tests-live` runs them:
  - `tests/fixture.rs`: a conditional group (`misc/cond`, after
    mail/faces), an `OPTIONS_SLAVE` flavor (`misc/slavey`, after
    databases/sqlite3@icu), three flavors sharing a file (`misc/flav`,
    after editors/emacs), a forced group member (`misc/forced`), a
    Latin-1 make.conf;
  - `tests/fixture_model.rs`: new options counted by line; a hand-written
    file, `:=`/`?=`, a continued line; `options.local` after the file,
    without one, kept by `-m`, locking `decide`; UNSET patterns
    (`misc/model-globs`); an `OPTIONS_SLAVE` option's PREVENTS
    (`misc/model-slave-prevents`); `WITH`/`WITHOUT`; `OPTIONS_OVERRIDE`,
    set and empty; a group named after a defined member
    (`misc/model-defined-group`); an IMPLIES chain (`misc/model-implies`);
    an overfull RADIO group against `make check-config`
    (`misc/model-radio`);
  - `tests/fixture_lists.rs`: `@all` and `@-`.

Every fixture port depends on a `ports-mgmt/pkg` stub, as every real port
depends on pkg. poudriere and synth themselves were not run: their side
of each row comes from reading their source.

## Not checked

- synth's pkgsrc mode, its NetBSD, Linux and SunOS profiles, and its test
  mode (`DEVELOPER=1`).
- A run against a real poudriere jail or a real synth build.
- MOVED entries in overlays; synth.ini lines longer than its 1,000-character
  limit.
