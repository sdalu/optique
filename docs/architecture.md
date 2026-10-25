# Architecture

How the code is laid out and how it behaves: the layers, the code map,
what each command runs, the model of a port's options, and the TUI. It
describes the code as it is; the reasons behind the shape, and what was
rejected, are in [DESIGN.md](../DESIGN.md); how each behaviour compares
with the ports framework, poudriere and synth is in
[conformance.md](conformance.md). Functions and types are named rather
than given line numbers, which drift.

## Layers

Each layer uses only those below it. The TUI and the command-line
interfaces sit side by side on the session.

```text
┌──────────────────────────────────────────────────────────────────────┐
│ entry           main.rs · cli.rs · cmd/ · color.rs                   │
│                 parse, dispatch, run_scan, banner, exit status       │
├──────────────────────────────────────────────────────────────────────┤
│ resolution      config/{mod,local,poudriere,synth}.rs · roots.rs     │
│                 synth.rs                                             │
│                 which build, tree, options dir, staged make.conf,    │
│                 blacklist, overlays; which roots                     │
├──────────────────────────────────────────────────────────────────────┤
│ query           query/{scanner,makerunner,wrapper,parse,refresher}   │
│                 cache/ · moved.rs · deps.rs                          │
│                 wrapper | make -f /dev/stdin → PortInfo, closure     │
├──────────────────────────────────────────────────────────────────────┤
│ model           model/{origin,port,options}.rs                       │
│                 optionsfile.rs · optfiles.rs                         │
│                 PortKey, PortInfo, PortOptions; options files read   │
├──────────────────────────────────────────────────────────────────────┤
│ session         session/                                             │
│                 saved / baseline / staged per options file;          │
│                 the framework's rules as functions                   │
├───────────────────────┬────────────────────────┬─────────────────────┤
│ writes                │ interfaces             │ TUI                 │
│ apply/ · clean.rs     │ report.rs · decide.rs  │ tui/{mod,ui,driver} │
│ staging.rs            │ JSON, plans            │ draft.rs            │
└───────────────────────┴────────────────────────┴─────────────────────┘
```

## Code map

| File | Responsibility | Main items |
| :-- | :-- | :-- |
| `main.rs` | Entry, dispatch, exit status (1 = something pending, 2 = usage, 3 = error) | `run`, `exit_pending`, `split_raw_origins` |
| `cmd/mod.rs` | The scan every command shares: settings, roots through MOVED, the closure, what it found said on stderr | `run_scan`/`Scanned`, `RootSet`, `roots_or_installed`, `query_ctx` |
| `cmd/scan.rs` | `scan`: the table or the JSON, and the gate | `cmd_scan`, `needs_attention`, `shown_ok` |
| `cmd/clean.rs` | `clean`: vanished, unused and redundant entries | `cmd_clean`, `clean_options_dir`, `find_redundant` |
| `cmd/write.rs` | A pass of writes and removals, and its summary | `WritePass`, `commit_writes`, `print_write_summary` |
| `cmd/sync.rs`, `cmd/decide.rs`, `cmd/tui.rs` | `sync`, `decide`, and the TUI over a fresh scan | `cmd_sync`, `cmd_decide`, `cmd_tui` |
| `color.rs` | When to paint a stream, and the escapes | `tint`, `stderr_color`, `stdout_color`, `ansi` |
| `cli.rs` | The command line: clap definitions, the checks clap cannot do across levels, roots from arguments and `-f` lists | `disambiguate_synth`, `check_action_first`, `Cli::check_globals`, `collect_roots` |
| `config/mod.rs` | Which build is configured and how queries see it: the resolution every build shares (overlays, options place, framework values, the staged make.conf and its hash) and the choice between local and poudriere | `Request`, `Settings`, `QueryEnv`, `resolve`, `resolve_host`, `finish`, `stage_make_conf` |
| `config/local.rs` | A local build: the host's make.conf, PORTSDIR and PORT_DBDIR | `resolve`, `make_defaults` |
| `config/poudriere.rs` | A poudriere build: poudriere.d's make.conf layers and hook plugins, the jail's system, options dir, blacklist, poudriere.conf settings | `resolve`, `jail_system`, `plugin_make_confs`, `concat_make_conf`, `Blacklist` |
| `config/synth.rs` | A synth build: synth.ini read as synth reads it, the profile's make.conf under synth's scan header, the tree synth falls back to | `resolve`, `resolve_synth`, `synth_ini_lookup`, `synth_ini_misreads` |
| `roots.rs` | Roots as the build tool reads them, before the scan: MOVED for the listed origins, build by build, then `@all`/`@-` | `apply_moved`, `MovedPolicy`, `MovedRoots`, `resolve`, `expand`, `query_flavors` |
| `synth.rs` | What synth does with the scan: with no list, the installed packages matched against its flavor index; the options-file check that refuses a build | `installed_roots`, `installed_in_index`, `flavor_index`, `refuses_port` |
| `query/wrapper.rs` | The makefile piped to `make`: includes the port, prints `OPTIQUE\|TAG\|value` lines | `WRAPPER` |
| `query/makerunner.rs` | One `make` per port, cleared environment, the builder's `USER` and `HOME`, a pool of workers, killable | `QueryCtx`, `Identity`, `run_query`, `MakeRunner`, `kill_active_makes` |
| `query/parse.rs` | The dump into a `PortInfo` | `parse_dump`, `parse_depends` |
| `query/scanner.rs` | The closure, breadth first from roots taken as given: cache, blacklist stop, a dependency on a nonexistent origin as an error | `scan`, `ScanResult`, `register` |
| `query/refresher.rs` | The TUI's background re-scans over the staging dir | `spawn`, `RefreshEvent` |
| `cache/mod.rs` | Query results on disk, one generation per tree × make.conf, a new commit's starting with what the diff leaves of the last | `Cache`, `CacheEntry`, `tree_key`, `Invalidation`, `SCHEMA` |
| `cache/makeconf.rs` | What an edit to make.conf's option policy reaches: which variables it touched, if nothing else, and which entries their new values change | `option_edit`, `Reach` |
| `moved.rs` | `MOVED` of the tree and its overlays, chains collapsed in file order as `parse_MOVED.awk` does, originspecs looked up as `check_moved` does | `Moved::load_all`, `Moved::resolve`, `MovedResult`, `ListedMove` |
| `deps.rs` | Alias resolution, dependency loops | `resolve`, `detect` (Tarjan), `DepLoop` |
| `model/origin.rs` | `category/port[@flavor]` | `PortKey::parse` |
| `model/port.rs` | One port as `make` describes it | `PortInfo`, `DepEdge` |
| `model/options.rs` | Its options and every make.conf layer naming them; bmake patterns | `PortOptions`, `OptionDef`, `OptionGroup`, `str_match`, `mc_value`, `forced_value`, `provenance` |
| `optionsfile.rs` | One options file (and its `options.local`) read as the makefile it is, each line it cannot model recorded; written as `make config` writes it, less what trips synth | `SavedOptionsFile`, `parse`, `with_local`, `unmanaged`, `render`, `decides` |
| `optfiles.rs` | Every options file of the options dir, each read from disk once | `OptionsFiles` |
| `session/mod.rs` | The editing model and the framework's rules | `Session`, `OptState`, `UiStatus`, `effective_with`, `unmanaged_file`, `moved`, `toggle` |
| `apply/mod.rs` | What to write and remove, and writing it atomically | `plan_writes`, `PendingWrite`, `plan_stale_removals`, `apply`, `commit` |
| `clean.rs` | Classifying options-dir entries | `classify_entries`, `split_unused`, `prune`, `read_through_moved`, `keep_read`, `redundancy_diff`, `remove_all` |
| `decide.rs` | A JSON plan applied to a session | `Plan`, `apply_plan` |
| `report.rs` | Every `--json` payload | `ScanReport`, `WriteReport`, `option_reports` |
| `staging.rs` | The TUI's private copy of the options dir | `StagingDb` |
| `draft.rs` | Unapplied TUI edits kept across runs | `Draft` |
| `tui/mod.rs` | The TUI's state, the run loop, the port list and its views | `App`, `build_app`, `event_loop`, `EditorRow`, `rebuild_visible` |
| `tui/keys.rs` | The keymap, shared by the event loop and the driver | `dispatch_key`, `KeyOutcome` |
| `tui/editor.rs` | The option editor's rows, cursor and toggle | `rebuild_editor`, `toggle_current` |
| `tui/undo.rs` | The undo stack and the draft a session left | `push_undo`, `undo_last`, `restore_draft_if_any` |
| `tui/apply_modal.rs` | What would be written, and writing it | `ApplyModal`, `commit_apply` |
| `tui/refresh.rs` | Re-queries after an edit, and what a change is blamed on | `flush_due_refreshes`, `drain_refresh_events` |
| `tui/overlays.rs` | The popups' state and the description panel | `WhyInfo`, `open_why`, `run_bulk` |
| `tui/ui/mod.rs` | Drawing: the frame, the status bar, the fitting helpers | `draw`, `draw_status_bar` |
| `tui/ui/list.rs`, `tui/ui/editor.rs`, `tui/ui/popups.rs` | Drawing the port list, the option editor, the popups | `draw_port_list`, `draw_editor`, `draw_why`, `draw_apply_modal` |
| `tui/ui/help.rs` | The help popup and the text of its tabs | `draw_help`, `HELP_TABS`, `help_lines` |
| `tui/driver.rs` | The TUI driven headless from stdin | `run_driver` |

## Commands

Every command but `clean` (without `--unused`) starts with the same scan.

```text
  argv ─▸ disambiguate_synth ─▸ check_action_first ─▸ parse ─▸ check_globals
                                                                      │
         ┌────────────────────────────────────────────────────────────┘
         ▾
  ┌─ run_scan ───────────────────────────────────────────────────────────┐
  │ config::resolve    Settings: tree, options dir, staged make.conf,    │
  │                    conf_hash, QueryEnv, blacklist, overlays, notes   │
  │ Cache::open        generation v<SCHEMA>-<tree>-<make.conf>           │
  │ Moved::load_all    MOVED of the tree and every overlay               │
  │ roots::apply_moved MOVED for the listed origins: flag or follow      │
  │ roots::resolve     @all, @-, FLAVOR_DEFAULT_ALL                      │
  │ scanner::scan      breadth-first closure, cache first, wrapper make  │
  │ deps::detect       dependency loops (Tarjan)                         │
  └──────────────────────────────────────────────┬───────────────────────┘
                                                 ▾
                                  Session::new (states per file)
                                                 │
        ┌───────────────┬─────────────────┬──────┴──────────┬────────────┐
        ▾               ▾                 ▾                 ▾            ▾
      scan            sync             decide             clean         tui
  rows, JSON,     sync_enabled_set  Plan::parse,    classify,     staging db,
  exit 1 when     plan_writes,      apply_plan,     --unused,     refresher,
  something       stale removals,   plan_writes,    --redundant,  event loop,
  is pending      commit            commit unless   remove_all    apply modal
                                    blocked: 1
```

- **scan** prints one row per port, or `--json`, and exits 1 when a port
  needs attention (`needs_attention`: a conflict, a decision owed, a file
  synth would refuse; never a blacklisted port), a loop blocks, or the
  scan has an error: a query failed, a listed origin is gone, or a port
  depends on a nonexistent origin.
- **sync** writes, for every port, what `make config` would leave: the
  saved value where the file has one, the computed one otherwise
  (`sync_enabled_set`); then removes the files of ports that lost every
  option.
- **decide** applies a plan's values through the same refusals as the
  TUI, then plans writes only for the files the plan touched. Anything
  rejected, unknown or in conflict blocks the whole commit and exits 1.
- **clean** classifies the options dir's entries (gone through MOVED,
  live), optionally drops those outside the closure (`--unused`) and
  those that change nothing (`--redundant`, one query per flavor sharing
  a file), never one optique cannot model; `--unused` refuses on any scan
  error but a make.conf `PORT_DBDIR`, and on a listed origin MOVED moves
  that was not followed. While make.conf sets `OPTIONS_FILE` or `OPTIONS_NAME`, `clean::prune`
  holds every vanished and unused entry; `clean::keep_read` keeps the
  entry a queried port's moved file lies in.
- **tui** adds a staging copy of the options dir, a refresher thread
  and the event loop on top of the session.

## Build resolution

`config::resolve` turns the command line into `Settings`, along one of
three paths: synth (`-s`), a local build (`-l`, or no poudriere.d), or
poudriere. Each stages one make.conf in the run's temp dir and hashes
it: the hash names the cache generation, so whatever changes a query's
answer is in the staged text.

| Build | Staged make.conf, in order | `QueryEnv` |
| :-- | :-- | :-- |
| poudriere | jail prelude (`USE_PACKAGE_DEPENDS`, `BATCH`), overlays' `OVERLAYS+=`, `PACKAGE_BUILDING…` unless `NO_PACKAGE_BUILDING`, the jail's system under `-j` (cross-build `MACHINE…` when its architecture is not the host's, then `_OSRELEASE`, `OSREL`, `OSVERSION`), the poudriere.d layers, hook plugins' `make.conf`, the ccache lines when poudriere.conf's `CCACHE_DIR` exists, `LOCALBASE=/nonexistent` | `Jail` |
| synth | `SYNTHPROFILE`, synth's scan header, ccache lines when `Directory_ccache` exists, `<profile>-make.conf`, `LOCALBASE=/nonexistent` | `Synth` |
| local | `__MAKE_CONF` or `/etc/make.conf`, read in place (`/dev/null` when there is none); overlays through a small wrapper make.conf | `Host` |

A layer reached twice through a symlink is read once. The blacklist is
read from the same layers, plus `qemu-blacklist` for a jail poudriere
would emulate. A local build also folds pkg's database (size, mtime)
and the invoking `USER` into the hash: what is installed, and who runs
the build, are part of its answer. Last, `with_framework` runs the probe
port (`wrapper::FRAMEWORK_PROBE`) once under the settled tree, make.conf
and environment, keeps the `_EXPORTED_VARS` bsd.port.mk reports
(`Settings::framework`), and folds them into the hash. Then
`with_options_place` asks make, reading no port's Makefile but run from
`ports-mgmt/pkg` (`config::probe_dir`), with no `PORT_DBDIR` on its
command line but, for a local build, the environment's in its own
(`makerunner::options_place`), what the make.conf alone sets
of `PORT_DBDIR` and `OPTIONS_FILE` (`Settings::options_place`, None when
make cannot say): `Settings::port_dbdir_error` compares the first with
where the build reads options, and `config::relocated_options` tells
`clean` from the second that entry names cannot be trusted.

`roots::apply_moved` reads MOVED for each listed originspec, as the build
reads it (`MovedPolicy` from `QueryEnv`: poudriere looks up every one,
a local build and synth only one in no tree), before anything expands. A
root MOVED moves becomes a `ListedMove`: left out with its target and
counted as pending by `cmd_scan`, or, under `--follow-moved` (never under
`-s`), replaced by its target. The old name is kept out of the closure's
aliases, which dependency edges resolve through: `decide::apply_plan`
alone finds a followed move's target from `Session::listed_moves`. A root
naming a subpackage (`roots::Listed`) is looked up whole
(`Moved::resolve_spec`). A removed
one, a cycle or a flavor clash is an error. `Scanned::moves` carries them
to every command: the JSON `moved`, `clean --unused`'s refusal, the TUI's
`MOVED:` line and first message (`Session::listed_moves`). This runs on
every invocation, never from the cache. `roots::resolve` then turns the
roots into the scan's: `@all` and (under `FLAVOR_DEFAULT_ALL`) bare origins
become every flavor of the port; `@-` becomes the default flavor.
`scanner::scan` takes its roots as given; a dependency never goes through
MOVED. Under `-s` with no list, `run_scan` first takes the installed
packages and keeps those synth's index has (`synth::installed_in_index`:
the categories' `SUBDIR` lines and each port's flavors).

## Querying and caching

`run_query` runs `/usr/bin/make -C <port> -f /dev/stdin optique-config`
with `PORTSDIR`, `PORTDIR`, `PORT_DBDIR` and the framework values
(`QueryCtx::framework`) on the command line, `__MAKE_CONF` and `FLAVOR`
when set, and an environment holding only
`PATH`, `TMPDIR` and `PKG_DBDIR`, with `USER` and `HOME` by the query's
`Identity`: `root` and `/root` for a poudriere or synth build (`QueryEnv`
`Jail` or `Synth`), the invoking user's own for a local one. The wrapper
prints the port's `OPTIONS_FILE` when make.conf moved it, its options as
computed (`COMPLETE_OPTIONS_LIST`, `PORT_OPTIONS`), every
make.conf layer naming them, groups, per-option definitions
(implies, prevents, descriptions, dependencies), dependencies, and
BROKEN/IGNORE/DEPRECATED; `parse_dump` makes a `PortInfo` of it. An
unreadable options file fails the port rather than reading as absent.

`scanner::scan` walks breadth first from the roots `apply_moved` left;
a dependency goes to the cache, then a worker, when its
origin is in some tree. One in no tree is never walked nor redirected:
`register` records it as an error on the depending port, saying what
MOVED says of it, except for an IGNORE'd or blacklisted port and a
test-only edge, which poudriere's `deps_sanity` never checks. A
blacklisted port is kept but its dependencies are not queued. Each answer is stored under the
port's canonical key with an alias from the requested one.

A generation is named by `SCHEMA`, the trees (`cache::trees`: git HEAD,
or the mtimes of `Mk/bsd.port.mk`, `MOVED`, `UPDATING`; each overlay's
walk) and `conf_hash`, which holds the staged make.conf, the framework
values, and for a local build pkg's database and `USER`. Its file starts
with a `Header` naming the commit, overlays and full `conf_hash`. An entry
answers while the bytes of the port's options file and `options.local`
are unchanged. A new generation of a git tree is seeded by `carry_over`:
the latest earlier generation with the same overlays and `conf_hash` is
diffed against it with `git diff-tree`, and `Invalidation::keeps` drops
the entries whose `PortInfo::makefiles`, owned port directories or
category the changes touch.

## The options model

`session::effective_with` computes what `make` would enable for a port
from a given options file, in the framework's order:

```text
            OPTIONS_OVERRIDE defined?
             │                     │
             no                    yes
             ▾                     ▾
  ┌──────────────────────────┐  ┌──────────────────────────────┐
  │ OPTIONS_DEFAULT          │  │ the override's value, as is  │
  │ OPTIONS_SET, _UNSET      │  │ (file and options.local      │
  │ <name>_SET, _UNSET       │  │  never read)                 │
  │ options file +           │  └──────────────┬───────────────┘
  │   options.local SET/UNSET│                 │
  └────────────┬─────────────┘                 │
               └───────────────┬───────────────┘
                               ▾
  ┌──────────────────────────────────────────────────────────────┐
  │ OPTIONS_SET_FORCE, OPTIONS_UNSET_FORCE,                      │
  │ <name>_SET_FORCE, <name>_UNSET_FORCE                         │
  │ WITH, WITHOUT  (make.conf)                                   │
  ├──────────────────────────────────────────────────────────────┤
  │ IMPLIES, transitively; a target goes in, listed or not       │
  ├──────────────────────────────────────────────────────────────┤
  │ + OPTIONS_SLAVE                                              │
  ├──────────────────────────────────────────────────────────────┤
  │ − members of a SINGLE/MULTI group named after an option      │
  │   that is off (pre-check-config)                             │
  └──────────────────────────────┬───────────────────────────────┘
                                 ▾
                   PORT_OPTIONS as make computes it
```

Each SET entry goes in, word for word, only when it matches an option
of the port as a bmake pattern; each UNSET entry removes every option it
matches; each scope's UNSET runs before the next scope's SET. The
options file and `options.local` are read as one makefile, the second
continuing the first even when there is no first: `+=`, `=`, `:=`,
`?=`, backslash continuations, `#` comments and `\#`, every SET entry of
both going in before any UNSET one. Any other line (an assignment to
another variable than the lists and `make config`'s bookkeeping, `!=`, a
directive, a `$` expansion, no assignment at all) is recorded in
`unmodeled` and leaves the file to make. An IMPLIES target goes in
unchecked, as bsd.options.mk adds it; a PREVENTS word is a pattern. A SINGLE or MULTI group is conditional when its name is
in the port's `ALL_OPTIONS`. `nofile_effective` is the same with no
options file: the defaults plus make.conf (and `options.local`, where
there is one, for the redundancy checks).

The session keeps one `OptState` per options file, shared by the
flavors reading it:

| Field | What it holds |
| :-- | :-- |
| `saved` | the file on disk with `options.local` read after it; `options.local` alone when there is no file |
| `baseline` | what `sync` would write with no edits |
| `staged` | the enabled set being edited |

A shared file is judged from its owner's view (`session::outranks`): the
default flavor, else the flavor with the most options, the first on ties.
Then:

| Question | Function | Rule |
| :-- | :-- | :-- |
| Unmanaged | `unmanaged_file`, `moved` | make.conf moved `OPTIONS_FILE` for the port (its own state in `moved_states`, the shared file left to the other flavors), a line `unmodeled` (an `options.local` `?=` to a list among them), or an `options.local` resetting a list: no `saved`, make's own `PORT_OPTIONS` as the state, every option locked, no make.conf verdict |
| Status | `status` | conflict if the staged outcome breaks PREVENTS or a checked group; ok under `OPTIONS_OVERRIDE` or for an unmanaged file; else unconfigured (no file), stale (options new or gone), ok |
| New option | `SavedOptionsFile::decides` | no SET entry naming it, nor UNSET entry matching it, in the file or `options.local` (make's NEW_OPTIONS, less its SET-glob defect) |
| Owed a decision | `undecided_options` | new, and no make.conf layer, implied-by-forced or group choice settles it; none under `-m` |
| Locked | `toggle` refusals | `*_FORCE`, `WITH`/`WITHOUT`, an `options.local` UNSET entry, `OPTIONS_OVERRIDE`, an unmanaged file, an implier still on, a group left empty, a forced-on sibling |
| Deviates from make.conf | `mc_deviates` | make.conf takes a stance and the staged outcome differs from the no-file outcome |

The TUI's view rules (`m`, `w`, `-m`) are layered on `status` in
`App::effective_status`, not in the session.

## Writing files

`apply::plan_writes_for` groups the ports by options file, leaving out a
flavor make.conf moves `OPTIONS_FILE` for (`session::moved`), and, for
each:

1. leaves it alone when any flavor reading it has an `OPTIONS_OVERRIDE`
   or when it is unmanaged (`session::unmanaged_file`);
2. picks the writer as the session picks the owner (`session::outranks`):
   the default flavor, else the flavor with the most options, the first
   on ties, whose file is then the union of every present flavor's
   options plus the old file's lines none of them has;
3. under `-m`, removes it when, with `options.local` still read, it
   changes nothing for any flavor and holds no line those flavors cannot
   see;
4. under `Covered::Leave` (`sync`, the TUI's apply; `decide` writes
   it), creates none when there is no file, the caller names no edit to
   it, and make.conf leaves no flavor anything undecided but options the
   file would never record (`session::unstored_under`: another flavor's
   that a default-flavor owner lacks), judged as `Session::undecided`
   judges the rows, under `-m` too;
5. skips it when it is up to date (make would end up the same, and the
   list is complete), else renders it as `make config` would, less the
   `_OPTIONS_READ` line and with a pkgname comment of 20 bytes or more,
   so no line trips synth's options check.

`apply::apply` writes every temporary file and fsyncs it before renaming
any; one failure discards them all. Removals run only after every write
succeeded.

## The TUI

```text
  ┌─ event_loop, every tick ─────────────────────────────────────────┐
  │ 1 drain_refresh_events   merge finished re-scans into Session,   │
  │                          GC unreachable ports, blame IGNORE      │
  │ 2 flush_due_refreshes    ports edited 300 ms ago and no scan     │
  │                          running: staged set → staging db,       │
  │                          batch → refresher                       │
  │ 3 draw                   port list · option pane · status bar    │
  │ 4 poll input (150 ms)    dispatch_key → App / Session            │
  └──────────────┬──────────────────────────────────▴────────────────┘
                 │ batch of ports                   │ RefreshEvent
                 ▾                                  │ Progress, Done
  ┌──────────────────────────────────┐              │
  │ refresher thread                 │              │
  │ scanner::scan over the staging   ├──────────────┘
  │ db: PORT_DBDIR = staged copies   │
  └──────────────────────────────────┘

  key ─▸ toggle / bulk / d / U / u ─▸ Session.staged ─▸ mark pending
  a   ─▸ plan_writes ─▸ apply modal ─▸ y: re-plan; unchanged? ─▸ commit
                                          changed? ─▸ show again
  q   ─▸ edits staged? ─▸ draft saved for the next run, else quit
```

`App` groups its state by purpose: the list (`visible`, `filter`, the
view flags `t`/`m`/`w`/`s`/`L`, `held`), the option pane (`editor_rows`,
`editor_idx`, `editor_port`), the overlays (apply modal, quit prompt,
bulk prompt, help, why, option info, pkg-help), the refresh cycle
(`pending`, `to_seed`, `refreshing`, the staging db, the refresher), and
undo (up to 100 steps).

`EditorRow` is a group header, an option (the only selectable row), the
obsolete section (options the file has and the port lost) or the
not-in-this-flavor section (another flavor's options in a shared file).

The apply modal re-plans when confirmed: a plan that changed since it was
shown is shown again, never written. A successful apply reloads only the
files it touched and discards the draft; quitting with edits staged keeps
them in a draft, restored on the next run in the same options dir.

`tui/driver.rs` runs the same `App`, keymap and drawing code against an
in-memory terminal: `key`, `keys`, `dump`, `state`, `settle`, `resize`,
`quit`, one per line on stdin.

## Invariants

- Nothing is written before apply, and an apply is all or nothing.
- Every input that changes a query's answer is in the cache key or the
  entry: the staged make.conf text, the framework values, a local build's
  pkg database and `USER`, overlays, `SCHEMA`, the options file and
  `options.local` bytes, and the tree, either as the generation's commit
  or, for an entry carried over, as the files make read for it, the
  directories it owns and its category; the exceptions are the ones the
  man page's CAVEATS lists.
- A query sees no environment variable but `PATH`, `TMPDIR`,
  `PKG_DBDIR`, and the `USER` and `HOME` of whoever runs the build (root
  in a builder); `PORTSDIR` and `PORT_DBDIR` are always passed, absolute.
- optique never writes or removes an options file it cannot model.
- Each options file is read from disk at most once per run; staging is
  seeded from those bytes, not from a second read.
- An unreadable options file is an error, never a missing one.
- One state per options file; a flavor sharing it is judged from the
  owner's view, and a non-default flavor never drops what it cannot see.
- The TUI writes nothing to staging while a re-scan is running, and
  never lets the option pane show another port than the selection.
