# The TUI

The TUI is optique's default interface: run `optique` with port origins or
`-f pkglist` and, with no subcommand, it opens one screen over the whole
dependency closure instead of poudriere's per-port dialog
(`poudriere options -C`). Every port is queried in parallel before anything
is drawn, so the triage, the edits and the apply all happen against one
already-scanned closure rather than one dialog per port. Nothing on disk
changes before you apply: quitting at any point before that is always safe.
It needs a real terminal — standard input and output must both be a
tty — unless `--drive` is given (see
[Driving the TUI headless](#driving-the-tui-headless)).

## The screen

<!-- screen main -->
```text
┌ Ports (21) ───────────────────┐┌ www/nginx — nginx-1.30.6_4,3 ─────────────┐
│* www/nginx                    ││  [ ] DEBUG                     off        │
│? devel/gmake                  ││  [x] DEBUGLOG                  off        │
│! ports-mgmt/pkg               ││  [x] DSO                       on         │
│✓ converters/libiconv          ││  [x] FILE_AIO                  on         │
│✓ devel/cmake-core             ││  [x] IPV6                      on         │
│✓ devel/gettext-runtime        ││  [ ] NJS                       off        │
│✓ devel/gettext-tools          ││  [ ] NJS_QJS                   off        │
│✓ devel/libtextstyle           ││  [ ] NJS_XML                   off        │
│✓ devel/libunistring           ││  [ ] OTEL                      off        │
│✓ devel/ninja                  ││  [x] THREADS                   on         │
│✓ devel/p5-Locale-libintl      ││  [x] WWW                       on         │
│✓ devel/pcre2                  ││── HTTPGRP — group: Modules that require HT│
│✓ devel/pkgconf                ││  [ ] GOOGLE_PERFTOOLS          off        │
│✓ devel/readline               ││  [x] HTTP                      on      imp│
│✓ lang/perl5.42                ││  [x] HTTP_ADDITION             on         │
│✓ lang/python312               ││  [x] HTTP_AUTH_REQ             on         │
│✓ misc/help2man                ││  [x] HTTP_CACHE                on         │
│✓ print/texinfo                ││  [x] HTTP_DAV                  on         │
│✓ security/rhash               ││  [ ] HTTP_DEGRADATION          off        │
│✓ textproc/expat2              │┌ DEBUGLOG ─────────────────────────────────┐
│✓ textproc/libxml2             ││  Enable debug log (--with-debug)          │
└───────────────────────────────┘└───────────────────────────────────────────┘
 0✗ 1* 1? 1! 18✓ (+20 optionless)  Space:toggle n/p:problems t:attention m:mc-
```
<!-- /screen -->

The capture shows `www/nginx` and its closure. The left pane lists every port in the closure that has options,
problems first, one marker per row; `www/nginx` itself is `*` because it
has staged, unapplied edits, `devel/gmake` is `?` because it has never been
configured, and `ports-mgmt/pkg` is `!` because its option list changed
since its file was written. The rest are `✓`: already consistent. The
right pane shows the selected port's options — `www/nginx` here — each as a
row with its checkbox, its name, the port's own default (`off`/`on`), and
the value make.conf dictates when one of its layers mentions the option
(blank otherwise); `HTTP`'s row ends in `imp`, the pane-edge truncation of
an `implied by` note — another enabled option implies it. A row
introduced by `──` is a group header (`HTTPGRP` here). The bottom line is
the status bar: how many ports carry each marker (`0✗ 1* 1? 1! 18✓`), how
many ports without options the list hides (`+20 optionless`), and the
start of the key legend, cut off by the screen width — see [Keys](#keys)
for the rest.

## Port markers

Status, left to right as the pane shows them, worst first:

| Marker | Meaning                                                                                    |
| :----: | :----------------------------------------------------------------------------------------- |
| `✗`    | conflict — staged options violate `PREVENTS` or a group rule                               |
| `*`    | edited — staged changes not yet applied                                                    |
| `?`    | unconfigured — no saved options file                                                       |
| `!`    | stale — the option list changed since the file was written                                 |
| `≠`    | contradicts the make.conf `OPTIONS_SET`/`OPTIONS_UNSET` policy; shown only in the `w` view |
| `≈`    | needs no attention because make.conf already decides it; shown only in the `m` view        |
| `✓`    | ok                                                                                         |

Three further markers may trail the status one:

| Marker | Meaning                                                                   |
| :----: | :------------------------------------------------------------------------ |
| `⊘`    | blacklisted for this jail, tree and set — never needs attention           |
| `⚠`    | the port is `BROKEN` or `IGNORE`d with its current options                |
| `∞`    | caught in a dependency loop — see [Dependency loops](dependency-loops.md) |

## The option pane

<!-- screen options -->
```text
┌ Ports (21) ───────────────────┐┌ www/nginx — nginx-1.30.6_4,3 ─────────────┐
│? devel/gmake                  ││  [ ] DEBUG                     off        │
│! ports-mgmt/pkg               ││  [ ] DEBUGLOG                  off        │
│✓ converters/libiconv          ││  [x] DSO                       on         │
│✓ devel/cmake-core             ││  [x] FILE_AIO                  on         │
│✓ devel/gettext-runtime        ││  [x] IPV6                      on         │
│✓ devel/gettext-tools          ││  [ ] NJS                       off        │
│✓ devel/libtextstyle           ││  [ ] NJS_QJS                   off        │
│✓ devel/libunistring           ││  [ ] NJS_XML                   off        │
│✓ devel/ninja                  ││  [ ] OTEL                      off        │
│✓ devel/p5-Locale-libintl      ││  [x] THREADS                   on         │
│✓ devel/pcre2                  ││  [x] WWW                       on         │
│✓ devel/pkgconf                ││── HTTPGRP — group: Modules that require HT│
│✓ devel/readline               ││  [ ] GOOGLE_PERFTOOLS          off        │
│✓ lang/perl5.42                ││  [x] HTTP                      on      imp│
│✓ lang/python312               ││  [x] HTTP_ADDITION             on         │
│✓ misc/help2man                ││  [x] HTTP_AUTH_REQ             on         │
│✓ print/texinfo                ││  [x] HTTP_CACHE                on         │
│✓ security/rhash               ││  [x] HTTP_DAV                  on         │
│✓ textproc/expat2              ││  [ ] HTTP_DEGRADATION          off        │
│✓ textproc/libxml2             │┌ FILE_AIO ─────────────────────────────────┐
│✓ www/nginx                    ││  Enable file aio                          │
└───────────────────────────────┘└───────────────────────────────────────────┘
 0✗ 0* 1? 1! 19✓ (+20 optionless)  Space:toggle n/p:problems t:attention m:mc-
```
<!-- /screen -->

Options are listed in framework order under group headers such as
`HTTPGRP` above; a group's kind is enforced on every toggle — `SINGLE`
needs exactly one member on, `RADIO` at most one, `MULTI` at least one.
Each row carries, in order: an `N` in the first column when the option was
added to the port since its options file was written (so every new option
is findable down the pane's left edge); the checkbox — `[x]`/`[ ]` for a
plain option, `(o)`/`( )` for a `SINGLE`/`RADIO` member; the option's name,
coloured yellow when the staged value deviates from the port's default and
magenta when it contradicts make.conf; the port's own default (`on`/`off`);
the value make.conf dictates, left blank when no layer mentions the
option and otherwise coloured by which knob decided it — green for the
global `OPTIONS_SET`/`OPTIONS_UNSET`, bright green for the per-port
`${OPTIONS_NAME}_SET`/`_UNSET`, red bold for a `*_FORCE` knob, which also
locks the toggle; then `implied by X` when an `IMPLIES` chain locked the
option on, and a `⚠broken`/`⚠ignored` badge on an option whose enabling
marks the port `BROKEN` or `IGNORE` (`<OPT>_BROKEN`/`<OPT>_IGNORE`). An
option the port no longer has
is struck through and dropped on apply; one excluded by the current
flavor but managed through the default flavor's shared file is listed in
its own section.

Whatever room is left on the row, after the badges, carries the option's
own description — the one thing the row usually has no space for; a row
with less than about sixteen columns of room drops the description
instead of showing a word fragment. A panel below the rows follows the
cursor and gives the selected option's description in full, wrapped,
growing to at most a third of the pane; an option the ports framework
describes nowhere reads `(no description)`. `i` opens a detail popup for
the selected option instead: its description, group, `IMPLIES`/`PREVENTS`,
and the dependencies enabling it would add.

A `[h] help` marker at the right of the pane's title means the port ships
a `pkg-help` file — the free-form notes `make config` offers behind its
Help button (`${PKGDIR}/pkg-help`); `h` opens it in a scrollable popup
(`j`/`k`, arrows, `PgUp`/`PgDn`, `g`/`G`; any other key closes it), read
from disk every time it opens. `F1` inside `make config` is portconfig's
own Ports Collection help, not the port's.

### The port's description

<!-- screen about -->
```text
┌ Ports (1) — filter: pcre2▏ ───┐┌ devel/pcre2 — pcre2-10.49 ────────────────┐
│✓ devel/pcre2                  ││Perl Compatible Regular Expressions        │
│                               ││library, version 2                         │
│                               ││PCRE2 is the name used for a revised API   │
│                               ││for the PCRE library, which is a set of    │
│                               ││functions, written in C, that implement    │
│                               ││regular expression pattern matching using  │
│                               ││the same syntax and semantics as Perl, wit…│
│                               │┌───────────────────────────────────────────┐
│                               ││  [x] DOCS         on                      │
│                               ││  [ ] LIBBZ2       off                     │
│                               ││  [ ] LIBZ         off                     │
│                               ││  [x] STATIC       on                      │
│                               ││── CLI — radio (pick at most one): Command-│
│                               ││  ( ) LIBEDIT      off                     │
│                               ││  ( ) READLINE     off                     │
│                               ││                                           │
│                               ││                                           │
│                               ││                                           │
│                               ││                                           │
│                               │┌ DOCS ─────────────────────────────────────┐
│                               ││  Build and/or install documentation       │
└───────────────────────────────┘└───────────────────────────────────────────┘
 port description shown above the options when there is room
```
<!-- /screen -->

`I` shows or hides the port's description above the options pane — it
starts hidden. It is the port's `COMMENT` and as much of its `pkg-descr`
as the option rows leave room for, and it never makes the rows scroll to
fit more. `i` on the port list shows the same information, in full,
in a popup — the port's `COMMENT`, `WWW` and `pkg-descr` — in the same
popup `h` uses for `pkg-help`. On the option pane, `i` instead opens the
detail popup for the selected option (see
[The option pane](#the-option-pane)).

## Keys

Keys are grouped here as the in-TUI help (`?`/`F1`) groups them.

### Navigate and edit

| Key               | Effect                                              |
| :---------------- | :-------------------------------------------------- |
| `j`/`k`, `↑`/`↓`  | move the selection                                  |
| `g`/`G`           | jump to the first / last entry                      |
| `PgUp`/`PgDn`     | page                                                |
| `Enter`, `l`      | edit the selected port                              |
| `Esc`, `Tab`, `←` | back to the port list                               |
| `Space`           | toggle the option, enforcing group rules            |
| `d`               | reset the port to its defaults                      |
| `u`               | revert the port to its saved state                  |
| `U`               | undo the last option change, up to 100 back         |
| `B`               | bulk-set one option across every visible port       |
| `n`/`p`           | next / previous port needing attention              |
| `f`               | jump to the next flavor of the same origin          |
| `i`               | option details; on the port list, its description   |
| `I`               | show / hide the port description above the options  |
| `h`               | the port's pkg-help notes, when its title has `[h]` |
| `r`               | why is this port here? (`Enter` jumps, `r` walks)   |

### Views and actions

| Key           | Effect                                                 |
| :------------ | :----------------------------------------------------- |
| `t`           | show only ports needing attention, hiding `✓` ones     |
| `s`           | problems first, or a stable alphabetical order         |
| `m`           | count ports make.conf fully decides as ok (marker `≈`) |
| `w`           | flag ports contradicting make.conf's policy (`≠`)      |
| `L`           | show only the ports in a dependency loop (`∞`)         |
| `/`           | filter the port list                                   |
| `a`           | apply: preview every file diff, then write             |
| `?`, `F1`     | the in-TUI help: markers, badges, keys                 |
| `Ctrl-L`      | repaint the whole screen                               |
| `q`, `Ctrl-C` | quit; staged edits can be kept as a draft              |

`m` applies to stale ports when make.conf decides every option they
gained, and to unconfigured ones when it decides all of them.

## Why is this port here?

<!-- screen why -->
```text
┌ Ports (1) — filter: libunistri┐┌ devel/libunistring — libunistring-1.4.2 ──┐
│✓ devel/libunistring           ││  [x] DOCS         on                      │
│                               ││                                           │
│                               ││                                           │
│                               ││                                           │
│           ┌ why devel/libunistring? ───────────────────────────┐           │
│           │www/nginx  (root)                                   │           │
│           │ └─ devel/pcre2                                     │           │
│           │  └─ devel/cmake-core                               │           │
│           │   └─ dns/libidn2                                   │           │
│           │    └─ devel/libunistring                           │           │
│           │                                                    │           │
│           │direct dependents (3):                              │           │
│           │  devel/gettext-tools                               │           │
│           │  dns/libidn2                                       │           │
│           │  print/texinfo                                     │           │
│           │                                                    │           │
│           │↑/↓ move · Enter jump to port · r why of selection ·│           │
│           └────────────────────────────────────────────────────┘           │
│                               ││                                           │
│                               │┌ DOCS ─────────────────────────────────────┐
│                               ││  Build and/or install documentation       │
└───────────────────────────────┘└───────────────────────────────────────────┘
 0✗ 0* 0? 0! 1✓ (+20 optionless)  Space:toggle n/p:problems t:attention m:mc-o
```
<!-- /screen -->

`r` opens the why-overlay for the selected port: its dependency chain
from a root of the closure down to it, its direct dependents, and, when
the port is caught in one, the [dependency loop](dependency-loops.md) it
belongs to, rotated to start at the port asked about. Every port named in
the overlay is selectable with `↑`/`↓`; `Enter` jumps the list to it, and
`r` re-opens the explanation there, which is how a chain or a loop is
walked port by port.

## Applying changes

<!-- screen apply -->
```text
┌ Ports (21) ───────────────────┐┌ www/nginx — nginx-1.30.6_4,3 ─────────────┐
│* www/nginx                    ││  [ ] DEBUG                     off        │
│? devel/gmake                  ││  [x] DEBUGLOG                  off        │
│! ports-mgmt/pkg               ││  [x] DSO                       on         │
│✓ conve┌ apply? y/n (j/k scroll) ───────────────────────────────────┐       │
│✓ devel│3 options file(s) will be written to                        │       │
│✓ devel│poudriere.d/server-options:                                 │       │
│✓ devel│                                                            │       │
│✓ devel│devel/gmake                             new file (1 options)│       │
│✓ devel│ports-mgmt/pkg                          dropped: OLDOPT     │       │
│✓ devel│www/nginx                               +DEBUGLOG           │       │
│✓ devel│                                                            │       │
│✓ devel│                                                            │uire HT│
│✓ devel│                                                            │       │
│✓ devel│                                                            │    imp│
│✓ lang/│                                                            │       │
│✓ lang/│                                                            │       │
│✓ misc/│                                                            │       │
│✓ print│                                                            │       │
│✓ secur└────────────────────────────────────────────────────────────┘       │
│✓ textproc/expat2              │┌ DEBUGLOG ─────────────────────────────────┐
│✓ textproc/libxml2             ││  Enable debug log (--with-debug)          │
└───────────────────────────────┘└───────────────────────────────────────────┘
 0✗ 1* 1? 1! 18✓ (+20 optionless)  Space:toggle n/p:problems t:attention m:mc-
```
<!-- /screen -->

Toggling an option re-queries the port in the background against a
staging `PORT_DBDIR` seeded from the real options directory; a dependency
the option adds or removes appears or disappears in the left pane within
a second or two, and newly appeared dependencies join the triage already
marked `?`/`!` while dropped ones vanish, keeping any edits already
staged on them in case they reappear. Nothing touches the real options
directory until `a` is pressed. That previews every file that would
change as a diff — `+OPT`/`-OPT` for a changed option, `new file` for a
port gaining an options file, `dropped: OPT` for an option the port no
longer has — as the capture shows for `devel/gmake`,
`ports-mgmt/pkg` and `www/nginx`, then writes every file atomically in
one pass (temporary file, `fsync`, `rename`) to the set's options
directory.

Each options file is read once, when the TUI starts, and judged as it
was then: a file edited outside optique while the interface is open is
left alone by apply unless its port is also edited in the interface.
After an apply, the files it wrote are read again.

Under `-m`/`--minimal`, only deviations from defaults and make.conf are
persisted, and a bare default counts as already decided, so the
interface flags only deviations and conflicts. A port whose configuration
matches what its defaults and make.conf already produce gets no options
file at all: apply skips writing it, and removes an existing file that
had become identical.

## Quitting and drafts

`q` or `Ctrl-C` quits. Quitting first stops any in-flight background
`make` process and the commands it is running, so nothing keeps churning
after the interface is gone; the quit dialog shows how many were
stopped, and the count is also reported on stderr.

Quitting with staged, unapplied changes opens a prompt first: `s` saves
them as a **draft** under `~/.cache/optique/drafts/`, one file per
options directory, named after a hash of its path; `d` discards them;
any other key cancels and returns to the interface. The next TUI launch
on the same options directory restores the draft's staged edits —
options that vanished from a port meanwhile are dropped, and the
restored ports are re-queried so their dependencies reappear. The draft
is cleared as soon as the changes are applied (they are on disk by then)
or discarded; `u` still reverts an individual port as usual. Quitting
with nothing staged skips the prompt and clears the draft too: a
restored draft undone by hand has nothing left to bring back.

## Driving the TUI headless

`optique tui --drive` runs the interface headless, for debugging and
end-to-end testing: the closure is scanned as usual, the keymap, the
application state and the drawing code are the interactive ones, but
instead of taking over a terminal it renders into an in-memory screen
(100x35 cells by default) and reads one command per line from standard
input. It reproduces a rendering bug or tests the TUI in CI, or anywhere
else without a terminal.

```sh
printf 'dump\nkey /\nkeys nginx\nkey enter\nstate\nquit\n' |
    optique tui --drive -o /tmp/opt www/nginx
```

Blank lines and lines starting with `#` are ignored. Every other line is
acknowledged with `ok …`/`err …`, except `dump` and `state`, which print
their payload instead; output is flushed after each command so the
driver can be held open on a pipe, and a malformed command is reported
without ending the session. Pending background re-queries are merged
before each command, exactly as the interactive event loop does on every
tick.

- `key <spec>` — press one key: an optional `ctrl-`/`alt-` prefix, then a
  single character (case matters: `B` is the bulk key, `b` is not) or a
  name (`enter esc tab backtab space backspace up down left right pgup
  pgdn home end f1`…`f12 question slash`)
- `keys <text>` — press each character of `<text>` in turn, spaces
  included, for typing into the filter or bulk prompts
- `dump` — draw a frame and print it: `screen <W>x<H>`, then one line per
  row with trailing blanks stripped, then `end`
- `state` — one line of JSON: `focus`, `selected`, `visible`, `listable`,
  `refreshing`, `pending`, `message`, `overlay`, `filter`, `dirty`, `loops`
- `settle [timeout_ms]` — wait until no background re-query is
  outstanding (default 5000 ms), answering `ok settled` or
  `err settle timeout`; also what lets the 300 ms edit debounce expire
  before a `dump`
- `resize <W>x<H>` — recreate the in-memory screen at that size, clamped
  to 20–500 cells
- `quit` — end the session; so do end of file on standard input and any
  key the interface itself treats as quitting

Nothing is written to the options directory unless the script drives the
apply flow itself, so pointing `-o` at a scratch directory keeps a
session harmless. `tui --drive` is a keystroke-level protocol over the
real keymap and drawing code — a debugging tool, not a way to configure
ports from a script; for that, see [Machine interface](machine-interface.md).
