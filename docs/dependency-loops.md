# Dependency loops

A dependency loop is a port that is, directly or through other ports, its
own dependency. `poudriere bulk` cannot build any port caught in one,
because none of them can go first, and optique sees the whole dependency
closure before any build starts, so it reports every loop up front instead
of letting the build discover it (see
[DESIGN.md](../DESIGN.md#dependency-loops-tarjan-iterative-only-poudrieres-own-edges)
for how the search works).

## How they are reported

Whatever subcommand you run, every loop in the closure goes to standard
error as a shortest closed walk through it:

```text
loop: devel/a → devel/b → devel/a
      also tangled: devel/c devel/d
```

The `also tangled:` line appears when the tangle is bigger than that one
walk — several loops can share a port, and those extra ports still reach
every other member, so none of them can be built first either. It names up
to a dozen by origin and counts the rest; `--json` always carries the whole
list.

In `scan`, a port caught in a loop keeps its own status (`ok`,
`unconfigured`, `stale` or `conflict`) and gets a trailing
`[dependency loop]` on its row; the summary counts the loops found. With
`--json`, each loop is an object under `loops`, with `ports` (the whole
tangle), `cycle` (the shortest walk) and `blacklisted` (true when every
member is blacklisted, so the loop holds nothing up); under `summary`,
`loops` counts them and `loops_blocking` counts the ones with at least one
member that is not blacklisted — see
[commands.md](commands.md) for how that number feeds `scan`'s exit status,
and [machine-interface.md](machine-interface.md) for the rest of the JSON
object.

In the TUI, every port caught in a loop carries the `∞` marker in the list,
and the status bar counts them. `L` narrows the list to just those ports —
refused rather than emptying the list when the closure holds no loop, since
there is nothing to show. `r` opens the "why is this port here" popup,
which for a looping port walks the cycle one port per row, starting at the
port you asked about; every row is selectable, so `Enter` jumps to that
port and `r` re-opens the walk from there. Both the marker and the `L` view
are recomputed after every background re-query, so an option edit that
creates or breaks a loop is announced on the status line, and breaking the
last loop drops the `L` view rather than leaving it empty. See
[tui.md](tui.md#why-is-this-port-here) for the popup itself.

## Which dependencies count

Only the dependency edges `poudriere bulk` actually builds are followed:
`PKG`, `EXTRACT`, `PATCH`, `FETCH`, `BUILD`, `LIB` and `RUN` depends. An
edge that none of those lists asks for is left out of the search.

`TEST_DEPENDS` are not in the closure on a current tree. optique follows
the framework's own dependency list, `_UNIFIED_DEPENDS`, which since ports
commit e7679b71cf89 (2026-09-25) leaves them out: the ports that only
`make test` or `poudriere bulk -t` installs are not configured, as
`poudriere options` does not configure them either, and poudriere builds
them with their defaults. A port's `TEST` option is a different thing: what
it adds to the build (`TEST_BUILD_DEPENDS` and the like) is an ordinary
dependency and stays in the closure.

On an older tree the test dependencies are still in that list, and their
ports are configured like any other, but their edges are left out of the
loop search: the tree's test dependencies are cyclic on a grand scale, and
counting them would report hundreds of ports as one loop on an ordinary
build, which is why a healthy tree reports no loop at all.

## Breaking a loop

A loop is usually option-dependent: one port's enabled option pulls in a
dependency that, through the rest of the chain, comes back to it. Turning
that option off breaks the loop.

To find which option is responsible, walk the cycle with `r` and `Enter` in
the TUI (see above) to see which port leads to which, then open each
port's options and use `i` on a candidate to see "the dependencies
enabling it would add" — the option whose added dependency matches the
next port on the walk is the one to turn off.

If every port in a loop is blacklisted for the current jail, tree and set,
`poudriere` will not build any of them, so the loop no longer counts
against `scan`'s exit status — see
[configuration.md](configuration.md#the-blacklist). That does not remove
the loop from the tree; it only means none of its members are yours to
build, so it is no longer in your way.
