# synth

synth 3.15 builds packages in chroot "builders" assembled from null mounts
and tmpfs. Before building it starts one more chroot, the scanner, asks
`make` there for each port's version, options and dependencies, and compares
the answers with the packages already in its repository. Paths below are
relative to the synth source tree. `Definitions.software_framework` is the
constant `ports_collection` (`src/definitions.ads`), so the pkgsrc branches
of the code are dead in this build and left out.

One run, from the configuration to the build:

```text
synth.ini ─▸ profile ─▸ Directory_*         (host)
                │
                ▾
ensure_port_index ──────▸ /var/cache/synth/<profile>-index
                │         one origin[@flavor] per line
                ▾
store_origins: arguments, a file, or installed packages
                │         each must match an index line
                ▾
scanner chroot SL09 ────▸ one make -V run per port, depth first
                │         15 values by line; *_DEPENDS ▸ blocked_by
                ▾
limited_cached_options_check
                │         Directory_options/<cat>_<port>/options
                │         vs the scanned option names
                ▾  any stale file: stop
limited_sanity_check ───▸ per package in Directory_packages/All:
                │         file name, options, ABI; then dependencies,
                │         repeated until nothing changes
                ▾
ignored ports and their dependents removed; build the rest
```

Contents:

- [Configuration file](#configuration-file)
- [The build environment](#the-build-environment)
- [Options](#options)
- [The scan](#the-scan)
- [The rebuild decision](#the-rebuild-decision)
- [Limits visible in the code](#limits-visible-in-the-code)
- [Sources](#sources)

## Configuration file

The configuration is one INI file, `/usr/local/etc/synth/synth.ini`
(`Parameters.conf_location`, built from `synth_confdir`, which is
`host_localbase & "/etc/synth"`). It holds one master section and one section
per profile:

```ini
; This Synth configuration file is automatically generated
; Take care when hand editing!

[Global Configuration]
profile_selected= LiveSystem

[LiveSystem]
Operating_system= FreeBSD
Directory_packages= /var/synth/live_packages
Directory_repository= /var/synth/live_packages/All
Directory_portsdir= /usr/ports
Directory_options= /var/db/ports
Directory_distfiles= /usr/ports/distfiles
Directory_buildbase= /usr/obj/synth-live
Directory_logs= /var/log/synth
Directory_ccache= disabled
Directory_system= /
Number_of_builders= 6
Max_jobs_per_builder= 4
Tmpfs_workdir= true
Tmpfs_localbase= true
Display_with_ncurses= true
leverage_prebuilt= false
```

The key order is the one `Parameters.generated_section` writes. The values
shown are the defaults of `Parameters.load_specific_profile` for an
eight-core FreeBSD host with enough memory.

### How a value is read

The ini-files library (`config.adb`, `Get_Value`, `Value_Of`) re-reads the
file for every key, case-sensitively (`Parameters.load_configuration` opens
it with `On_Type_Mismatch => Be_Quiet`):

- A header is a line starting with `[`; the name runs to the first `]` and
  keeps any spaces. An indented header is not one. Every header with the
  same name opens the same section.
- One-character lines and lines starting with `;` or `#` are skipped; on
  other lines everything from the first `;` or `#` is cut off.
- The key is the text before the first `=` and the value the text after it,
  both trimmed of spaces only; a tab stays part of the key.
- The first matching key wins. An absent or empty string value, or an
  integer that does not parse, yields the default silently. A boolean other
  than `true` or `false` (any case) raises an exception, and synth stops
  with "Configuration failed to load."

`Parameters.param_set` treats a key as set when it has a non-empty value
(it asks with the sentinel default `this-is-garbage`); `section_exists` is
the same test on one key, not a test on the header.

### Choosing the profile

`Parameters.load_configuration` picks the profile in this order:

1. The `SYNTHPROFILE` environment variable, if it is non-empty and the
   section it names has a non-empty `Directory_packages`.
2. `profile_selected` in `[Global Configuration]`, if non-empty.
3. `LiveSystem` (`Parameters.live_system`).

There is no command-line flag for it (`synth.adb` reads only a command
keyword). `synth configure` (`Actions.launch_configure_menu`) switches
profile by rewriting `profile_selected` (`write_master_section`).

### How missing keys are filled

If `synth.ini` does not exist, `load_configuration` first checks that a
ports directory can be found (`determine_portsdirs`), then creates the file
with the two comment lines. If `[Global Configuration]` has no
`profile_selected`, a blank `[Global Configuration]` header is appended. If
the selected profile has no `Directory_packages`, a blank `[<profile>]`
header is appended, even when a header of that name already exists.

`Parameters.load_specific_profile` then reads every key. The constant
defaults are those of the example above; the others are computed:

| Key                    | Default when absent or empty          |
| :--------------------- | :------------------------------------ |
| `Directory_portsdir`   | `determine_portsdirs` (below)         |
| `Directory_distfiles`  | `make -V DISTDIR` on the host (1)     |
| `Operating_system`     | `make -V OPSYS` on the host (1)       |
| `Number_of_builders`   | `default_parallelism`, by CPU count   |
| `Max_jobs_per_builder` | same                                  |
| `Tmpfs_workdir`        | true if memory / builders >= 1280 MiB |
| `Tmpfs_localbase`      | same                                  |

(1) Run as `make -C <portsdir>/devel/gmake -V <var>`.

`Directory_repository` is never read: it is always `Directory_packages`
plus `/All`, and only written back. The two `make` queries (`query_generic`)
run on the host with its `/etc/make.conf`; `devel/gmake` is only a directory
to run `make` in.

`determine_portsdirs`, used only when `Directory_portsdir` is not set, tries
in order:

1. `PORTSDIR` from the environment, if it names an existing path.
2. `make -f /usr/share/mk/bsd.port.mk -V PORTSDIR` on the host; an empty
   answer is an error ("a blank PORTSDIR is defined in /etc/make.conf").
3. `/usr/dports`, then `/usr/ports`, whichever exists first.

`Operating_system` must be `FreeBSD` or `DragonFly` (`opsys_ok`), or synth
stops. `default_parallelism` maps the CPU count (`PortScan.set_cores`,
capped at 64) to builders and jobs: 1 to 1/1, 2-3 to 2/2, 4-5 to 3/3, 6-7
to 4/3, 8-9 to 6/4, 10-11 to 8/4, and above that three quarters of the
cores with 5 jobs.

When any of the 16 profile keys is not set (`all_params_present`), the whole
profile section is rewritten from the loaded record with
`internal_config.Replace_Section`, so computed defaults are frozen into the
file on first use and comments inside the section are lost. When
`profile_selected` was missing, `write_master_section` writes the profile
just loaded, which may be the one `SYNTHPROFILE` chose.

The manual says the options directory's initial value "comes from a system
scan" (`synth.1`); the code uses the constant `std_options`, `/var/db/ports`.

For every command except `configure`, `Parameters.all_paths_valid` requires
the system, packages, ports, distfiles, logs and options directories to
exist, and `Directory_ccache` too unless it is `disabled`.

## The build environment

### Mounts

`Replicant.launch_slave` builds each builder, and the scanner, as a tmpfs
at `<Directory_buildbase>/SL<nn>`; the scanner is number 9
(`PortScan.scan_slave`, `ss_base` `/SL09`), and fills it with null mounts:

```text
<Directory_buildbase>/SL<nn>          tmpfs; the scanner is SL09
├── bin sbin lib libexec           ro ◂ Directory_system
├── usr/bin include lib libdata    ro ◂ Directory_system
│       libexec sbin share
├── usr/src usr/lib32 boot         ro ◂ Directory_system, if they exist
├── etc/                              created, filled by launch_slave
├── xports                         ro ◂ Directory_portsdir
├── options                        ro ◂ Directory_options
├── packages                       rw ◂ Directory_packages
├── distfiles                      rw ◂ Directory_distfiles
├── construction                   rw   tmpfs if Tmpfs_workdir,
│                                       else SL<nn>_work
├── usr/local                      rw   tmpfs if Tmpfs_localbase,
│                                       else a directory
└── ccache                         rw ◂ Directory_ccache, if it exists
```

With `Directory_system` equal to `/` the host paths are used directly
(`clean_mount_point`). `/etc` is not mounted: it is created empty and filled
by `launch_slave` (`create_make_conf`, `create_passwd`, `copy_resolv_conf`
and others). The host's `/etc/make.conf` therefore never reaches a builder
or the scanner; `/usr/share/mk` comes from `Directory_system`.

### make.conf

`Replicant.create_make_conf` writes `/etc/make.conf` inside every chroot,
scanner included. Its content, in order:

```make
SYNTHPROFILE=<profile>
USE_PACKAGE_DEPENDS_ONLY=yes
PACKAGE_BUILDING=yes
BATCH=yes
PKG_CREATE_VERBOSE=yes
PORTSDIR=/xports
DISTDIR=/distfiles
WRKDIRPREFIX=/construction
PORT_DBDIR=/options
PACKAGES=/packages
MAKE_JOBS_NUMBER_LIMIT=<Max_jobs_per_builder>
DEVELOPER=1                  # only in test mode (synth test)
WITH_CCACHE_BUILD=yes        # only if Directory_ccache exists
CCACHE_DIR=/ccache           # idem
# then, copied verbatim if present:
#   /usr/local/etc/synth/<profile>-make.conf
#   <Directory_buildbase>/Base/varcache.conf
```

`concatenate_makeconf` appends the two files line by line and skips a
missing one. make keeps the last assignment, so the profile file can override
synth's header (`PORT_DBDIR`, `OPTIONS_SET`, `<cat>_<port>_SET`...) and
`varcache.conf` overrides both. The ccache test is `Ada.Directories.Exists`
on the configured string, so `disabled` is tested as a relative path.

### varcache.conf

`Replicant.initialize` recreates `<Directory_buildbase>/Base`, and
`Replicant.Platform.cache_port_variables` writes `Base/varcache.conf`:

- `HAVE_COMPAT_IA32_KERN`, `CONFIGURE_MAX_CMD_LEN`, `_PERL5_FROM_BIN` and,
  when non-empty, the `_CCVERSION_*`, `_CXXINTERNAL_*`, `_OBJC_CCVERSION_*`
  and twelve `CC_OUTPUT_921dbbb2_*` compiler probes, read by position from
  one host run of `make __MAKE_CONF=/dev/null -C <portsdir>/ports-mgmt/pkg`
  with `USES="python compiler:features objc" GNU_CONFIGURE=1 USE_JAVA=1
  USE_LINUX=1`;
- `_ALTCCVERSION_921dbbb2=none`, `_OBJC_ALTCCVERSION_921dbbb2=none`,
  `_SMP_CPUS=<cores>`, `UID=0`, `PYTHONBASE=/usr/local`, `_PKG_CHECKED=1`;
- `ARCH`, mapped from `file -b <Directory_system>/bin/sh`
  (`get_arch_from_bourne_shell`: `amd64`, `aarch64`, `armv7`, `i386`, else
  `BADARCH`);
- `OPSYS=FreeBSD`, and `OSVERSION` from the `__FreeBSD_version` define in
  `<Directory_system>/usr/include/sys/param.h`;
- `_OSRELEASE=<major>.<minor>-SYNTH` derived from it (`create_OSRELEASE`),
  and `OSREL` without the suffix.

The platform therefore comes from the system root's files, not `uname`; the
compiler probes come from the host.

### Environment of a make run

The scanner's `make` is started through `/usr/bin/env -i` with only
`PATH=/sbin:/bin:/usr/sbin:/usr/bin TERM=dumb USER=root HOME=/root LANG=C`
(`PortScan.scan_environment`). Builders use a different list
(`PortScan.Buildcycle.environment_override`): the same plus `/usr/local/sbin`
and `/usr/local/bin` in `PATH`, `PORTSDIR=/xports`, `PKG_DBDIR`,
`PKG_CACHEDIR`, `SSL_NO_VERIFY_PEER=1`, and every line containing `=` from
`/usr/local/etc/synth/<profile>-environment` (`obtain_custom_environment`).
That profile environment file is not applied to the scanner.

## Options

synth never writes options files and has no command that edits them
(`synth configure` edits only `synth.ini`). The ports framework in the
chroot decides the options, from `PORT_DBDIR=/options` (the read-only mount
of `Directory_options`) and whatever `<profile>-make.conf` sets. The manual
leaves it to the user to point `make config` at a non-default
`Directory_options` (`synth.1`, Port options directory).

synth reads options at three points. During the scan,
`populate_set_options` stores each name of `SELECTED_OPTIONS` (value true)
and `DESELECTED_OPTIONS` (false), split on spaces, in the port's `options`
map; a name already present is not overwritten.

### Cached options check

Before anything is built, dry runs (`synth status`) included,
`PortScan.Pilot.sanity_check_then_prefail` calls
`PortScan.Packages.limited_cached_options_check`, which runs
`passed_options_cache_check` on every port in the queue.

`passed_options_cache_check` reads the host file
`<Directory_options>/<name>/options`, where `<name>` is the port's key with
`/` replaced by `_`. If the file does not exist, the port passes. Otherwise
each line is examined:

- if bytes 17-18 are `+=`, the rest of the line from byte 19 is an option
  name (an `OPTIONS_FILE_SET+=` line);
- else if bytes 19-20 are `+=`, the rest from byte 21 is an option name (an
  `OPTIONS_FILE_UNSET+=` line);
- other lines are ignored.

The port passes when every such name is in its `options` map and the
number of such lines (not of distinct names) equals the size of the map.
Whether a name was under `SET` or `UNSET` is not compared with the port's
current selection. One failing port is enough: synth prints "Cached options
obsolete: <origin>" for each, sets the exit status to failure, suggests
`make -C <portsdir>/<origin> config` or `rmconfig`, and stops the command.
The file is left as it is.

### Checking existing packages

`PortScan.Packages.passed_option_check` compares a package with the port.
For a local package it runs `pkg query -F <Directory_packages>/All/<pkgfile>
%Ok:%Ov`; for a remote one, `pkg rquery -r <repo> -U %Ok:%Ov <pkgname>`.
Each output line must be `<name>:on` or `<name>:off`. The package fails when
an option is absent from the port's map, when its value differs from the
port's, or when the counts differ. A failing local package is marked for
deletion (`initial_package_scan`); a failing remote package is not used
(`remote_package_scan`).

## The scan

### The flavor index

Every command except `configure` first calls
`PortScan.Pilot.ensure_port_index` for `/var/cache/synth/<profile>-index`,
one origin per line. It is regenerated when missing, or when
`index_out_of_date` finds the ports directory, a category directory or an
entry directly inside a category newer than the index and either `TERM` is
`dumb` or the port directories differ from the index
(`tree_directories_match`). Otherwise synth asks; "no" touches the index.

`PortScan.generate_ports_index` builds the list of ports with
`prescan_ports_tree` on the host's `Directory_portsdir`: every directory
matching `[a-z]*` except `base`, `distfiles` and `packages`, sorted; for a
category with a `Makefile`, every line matching
`^SUBDIR[[:space:]]*[:+:]=[[:space:]]*([[:graph:]]*)` after trimming
(`grep_Makefile`); for a category without one, every subdirectory
(`walk_all_subdirectories`). It then runs the normal scan of all those ports
in the scanner with dependency and option parsing turned off (`fullpop`
false), and writes `<origin>@<flavor>` for each entry of `FLAVORS`, in the
order make returned them, or the bare origin when `FLAVORS` is empty.

### The port list

`PortScan.Pilot.store_origins` builds the list from the arguments: a single
argument naming an existing file is read one origin per non-blank line
(`valid_file`); otherwise each argument, less a trailing `/`, is an origin.
Each must appear verbatim in the index (`input_origin_valid`), so a flavored
port without `@flavor` is rejected with a list of its flavors
(`suggest_flavor_for_bad_origin`). `ports-mgmt/pkg` is dropped; it is built
first (`prerequisites_available`).

- `upgrade-system`, `prepare-system` and argument-less `status`
  (`upgrade_system_everything`) take `pkg query -a %o:%n` on the host. An
  origin in the index unflavored is used as is; otherwise the package's
  `flavor` annotation (`pkg query %At:%Av`) gives `<origin>@<flavor>`.
  Unmatched packages are reported as ignored.
- `everything` and `status-everything` (`fully_scan_ports_tree`) scan every
  index line, split round-robin into up to 64 lots scanned in parallel
  (`parallel_deep_scan`; `get_max_lots`, about 1.5 times the core count).

### The query

For each port, `populate_port_data_fpc` runs, with `env -i` as above:

```sh
/usr/sbin/chroot <Directory_buildbase>/SL09 /usr/bin/make \
    -C /xports/<category>/<port> [FLAVOR=<flavor>] <cached variables> \
    -VPKGVERSION -VPKGFILE:T -VMAKE_JOBS_NUMBER -VIGNORE \
    -VFETCH_DEPENDS -VEXTRACT_DEPENDS -VPATCH_DEPENDS \
    -VBUILD_DEPENDS -VLIB_DEPENDS -VRUN_DEPENDS \
    -VSELECTED_OPTIONS -VDESELECTED_OPTIONS -VUSE_LINUX \
    -VFLAVORS -VLIB_DEPENDS_ALL
```

`FLAVOR=` is added when the key has an `@`. The cached variables come from
`set_port_cache_variables`, which once per scan runs `make -C /xports` in the
scanner with `-V` for `ARCH OPSYS _OSRELEASE OSREL OSVERSION _PKG_CHECKED
HAVE_COMPAT_IA32_KERN _SMP_CPUS CONFIGURE_MAX_CMD_LEN` and passes the answers
as `NAME=value` arguments to every port query.

Standard output and standard error go to one file (`Unix.external_command`,
`synexec.c`), `/tmp/synth.scanner.<lot>.out` on the host (lot 09 for
single-port scans), and lines are assigned by position:

| Line | Variable              | Use                                        |
| :--- | :-------------------- | :----------------------------------------- |
| 1    | `PKGVERSION`          | `port_version`                             |
| 2    | `PKGFILE:T`           | `package_name`, the expected package file  |
| 3    | `MAKE_JOBS_NUMBER`    | `jobs`; `Number_of_builders` if unparsable |
| 4    | `IGNORE`              | `ignore_reason`; non-blank: port ignored   |
| 5-7  | `FETCH/EXTRACT/PATCH` | dependencies                               |
| 8    | `BUILD_DEPENDS`       | dependencies                               |
| 9-10 | `LIB/RUN_DEPENDS`     | dependencies, also recorded as library/run |
| 11   | `SELECTED_OPTIONS`    | options map, value true                    |
| 12   | `DESELECTED_OPTIONS`  | options map, value false                   |
| 13   | `USE_LINUX`           | non-blank requests linprocfs               |
| 14   | `FLAVORS`             | flavor list                                |
| 15   | `LIB_DEPENDS_ALL`     | dependencies, as build type                |

A port is marked scanned even if the file had fewer lines. A non-zero exit
of `make` raises `bmake_execution`.

### Parsing dependencies

`populate_set_depends` splits a dependency variable on spaces and tabs. For
each word:

1. The first `:` must be at byte 2 or later, or `make_garbage` is raised.
2. After that colon, a leading `/xports/` or `../../` is removed.
3. Anything from the next `:` on (a target such as `:patch`) is removed
   (`scrub_phase`). The rest, including an `@flavor`, is the key.
4. The key is looked up in the scanned port table. If absent, the base
   origin is looked up in `so_porthash`, which maps it to the first
   index line for that origin, that is the default flavor. If that fails
   too, `nonexistent_port` is raised.

The target is added to the port's `blocked_by` set, whatever the type. For
`LIB_DEPENDS` and `RUN_DEPENDS` it is also added to `librun`, and
`min_librun` counts the distinct entries. `TEST_DEPENDS`, `PKG_DEPENDS` and
per-subpackage `RUN_DEPENDS` are not queried. `LIB_DEPENDS_ALL` is parsed as
a build dependency; in this host's `Mk/bsd.port.mk` (the `depends` target
block) it is the union of `LIB_DEPENDS` over all subpackages.

### Walking the closure

For named ports, `PortScan.Pilot.scan_stack_of_single_ports` starts the
scanner, extracts `pkg-static` into it from
`/packages/Latest/pkg.pkg` (`Replicant.Platform.standalone_pkg8_install`),
and calls `PortScan.scan_single_port` for each origin in the list. That
queries the port, then recurses depth-first through `blocked_by` (`dig`),
one `make` at a time; a port already scanned is not queried again. A
dependency met again while it is on the current path raises
`circular_logic`, which stops the command. Any other failure in the closure
drops that command-line origin with "it will not be considered" and the scan
goes on with the next one.

The closure covers every dependency type, not only what is needed to run.
`set_build_priority` then fills each port's `blocks` set (the reverse of
`blocked_by`) and ranks ports by the number of ports that depend on them,
directly or not (`drill_down`); a cycle found there also raises
`circular_logic`.

### IGNORE

A non-blank `IGNORE` only marks the port; its dependencies are still scanned
and queued. After the package checks, `sanity_check_then_prefail` takes each
ignored port still queued (`PortScan.Ops.next_ignored_port`), logs it, and
removes it with every port depending on it (`cascade_failed_build`). An
ignored port whose package passed the checks has already left the queue and
is not reported. No other variable (`BROKEN`, `FORBIDDEN`) is queried.

## The rebuild decision

`PortScan.Packages.limited_sanity_check` decides, for every port in the
queue, whether its package can be kept. For each scanned port,
`initial_package_scan` does:

1. Version: the package file `<Directory_packages>/All/<PKGFILE:T>` must
   exist. Since the file name carries the version, a version change means
   the file is absent and the port is built. Nothing else compares versions.
2. Options: `passed_option_check`, described above. Failure marks the file
   for deletion.
3. ABI: `passed_abi_check` accepts a `pkg query -F <file> %q` of
   `FreeBSD:<major>:amd64`, `freebsd:<major>:x86:64` or either with `*` as
   architecture, from `file -b <Directory_system>/bin/sh`
   (`Replicant.Platform.determine_package_architecture`; amd64 shown).
   Failure marks it for deletion.
4. If both pass, `pkg query -F <file> %do:%dn-%dv` is stored for the next
   step.

Then `check_package` runs over the queue repeatedly until a pass changes
nothing. For a package still available, `passed_dependency_check` reads each
recorded dependency `<origin>:<name>-<version>` and requires:

- the origin to be in the index; for a flavored origin, the flavor whose
  scanned package name has the same part before the first `-` is used;
- the recorded `<name>-<version>.pkg` to equal that port's `PKGFILE:T`;
- that port's package to be available (present and not marked for deletion,
  or found remotely);
- at most `librun` entries in total, and at least `min_librun`.

A package that fails is marked for deletion, which in turn fails its
dependents on the next pass. Changes therefore propagate only through
library and run dependencies, the ones `pkg` records; a changed build
dependency does not cause a rebuild. Packages kept are removed from the
queue with `PortScan.Ops.cascade_successful_build`; marked files are deleted
(`prune_packages`) unless this is a dry run.

With `leverage_prebuilt` true, ports without a usable local package (other
than those named to `force` or `test`, `never_remote`) are checked against
the first repository other than `Synth` that `pkg -vv` lists
(`located_external_repository`), with the same ABI, options and dependency
tests on `pkg rquery` output; matches are fetched with `pkg fetch` into
`Directory_packages`. `force` and `test` delete the named ports' packages
first (`force_delete`), so those are always built.

## Limits visible in the code

- The cached options check derives the directory from the port key, so for a
  flavored port it looks for `<cat>_<port>@<flavor>/options`. The framework's
  default `OPTIONS_NAME` is `PKGORIGIN` with `/` replaced by `_`
  (`Mk/bsd.options.mk`), without the flavor, so for flavored ports the file is
  never found and the check always passes. A port that sets its own
  `OPTIONS_NAME` is checked against the wrong file in the same way.
- The cached options check reads the host's `Directory_options`; options set
  through `<profile>-make.conf` are not part of that check, only of the scan.
- `passed_options_cache_check` slices bytes 17-18 and 19-20 of each line
  without checking its length. The build (`default.gpr`) keeps Ada's run-time
  checks, so a line shorter than 18 bytes, or an 18- or 19-byte line without
  `+=` at 17-18, raises `Constraint_Error`. No handler on the call path
  (`limited_cached_options_check`, `sanity_check_then_prefail`, `synth.adb`)
  catches it.
- The scan output is parsed by line position with standard error merged into
  it, so a line of diagnostics from `make` shifts every following value.
- A dependency without `@flavor` on a flavored port resolves to the first
  flavor in the index, that is the first entry `FLAVORS` returned when the
  index was built.
- `index_out_of_date` looks no deeper than the entries of each category
  directory, so an in-place edit of a port's `Makefile` (a new flavor, say)
  does not by itself mark the index stale.
- `id2pkgname` and the dependency check assume the package suffix is
  `.pkg` (four bytes).

## Sources

synth 3.15 (`Definitions.synth_version_major`/`minor`), as packaged by
`ports-mgmt/synth` (`DISTVERSION=3.15`); the port's one patch,
`files/extrapatch-nowatchdog-portscan-buildcycle.adb`, only disables the
build hang monitor. Files read: `synth.adb`, `synth.1`, `default.gpr`,
`src/definitions.ads`, `src/parameters.ads`, `src/parameters.adb`,
`src/portscan.ads`, `src/portscan.adb`, `src/portscan-pilot.adb`,
`src/portscan-packages.adb`, `src/portscan-ops.adb` (cascade and ignore
functions), `src/portscan-buildcycle.adb` (`environment_override`,
`obtain_custom_environment`), `src/portscan-buildcycle-ports.adb`
(`exec_phase`), `src/replicant.ads`, `src/replicant.adb`,
`src/replicant-platform.adb`, `src/unix.adb`, `src/synexec.c`,
`src/actions.adb` (configure menu); the ini-files library's `config.adb`
(`Get_Value`, `Value_Of`, `Replace_Section`); and, from the ports tree on
this host, `Mk/bsd.options.mk` and `Mk/bsd.port.mk`.
