# FreeBSD port skeleton for optique

`sysutils/optique` is a ready-to-use cargo port for this repository. It is
kept in-tree so the packaging metadata stays in sync with `Cargo.toml` and
`Cargo.lock`, but it is **not yet complete**: `distinfo` is missing, because
the upstream release tarball does not exist until a `v0.1.0` tag is pushed to
<https://github.com/sdalu/optique>. Both `TODO` comments in the Makefile mark
that.

## Using the skeleton

Either copy the directory into a ports tree:

```sh
cp -R ports/sysutils/optique /usr/ports/sysutils/optique
```

...or, better, expose `ports/` as a poudriere/ports overlay so the checkout
stays authoritative:

```sh
# one-off, from a ports-tree-shaped overlay
make -C /path/to/optique/ports/sysutils/optique PORTSDIR=/usr/ports <target>

# poudriere overlay (13.0+)
poudriere ports -c -m null -M /path/to/optique/ports -p optique-overlay
poudriere bulk -j <jail> -p default -O optique-overlay sysutils/optique
```

Read-only sanity checks work straight from the checkout and need no network:

```sh
cd ports/sysutils/optique
make PORTSDIR=/usr/ports -V PKGNAME        # optique-0.1.0
make PORTSDIR=/usr/ports -V PLIST_FILES
make PORTSDIR=/usr/ports -V _DISTFILES | tr ' ' '\n' | wc -l
```

## Refreshing after a release

Once the `v0.1.0` (or later) tag is published upstream:

1. Bump `DISTVERSION` to match `Cargo.toml`'s `version`.
2. Regenerate the crate list from the committed lock file:

   ```sh
   make PORTSDIR=/usr/ports cargo-crates       # needs the distfile extracted
   ```

   Without a distfile you can produce byte-identical output offline with the
   framework's own script:

   ```sh
   awk -f /usr/ports/Mk/Scripts/cargo-crates.awk ../../../Cargo.lock
   ```

   Paste the result over the `CARGO_CRATES=` block. `make cargo-crates-merge`
   does the edit and the checksums in one step on a full ports tree.
3. Generate `distinfo` (fetches the GitHub tarball and all 108 crates):

   ```sh
   make PORTSDIR=/usr/ports makesum
   ```
4. Re-run `make -V PKGNAME`/`-V _DISTFILES` and, if available, `portlint -AC`
   and `portclippy`.

## Test build

```sh
poudriere testport -j <jail> -p default -O optique-overlay sysutils/optique
```

`testport` covers stage-qa, the plist (`PLIST_FILES` must match exactly what
lands in `${STAGEDIR}`) and the man page compression that turns
`optique.8` into `share/man/man8/optique.8.gz`.

## Notes

- `GH_TAGNAME=v${DISTVERSION}` is used, as the tag is expected to carry a `v`
  prefix. The equivalent idiom `DISTVERSIONPREFIX=v` yields a shorter
  `DISTNAME`; either is acceptable, but do not set both.
- The port deliberately defines no `OPTIONS_DEFINE`. The man page is the only
  extra artefact, installed by `post-install` with `INSTALL_MAN`; the
  framework compresses it, hence the `.gz` suffix in `PLIST_FILES`.
- `CATEGORIES=sysutils ports-mgmt` means the canonical location is
  `sysutils/optique`, with `ports-mgmt` as a secondary category only.
