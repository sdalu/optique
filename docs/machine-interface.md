# Machine interface

Every subcommand but `tui` can answer in JSON instead of printing its table or
listing, so a script or an agent never has to read a screen or parse text
meant for a terminal. One JSON object per invocation goes to standard output
and nothing else does: the startup banner, progress, warnings and the summary
line all stay on standard error, so standard output is safe to pipe into a
parser without `-q`. Two commands carry the actual work: `scan --json
--options` says what the dependency closure is and what each option means, and
`decide` sets option values from a plan.

## The loop: scan, decide, check, apply

1. **Scan.** `optique scan -z ws --json --options -f pkglist >
   state.json` resolves the closure and reports what needs deciding
   and what the choices are (see
   [What `scan --json --options` says](#what-scan---json---options-says)).
2. **Decide.** Something outside optique turns that state into a
   plan — the caller's own judgement, for example a `jq` filter —
   written as `plan.json` (see [Plans for `decide`](#plans-for-decide)).
3. **Check.** `optique decide -z ws -n < plan.json` builds the whole
   report, per-file diff included, and writes nothing (`-n`/`--dry-run`).
4. **Apply.** `optique decide -z ws < plan.json` carries the plan out:
   every named port's options file is written in one pass, or none of
   them are (see [The contract](#the-contract)).

`sync --json` answers in the same shape as `decide`, with `rejected`,
`conflicts` and `unknown` always empty, since it asks for nothing — a caller
that only wants to know what changed on disk can treat the two
interchangeably. `tui --drive` is a separate, keystroke-level protocol over
the real keymap and drawing code, for reproducing a rendering bug or testing
the TUI in CI; it is a debugging tool, not the way to configure ports from a
script — see [tui.md](tui.md#driving-the-tui-headless).

## What `scan --json --options` says

`scan --json` prints one object:

| key           | meaning                                                   |
| :------------ | :-------------------------------------------------------- |
| `options_dir` | the resolved options directory                            |
| `ports_tree`  | the ports tree used                                       |
| `overlays`    | the trees laid over it, in lookup order                   |
| `ports`       | one entry per port in the closure                         |
| `loops`       | dependency loops found in the closure (details below)     |
| `summary`     | counts, including `pending`, which drives the exit status |
| `errors`      | what could not be checked, each `{subject, message}`      |
| `moved`       | listed origins `MOVED` moves, each `{listed, to, reason, followed}` |

See [dependency loops](dependency-loops.md) for the shape of a loop entry. An
error is a port whose query failed or a listed origin `MOVED` says is gone,
missing from `ports` with whatever it depends on, or one entry per
dependency of a port in `ports` on an origin in no tree (`depends on
nonexistent origin <origin>`, with what `MOVED` says of it). An element of
`moved` is a listed originspec (`listed`) `MOVED` sends to another (`to`),
for `reason` (possibly empty); `followed` is true under `--follow-moved`,
when `to` was scanned in its place, and false otherwise, when neither is in
`ports` and the list needs a fix ([commands](commands.md#choosing-the-ports)).
`summary` holds `total`, `unconfigured`, `stale`, `conflict`, `ok`,
`optionless`, `blacklisted`, `attention` (ports owing a decision), `loops`,
`loops_blocking` (loops with a member that is not blacklisted), `moved` (the
elements of `moved` not followed), `errors` (the length of the `errors`
list above) and `pending`, which is `attention` + `loops_blocking` +
`moved` + `errors` and is what the exit status reports. An options
file that exists but cannot be read is one such error, reported for its port
as `cannot read options file <path>: <reason>` rather than taken for an
unconfigured one.

Each element of `ports` has `port`, `pkgname`, `status` (`ok`, `unconfigured`,
`stale` or `conflict`), `undecided`, `added` and `removed` (both only
meaningful for `stale`), `mc_covered`, `blacklisted`, `in_loop`,
`options_file`, `overlay` (the overlay the port is taken from; `null`
for the ports tree) and `synth_refuses` (under `-s`, why synth would
refuse to build with the saved file; `null` otherwise). With `--options`, two more keys appear: `violations` (why the
status is `conflict`) and `options`, an array in the framework's own option
order.

Each element of `options` carries everything a decision needs without
re-running `make`:

| key                 | meaning                                          |
| :------------------ | :----------------------------------------------- |
| `name`              | the option's name                                |
| `staged`            | value that would be written now                  |
| `default`           | the port's own default                           |
| `saved`             | file's value; `null` = no entry (`undecided`)    |
| `new`               | added since the file was written[^4]             |
| `makeconf`          | `{value, scope}`: `global`/`port`/`force`/`with`/`override`[^1] |
| `locked`            | the file cannot change this value                |
| `locked_by`         | why: a `*_FORCE` knob, `WITH`/`WITHOUT`, `options.local` (an UNSET entry there matches it; `decide` refuses it), an implying option, or what leaves the file to make[^5] |
| `deviates_makeconf` | staged value contradicts `makeconf`              |
| `group`             | `{name, kind, members}`[^2]                      |
| `implies`           | options this one turns on                        |
| `prevents`          | options this one conflicts with                  |
| `prevents_msg`      | the conflict's message, if the framework has one |
| `desc`              | the option's description                         |
| `broken`            | message, if enabling marks the port `BROKEN`     |
| `ignore`            | message, if enabling marks the port `IGNORE`     |
| `adds_deps`         | dependency origins the option declares, sorted   |
| `uses`              | `USES` frameworks it activates                   |

[^1]: absent when no layer mentions the option. `with` is make.conf's
    `WITH`/`WITHOUT`, applied after the `*_FORCE` knobs and pinning the
    option as they do. `override` is make.conf's `OPTIONS_OVERRIDE`
    (`bsd.options.mk:300-304`): defined for the port, even empty, it
    replaces the whole decision — defaults, `OPTIONS_SET`/`UNSET`, the
    per-port pair and the options file are not read, and `PORT_OPTIONS`
    becomes its value as given. The `*_FORCE` knobs and `WITH`/`WITHOUT`
    (`bsd.options.mk:367-409`) still run after it, so an option they pin
    reports its pin as usual (`scope: "force"` or `"with"`, `locked_by:
    "make.conf *_FORCE"` or `"make.conf WITH/WITHOUT"`); every other option
    of such a port reports `value` (whether it is named in the override),
    `scope: "override"`, `locked: true` and `locked_by: "make.conf
    OPTIONS_OVERRIDE"`. `undecided` is always empty for such a port and
    `status` is never `stale` or `unconfigured`
    (`conflict` still applies when the outcome breaks a group or `PREVENTS`
    rule). `decide` refuses any value for such a port's options, and `sync`
    never writes or removes its options file.
[^2]: `kind` is `group`, `multi`, `single` or `radio`.
[^4]: that is, the file has no SET or UNSET line for it, as make counts
    `NEW_OPTIONS`; a line settles the option even when the file's
    `_FILE_COMPLETE_OPTIONS_LIST`, which make never reads, misses it.
[^5]: an options file optique cannot model is left to make
    ([commands](commands.md#sync)): every option of its port reports
    `locked: true` with `locked_by` `"options file"`, `"options.local"` or
    `"make.conf OPTIONS_FILE"`, whichever makes it so, ahead of any other
    reason, no `makeconf` and a `null` `saved`; `staged` is make's own
    outcome, `undecided` is empty, `status` is never `stale` or
    `unconfigured`, `decide` refuses its options, and no command writes or
    removes the file.

## Plans for `decide`

`decide` reads a plan on standard input: one JSON object of ports, each an
object of option names to booleans:

```json
{ "www/nginx": { "LUA": true, "DOCS": false },
  "devel/git@lite": { "CONTRIB": false } }
```

Values go through the same rules the TUI enforces — group kinds, the `IMPLIES`
closure, the `PREVENTS` check, and the `*_FORCE` and implied locks — so a plan
can never record a state the interface would refuse. Options are applied in
name order within each port, which fixes the outcome when one implies another
or they share a group.

The port list is optional. With none, the plan's own ports are the closure
roots, so a plan is self-contained. Given one — positional origins and/or `-f
pkglist` — the closure comes from it, and a plan naming a port outside it is
reported as `unknown` rather than guessed at. Only the named ports' options
files are written; `sync` refreshes the rest. Writing a port's file
necessarily adopts its current option list, exactly as `sync` would: an option
the tree has added since the file was written cannot be left unrecorded.

## The contract

The interface is meant to be safe to drive blind:

- **Exit status answers "is there anything left to do."** `scan` exits
  1 while a decision is pending; `decide` exits 1 when it did not
  honour the plan. A run that fails outright exits 3, never 1, and bad
  usage exits 2.
- **All-or-nothing.** A plan that cannot be carried out in full changes
  nothing: `decide` only writes when nothing was rejected, no port was
  unknown and no result conflicts.
- **Everything refused is named.** A value the rules turned down, a
  value another value in the plan undoes, a port outside the closure,
  a resulting conflict: each lands in `rejected`, `unknown` or
  `conflicts` with a reason, instead of being silently dropped.
- **Replayable.** Asking for the value a port already has is not a
  change and not an error, so the same plan can be applied twice and
  the second run writes nothing.
- **`-n` previews.** `decide -n` and `sync -n` build the whole report,
  per-file diff included, and touch nothing.

## Report shapes

`decide` and `sync --json` report the same shape:

| key           | meaning                                            |
| :------------ | :------------------------------------------------- |
| `options_dir` | the resolved options directory                     |
| `dry_run`     | nothing was written, whatever the plan said        |
| `applied`     | the options dir was actually changed               |
| `writes`      | one entry per file written (or that would be)      |
| `removals`    | one entry per file removed as dead config          |
| `rejected`    | option values the port's own rules refused         |
| `conflicts`   | ports violating `PREVENTS`/a group rule[^3]        |
| `unknown`     | plan ports the closure does not hold               |
| `warnings`    | notices, e.g. a flavor disagreeing on an option    |
| `errors`      | ports/files that failed, each `{subject, message}` |
| `summary`     | `{written, removed, failed, rejected, conflicts}`  |
| `moved`       | listed origins `MOVED` moves, as `scan` has them   |

[^3]: non-empty means nothing was written.

A `moved` entry does not change `decide`'s exit status, which answers for
the plan; a plan naming a moved origin that was not followed finds no port
for it, in `unknown`.

Each `writes` entry has `port`, `options_name`, `file` (its absolute path),
`new_file`, `diff` (`+OPT`/`-OPT` for options the file already knew),
`adopted` (options the port gained since, each `{name, on}`; empty for a new
file, whose whole state is `enabled`), `dropped` (options the port no longer
has), `enabled` (the complete set the file will record) and `new_deps`
(origins the newly enabled options declare as dependencies). The old file is
read as `scan` reads it: an option a SET or UNSET entry matches, `A*`
included, is one it knew (in `diff`, not `adopted`), and a pattern entry is
`dropped` only once it matches no option of the port.

`clean --json` reports a different shape: `options_dir`, `dry_run`, `removals`
(each `{options_name, reason}`), `kept` (only under `-v`, each `{options_name,
reason}`), `warnings`, `errors` and `summary` (`{entries, removed, failed}`).

All three shapes — `scan`, `decide`/`sync`, and `clean` — carry an `errors`
list of `{subject, message}` for whatever failed outright, and each summary's
count of them means something different for the exit status: `scan`'s
`summary.errors` is folded into `summary.pending` alongside `attention` and
`loops_blocking` (see above), so a query failure alone is enough to make
`scan` exit 1. `decide` exits 1 when `summary.failed` is above zero, on top of
exiting 1 for a plan that was not honoured. `sync` and `clean` instead report
per-file failures in `summary.failed` without that failing the run: both still
exit 0; see `optique(8)`'s EXIT STATUS section.

## An example

Turn `DOCS` off on every port in a list that actually has it. Asking for an
option a port lacks is a plan error, not a no-op, so the `--options` detail is
what the filter reads:

```sh
optique scan -z ws --json --options -f list |
    jq '[.ports[]
         | select(any(.options[]; .name == "DOCS"))
         | {(.port): {DOCS: false}}] | add' |
    optique decide -z ws -f list
```

