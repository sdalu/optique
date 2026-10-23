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
┌ Ports (21 · 20 optionless) ───┐╔ pkg-2.8.4 ════════════════════════════════╗
│?  devel/gmake                 │║STALE: obsolete OLDOPT                     ║
│!* ports-mgmt/pkg              │║  [ ] DOCS         on                      ║
│── 19 ok ──────────────────────│║── obsolete (no longer exist; dropped on…  ║
│✓  converters/libiconv         │║  [x] OLDOPT                               ║
│✓  devel/cmake-core            │║                                           ║
│✓  devel/gettext-runtime       │║                                           ║
│✓  devel/gettext-tools         │║                                           ║
│✓  devel/libtextstyle          │║                                           ║
│✓  devel/libunistring          │║                                           ║
│✓  devel/ninja                 │║                                           ║
│✓  devel/p5-Locale-libintl     │║                                           ║
│✓  devel/pcre2                 │║                                           ║
│✓  devel/pkgconf               │║                                           ║
│✓  devel/readline              │║                                           ║
│✓  lang/perl5.42               │║                                           ║
│✓  lang/python312              │║                                           ║
│✓  misc/help2man               │║                                           ║
│✓  print/texinfo               │║                                           ║
│✓  security/rhash              │╟ DOCS ─────────────────────────────────────╢
│✓  textproc/expat2             │║  Build and/or install documentation       ║
│✓  textproc/libxml2            │║                                           ║
└───────────────────────────────┘╚══════════════════════════════ -DOCS · 1/1 ╝
 0✗ 1* 1? 1! 19✓  ?:help a:apply q:quit Space:toggle i:info d:defaults …
```
<!-- /screen -->

The capture shows `www/nginx` and its closure. The left pane lists every port in the closure that has options,
problems first, two marker columns per row (see [Port markers](#port-markers)):
`devel/gmake` is `?` because it has never been configured, and
`ports-mgmt/pkg` is `!` because its option list changed since its file was
written — `!*` here, with staged, unapplied edits. A
`── 19 ok ──` row parts them from the rest, `✓`: already consistent. The
right pane shows the selected port's options — `ports-mgmt/pkg` here,
titled with its package, `pkg-2.8.4` — each as a row with its checkbox,
its name, the port's own default (`off`/`on`), and what make.conf says
of the option when one of its layers names it (blank otherwise).
`OLDOPT`, which the port's file records but the tree no longer has, is
named by the `STALE: obsolete OLDOPT` line above the rows and listed
under `── obsolete`, struck through, after the last option: the cursor
stops at that option, and on it the section scrolls into view. The pane's bottom border gives
the port's staged change, `-DOCS`, and the cursor's place among its
options, `1/1`. The list's title
counts the ports listed and, after a `·`, the ports without options the
list never shows (`Ports (21 · 20 optionless)`; the part is left out when
there are none), then the view it is in, `— needs attention` under `t`
and `— in a dependency loop` under `L`. Too long for the pane, the title
drops the optionless count first, the view being kept before it, then
names the view in short — `— attention`, `— in a loop` — and only then
loses its end to a `…` at a word: `Ports (8/142) — attention` at 80
columns under `t`. The pane with the focus has a double border, the other a
single one. The bottom line is the status bar: how many ports are in
each state, and how many of them are edited (`0✗ 1* 1? 1! 19✓`) — then,
when there are any, the ports caught in a dependency loop (`2∞ L:loops`)
and the progress of a background re-query (`⟳ refreshing deps 3/5`) — and the key legend:
`?:help a:apply q:quit` first, then the keys of the focused pane —
`Enter:edit n/p:next-problem t:attention /:filter v:description` on the
port list, `Space:toggle i:info d:defaults U:revert u:undo
n/p:next-problem` in the option pane. What does not fit the width is cut
at a whole key, a `…` standing for the rest; at 80 columns the counts and
`?:help a:apply q:quit` always show. See [Keys](#keys) for every key.

### Messages

A message takes the status bar's place, green for news and red for an
error, until the next key. One the background sets — a re-query's result,
a port that just turned `BROKEN` or `IGNORE`, a dependency loop made or
broken — does not take the place of a message set since the last key,
which there has not been a key's time to read, unless it is an error; and
nothing but an error replaces an error. A re-query that changed the
closure names the ports it brought in and dropped when there are three or
fewer (`dependencies refreshed: +audio/opencore-amr −devel/foo`), and
counts them past that (`dependencies refreshed: +5 −2 port(s)`).

At start, once the closure is scanned, the first message sums up what
needs doing, in list order, leaving out what is at zero:

```text
3 conflicting · 4 never configured · 2 stale — a: write defaults for all · n: next problem · ?: help
```

`a: write defaults for all` shows only when some ports were never
configured: the apply writes them their defaults in one go.

and, with nothing to do, `all 21 ports are configured — ?: help`. When a
draft was restored (see [Quitting and drafts](#quitting-and-drafts)), it
comes first on the same line, with how many ports it staged again and how
old it is:

```text
restored draft (2 ports, 5m ago, U per port reverts) · 3 conflicting · 4 never configured — a: write defaults for all · n: next · ?: help
```

On a screen too narrow for the whole line, `U per port reverts` goes
first, then the rest is cut at a whole part with `…`, the keys from the
right before any count; the draft's count and age always stay. Being a
message set before the first key, the summary stays up through the
re-query a restored draft starts.

## Port markers

Each row starts with two marker columns. The first is the port's state,
left to right as the pane sorts them, worst first:

| Marker | Meaning                                                                                    |
| :----: | :----------------------------------------------------------------------------------------- |
| `✗`    | conflict — staged options violate `PREVENTS` or a group rule                               |
| `?`    | unconfigured — no saved options file                                                       |
| `!`    | stale — the option list changed since the file was written                                 |
| `≠`    | contradicts the make.conf `OPTIONS_SET`/`OPTIONS_UNSET` policy; shown only in the `w` view |
| `≈`    | needs no attention because make.conf already decides it; shown only in the `m` view        |
| `✓`    | ok                                                                                         |

The second is `*` when the port has staged changes not yet applied, and
blank otherwise. An edit hides no state: an edited port that has never
been configured reads `?*`, an edited conflicting one `✗*`, an edited ok
one `✓*`. The order, `n`/`p` and `t` go by the state alone, so an edited
ok port sorts with the ok ones, and `n`/`p` and `t` pass it by; the
status bar counts every port in its state and the edited ones again,
under `*`.

In problems-first order, when the list holds both, a divider row parts
the ports needing attention from the ok ones (`≈` and `✓`) and counts
the latter:

```text
│?  devel/gmake                 │
│!  ports-mgmt/pkg              │
│── 19 ok ──────────────────────│
│✓  converters/libiconv         │
```

It is not a port: no key selects it, and the selection steps over it.
Neither `s` (alphabetical order) nor `t` (no ok port listed) shows it.

Four further markers may trail the port's name:

| Marker | Meaning                                                                   |
| :----: | :------------------------------------------------------------------------ |
| `⊘`    | blacklisted for this jail, tree and set — never needs attention           |
| `⚠`    | the port is `BROKEN` or `IGNORE`d with its current options                |
| `∞`    | caught in a dependency loop — see [Dependency loops](dependency-loops.md) |
| `+N`   | N options added since the port's file was written, each marked `N` in the option pane; shown even on a port `-m` or the `m` view settles (`≈`) |

A row too long for the pane loses its category first, the port's name,
its `@flavor` and these markers kept whole: `multim…/ffmpeg@default ⚠`,
then `…/ffmpeg@default ⚠`. Still too long, the end of the name and flavor
goes to a `…`; a marker never does. A row that fits is left as it is.

## The option pane

<!-- screen options -->
```text
┌ Ports (21 · 20 optionless) ───┐╔ nginx-1.30.6_5,3 ═════════════════════════╗
│!  ports-mgmt/pkg              │║  [ ] DEBUG                     off        ║
│── 19 ok ──────────────────────│║  [ ] DEBUGLOG                  off        ║
│✓  converters/libiconv         │║  [x] DSO                       on         ║
│✓  devel/cmake-core            │║  [x] FILE_AIO                  on         ║
│✓  devel/gettext-runtime       │║  [x] IPV6                      on         ║
│✓  devel/gettext-tools         │║  [ ] NJS                       off        ║
│✓  devel/libtextstyle          │║  [ ] NJS_QJS                   off        ║
│✓  devel/libunistring          │║  [ ] NJS_XML                   off        ║
│✓  devel/ninja                 │║  [ ] OTEL                      off        ║
│✓  devel/p5-Locale-libintl     │║  [x] THREADS                   on         ║
│✓  devel/pcre2                 │║  [x] WWW                       on         ║
│✓  devel/pkgconf               │║── HTTPGRP — group: Modules that require…  ║
│✓  devel/readline              │║  [ ] GOOGLE_PERFTOOLS          off        ║
│✓  lang/perl5.42               │║  [x] HTTP                      on      im…║
│✓  lang/python312              │║  [x] HTTP_ADDITION             on         ║
│✓  misc/help2man               │║  [x] HTTP_AUTH_REQ             on         ║
│✓  print/texinfo               │║  [x] HTTP_CACHE                on         ║
│✓  security/rhash              │║  [x] HTTP_DAV                  on         ║
│✓  textproc/expat2             │╟ FILE_AIO ─────────────────────────────────╢
│✓  textproc/libxml2            │║  Enable file aio                          ║
│✓  www/nginx                   │║                                           ║
└───────────────────────────────┘╚════════════════════════════════════ 4/118 ╝
 0✗ 0* 1? 1! 19✓  ?:help a:apply q:quit Space:toggle i:info d:defaults …
```
<!-- /screen -->

Options are listed in framework order under group headers such as
`HTTPGRP` above; a group's kind is enforced on every toggle — `SINGLE`
needs exactly one member on, `RADIO` at most one, `MULTI` at least one.
Each row carries, in order: an `N` in the first column when the option was
added to the port since its options file was written, or the file does not
settle it (missing from its `_FILE_COMPLETE_OPTIONS_LIST`, or without a
SET/UNSET line: only a hand edit leaves either, and apply rewrites it), so
every new option is findable down the pane's left edge; the checkbox — `[x]`/`[ ]` for a
plain option, `(o)`/`( )` for a `SINGLE`/`RADIO` member; the option's name,
coloured yellow when the staged value deviates from the port's default and
magenta when it contradicts make.conf; the port's own default (`on`/`off`);
what make.conf says of the option, whatever decides its value now — the
options file included: the value of the last layer naming it, in the
framework's order (the global knob, then the per-port one, then the
`*_FORCE` knobs, the later winning), left blank only when no layer names
the option and otherwise coloured by that layer — green for the
global `OPTIONS_SET`/`OPTIONS_UNSET`, bright green for the per-port
`${OPTIONS_NAME}_SET`/`_UNSET`, red bold for a `*_FORCE` knob, which also
locks the toggle; then `implied by X` when an `IMPLIES` chain locked the
option on, and a `⚠broken`/`⚠ignored` badge on an option whose enabling
marks the port `BROKEN` or `IGNORE` (`<OPT>_BROKEN`/`<OPT>_IGNORE`). An
option the port no longer has
is struck through and dropped on apply; one excluded by the current
flavor but managed through the default flavor's shared file is listed in
its own section.

Above the rows, a line each says what is wrong with the port or whose
view it is: `BROKEN:`, `IGNORE:` and the option changes likely to blame,
`DEPRECATED:`, the flavor owning its options file, a `CONFLICT:` per
rule the staged options break, and `STALE:` when the port's option list
changed since its file was written, naming the options added and those
gone — `STALE: new A B · obsolete X`, each part only when it has options.
For a port the list counts as done (`≈` under `-m` or the `m` view, or
`⊘`), the `STALE:` line is dimmed and says what settles it:
`STALE: obsolete X — settled by the defaults (-m)`.
A `CONFLICT:` names make.conf's hand in it after a `—`: the members on
because a make.conf layer sets them where the options file does not, and
that layer — `— A, B on by make.conf OPTIONS_SET`, or
`${OPTIONS_NAME}_SET` for the per-port knob; a member a `*_FORCE` knob
pins on — `— A forced on by make.conf; fix it there`; for a group with no
member on, the members to pick from, and those make.conf turns off —
`— pick one of A B C (A off by make.conf)`. A `PREVENTS` conflict keeps
its own text, and gains make.conf's part when make.conf sets one of the
two. A row, a group header or one of those
lines too wide for the pane ends in `…` after its last whole word, rather
than being cut mid-word at the border; a `CONFLICT:` or `STALE:` line
wraps first to a second line, under its text, and is cut only past that. The list's
title is cut the same way. An option row too wide for the pane drops only
its trailing description: its checkbox, name, default, make.conf value and
badges always stay, and a row still too wide has the last of them cut in
its own text at the border — `HTTP`'s `implied by` note reads `im…` in
the capture above — never with a `…` after a whole column.

The pane's title is the port's versioned package name (`nginx-1.30.6_5,3`;
the origin is the list's row), cut with `…` at a word when it does not
fit. A flavored port's title
lists its flavors after `flavors:`, marking with `*` the flavor whose view
owns the shared options file and with `‹` the flavor shown in the pane:
`flavors: py311* py312‹` is the `py312` view of a file the `py311` view
owns. A flavor the closure holds is underlined — `f` reaches it; one the
port declares but the closure does not hold is plain. Underline is an
attribute, not a colour, so it reads whether the pane has the focus or
not.

The pane's bottom border carries what is not a row. On its left,
`overlay: NAME` for a port an overlay answers for rather than the ports
tree. On its right, the port's staged changes against its saved file,
`+OPT -OPT`, additions first — shown only while it has some — and, while
the pane has the focus and the cursor is on an option, the cursor's place
among the port's options, `I/N`, the two parted by `·` — `+DEBUGLOG ·
4/118`, say, for nginx. Where they do not fit, the overlay's name is cut first, then the
changes lose their start to a `…`: the place always shows. A port with
none of them has a plain border.

Whatever room is left on the row, after the badges, carries the option's
own description — the one thing the row usually has no space for; a row
with less than about sixteen columns of room drops the description
instead of showing a word fragment, and one too long ends in `…` at a
word — or goes, when not even its first two words fit. A panel below the rows —
one line when the pane is 56 columns wide or more (a terminal of about 100
columns), two on a narrower one, so it never changes with the option —
titled with the option's name, follows the cursor and gives the selected
option's description, wrapped, marking with `…` what does not fit; an
option the ports framework describes nowhere reads `(no description)`.
The panel keeps its place and height whatever the option, so the rows
never move; while no option is highlighted — the port list has the focus —
it stays, untitled, and reads `Enter: edit the options`. The rules
above and below the rows join the pane's border: `╟`…`╢` while the pane
has the focus and its border is double, `├`…`┤` otherwise. `i` opens a detail popup for
the selected option instead: its description, group, `IMPLIES`/`PREVENTS`,
and the dependencies enabling it would add.

A `[h] notes` marker at the right of the pane's title means the port ships
a `pkg-help` file — the free-form notes `make config` offers behind its
Help button (`${PKGDIR}/pkg-help`); `h` opens it in a scrollable popup,
read from disk every time it opens. The file is written for an 80-column
terminal, hard-wrapped: the popup joins the lines of a paragraph and wraps
it to its own width, a blank line ending the paragraph, while a line that
starts with a blank or a bullet (`- `, `* `, `• `), or holds a run of two
spaces or more (a table, say — two spaces after a full stop do not count),
keeps a line of its own, wrapped under its
indent, as a `WWW:` line does. The popup is 76% of the screen wide but at least 84 columns, so an
80-column line fits on a screen 88 columns wide or more. Every popup that
scrolls — this one, the help, `r`, `i`, the apply preview — takes the same
keys: `j`/`k` or `↑`/`↓` a row, `g`/`G` or `Home`/`End` the first or the
last, `PgUp`/`PgDn` or `Ctrl-B`/`Ctrl-F` a page; none of them closes it,
and its other keys are on its bottom border. Any popup too small for its
text shows a scrollbar on its right border, between `▲` and `▼`, its thumb
as tall as the share in view and where it is. `F1` inside `make config` is
portconfig's own Ports Collection help, not the port's.

### The port's description

<!-- screen about -->
```text
╔ Ports (1 · 20 optionless) ════╗┌ pcre2-10.49 ──────────────────────────────┐
║✓  devel/pcre2                 ║│Perl Compatible Regular Expressions        │
║                               ║│library, version 2                         │
║                               ║│PCRE2 is the name used for a revised API   │
║                               ║│for the PCRE library, which is a set of    │
║                               ║│functions, written in C, that implement…   │
║                               ║├────────────────────────── [v] full text ──┤
║                               ║│  [x] DOCS         on                      │
║                               ║│  [ ] LIBBZ2       off                     │
║                               ║│  [ ] LIBZ         off                     │
║                               ║│  [x] STATIC       on                      │
║                               ║│── CLI — radio (pick at most one):…        │
║                               ║│  ( ) LIBEDIT      off                     │
║                               ║│  ( ) READLINE     off                     │
║                               ║│                                           │
║                               ║│                                           │
║                               ║│                                           │
║                               ║│                                           │
║                               ║│                                           │
║                               ║├───────────────────────────────────────────┤
║                               ║│  Enter: edit the options                  │
║                               ║│                                           │
╚════════════════ filter: pcre2 ╝└───────────────────────────────────────────┘
 port description shown (V: hide · v: full text)
```
<!-- /screen -->

The port's description sits above the options pane — shown at start when
the terminal has 30 rows or more; `V` shows or hides it until you quit.
With no `V` pressed, it follows the terminal: a resize across 30 rows
shows or hides it. It is the port's `COMMENT` and as much of its `pkg-descr`
as fits in five lines, a bulleted line keeping a line of its own. The
panel keeps that height from one port to the next, so the option rows do
not move; it shrinks only on a pane too short to leave the options four
rows, and never depends on how many options the port has, which scroll
under it. When the text is cut, the rule under the panel says
`[v] full text`: `v`, from either pane, shows the port's `COMMENT`, `WWW`
and `pkg-descr` in full, in the popup `h` uses for `pkg-help`, laid out
the same way (`i` is the
selected option's detail popup, in the option pane, see
[The option pane](#the-option-pane); on the port list it only says so). A port that has
neither a `COMMENT` nor a `pkg-descr` reads `No description available`; a
`pkg-descr` that cannot be read says so, and is tried again the next time
the port is selected.

## Keys

Keys are grouped here as the in-TUI help (`?`/`F1`) groups them.

### Navigate and edit

| Key                              | Effect                                                    |
| :------------------------------- | :-------------------------------------------------------- |
| `j`/`k`, `↑`/`↓`                 | move the selection                                        |
| `g`/`G`                          | jump to the first / last entry                            |
| `PgUp`/`PgDn`, `Ctrl-B`/`Ctrl-F` | page: the rows shown, less one                            |
| `Enter`, `Tab`, `→`              | edit the selected port                                    |
| `Esc`, `Tab`, `←`                | back to the port list                                     |
| `Space`                          | toggle the option, enforcing group rules                  |
| `d`                              | reset the port to its defaults, from either pane          |
| `U`                              | revert the port to its saved state, from either pane      |
| `u`                              | undo the last change, on any port, up to 100 back         |
| `B`                              | bulk-set one option across the listed ports               |
| `n`/`p`                          | next / previous port needing attention                    |
| `f`                              | jump to the next flavor of the same origin                |
| `i`                              | the selected option's details, in the option pane         |
| `v`                              | the port's full description (`COMMENT`, `WWW`, ...)       |
| `V`                              | show / hide the port description above the options        |
| `h`                              | the port's pkg-help notes, when its title has `[h] notes` |
| `r`                              | why is this port here? (`Enter` jumps, `r` walks)         |

`Space` alone toggles: `Enter` takes the list into the option pane and
does nothing once there. On the port list `Space` toggles nothing and
says so: `Enter: edit the options`. From the option pane, `n` and `p`
move to the port and keep the pane. Entering the pane, and reaching another port from
it — `n`, `p` or a jump from `r` — puts the cursor on the port's first
option needing attention: for a conflicting port, the first member, in row
order, of the first rule it breaks; else, for a stale one, its first new
option (`N`); else its first option. Never on a group header. `f` keeps the
option the cursor is on, to compare it across flavors, and lands the same
way only when the other flavor lacks it.

`B` asks for `OPTION=on`, `OPTION=off`, `OPTION+` or `OPTION-`, prefilled
with `OPTION=` when the cursor is on an option, and sets it on every
listed port that has it. The prompt's title says which ports those are —
`bulk over the 12 listed ports`, then `(filter: TEXT)`, `(attention
only)` or `(in a loop)` for what narrows them — and its bottom border
counts, as you type, the listed ports carrying the option named so far,
`5 carry DOCS`, or says in red `no listed port has DOCS`. `Ctrl-U`
clears the line, `Enter` applies, `Esc` cancels; an input that does not
parse is quoted whole in the error.

### Views and actions

| Key           | Effect                                                 |
| :------------ | :----------------------------------------------------- |
| `t`           | show only ports needing attention, hiding `✓` ones     |
| `s`           | problems first, or a stable alphabetical order         |
| `m`           | count ports make.conf fully decides as ok (marker `≈`) |
| `w`           | flag ports contradicting make.conf's policy (`≠`)      |
| `L`           | show only the ports in a dependency loop (`∞`)         |
| `/`           | filter the port list, starting afresh                  |
| `Esc`         | on the port list, clear the filter                     |
| `a`           | apply: preview, then write; `Y`/`s` on a conflict      |
| `?`, `F1`     | the in-TUI help: markers, badges, keys                 |
| `Ctrl-L`      | repaint the whole screen                               |
| `q`, `Ctrl-C` | quit; staged edits can be kept as a draft              |

The filter shows on the list's bottom border, at its right — `filter:
TEXT▏` while it is being typed, `filter: TEXT` once `Enter` leaves it on
the list — or `↑`/`↓`, which also move on the list, so a few letters and an
arrow reach the port (`j`/`k` stay letters of the filter); `Esc` while
typing drops it. A filter too long for the border
loses its start to a `…` (`filter: …ginx-module▏`), so the end being
typed stays in view; the title keeps the count and the views.
`Ctrl-C` quits from anywhere — a popup, the filter, a prompt — and a
Ctrl-letter is never read as its plain letter.

`m` applies to stale ports when make.conf decides every option they
gained, and to unconfigured ones when it decides all of them.

## Why is this port here?

<!-- screen why -->
```text
╔ Ports (1 · 20 optionless) ════╗┌ libunistring-1.4.2 ───────────────────────┐
║✓  devel/libunistring          ║│  [x] DOCS         on                      │
║                               ║│                                           │
║                               ║│                                           │
║                               ║│                                           │
║           ┌ why devel/libunistring? ───────────────────────────┐           │
║           │www/nginx  (root)                                   │           │
║           │ └─ devel/pcre2                                     │           │
║           │  └─ devel/cmake-core                               │           │
║           │   └─ dns/libidn2                                   │           │
║           │    └─ devel/libunistring                           │           │
║           │                                                    │           │
║           │direct dependents (3):                              │           │
║           │  devel/gettext-tools                               │           │
║           │  dns/libidn2                                       │           │
║           │  print/texinfo                                     │           │
║           │                                                    │           │
║           │                                                    │           │
║           └ Enter jump · r why · j/k move · other key closes ──┘           │
║                               ║├───────────────────────────────────────────┤
║                               ║│  Enter: edit the options                  │
║                               ║│                                           │
╚═════════ filter: libunistring ╝└───────────────────────────────────────────┘
 0✗ 0* 0? 0! 1✓  ?:help a:apply q:quit Enter:edit n/p:next-problem …
```
<!-- /screen -->

`r` opens the why-overlay for the selected port: its dependency chain
from a root of the closure down to it, its direct dependents, and, when
the port is caught in one, the [dependency loop](dependency-loops.md) it
belongs to, rotated to start at the port asked about. Every port named in
the overlay is selectable with the popups' scroll keys (`j`/`k` or `↑`/`↓`,
`g`/`G`, `PgUp`/`PgDn`), the view following; `Enter` jumps the list to it, and
`r` re-opens the explanation there, which is how a chain or a loop is
walked port by port. Any other key closes it.

## Applying changes

<!-- screen apply -->
```text
╔ Ports (21 · 20 optionless) ═══╗┌ nginx-1.30.6_5,3 ─────────────────────────┐
║!  ports-mgmt/pkg              ║│  [ ] DEBUG                     off        │
║── 19 ok ──────────────────────║│  [x] DEBUGLOG                  off        │
║✓  converters/libiconv         ║│  [x] DSO                       on         │
║✓  deve┌ apply? ────────────────────────────────────────────────────┐       │
║✓  deve│3 options file(s) will be written to …/server-options:      │       │
║✓  deve│                                                            │       │
║✓  deve│devel/gmake                             new file (1 of 1 on)│       │
║✓  deve│ports-mgmt/pkg                          dropped: OLDOPT     │       │
║✓  deve│www/nginx                               +DEBUGLOG           │       │
║✓  deve│                                                            │       │
║✓  deve│                                                            │       │
║✓  deve│                                                            │uire…  │
║✓  deve│                                                            │       │
║✓  lang│                                                            │    im…│
║✓  lang│                                                            │       │
║✓  misc│                                                            │       │
║✓  prin│                                                            │       │
║✓  secu│                                                            │       │
║✓  text└ y: write · n: cancel · j/k scroll ─────────────────────────┘───────┤
║✓  textproc/libxml2            ║│  Enter: edit the options                  │
║✓* www/nginx                   ║│                                           │
╚═══════════════════════════════╝└──────────────────────────────── +DEBUGLOG ┘
 0✗ 1* 1? 1! 19✓  ?:help a:apply q:quit Enter:edit n/p:next-problem …
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
`ports-mgmt/pkg` and `www/nginx`, then, on `y`, writes every file
atomically in one pass (temporary file, `fsync`, `rename`) to the set's
options directory. The keys sit on the preview's bottom border, in a
shorter form (`y write · n cancel · j/k`) on a narrow box, and the
`written to` line shortens a directory too long for it to `…` and its
last two components. The result, `apply — done`, shows the directory
whole, and the list behind it — its order and its `── N ok ──`
divider — is already rebuilt. If any file cannot be written, none is and
nothing is removed: the result, `apply — FAILED` in a red border, names
the directory once, shortened the same way, then each failed port with
its reason, the path in it cut to its last components, and the edits
stay staged. Once the result is closed, the status bar says what the
apply did until the next key: `2 file(s) written`, followed by
`, 1 removed` and `, 1 skipped: conflicting` when it did so, or in red
`apply failed: nothing written — ` and the first reason.

A port whose staged options still violate their constraints (`✗`) makes
its file conflicting: the build's `check-config` would fail on it. When
the pass would write one, the preview reads `apply with N conflicting?`,
lists those ports first under `conflicting (Y writes them, s skips
them):`, and `y` writes nothing, leaving the preview open with a line
naming the keys. `Y` writes every file, conflicting ones included; `s`
writes every file but the conflicting ports' (and removes, under `-m`,
every planned file but theirs), and the result adds `N port(s) skipped:
conflicting options`. A skipped port's file stays as it is on disk; its
edits stay staged, still marked `✗`, and still in the draft a quit
offers to keep. Flavors share one options file, so the file conflicts
when any of them does. `n`, `N` or `Esc` cancels; any other key but the
scroll keys, `Enter` included, leaves the preview open and names the
keys, `y writes · n cancels` (or, with a conflict, the line above).

Each options file is read once, when the TUI starts, and judged as it
was then: a file edited outside optique while the interface is open is
left alone by apply unless its port is also edited in the interface.
After an apply, the files it wrote are read again.

Under `-m`/`--minimal`, only deviations from defaults and make.conf are
persisted, and a bare default counts as already decided, so the
interface flags only deviations and conflicts: a port the defaults and
make.conf settle shows `≈` (as under the `m` view) rather than `?` or
`!`, file or no file. A port whose configuration
matches what its defaults and make.conf already produce gets no options
file at all: apply skips writing it, and removes an existing file that
had become identical.

## Quitting and drafts

`q` or `Ctrl-C` quits. Quitting first stops any in-flight background
`make` process and the commands it is running, so nothing keeps churning
after the interface is gone; the quit dialog shows how many were
stopped, and the count is also reported on stderr.

Quitting with staged, unapplied changes opens a prompt first,
`Keep staged edits as a draft?`: `y` (or a second `Ctrl-C`) keeps them
as a **draft** under `~/.cache/optique/drafts/`, one file per options
directory, named after a hash of its path, and quits; `n` discards them,
an older draft included, and quits; `Esc` or any other key stays in the
interface. The next TUI launch
on the same options directory restores the draft's staged edits —
options that vanished from a port meanwhile are dropped, and the
restored ports are re-queried so their dependencies reappear — and its
first message says so: `restored draft (2 ports, 5m ago, …)`, ahead of
what else the closure needs (see [Messages](#messages)). The draft
is cleared as soon as the changes are applied (they are on disk by then)
or discarded; `U` still reverts an individual port as usual. Quitting
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
tick, and a frame is drawn before each key, as that loop draws one, so
the page keys move by what the screen shows.

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
