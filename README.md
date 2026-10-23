# optique

Configure the options of a whole build at once, in one screen: for
poudriere, for synth, or for `make` on the host.

`poudriere options -C <list>` walks the dependency tree one port at a time,
with a dialog for each, in dependency order, with no overview and no way
back. optique resolves the whole dependency closure of a package list **in
parallel** (about a thousand ports in under a minute cold, well under a
second once cached), shows every port in one interface with the ones that
need a decision first, and writes every options file in a single atomic
pass when you apply. It reads exactly what poudriere would — the options
directory, the make.conf layers, the blacklist — for the jail, tree and set
you name, synth's for a synth profile, and the host's own `/var/db/ports`
when there is no poudriere at all or `-l` asks for a local build.

<!-- screen main -->
```text
┌ Ports (21 · 20 optionless) ───┐╔ www/nginx — nginx-1.30.6_5,3 ═════════════╗
│!  ports-mgmt/pkg              │║Robust and small WWW server                ║
│── 19 ok ──────────────────────│║NGINX is a high performance edge web server║
│✓  converters/libiconv         │║with the lowest memory footprint and the   ║
│✓  devel/cmake-core            │║key features to build modern and efficient ║
│✓  devel/gettext-runtime       │║web infrastructure.…                       ║
│✓  devel/gettext-tools         │╟────────────────────────── [v] full text ──╢
│✓  devel/libtextstyle          │║  [ ] DEBUG                     off        ║
│✓  devel/libunistring          │║  [x] DEBUGLOG                  off        ║
│✓  devel/ninja                 │║  [x] DSO                       on         ║
│✓  devel/p5-Locale-libintl     │║  [x] FILE_AIO                  on         ║
│✓  devel/pcre2                 │║  [x] IPV6                      on         ║
│✓  devel/pkgconf               │║  [ ] NJS                       off        ║
│✓  devel/readline              │║  [ ] NJS_QJS                   off        ║
│✓  lang/perl5.42               │║  [ ] NJS_XML                   off        ║
│✓  lang/python312              │║  [ ] OTEL                      off        ║
│✓  misc/help2man               │║  [x] THREADS                   on         ║
│✓  print/texinfo               │║  [x] WWW                       on         ║
│✓  security/rhash              │║── HTTPGRP — group: Modules that require HT║
│✓  textproc/expat2             │╟ DEBUGLOG ─────────────────────────────────╢
│✓  textproc/libxml2            │║  Enable debug log (--with-debug)          ║
│✓* www/nginx                   │║                                           ║
└───────────────────────────────┘╚═══════════════════════════════════════════╝
 0✗ 1* 1? 1! 19✓  ?:help a:apply q:quit Space:toggle i:info d:defaults …
```
<!-- /screen -->

The same scan drives four other commands, for scripts and cron: a report
with an exit-status gate, an unattended refresh, a JSON interface for
setting values from a plan, and a cleaner for the options directory.

## Install

optique needs FreeBSD and a ports tree; building it needs `lang/rust`.

```sh
make build
make install                 # as root: /usr/local/bin and the man page
```

`PREFIX` and `DESTDIR` are honoured. optique is also in the FreeBSD ports
tree as `ports-mgmt/optique` (`pkg install optique`); see
[docs/port.md](docs/port.md) for keeping the port up to date.

## Quick start

Review and set the options of a package list's whole closure (for
synth, of everything installed), for the build you run:

```sh
# poudriere: the set "server" (add -j jail, -p tree as you build them)
optique -z server -f server.lst

# synth: the installed packages, with synth.ini's selected profile (or
# --synth=name); -f list replaces them with a list, as for the others
optique -s

# make on this host (/var/db/ports, /etc/make.conf), even where poudriere
# is installed; without poudriere.d, -l is implied
optique -l -f server.lst
```

The same flags select the build for every command:

```sh
# What is left to decide? Exit status 1 while anything is pending, 3 on
# an error
optique scan -z server -f server.lst

# The unattended refresh after a ports tree update: keep saved choices,
# take defaults for new options, drop removed ones
optique sync -z server -f server.lst
optique sync -s            # synth: everything installed, before upgrade-system
```

## The workflow

```text
┌───────────┐   ┌───────────┐   ┌───────────┐   ┌───────────┐   ┌───────────┐
│   update  │──▸│  scan the │──▸│  triage,  │──▸│   apply   │──▸│ poudriere │
│  the tree │   │  closure  │   │   decide  │   │   atomic  │   │    bulk   │
└───────────┘   └───────────┘   └───────────┘   └───────────┘   └───────────┘
                └───────────────── optique ─────────────────┘
                     or, unattended: optique sync -f list
```

After a ports tree update, some ports gain or lose options and new
dependencies appear. optique scans the closure, sorts the ports that need a
decision to the top (`?` never configured, `!` options changed since the
file was written, `✗` conflicting), lets you accept or edit each one with
the framework's own rules enforced, and writes everything when you apply.
`poudriere bulk` then finds every port configured and starts building at
once. For cron or CI, `sync` does the middle three steps without asking.

## Commands

| Command         | What it does                                             |
| :-------------- | :------------------------------------------------------- |
| `tui` (default) | one screen over the closure; nothing written until apply |
| `scan`          | report what needs a decision; exit 1 while anything does |
| `sync`          | refresh every options file without asking                |
| `decide`        | set values from a JSON plan, all or nothing              |
| `clean`         | remove options files of gone, unused or redundant ports  |

Every command takes the same global options after its name (`-z set`,
`-j jail`, `-p tree`, `-P dir`, `-O overlay`, `-o dir`, `-s profile`,
`-l`, `-f pkglist`, ...), and every one but the TUI can answer in JSON.

## Documentation

- [The TUI](docs/tui.md): the screen, markers, keys, applying, drafts
- [Commands](docs/commands.md): each subcommand, the global options, exit
  status
- [Machine interface](docs/machine-interface.md): JSON reports and
  `decide` plans, for scripts and agents
- [Configuration](docs/configuration.md): what optique reads, in which
  order, for poudriere and for synth
- [Dependency loops](docs/dependency-loops.md): how they are found and
  broken
- [How it works](docs/how-it-works.md): the query pipeline and the cache
- [The FreeBSD port](docs/port.md): updating `ports-mgmt/optique` for a
  release, and test-building it
- `optique(8)`, the man page: every flag, key, JSON key and exit status
  (`man optique`, or `mandoc man/optique.8` from the tree)
- [DESIGN.md](DESIGN.md): why optique is shaped the way it is

## Building and testing

The `Makefile` wraps cargo and runs under BSD and GNU make alike; `make`
alone lists its targets.

```sh
make check          # preflight: man page lint, one release everywhere
make build          # cargo build --release
make tests          # unit and hermetic CLI tests
make tests-live     # end to end against /usr/ports, on a FreeBSD host
make screens        # redraw the text screenshots in README.md and docs/
make check-port     # is the FreeBSD port (ports-mgmt/optique) up to date?
```

[CHECKLIST.md](CHECKLIST.md) lists what a change has to pass before it
lands.

## License

BSD-2-Clause; see [LICENSE](LICENSE).
