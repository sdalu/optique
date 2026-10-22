#!/bin/sh
# check-version.sh — does every reader of the release agree with Cargo.toml?
# usage: check-version.sh <release>     (the Makefile passes what it parsed)
set -eu

# Cargo.toml's [package] version is the release; nothing generates it and
# everything else reads it. Four other places hold the same number, and each
# is asked in its own language rather than re-parsed here:
#
#   cargo          what `optique --version` reports (clap reads
#                  CARGO_PKG_VERSION), and what the Makefile's sed must match
#   Cargo.lock     the package entry a --locked or --frozen build checks;
#                  cargo rewrites it on the next build, so a bump committed
#                  without one leaves it behind
#   the port       ports/ports-mgmt/optique's DISTVERSION, which cannot read
#                  Cargo.toml: the port is copied into a ports tree, away
#                  from this checkout, and is evaluated before any extract
#   the tags       scripts/checktag.sh
#
# A reader whose tool is missing is reported as unchecked, never passed.
#
# Exit status: 0 every reader that could be asked agrees; 1 one disagrees.

want=${1:?usage: check-version.sh <release>}
top=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
bad=0

say() {
    printf '  version: %s\n' "$*"
}

pkgid=$(cd "$top" && cargo pkgid --offline 2>/dev/null) || pkgid=''
got=${pkgid##*[#@]}
if [ -z "$pkgid" ]; then
    say 'cargo: unchecked (cargo pkgid failed)'
elif [ "$got" != "$want" ]; then
    say "the Makefile parsed $want from Cargo.toml, cargo reads $got"
    bad=1
fi

got=$(awk '
    $0 == "[[package]]" { p = 0 }
    $0 == "name = \"optique\"" { p = 1 }
    p && /^version = / { gsub(/^version = "|"$/, ""); print; exit }
' "$top/Cargo.lock")
if [ "$got" != "$want" ]; then
    say "Cargo.toml says $want, Cargo.lock ${got:-has no optique entry}"
    say '         (run cargo build and commit Cargo.lock)'
    bad=1
fi

portsdir=${PORTSDIR:-/usr/ports}
bsdmake=${BSDMAKE:-make}
if [ ! -f "$portsdir/Mk/bsd.port.mk" ]; then
    say "port: unchecked (no ports tree at $portsdir)"
else
    got=$($bsdmake -C "$top/ports/ports-mgmt/optique" PORTSDIR="$portsdir" \
        -V DISTVERSION 2>/dev/null) || got=''
    if [ -z "$got" ]; then
        say "port: unchecked ($bsdmake -V DISTVERSION answered nothing)"
    elif [ "$got" != "$want" ]; then
        say "Cargo.toml says $want, the port's DISTVERSION $got"
        bad=1
    fi
fi

sh "$top/scripts/checktag.sh" "$want" || bad=1

exit "$bad"
