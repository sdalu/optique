# The FreeBSD port

optique is packaged in the FreeBSD ports tree as
[`ports-mgmt/optique`](https://cgit.freebsd.org/ports/tree/ports-mgmt/optique),
beside poudriere and synth; `pkg install optique` installs it. The port
builds the release tagged on <https://github.com/sdalu/optique>
(`v${DISTVERSION}`); its crate list is `Makefile.crates`, and `distinfo`
holds the checksums of the crates and of the tarball GitHub serves for the
tag.

The port is updated in the ports tree **after** a release, since only then
does GitHub serve the tarball: it lags a release by design, so nothing here
gates on it. `make check-port` reports how it stands against this checkout,
reading the port in `PORTSDIR` (default `/usr/ports`):

```sh
make check-port              # report only
make check-port STRICT=1     # exit 1 when the port lags
```

It compares the port's `DISTVERSION` with the latest `v*` tag, its
`CARGO_CRATES` with `Cargo.lock` at the tag it builds, and its `distinfo`
with `CARGO_CRATES`; and says when `Cargo.lock` has changed since that tag,
so that the next update needs a new crate list as well.

## Updating the port for a release

After `make tag` and `git push origin v<release>`, in a ports tree:

```sh
cd /usr/ports/ports-mgmt/optique
# 1. DISTVERSION= <release> in the Makefile
make makesum                           # 2. checksums, the GitHub tarball included
make extract                           # 3. the release's sources, with its Cargo.lock
make cargo-crates > Makefile.crates    # 4. the crate list, from that Cargo.lock
make makesum                           # 5. again, for the crates step 4 added
make clean
portlint -AC                           # 6. and portclippy, if installed
```

`make cargo-crates` reads `Cargo.lock` from the extracted release; step 3
extracts it on its own so that the extraction's messages do not end up in
`Makefile.crates`. The tarball checksummed in step 2 has to be the one
GitHub serves: one made locally with `git archive` need not have the same
bytes. Then test-build it (below), commit to the ports tree, and run
`make check-port` here, which should now say `up to date`.

## Test build

An unreleased change to the port is tried from a copy laid over the ports
tree as a poudriere overlay (poudriere 3.4.0 or later), so the ports tree
itself stays untouched:

```sh
mkdir -p ~/optique-overlay/ports-mgmt
cp -R /usr/ports/ports-mgmt/optique ~/optique-overlay/ports-mgmt/
# edit ~/optique-overlay/ports-mgmt/optique, then:
poudriere ports -c -m null -M ~/optique-overlay -p optiqueoverlay
poudriere testport -j <jail> -p default -O optiqueoverlay ports-mgmt/optique
# poudriere options has no -O; optique configures the overlay's ports
optique tui -O optiqueoverlay ports-mgmt/optique
```

`testport` covers stage-qa, the plist (`PLIST_FILES` must match exactly what
lands in `${STAGEDIR}`) and the man page compression that turns
`optique.8` into `share/man/man8/optique.8.gz`.

For a quick host-side validation without poudriere, from the port's
directory:

```sh
make NO_DEPENDS=1 BATCH=1 \
    DISTDIR=/tmp/optique/distfiles WRKDIRPREFIX=/tmp/optique/wrk \
    stage check-plist stage-qa
```

`DISTDIR` and `WRKDIRPREFIX` make it runnable without root: the distfiles go
somewhere writable rather than into the shared `/usr/ports/distfiles`, and
the `work/` directory stays out of the tree. `stage-qa` is named because it
otherwise runs only under `DEVELOPER=yes`. `BATCH=1` keeps the framework
from popping an interactive OPTIONS dialog for a dependency (lang/rust, for
instance) and hanging a non-interactive session; `NO_DEPENDS=1` uses the
installed rust toolchain instead of building the tree's.

## Notes

- `DISTVERSIONPREFIX=v`: the tag carries a `v` prefix (`make tag` makes it
  so). The equivalent `GH_TAGNAME=v${DISTVERSION}` would do as well, but
  not both.
- `LICENSE=BSD2CLAUSE`; from 0.3.0, the first release that ships `LICENSE`,
  the port can also set `LICENSE_FILE=${WRKSRC}/LICENSE`.
- The port defines no `OPTIONS_DEFINE`. The man page is the only extra
  artefact, installed by `post-install` with `INSTALL_MAN`; the framework
  compresses it, hence the `.gz` in `PLIST_FILES`.
- poudriere tree names may contain neither `.` nor `-`, hence
  `optiqueoverlay` above.
