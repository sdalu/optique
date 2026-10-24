# Conformance

optique does not reimplement the ports framework: every port is evaluated
by `make` itself (see [How it works](how-it-works.md)). What it does model
is the part around those queries: which environment `make` runs in, which
files poudriere or synth would read, which ports a list names, and the
framework's options rules where optique must reason about a file before
writing it (what is new, stale, decided, redundant, in conflict). This page
records, one behaviour per row, how optique stands against each upstream.

Checked against, in October 2026:

| Upstream | Revision |
| :-- | :-- |
| ports framework | `/usr/ports/Mk` of the FreeBSD ports tree (`bsd.options.mk`, `bsd.port.mk`, `Mk/Scripts`) |
| poudriere | git `3b268b38` (2026-10-19) |
| synth | 3.15 (`ports-mgmt/synth`), with its INI reader ini-files 11 (`misc/ini_file_manager`) |

How each upstream works, independent of optique, is in the reference
pages: [ports framework](reference/ports-framework.md),
[poudriere](reference/poudriere.md), [synth](reference/synth.md). Each row
names the upstream by file and function and links ("ref") to the section
that describes it.

Verdicts:

- **matches**: optique does what the upstream does.
- **differs on purpose**: with the reason, often in [DESIGN.md](../DESIGN.md);
  this includes an upstream defect that changes nothing built, which
  optique does not copy.
- **conforms to an upstream defect**: a defect that changes what gets built,
  or makes the tool fail on something optique wrote (marked radical in the
  reference pages), which optique follows and says so (DESIGN.md,
  "Upstream's behaviour, not its bugs"). A tool failing on its own input is
  left to the user.
- **not modelled**: with why it does not change a result.
- **known gap**: with what it can change.

## The ports framework

| Behaviour | Upstream | Verdict |
| :-- | :-- | :-- |
| Option list: `OPTIONS_DEFINE`, groups and `OPTIONS_RADIO_*` (each with `_${ARCH}`), `OPTIONS_EXCLUDE` (`_${ARCH}`, `_${OPSYS}`, `_${OPSYS}_${OSREL:R}`) ([ref](reference/ports-framework.md#the-option-list)) | `bsd.options.mk` | matches: taken from `make` as computed (`COMPLETE_OPTIONS_LIST`) |
| Defaults: `OPTIONS_DEFAULT` (`_${ARCH}`, `OPTIONS_DEFAULT_OTHER_ARCHS`), DOCS/NLS/EXAMPLES/IPV6 ([ref](reference/ports-framework.md#how-port_options-is-built)) | `bsd.options.mk` | matches: taken from `make` as computed |
| The pass excluding an option that implies an excluded one is one level deep and skips group members ([ref](reference/ports-framework.md#the-option-list)) | `bsd.options.mk` | **conforms to an upstream defect**: the list is make's own, and an escaping member's IMPLIES still turns the excluded option on in the outcome |
| Layer order: `OPTIONS_SET`/`UNSET`, `${OPTIONS_NAME}_SET`/`_UNSET`, the options file and `options.local`, the four `*_FORCE`, `WITH`/`WITHOUT`, then IMPLIES ([ref](reference/ports-framework.md#how-port_options-is-built)) | `bsd.options.mk` | matches |
| UNSET-side words are make patterns; a SET-side word is added word for word, only when it matches `COMPLETE_OPTIONS_LIST` ([ref](reference/ports-framework.md#how-the-words-are-matched)) | `bsd.options.mk` | matches (bmake's `Str_Match`) |
| A SET-side glob (`OPTIONS_SET=E*`, `WITH=E*`, `OPTIONS_FILE_SET+=E*`) turns nothing on yet takes every match out of `NEW_OPTIONS` ([ref](reference/ports-framework.md#how-the-words-are-matched)) | `bsd.options.mk` | **differs on purpose**: a defect that changes nothing built; such a word settles nothing in optique, so its matches stay undecided |
| `NEW_OPTIONS`: an option no layer names (make.conf SET/UNSET, FORCE, `WITH`/`WITHOUT`, the file, `options.local`), empty under `OPTIONS_OVERRIDE` ([ref](reference/ports-framework.md#how-port_options-is-built)) | `bsd.options.mk` | matches as `undecided`, by mention; **differs on purpose** for SINGLE/RADIO siblings and options implied by a forced one, judged by outcome ("make.conf decisions are judged by outcome"). "new" is the file-level notion: no line in the file or `options.local` |
| `WITH`/`WITHOUT` in make.conf, after FORCE ([ref](reference/ports-framework.md#how-port_options-is-built)) | `bsd.options.mk` | matches; locked like FORCE, JSON scope `with` |
| IMPLIES: transitive, after `WITHOUT` (so nothing unsets an implied option), its target put in whether listed or not ([ref](reference/ports-framework.md#implies-and-prevents)) | `bsd.options.mk` | matches; the target's own PREVENTS is checked too, an excluded option included |
| PREVENTS words are patterns (`${PORT_OPTIONS:M${conflict}}`) ([ref](reference/ports-framework.md#implies-and-prevents)) | `bsd.port.mk` `pre-check-config` | matches; the conflict names the word as written |
| `OPTIONS_SLAVE`, added to `PORT_OPTIONS` last ([ref](reference/ports-framework.md#how-port_options-is-built)) | `bsd.options.mk` | matches; its PREVENTS are checked, its IMPLIES are not, as in the framework |
| Conditional SINGLE/MULTI groups (named in `ALL_OPTIONS`) checked only while that option is on, their members dropped otherwise ([ref](reference/ports-framework.md#the-config-targets)) | `bsd.port.mk` `pre-check-config` | matches: `ALL_OPTIONS` is dumped and tested as make tests it |
| A pruned conditional group's members keep the dependencies their helpers added (`mail/faces` with `WITH=SOX`) ([ref](reference/ports-framework.md#the-config-targets)) | `bsd.port.mk` `pre-check-config` | **conforms to an upstream defect**: dependencies are make's own, so the closure keeps them (`a_pruned_group_members_dependencies_stay`) |
| RADIO and PREVENTS always checked; SINGLE never picks a member by itself ([ref](reference/ports-framework.md#the-config-targets)) | `bsd.port.mk` `pre-check-config` | matches |
| `OPTIONS_OVERRIDE`, defined even empty: replaces defaults, make.conf SET/UNSET and the file; FORCE, `WITH`/`WITHOUT`, IMPLIES and `OPTIONS_SLAVE` still apply ([ref](reference/ports-framework.md#how-port_options-is-built)) | `bsd.options.mk` | matches: the port is fully decided, its file untouched by `sync`/`decide`/apply; JSON reports scope `override`, except an option FORCE or `WITH`/`WITHOUT` pins, even when the file is one optique cannot model, which make does not read then |
| The options file is a makefile: several words per line, spaces around `+=`, `=` and `:=` reset, `?=`, a trailing backslash, `#` comments and `\#` ([ref](reference/ports-framework.md#how-the-words-are-matched)) | `bsd.options.mk` (`.include`) | matches |
| What the file or `options.local` assigns besides `OPTIONS_FILE_SET`/`UNSET` counts only after the inclusion (SET/UNSET lists and `OPTIONS_OVERRIDE` inert, FORCE and `WITH` live); a `!=` runs on every query; directives and `${VAR}` are evaluated ([ref](reference/ports-framework.md#how-the-words-are-matched)) | `bsd.options.mk` | matches by leaving such a file to make: it is unmanaged, warned about with its line, never written or removed, its port shown as make computes it, its options locked |
| `options.local` read after the options file; its SET entries are defaults the file's UNSET overrides, its UNSET entries win over the file ([ref](reference/ports-framework.md#how-the-words-are-matched)) | `bsd.options.mk` (`.sinclude ${OPTIONS_FILE}.local`) | matches; never written; an option its UNSET entries match is locked (`locked_by: "options.local"`); one assigning a list afresh, or with `?=` (which assigns only while the file leaves that list out, so a write can bring it to life), makes the file unmanaged; `-m` and `clean --redundant` judge a file redundant only when `options.local` alone gives the same outcome |
| `OPTIONS_FILE?=` moved by make.conf ([ref](reference/ports-framework.md#naming-the-options-file)) | `bsd.options.mk` | matches by leaving the port alone: warned about, unmanaged; moved for some flavors only, those are left alone and the others still read, and manage, the shared file; `clean` removes no entry as vanished or unused while make.conf sets it (asked once per run, from `ports-mgmt/pkg` without reading its Makefile), and under `--unused`/`--redundant` keeps the entry a queried port's moved file lies in |
| `OPTIONS_NAME`, `PORT_DBDIR`, `PORTSDIR` ([ref](reference/ports-framework.md#naming-the-options-file)) | `bsd.options.mk`, `bsd.port.mk` | `OPTIONS_NAME` matches (dumped); `PORT_DBDIR` and `PORTSDIR` are passed on the command line, which overrides make.conf; `PORT_DBDIR` matches by reporting it: a make.conf one other than where the build reads options (the options dir, a poudriere jail's `/var/db/ports`, synth's `/options`), asked once per run from `ports-mgmt/pkg` without reading its Makefile (so a `.CURDIR` test in make.conf answers as for a build) and, for a local build, with the environment's `PORT_DBDIR` a make.conf `?=` defers to, is an error in the banner and in `scan`'s `errors` |
| `OPTIONS_NAME` ignores the flavor: flavors share one file ([ref](reference/ports-framework.md#naming-the-options-file)) | `bsd.options.mk` | matches; who writes it: **differs on purpose** ("Flavors sharing one options file") |
| What `make config` writes: header, `_OPTIONS_READ`, list, one SET/UNSET line per option ([ref](reference/ports-framework.md#the-config-targets)) | `bsd.port.mk` `do-config` | **differs on purpose**: no `_OPTIONS_READ` (nothing reads it) and a pkgname comment of 20 bytes or more, so that synth's line probe never aborts on a file optique wrote (synth table) |
| `do-config` marks an option set when `grep -qw` finds it, so `FOO` matches inside `FOO-BAR` ([ref](reference/ports-framework.md#the-config-targets)) | `bsd.port.mk` `do-config` | **differs on purpose**: optique writes each option's own value |
| `rmconfig` keeps a directory holding `options.local` ([ref](reference/ports-framework.md#the-config-targets)) | `bsd.port.mk` | matches (`clean`) |
| Dependencies: `<KIND>_DEPENDS_ALL` and `_UNIFIED_DEPENDS` ([ref](reference/ports-framework.md#assembly)) | `bsd.port.mk` | matches: read from make's dump |
| A port defining its own `depends` target has no `<KIND>_DEPENDS_ALL` or `_UNIFIED_DEPENDS` ([ref](reference/ports-framework.md#assembly)) | `bsd.port.mk` | **conforms to an upstream defect**: such a port shows no dependencies, as poudriere reads the same lists |
| `origin@flavor~subpkg` split at the `@` only, the `~subpkg` staying in the flavor ([ref](reference/ports-framework.md#kinds-and-syntax)) | `Mk/Scripts/do-depends.sh` | **conforms to an upstream defect** for `origin@flavor~subpkg`; `origin~subpkg` without a flavor matches: the dependency is on `origin`, whose one options file every subpackage shares |
| `all-depends-list` without `DEPENDS_SHOW_FLAVOR` merges a port's flavors ([ref](reference/ports-framework.md#lists-and-closures)) | `Mk/Scripts/depends-list.sh` | not modelled: optique never uses it, walking each flavor itself |
| `bsd.options.desc.mk` loaded only for `*config*` targets ([ref](reference/ports-framework.md#querying-with-make--v)) | `bsd.port.mk` | matches: the wrapper's target is `optique-config` |

## poudriere

| Behaviour | Upstream | Verdict |
| :-- | :-- | :-- |
| Configuration files: `poudriere.conf`, then `poudriere.d`'s layers ([ref](reference/poudriere.md#configuration-files)) | `include_poudriere_confs` | matches for plain assignments; a value set by shell logic is missed: **differs on purpose** |
| Without `-p`, neither `default-poudriere.conf` nor `<jail>-default-poudriere.conf` is read, while `default-make.conf` is ([ref](reference/poudriere.md#configuration-files)) | `include_poudriere_confs`, `setup_makeconf` | **conforms to an upstream defect** |
| `POUDRIERE_ETC` from the environment moves the etc directory as `-e` does ([ref](reference/poudriere.md#configuration-files)) | `bin/poudriere.in` | matches, with a banner note; a path that is not a directory is an error where poudriere goes on: **differs on purpose** |
| `poudriere -e etcdir` | `bin/poudriere.in` | **known gap**: optique has no `-e`; `POUDRIERE_ETC` does the same |
| Options directory: `[jail-][tree-][set-]options` candidates, first existing wins ([ref](reference/poudriere.md#which-directory-a-build-reads)) | `common.sh` `do_portbuild_mounts`, `options.sh` | matches; where none exists optique writes the one bulk reads, where `poudriere options` would create a new one (man page) |
| `-o name` under poudriere.d ([ref](reference/poudriere.md#poudriere-options)) | `options.sh` | **differs on purpose**: optique's `-o` is the directory itself, used verbatim, as `-P` is the tree |
| make.conf layers, order, realpath dedupe; hook plugins' `make.conf` after them ([ref](reference/poudriere.md#the-layered-files)) | `setup_makeconf`, `append_make` | matches |
| Jail prelude: `USE_PACKAGE_DEPENDS`, `BATCH`, `OVERLAYS+=`, `PACKAGE_BUILDING(_FLAVORS)` unless `NO_PACKAGE_BUILDING`, in that order ([ref](reference/poudriere.md#makeconf-for-a-build)) | `jail_start`, `setup_makeconf` | matches |
| ccache: `WITH_CCACHE_BUILD=yes`, `CCACHE_DIR=/root/.ccache` after the layers when `CCACHE_DIR` is a directory ([ref](reference/poudriere.md#makeconf-for-a-build)) | `setup_ccache` | matches |
| `FLAVOR_DEFAULT_ALL` ([ref](reference/poudriere.md#ports-to-build)) | `include_poudriere_confs` | matches, read as `NO_PACKAGE_BUILDING` is |
| The jail's system: cross-build `MACHINE`/`MACHINE_ARCH`/`ARCH`, the jail's `OSVERSION`/`OSREL` ([ref](reference/poudriere.md#makeconf-for-a-build)) | `setup_makeconf`, `setup_ports_env`, `need_cross_build` | `OSVERSION`/`OSREL` match under `-j`; the cross-build lines are written whenever the jail's MACHINE_ARCH differs from the host's: **differs on purpose**, optique always runs the host's own `make` |
| Other jail lines: `WRKDIRPREFIX`, `PACKAGES`, `DISTDIR`, `FORCE_PACKAGE`, stripped `DEVELOPER=`, `ports_env.sh`, `/etc/make.nxb.conf` for qemu jails ([ref](reference/poudriere.md#makeconf-for-a-build)) | `jail_start`, `setup_makeconf` | not modelled: none of those read changes an option or a dependency (`make.nxb.conf` and `ports_env.sh` not traced in full) |
| An empty `/usr/local` ([ref](reference/poudriere.md#environment-of-the-metadata-queries)) | the jail itself | `LOCALBASE=/nonexistent`: **differs on purpose** ("Queries see a jail, not the host") |
| Query environment: `USER=root`, `HOME=/root` (`bsd.ccache.mk` falls back to `/root/.ccache` only for root) ([ref](reference/poudriere.md#environment-of-the-metadata-queries)) | `jail_start`, `injail` | matches |
| `DISABLE_MAKE_JOBS`, the `BUILD_AS_NON_ROOT` UID/GID removal ([ref](reference/poudriere.md#environment-of-the-metadata-queries)) | `common.sh` | matches: added after gathering, so no query sees them, nor optique's |
| Blacklist: layers, words, literal match, `@flavor`/`@all`/`@-`; `qemu-blacklist` for an emulated jail ([ref](reference/poudriere.md#gathering-metadata)) | `load_blacklist`, `deps_fetch_vars`, `need_emulation` | matches |
| A blacklisted port's dependencies are not gathered ([ref](reference/poudriere.md#gathering-metadata)) | `gather_port_vars_port` | matches |
| An IGNORE'd port's dependencies are not gathered nor checked ([ref](reference/poudriere.md#ignore-and-broken)) | `gather_port_vars_port` | not checked, as upstream; walked: **differs on purpose**, IGNORE often depending on the options being configured |
| A dependency on a nonexistent origin aborts the run, naming its MOVED target or EXPIRED reason ([ref](reference/poudriere.md#checks-on-the-result)) | `deps_sanity`, `gather_port_vars_port` | matches: an error on the depending port, counted in `scan`'s exit status; the moved-to port is not pulled in through that edge, nor reached through it once a listed origin's move is followed. MOVED is read for the dependency's originspec whole, as `deps_sanity` passes it: `origin~sub` gets no moved-to note |
| `TEST_DEPENDS` never gathered without `bulk -t` ([ref](reference/poudriere.md#gathering-metadata)) | `common.sh` | matches on a current tree; on a tree before ports `e7679b71cf89`, whose unified list folds them in, test-only dependencies are walked too, but never checked nor counted in a loop |
| Origins: first word of a list line, `#` lines skipped, one leading and one trailing `/` trimmed ([ref](reference/poudriere.md#ports-to-build)) | `common.sh` list reading | matches |
| `origin@all`, `origin@-`, `FLAVOR_DEFAULT_ALL=yes` ([ref](reference/poudriere.md#ports-to-build)) | `common.sh` flavor expansion | matches, MOVED read for the listed originspec first (`old@all` is `new@all` once followed) |
| A listed `origin[@flavor]~subpackage` ([ref](reference/poudriere.md#ports-to-build)) | `common.sh` `originspec_decode` | matches: the subpackage is split off first and dropped, the port rooted as `origin[@flavor]`, its options being the origin's |
| A listed `origin[@flavor]~subpackage` goes to `_check_moved` whole: its exact lookup never matches, so `origin~sub` is never moved, and `origin@flavor~sub` only by the bare origin's entry, never by `origin@flavor`'s ([ref](reference/poudriere.md#moved)) | `_listed_ports`, `_check_moved` | **conforms to an upstream defect**: it changes which origin is built, so a listed subpackage is looked up as poudriere looks it up, under every build that reads MOVED (`a_listed_subpackage_is_not_moved_by_check_moved`) |
| `-a` (every port) | `bulk.sh` | **known gap**: no `-a`; a full list roots only default flavors |
| MOVED applied to listed origins only: removed or expired is an error ([ref](reference/poudriere.md#moved)) | `_listed_ports`, `check_moved` | matches |
| MOVED renames a listed origin, and poudriere builds the target, saying so ([ref](reference/poudriere.md#moved)) | `_listed_ports`, `check_moved` | **differs on purpose**: flagged, neither origin scanned, pending in `scan`'s exit status, the list needing a fix; `--follow-moved` matches, the target configured and the rename noted |
| MOVED per originspec: `origin@flavor`'s own entry, else the bare origin's with the listed flavor re-attached; a target naming another flavor stops the run ([ref](reference/poudriere.md#moved)) | `_check_moved` | matches: the clash is an error on the listed origin, or in a dependency's `deps_sanity` message |
| A chain ending at an origin in no tree with no entry of its own is no move ([ref](reference/poudriere.md#moved)) | `check_moved` | matches: the listed origin is taken as given, and fails as one in no tree |
| MOVED chains collapsed in file order: `A\|B` then `B\|C` sends `A` to `C` even when `B` exists again ([ref](reference/poudriere.md#moved)) | `awk/parse_MOVED.awk` `update_forwards`, `check_moved` | **conforms to an upstream defect** |
| A MOVED cycle (`a\|b`, `b\|a`) loops forever in `check_moved` when its collapsed target is missing ([ref](reference/poudriere.md#moved)) | `check_moved` | **differs on purpose**: poudriere failing on its own input, the tree's MOVED; the origin is gone for optique, an error that names the cycle, and optique never loops |
| `_listed_ports` runs `check_moved` before testing the origin, so a listed origin still in the tree but with an entry moves (`math/mumps\|math/mumps-edf`, the name reused since) ([ref](reference/poudriere.md#moved)) | `_listed_ports`, `check_moved` | **conforms to an upstream defect** under poudriere: flagged as moved, or followed under `--follow-moved`, the message naming the defect when the listed originspec is buildable as it stands (bare, `@all` or `@-`); a flavor-level entry (`audio/kid3@qt5\|audio/kid3@qt6`) retires a flavor and reads as a plain move. A local build (`make config`, no MOVED) and synth take an origin in the tree as it is |
| Overlays: `-O` order, `OVERLAYS+=`, overlay MOVED files after the main one ([ref](reference/poudriere.md#makeconf-for-a-build)) | `bulk.sh`, `jail_start`, `load_moved` | matches |
| Changed options: pkg's recorded options (`%Ok %Ov`) against `SELECTED_OPTIONS`/`DESELECTED_OPTIONS`, as exact strings ([ref](reference/poudriere.md#changed-options)) | `CHECK_CHANGED_OPTIONS` in `common.sh` | not modelled: optique rewrites a file only when the outcome or its option list changes, which changes nothing poudriere compares |
| `poudriere options`: `make config` on the host, a make.conf without the prelude, `LOCALBASE` and overlays quirks ([ref](reference/poudriere.md#poudriere-options)) | `options.sh` | **differs on purpose**: optique asks what the jail's build will see |
| `testport -c` writes into the reference jail's copy, never saved back | `testport.sh` | not modelled: optique configures builds, not test runs |
| The old `<mnt>/ports` tree layout | `common.sh` | not modelled; fails loudly |

## synth

| Behaviour | Upstream | Verdict |
| :-- | :-- | :-- |
| Profile: `-s name`, else `$SYNTHPROFILE` (when its section has `Directory_packages`), else `profile_selected`, else LiveSystem ([ref](reference/synth.md#choosing-the-profile)) | `parameters.adb` | matches |
| Profile keys from the profile's own section, never `[Global Configuration]` ([ref](reference/synth.md#how-a-value-is-read)) | `parameters.adb` | matches |
| INI syntax: `;`/`#` comments, case-sensitive keys and sections, the first match (a bare or empty one included) ends the search, a line of one character skipped ([ref](reference/synth.md#how-a-value-is-read)) | ini-files `config.adb` `Get_Value` | matches |
| The INI reader keeps a tab in a key or value, keeps a CRLF file's `\r`, keeps the spaces of `[ name ]`, and takes no indented `[name]` for a header ([ref](reference/synth.md#how-a-value-is-read)) | ini-files `config.adb` `Get_Value` | **conforms to an upstream defect**: read as synth reads it, each such line named in a note |
| A UTF-8 byte-order mark is read as part of the first line, so a header there is none and its settings are in no section | ini-files `config.adb` `Get_Value` | **conforms to an upstream defect**: read as synth reads it, named in a note |
| A header with no `]` raises `Constraint_Error`, so synth refuses to run ([ref](reference/synth.md#how-a-value-is-read)) | ini-files `config.adb` `Get_Value` | **differs on purpose**: the user's own input, not optique's business; optique skips the line and says nothing |
| An unreadable synth.ini makes synth refuse to run ([ref](reference/synth.md#configuration-file)) | `parameters.adb` `load_configuration` | matches: optique stops with an error. The file is read as bytes, as synth reads it: a `Directory_*` path keeps them (a Latin-1 path is the directory synth uses), other text decodes them lossily |
| `Directory_options` (default `/var/db/ports`) as `PORT_DBDIR`, `options.local` included ([ref](reference/synth.md#options)) | `parameters.adb`, `replicant.adb` | matches |
| Missing `Directory_portsdir`: `$PORTSDIR`, `make -V PORTSDIR`, `/usr/dports`, `/usr/ports`, the first that exists ([ref](reference/synth.md#how-missing-keys-are-filled)) | `parameters.adb` `determine_portsdirs` | matches, with a note naming the source; make is asked as synth asks it (`query_portsdir`: `make -f /usr/share/mk/bsd.port.mk -V PORTSDIR` from the working directory, which falls back to `/usr/ports`, so `/usr/dports` is reached only when make fails); synth's stop on a make query that fails over a missing tree is a defect not copied: **differs on purpose**, optique moves to the next candidate |
| Write-back of missing keys into synth.ini, dropping comments; a duplicate blank profile header ([ref](reference/synth.md#how-missing-keys-are-filled)) | `parameters.adb` | not modelled: optique never writes synth.ini |
| Scan make.conf: `SYNTHPROFILE`, `USE_PACKAGE_DEPENDS_ONLY`, `PACKAGE_BUILDING`, `BATCH`, the ccache lines when `Directory_ccache` exists, then `<profile>-make.conf` ([ref](reference/synth.md#makeconf)) | `replicant.adb` `create_make_conf` | matches; test mode's `DEVELOPER=1` not modelled (`-s` configures builds) |
| `Directory_ccache`'s default `disabled` is tested as a path relative to synth's working directory ([ref](reference/synth.md#makeconf)) | `replicant.adb` | **differs on purpose**: a defect that does not change a normal setup; `disabled` means no ccache |
| An unreadable `<profile>-make.conf` ([ref](reference/synth.md#makeconf)) | `replicant.adb` | **differs on purpose**: synth skips it silently; optique stops with an error rather than configure against settings the user did not mean to drop |
| `varcache.conf` (`OSVERSION`, `ARCH`, … from `Directory_system`), read after `<profile>-make.conf` ([ref](reference/synth.md#varcacheconf)) | `replicant-platform.adb` | not copied: while `Directory_system` is `/` the host gives the same values, and the rest changes no option or dependency; **known gap** when `Directory_system` is not `/` |
| An empty `/usr/local` in the scan chroot ([ref](reference/synth.md#mounts)) | `replicant.adb` | `LOCALBASE=/nonexistent`: **differs on purpose** ("Queries see a jail, not the host") |
| Scan environment `env -i PATH=… TERM=dumb USER=root HOME=/root LANG=C` ([ref](reference/synth.md#environment-of-a-make-run)) | `portscan.adb` `scan_environment` | matches for `USER` and `HOME`, the two the framework reads on these paths; `TERM` and `LANG` not modelled |
| `<profile>-environment` applied to builders, not to the scanner ([ref](reference/synth.md#environment-of-a-make-run)) | `portscan.adb`, `replicant.adb` | **conforms to an upstream defect**: optique's queries, like the scan, never see it |
| The scan reads its 15 values by line position, `make`'s stderr merged in ([ref](reference/synth.md#the-query)) | `portscan.adb` | not modelled: what a diagnostic line shifts cannot be predicted; optique reads tagged lines |
| No blacklist | — | matches |
| Implicit list: every installed package, its flavor from the `flavor` annotation, kept only where synth's index (the categories' `SUBDIR`, each port's flavors) has it ([ref](reference/synth.md#the-flavor-index)) | `portscan-pilot.adb`, `portscan.adb` `prescan_ports_tree` | matches |
| Explicit list: exact index keys only; a comment, `@all`, `@-`, a bare origin of a flavored port, or several words refuse the whole list ([ref](reference/synth.md#the-port-list)) | `portscan-pilot.adb` `valid_file`, `portscan.adb` `input_origin_valid` | **differs on purpose**: optique reads poudriere's list syntax in every mode; a list synth refuses is the user's input to fix, not optique's to report |
| A trailing `/` is stripped from an argument but kept, and refused, on a list-file line ([ref](reference/synth.md#the-port-list)) | `portscan-pilot.adb` | **differs on purpose**: synth failing on its own input; optique trims the `/` either way and says nothing |
| No MOVED: a listed origin in no tree is missing from the index ([ref](reference/synth.md#the-port-list)) | `portscan.adb` `input_origin_valid` | **differs on purpose**: one MOVED moves is flagged as for any build, pending, the list needing a fix; never followed, `--follow-moved` ignored with a note, synth never building the target |
| A dependency on a flavored port without `@flavor` goes to the first flavor in the index ([ref](reference/synth.md#parsing-dependencies)) | `portscan.adb` | matches: make's default flavor is the first in `FLAVORS` |
| A dependency's existence is judged from synth's `SUBDIR` index ([ref](reference/synth.md#walking-the-closure)) | `portscan.adb` | **differs on purpose**: optique tests for a directory with a Makefile, as poudriere does; a port missing from its category's `SUBDIR` is the user's tree to fix, synth failing on its own input, not optique's to report |
| `LIB_DEPENDS_ALL` parsed as a build dependency ([ref](reference/synth.md#parsing-dependencies)) | `portscan.adb` | not modelled: it moves synth's rebuild checks (`librun`), not the closure or a loop, which count every kind |
| The cached options check: a file whose entries differ in number from the port's options, or name one it lacks, fails the build ([ref](reference/synth.md#cached-options-check)) | `portscan-packages.adb` `passed_options_cache_check` | matches: such a port trips `scan`'s gate under `-s`, with the reason |
| That check counts lines, not names, so an option named twice fails ([ref](reference/synth.md#cached-options-check)) | `portscan-packages.adb` | **conforms to an upstream defect** |
| That check looks for `<cat>_<port>@<flavor>/options`, so a flavored port's file is never found and always passes ([ref](reference/synth.md#cached-options-check)) | `portscan-packages.adb` | **conforms to an upstream defect**: flavored ports are not checked |
| That check slices bytes 17-20 of each line unchecked, aborting on a short line ([ref](reference/synth.md#cached-options-check)) | `portscan-packages.adb` `passed_options_cache_check` | **conforms to an upstream defect** for what optique writes: no line it writes trips it. A file optique did not write is the synth user's: no warning |
| Prebuilt packages: an option absent from the port, a value that differs, or a count that differs rebuilds ([ref](reference/synth.md#checking-existing-packages)) | `portscan-packages.adb` | matches: optique writes complete files, and `-m`/`--redundant` remove a file rather than shorten it |

## How it is checked

Two kinds of tests pin the table:

- Unit tests model the framework's rules on hand-built port data: the
  layering, patterns, groups, flavors sharing a file, the file reader, the
  poudriere.d and synth.ini readers, MOVED, the query environment. They
  alone pin most poudriere and synth rows, driven through fake poudriere.d
  and synth trees: no fixture test runs a poudriere (`-j`) build, and one
  alone a synth (`-s`) scan, for a listed moved origin.
- Fixture tests run the binary over a handful of minimal ports laid on the
  host's real `/usr/ports/Mk` (`tests/ports`, `tests/common`), so `make`
  decides and optique must agree with it. `make tests-live` runs them:
  - `tests/fixture.rs`: a conditional group (`misc/cond`, after
    mail/faces), an `OPTIONS_SLAVE` flavor (`misc/slavey`, after
    databases/sqlite3@icu), three flavors sharing a file (`misc/flav`,
    after editors/emacs), a forced group member (`misc/forced`), a
    Latin-1 make.conf, a dependency on a moved origin, a listed moved
    origin flagged, followed under `--follow-moved`, kept by a local build
    while in the tree, and flagged under `-s`; a listed subpackage through
    MOVED, a followed move closing no loop through a dependency on the old
    origin; a make.conf `PORT_DBDIR`
    elsewhere, and a per-flavor `OPTIONS_FILE` against `clean`; an
    `OPTIONS_FILE` moved for one flavor leaving the shared file managed,
    in either order; an `options.local` `?=` leaving the file unmanaged,
    and its SET glob leaving a written file up to date; a TUI newcomer
    whose file optique cannot model, shown with make's outcome;
  - `tests/fixture_model.rs`: new options counted by line; a hand-written
    file, `:=`/`?=`, a continued line; `options.local` after the file,
    without one, kept by `-m`, locking `decide` by UNSET only, its SET a
    default; a file optique cannot model, a moved `OPTIONS_FILE`; UNSET
    patterns and SET globs (`misc/model-globs`); PREVENTS patterns and an
    unlisted IMPLIES target (`misc/model-patterns`); an `OPTIONS_SLAVE`
    option's PREVENTS (`misc/model-slave-prevents`); the PREVENTS of an
    excluded option a group member implies
    (`misc/model-implied-prevents`); `WITH`/`WITHOUT`;
    `OPTIONS_OVERRIDE`, set and empty; a group named after a defined member
    (`misc/model-defined-group`); an IMPLIES chain (`misc/model-implies`);
    an overfull RADIO group against `make check-config`
    (`misc/model-radio`); a pruned group member's dependency
    (`misc/condep`);
  - `tests/fixture_lists.rs`: `@all` and `@-`.

Every fixture port depends on a `ports-mgmt/pkg` stub, as every real port
depends on pkg. poudriere and synth themselves were not run: their side
of each row comes from reading their source.

## Not checked

- synth's pkgsrc mode, its NetBSD, Linux and SunOS profiles, and its test
  mode (`DEVELOPER=1`).
- A run against a real poudriere jail or a real synth build.
- A fixture test for MOVED in overlays (unit tests pin the order);
  synth.ini lines longer than its 1,000-character limit.
