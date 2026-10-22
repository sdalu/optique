# FreeBSD port skeleton for optique

`ports-mgmt/optique` is a ready-to-use cargo port for this repository. It is
kept in-tree so the packaging metadata stays in sync with `Cargo.toml` and
`Cargo.lock`. It builds the release tagged on
<https://github.com/sdalu/optique> (`v${DISTVERSION}`), and `distinfo` holds
the checksum of the tarball GitHub serves for that tag.

`make check-version` and `make check-crates` at the top of the checkout fail
when `DISTVERSION`, `CARGO_CRATES` or `distinfo` drift from `Cargo.toml` and
`Cargo.lock`.

## Using the skeleton

Either copy the directory into a ports tree:

```sh
cp -R ports/ports-mgmt/optique /usr/ports/ports-mgmt/optique
```

...or, better, expose `ports/` as a poudriere/ports overlay so the checkout
stays authoritative:

```sh
# one-off, from a ports-tree-shaped overlay
make -C /path/to/optique/ports/ports-mgmt/optique PORTSDIR=/usr/ports <target>

# poudriere overlay (13.0+)
poudriere ports -c -m null -M /path/to/optique/ports -p optique-overlay
poudriere bulk -j <jail> -p default -O optique-overlay ports-mgmt/optique
```

Read-only sanity checks work straight from the checkout and need no network:

```sh
cd ports/ports-mgmt/optique
make PORTSDIR=/usr/ports -V PKGNAME        # optique-0.1.0
make PORTSDIR=/usr/ports -V PLIST_FILES
make PORTSDIR=/usr/ports -V _DISTFILES | tr ' ' '\n' | wc -l
```

## Refreshing for a release

The port's copies of the release and the crate list are refreshed **before**
`make tag`, which refuses while `make check-version` or `make check-crates`
fails, and `distinfo` is finished **after** the tag is pushed, since only then
does GitHub serve the tarball. In `ports/ports-mgmt/optique`:

1. Bump `DISTVERSION` to `Cargo.toml`'s `version`.
2. If `Cargo.lock` changed, regenerate the crate list with the framework's own
   script and paste the result over the `CARGO_CRATES=` block:

   ```sh
   awk -f /usr/ports/Mk/Scripts/cargo-crates.awk ../../../Cargo.lock
   ```
3. Checksum the crates alone. Naming them as `DISTFILES` leaves out the
   release tarball, which does not exist yet; the `distinfo` written has no
   tarball line until step 4:

   ```sh
   make PORTSDIR=/usr/ports DISTFILES="$(make PORTSDIR=/usr/ports -V DISTFILES \
       | tr ' ' '\n' | grep '^rust/crates/' | tr '\n' ' ')" makesum
   ```

Commit, `make tag` at the top of the checkout, push the tag, then:

4. Checksum everything, which adds the GitHub tarball. It has to be GitHub's
   tarball: one made locally with `git archive` need not have the same bytes.

   ```sh
   make PORTSDIR=/usr/ports makesum
   ```
5. Re-run `make -V PKGNAME`/`-V _DISTFILES` and, if available, `portlint -AC`
   and `portclippy`, and commit `distinfo`.

## Test build

```sh
poudriere testport -j <jail> -p default -O optique-overlay ports-mgmt/optique
```

`testport` covers stage-qa, the plist (`PLIST_FILES` must match exactly what
lands in `${STAGEDIR}`) and the man page compression that turns
`optique.8` into `share/man/man8/optique.8.gz`.

For a quick host-side validation without poudriere:

```sh
make PORTSDIR=/usr/ports NO_DEPENDS=1 BATCH=1 \
    DISTDIR=/tmp/optique/distfiles WRKDIRPREFIX=/tmp/optique/wrk \
    stage check-plist stage-qa
```

`DISTDIR` and `WRKDIRPREFIX` make it runnable without root: the distfiles go
somewhere writable rather than into the shared `/usr/ports/distfiles`, and the
`work/` directory stays out of the checkout. `stage-qa` is named because it
otherwise runs only under `DEVELOPER=yes`.

`BATCH=1` matters for any scripted port build: without it the framework may
pop an interactive dialog4ports OPTIONS screen for a dependency (the tree's
lang/rust, for instance) and hang a non-interactive session. `NO_DEPENDS=1`
uses the installed rust toolchain instead of building the tree's.

## Notes

- `DISTVERSIONPREFIX=v` is used, as the tag carries a `v` prefix (`make tag`
  makes it so). The equivalent idiom `GH_TAGNAME=v${DISTVERSION}` is also
  acceptable, but do not set both.
- The port deliberately defines no `OPTIONS_DEFINE`. The man page is the only
  extra artefact, installed by `post-install` with `INSTALL_MAN`; the
  framework compresses it, hence the `.gz` suffix in `PLIST_FILES`.
- `CATEGORIES=ports-mgmt`: the port lives in `ports-mgmt/optique`, beside
  poudriere and synth.
