# optique

Fast FreeBSD ports options / dependency configurator for poudriere.

`poudriere options -C <list>` walks the dependency tree serially and pops an
interactive dialog per port. optique replaces that workflow: it scans the whole
dependency closure **in parallel** (~1000 ports in under a minute cold, well
under a second warm thanks to a persistent cache), shows one TUI over all
ports, and writes every options file in a single atomic pass at the end.

## Usage

```sh
# Interactive TUI over the closure of the given ports, for poudriere set
# "workstation" — the options dir is resolved exactly like poudriere(8)
# resolves it before a build (see "Poudriere layout" below)
optique -z workstation www/nginx mail/dovecot

# From poudriere pkglist file(s) — one origin[@flavor] per line, '#' comments;
# -f is repeatable and duplicates across files/arguments are dropped
optique -z server -f /usr/local/etc/poudriere.d/pkglist
optique -z server -f base-list -f extra-list www/nginx

# Non-interactive check: which ports are unconfigured or stale? Each flagged
# port lists the options make.conf does NOT decide ("undecided: …"), or
# "[mc-covered ≈]" when make.conf decides everything.
# Exit code is the gate: 0 = nothing pending, 1 = a human decision is pending
# (unconfigured/stale with undecided options, or a conflict), anything else =
# error. Ports that are fully mc-covered do NOT trip the gate. With --json one
# JSON object goes to stdout instead of the table; -q drops the per-port rows
# and the banner, keeping the summary and warnings on stderr
optique -z workstation scan -f pkglist
optique -z workstation scan --json -f pkglist
optique -z ws -q scan -f list || mail-me   # cron: only speak up when it matters

# Headless refresh (the fast `poudriere options -C` replacement):
# keep saved choices, adopt defaults for newly-added options, drop removed
optique -z workstation sync --dry-run -f pkglist
optique -z workstation sync -f pkglist

# Garbage-collect the options dir: drop entries whose port vanished from the
# tree (MOVED-aware: renames and removals are explained); with --redundant,
# also drop files that only repeat what defaults + make.conf dictate anyway;
# with --unused, also drop entries for ports outside the dependency closure
# of a package list (origins and/or -f pkglist — the list is required, and
# giving one without --unused is an error since plain clean ignores it)
optique -z workstation clean --dry-run
optique -z workstation clean --redundant
optique -z workstation clean --unused -f pkglist
optique -z workstation clean --unused --redundant --dry-run -f pkglist

# Full hygiene pass for one package list (refresh, then garbage-collect)
optique -z ws sync -f list && optique -z ws clean --unused --redundant -f list
```

Key flags (global): `-z set`, `-j jail` (make.conf layering), `-p tree`
(poudriere ports tree, default `default`), `-o dir` (explicit options dir,
bypasses poudriere resolution; default `/var/db/ports`), `-s [profile]`
(use synth(1)'s layout instead of poudriere's, see "Synth"), `-f pkglist`
(repeatable), `-J jobs`, `--no-cache`, `-v` (verbose: scan adds each port's
full `+ON`/`-OFF` option state and query warnings, sync adds the final state
per written file, clean explains kept entries — e.g. which options deviate
from defaults + make.conf), `-q` (quiet: no banner and no per-port/per-file
stdout listing; summary and warnings still go to stderr).

## Workflow

The typical cycle, replacing `poudriere options -C` before a bulk build:

1. **Update the ports tree** (`poudriere ports -u` / `git -C /usr/ports pull`).
   Tree updates add or remove options on some ports and pull new dependencies
   in — every affected port needs a configuration decision before building.

2. **Scan.** `optique -z <set> -f <pkglist>` resolves the full dependency
   closure of your package list in parallel — for each port: its options,
   groups, flavors, dependencies, and how the poudriere make.conf layers
   affect it. The first run on a fresh tree takes about a minute per
   thousand ports; afterwards the cache brings it down to well under a
   second, so re-opening optique is free.

3. **Triage.** The TUI opens with problems sorted to the top: `?` ports
   never configured, `!` ports whose option list changed since their file
   was written (with the added/removed options named), `✗` conflicts.
   `n`/`p` jumps between them; everything already consistent sits dimmed
   at the bottom. Ports without any options are handled automatically and
   never shown.

4. **Decide.** For each flagged port, either accept what's proposed (the
   saved choices plus defaults for `NEW` options — that is exactly what
   Apply will write if you touch nothing) or edit: `Space` toggles with
   group rules enforced, `IMPLIES` chains auto-enable, `FORCED` and implied
   options are locked with an explanation, `⚠` warns when a choice marks
   the port BROKEN. When a toggle changes the dependency set, the closure
   refreshes in the background within a second or two — newly appeared
   dependencies show up flagged `?`/`!` and join the triage, dropped ones
   vanish (keeping their edits in case you flip back).

5. **Apply.** `a` previews every file that would change as a diff
   (`+OPT`, `-OPT`, `new: OPT(on)`, `dropped: OPT`), warns if conflicts
   remain, then writes all options files atomically in one pass to the
   set's options dir. Nothing on disk changes before this point, so
   quitting without applying is always safe.

6. **Build.** Run `poudriere bulk` as usual — it finds every port already
   configured and starts building immediately instead of stopping at
   dialog after dialog.

For unattended use (cron, CI), step 3–5 collapse into
`optique -z <set> sync -f <pkglist>`: it keeps all saved choices, adopts
defaults for new options, drops removed ones, and prints what it changed —
add `--dry-run` to only report. A later interactive session can then revisit
anything sync decided.

## What the TUI shows

Left pane: every port in the dependency closure **that has options**
(optionless ports stay in the graph but are hidden), problems first:

- `✗` conflict — staged options violate `PREVENTS` or group constraints
- `*` edited — staged changes not yet applied
- `?` unconfigured — no saved options file
- `!` stale — the port's option list changed since the file was written
- `✓` ok, `⚠` port is BROKEN/IGNORE with the current options

Right pane: options in framework order with group headers (SINGLE = exactly
one, RADIO = at most one, MULTI = at least one — enforced on toggle). Each row
shows the staged value, the default (`def: on/off`), a `NEW` badge for options
added since the file was written, provenance badges (`mc` = global
OPTIONS_SET/UNSET, `mc:port` = per-port, `FORCED` = *_FORCE knobs that the
options file cannot override — toggle locked), `implied by X` locks from
`IMPLIES` chains, and `⚠broken/ignored` for options carrying `<OPT>_BROKEN` /
`<OPT>_IGNORE`. Name colors: yellow = deviates from the port's default,
magenta (+ `≠mc` badge) = contradicts your make.conf — the option is
mentioned there but the staged value differs from what defaults + make.conf
alone would produce. Obsolete options (removed from the port) appear struck
through and are dropped on apply; options excluded by the current flavor but
managed via the default flavor are listed separately. `i` opens a detail popup
for the selected option: description, group, IMPLIES/PREVENTS, and the
dependencies enabling it would add. When a background refresh turns a port
BROKEN/IGNORE, the banner adds a `likely cause:` line naming the recent option
changes (`+OPT`/`-OPT`) that most likely caused it.

Toggling an option re-queries the port in the background against a **staging
PORT_DBDIR** — dependencies added/removed by the option appear/disappear in
the list within a second or two (`⟳` in the status bar). Nothing touches the
real options dir until you hit `a` (apply), which previews every file diff
and then writes atomically (tmp + fsync + rename).

Quitting with staged changes offers to keep them: `s` saves them as a **draft**
(under `~/.cache/optique/drafts`, keyed by the options dir), `d` throws them
away. The next TUI launch on the same options dir restores the draft's staged
edits — options that vanished from a port meanwhile are dropped, and the
restored ports are re-queried so their dependencies show up again. The draft is
cleared as soon as you apply (the edits are on disk then) or discard-quit; `u`
reverts an individual port as usual.

Keys: `j/k` move · `Enter/l` edit port · `Space` toggle · `d` defaults ·
`u` revert · `U` undo the last option change (any port, up to 100 back) ·
`n/p` next/prev problem · `t` show only ports needing attention
(hide `✓` ok) · `m` treat stale/unconfigured ports as ok when every added option (stale) or
every option at all (unconfigured) is already decided by make.conf
(marker `≈`) · `w` flag ports whose options contradict
the global `OPTIONS_SET/UNSET` policy (marker `≠`, badge `≠mc` on the option
row) · `s` toggle problems-first vs stable alphabetical sort (alphabetical
keeps neighbors put while you work down the list; `n`/`p` still jump between
problems in either order) · `/` filter · `a` apply ·
`r` why-is-this-here (dependency chain) · `B` bulk-set an option across
visible ports · `f` next flavor of the same origin · `?`/`h`/F1 color-coded
in-TUI help (markers, badges, keys) · `q` quit.

### Debugging the TUI

`optique tui --drive` runs the interface headless: same keymap, same app, same
drawing code, but it renders into an in-memory screen and reads one command per
line from stdin. Useful to reproduce a rendering bug or to test the TUI in CI,
where there is no terminal.

```sh
printf 'dump\nkey /\nkeys nginx\nkey enter\nstate\nquit\n' |
    optique tui --drive -o /tmp/opt www/nginx
```

Every command is acknowledged with an `ok …`/`err …` line except `dump` and
`state`, which print their payload; output is flushed per command so the driver
can be held open on a pipe, and a bad command is reported without ending the
session:

- `key <spec>` — one key press: an optional `ctrl-`/`alt-` prefix, then a
  single character (case matters: `B` is the bulk key) or a name
  (`enter esc tab backtab space backspace up down left right pgup pgdn home
  end f1`…`f12 question slash`)
- `keys <text>` — press each character of `<text>`, for typing into the filter
  or bulk prompts
- `dump` — draw a frame and print it: `screen <W>x<H>`, one line per row, `end`
- `state` — one-line JSON: `focus`, `selected`, `visible`, `listable`,
  `refreshing`, `pending`, `message`, `overlay`, `filter`, `dirty`
- `settle [timeout_ms]` — wait for background re-queries to land (default 5000);
  this is also what lets the 300 ms edit debounce expire before a `dump`
- `resize <W>x<H>` — new in-memory screen size (default 100x35, clamped 20–500)
- `quit` — end the session; so do EOF and any key the TUI treats as quitting

## Poudriere layout

Given `-j jail`, `-p tree` (default `default`), `-z set`, optique resolves
the options directory exactly as poudriere(8) does before null-mounting one
over the jail's /var/db/ports — first existing wins:

    <jail>-<tree>-<set>-options   <jail>-<set>-options   <jail>-<tree>-options
    <tree>-<set>-options          <set>-options          <tree>-options
    <jail>-options                options

If none exists, apply creates the one `poudriere options` would have used
for the same flags (`[<jail>-][<tree>-][<set>-]options`, the tree component
only when `-p` was explicit). `-o dir` bypasses all of this; without any
poudriere.d, plain `/var/db/ports` is used.

make.conf is layered in poudriere(8)'s inclusion order, all existing files
concatenated: `make.conf`, `<set>-`, `<tree>-`, `<jail>-`, `<tree>-<set>-`,
`<jail>-<tree>-`, `<jail>-<set>-`, `<jail>-<tree>-<set>-make.conf` — so
queries see exactly what poudriere builds, including `DEFAULT_VERSIONS` and
`OPTIONS_SET/UNSET` knobs.

The `blacklist` chain is read with the same jail/tree/set expansion
(`blacklist`, `<set>-`, `<tree>-`, `<jail>-`, `<tree>-<set>-`,
`<jail>-<tree>-`, `<jail>-<set>-`, `<jail>-<tree>-<set>-blacklist`); like
poudriere, every existing file contributes entries — one port origin per
line, `#` comments, `*` globs allowed (`www/nginx*`). Blacklisted ports are
marked `⊘` in `scan` and in the TUI and never demand attention: poudriere
won't build them here, so their options can't hold a build up, and they
never trip `scan`'s exit-code gate whatever their status.

### Synth

`-s [profile]` (default profile `LiveSystem`) swaps the poudriere layout for
[ports-mgmt/synth](https://man.freebsd.org/synth)'s, so the same commands work
on a synth host: `optique -s scan -f list`, `optique -s sync www/nginx`,
`optique -s -r clean`. The ports tree comes from `Directory_portsdir` and the
options dir from `Directory_options` in the profile's section of
`/usr/local/etc/synth/synth.ini` (falling back to that file's
`[Global Configuration]` section, then to `/usr/ports` and `/var/db/ports`),
and make.conf is the single file `/usr/local/etc/synth/<profile>-make.conf` —
the one synth appends to each builder's make.conf; there is no layering and no
blacklist. `-o dir` still overrides the options dir; `-p` is a poudriere tree
name and is ignored (with a note) under `-s`, and `-s` cannot be combined with
`-z`/`-j`.

## How it works

One `make` invocation per port (~0.2–0.6 s, up to 16 in parallel) pipes a
wrapper makefile to `make -f /dev/stdin optique-config`: it includes the
port's Makefile and dumps options, groups, descriptions, IMPLIES/PREVENTS,
BROKEN/IGNORE, make.conf layers and `_UNIFIED_DEPENDS` as parse-time `.info`
lines, evaluated under the layered `__MAKE_CONF`.

Results are cached in `~/.cache/optique/` keyed on (ports tree git HEAD,
make.conf hash, options file content), so re-scans and background refreshes
only invoke make for ports whose inputs actually changed. Renamed ports are
resolved through `MOVED`; removed ones surface as per-edge errors.

## Building / testing

```sh
cargo build --release        # needs lang/rust
cargo test                   # unit tests, host-independent
cargo test -- --ignored      # live tests against /usr/ports
```

## Installing

The `Makefile` is a thin BSD make (bmake) wrapper around cargo:

```sh
make build                   # cargo build --release
make install                 # as root
```

It installs `optique` into `${PREFIX}/bin` and the man page into
`${PREFIX}/share/man/man8/optique.8.gz`, with `PREFIX` defaulting to
`/usr/local`. Both `PREFIX` and `DESTDIR` are honoured, so staging into a
package works as usual:

```sh
make install PREFIX=/opt DESTDIR=/tmp/stage
```

`make deinstall` removes both files again, `make test` runs the test suite and
`make lint` runs `mandoc -T lint` on the man page. The same thing by hand:

```sh
install -s -m 555 target/release/optique /usr/local/bin/optique
gzip -cn optique.8 > /usr/local/share/man/man8/optique.8.gz
```

Full documentation lives in the man page — every flag, the poudriere options-dir
and make.conf resolution chains, the TUI key bindings, the `scan --json` schema
and `scan`'s exit-code contract:

```sh
man optique                  # or: mandoc optique.8
```
