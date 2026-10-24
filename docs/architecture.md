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
│ entry           main.rs · cli.rs                                     │
│                 parse, dispatch, run_scan, banner, exit status       │
├──────────────────────────────────────────────────────────────────────┤
│ resolution      config.rs · roots.rs                                 │
│                 which build, tree, options dir, staged make.conf,    │
│                 blacklist, overlays; which roots                     │
├──────────────────────────────────────────────────────────────────────┤
│ query           query/{scanner,makerunner,wrapper,parse,refresher}   │
│                 cache.rs · moved.rs · deps.rs                        │
│                 wrapper | make -f /dev/stdin → PortInfo, closure     │
├──────────────────────────────────────────────────────────────────────┤
│ model           model/{origin,port,options}.rs                       │
│                 optionsfile.rs · optfiles.rs                         │
│                 PortKey, PortInfo, PortOptions; options files read   │
├──────────────────────────────────────────────────────────────────────┤
│ session         session.rs                                           │
│                 saved / baseline / staged per options file;          │
│                 the framework's rules as functions                   │
├───────────────────────┬────────────────────────┬─────────────────────┤
│ writes                │ interfaces             │ TUI                 │
│ apply.rs · clean.rs   │ report.rs · decide.rs  │ tui/{mod,ui,driver} │
│ staging.rs            │ JSON, plans            │ draft.rs            │
└───────────────────────┴────────────────────────┴─────────────────────┘
```

## Code map

| File | Responsibility | Main items |
| :-- | :-- | :-- |
| `main.rs` | Entry, dispatch, the shared scan, every command's body, the startup banner, exit status (1 = something pending, 2 = usage, 3 = error) | `run`, `run_scan`/`Scanned`, `cmd_scan`, `cmd_sync`, `cmd_decide`, `cmd_clean`, `clean_options_dir`, `find_redundant`, `cmd_tui`, `WritePass`, `commit_writes`, `needs_attention` |
| `cli.rs` | The command line: clap definitions, the checks clap cannot do across levels, roots from arguments and `-f` lists | `disambiguate_synth`, `check_action_first`, `Cli::check_globals`, `collect_roots` |
| `config.rs` | Which build is configured and how queries see it: tree, options dir, staged make.conf and its hash, query environment, blacklist, overlays, notes | `Request`, `Settings`, `QueryEnv`, `resolve`, `resolve_poudriere`, `resolve_local`, `resolve_synth`, `stage_make_conf`, `jail_system`, `plugin_make_confs`, `Blacklist` |
| `roots.rs` | Roots as the build tool reads them, before the scan | `resolve`, `apply_moved`, `expand`, `query_flavors`, `synth_installed` |
| `query/wrapper.rs` | The makefile piped to `make`: includes the port, prints `OPTIQUE\|TAG\|value` lines | `WRAPPER` |
| `query/makerunner.rs` | One `make` per port, cleared environment, a pool of workers, killable | `QueryCtx`, `run_query`, `MakeRunner`, `kill_active_makes` |
| `query/parse.rs` | The dump into a `PortInfo` | `parse_dump`, `parse_depends` |
| `query/scanner.rs` | The closure, breadth first: cache, MOVED, blacklist stop | `scan`, `ScanResult`, `register` |
| `query/refresher.rs` | The TUI's background re-scans over the staging dir | `spawn`, `RefreshEvent` |
| `cache.rs` | Query results on disk, one generation per tree × make.conf | `Cache`, `CacheEntry`, `tree_key`, `SCHEMA` |
| `moved.rs` | `MOVED` of the tree and its overlays | `Moved::load_all`, `Moved::resolve` |
| `deps.rs` | Alias resolution, dependency loops | `resolve`, `detect` (Tarjan), `DepLoop` |
| `model/origin.rs` | `category/port[@flavor]` | `PortKey::parse` |
| `model/port.rs` | One port as `make` describes it | `PortInfo`, `DepEdge` |
| `model/options.rs` | Its options and every make.conf layer naming them; bmake patterns | `PortOptions`, `OptionDef`, `OptionGroup`, `str_match`, `mc_value`, `forced_value`, `provenance` |
| `optionsfile.rs` | One options file (and its `options.local`) read as the makefile it is; written as `make config` writes it | `SavedOptionsFile`, `parse`, `with_local`, `render`, `decides` |
| `optfiles.rs` | Every options file of the options dir, each read from disk once | `OptionsFiles` |
| `session.rs` | The editing model and the framework's rules | `Session`, `OptState`, `UiStatus`, `effective_with`, `toggle` |
| `apply.rs` | What to write and remove, and writing it atomically | `plan_writes`, `PendingWrite`, `plan_stale_removals`, `apply`, `commit` |
| `clean.rs` | Classifying options-dir entries | `classify_entries`, `split_unused`, `redundancy_diff`, `remove_all` |
| `decide.rs` | A JSON plan applied to a session | `Plan`, `apply_plan` |
| `report.rs` | Every `--json` payload | `ScanReport`, `WriteReport`, `option_reports` |
| `staging.rs` | The TUI's private copy of the options dir | `StagingDb` |
| `draft.rs` | Unapplied TUI edits kept across runs | `Draft` |
| `tui/mod.rs` | The TUI's state, keys, refresh cycle, apply flow | `App`, `event_loop`, `dispatch_key`, `EditorRow` |
| `tui/ui.rs` | Drawing | `draw`, `draw_port_list`, `draw_editor`, the popups |
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
  │ roots::resolve     @all, @-, FLAVOR_DEFAULT_ALL (through MOVED)      │
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
  synth would refuse; never a blacklisted port), a loop blocks, or a
  query failed.
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
  a file).
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
into the hash: what is installed is part of its answer.

`roots::resolve` turns the listed origins into the scan's roots: `@all`
and (under `FLAVOR_DEFAULT_ALL`) bare origins become every flavor of the
port, after MOVED maps them to the port's current origin; `@-` becomes
the default flavor. Other roots meet MOVED in the scan, like any
dependency. Under `-s` with no list, `run_scan` first takes the installed
packages and keeps those synth's index has (`roots::synth_installed`:
the categories' `SUBDIR` lines and each port's flavors).

## Querying and caching

`run_query` runs `/usr/bin/make -C <port> -f /dev/stdin optique-config`
with `PORTSDIR`, `PORTDIR`, `PORT_DBDIR` on the command line,
`__MAKE_CONF` and `FLAVOR` when set, and an environment holding only
`PATH`, `HOME`, `TMPDIR` and `PKG_DBDIR`. The wrapper prints the port's
options as computed (`COMPLETE_OPTIONS_LIST`, `PORT_OPTIONS`), every
make.conf layer naming them, groups, per-option definitions
(implies, prevents, descriptions, dependencies), dependencies, and
BROKEN/IGNORE/DEPRECATED; `parse_dump` makes a `PortInfo` of it. An
unreadable options file fails the port rather than reading as absent.

`scanner::scan` walks breadth first from the roots: a dependency goes
through MOVED, then the cache, then a worker. A blacklisted port is kept
but its dependencies are not queued. Each answer is stored under the
port's canonical key with an alias from the requested one.

A cache entry is valid while the tree (git HEAD, or the mtimes of
`Mk/bsd.port.mk`, `MOVED`, `UPDATING`), the staged make.conf, the
overlays, `SCHEMA`, and the bytes of the port's options file and
`options.local` are unchanged.

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
  │ IMPLIES, transitively                                        │
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
`?=`, backslash continuations, `#` comments and `\#`; `!=` is ignored
with a warning. A SINGLE or MULTI group is conditional when its name is
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

A shared file is judged from its owner's view: the default flavor, else
the flavor with the most options. Then:

| Question | Function | Rule |
| :-- | :-- | :-- |
| Status | `status` | conflict if the staged outcome breaks PREVENTS or a checked group; ok under `OPTIONS_OVERRIDE` or an `options.local` reset; else unconfigured (no file), stale (options new or gone), ok |
| New option | `SavedOptionsFile::decides` | no SET/UNSET line for it in the file or `options.local` |
| Owed a decision | `undecided_options` | new, and no make.conf layer, implied-by-forced or group choice settles it; none under `-m` |
| Locked | `toggle` refusals | `*_FORCE`, `WITH`/`WITHOUT`, `options.local`, `OPTIONS_OVERRIDE`, an `options.local` reset, an implier still on, a group left empty, a forced-on sibling |
| Deviates from make.conf | `mc_deviates` | make.conf takes a stance and the staged outcome differs from the no-file outcome |

The TUI's view rules (`m`, `w`, `-m`) are layered on `status` in
`App::effective_status`, not in the session.

## Writing files

`apply::plan_writes` groups the ports by options file and, for each:

1. leaves it alone under `OPTIONS_OVERRIDE` or an `options.local` reset;
2. picks the writer: the default flavor, else the first flavor present,
   whose file is then the union of every present flavor's options plus
   the old file's lines none of them has;
3. under `-m`, removes it when, with `options.local` still read, it
   changes nothing for any flavor and holds no line those flavors cannot
   see;
4. skips it when it is up to date (make would end up the same, and the
   list is complete), else renders it as `make config` would.

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
- Every input that changes a query's answer is in the cache key: the
  tree, the staged make.conf text, overlays, `SCHEMA`, the options file
  and `options.local` bytes.
- A query sees no environment variable but `PATH`, `HOME`, `TMPDIR`,
  `PKG_DBDIR`; `PORTSDIR` and `PORT_DBDIR` are always passed, absolute.
- Each options file is read from disk at most once per run; staging is
  seeded from those bytes, not from a second read.
- An unreadable options file is an error, never a missing one.
- One state per options file; a flavor sharing it is judged from the
  owner's view, and a non-default flavor never drops what it cannot see.
- The TUI writes nothing to staging while a re-scan is running, and
  never lets the option pane show another port than the selection.
