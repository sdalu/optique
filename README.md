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
# "workstation" (options land in /usr/local/etc/poudriere.d/workstation-options)
optique -z workstation www/nginx mail/dovecot

# From a poudriere pkglist file
optique -z server tui -f /usr/local/etc/poudriere.d/pkglist

# Non-interactive check: which ports are unconfigured or stale?
optique -z workstation scan -f pkglist

# Headless refresh (the fast `poudriere options -C` replacement):
# keep saved choices, adopt defaults for newly-added options, drop removed
optique -z workstation sync --dry-run -f pkglist
optique -z workstation sync -f pkglist
```

Key flags (global): `-z set`, `-j jail` (make.conf layering), `-p tree`
(poudriere ports tree, default `default`), `-o dir` (explicit options dir,
bypasses poudriere resolution; default `/var/db/ports`), `-J jobs`,
`--no-cache`.

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
`<OPT>_IGNORE`. Obsolete options (removed from the port) appear struck
through and are dropped on apply.

Toggling an option re-queries the port in the background against a **staging
PORT_DBDIR** — dependencies added/removed by the option appear/disappear in
the list within a second or two (`⟳` in the status bar). Nothing touches the
real options dir until you hit `a` (apply), which previews every file diff
and then writes atomically (tmp + fsync + rename).

Keys: `j/k` move · `Enter/l` edit port · `Space` toggle · `d` defaults ·
`u` revert · `n/p` next/prev problem · `/` filter · `a` apply · `q` quit.

## How it works

One `make` invocation per port (~0.2–0.6 s, up to 16 in parallel) pipes a
wrapper makefile to `make -f /dev/stdin optique-config`: it includes the
port's Makefile and dumps options, groups, descriptions, IMPLIES/PREVENTS,
BROKEN/IGNORE, make.conf layers and `_UNIFIED_DEPENDS` as parse-time `.info`
lines. poudriere's make.conf layering (`make.conf`, `<jail>-`, `<set>-`,
`<jail>-<set>-make.conf`) is reproduced via `__MAKE_CONF`, so queries see
exactly what poudriere builds — including `DEFAULT_VERSIONS`.

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
