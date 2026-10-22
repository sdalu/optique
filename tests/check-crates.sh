#!/bin/sh
# check-crates.sh — does the port's CARGO_CRATES still name what Cargo.lock does?
# usage: check-crates.sh
set -eu

# The port cannot read Cargo.lock either (see check-version.sh), so it carries
# the crate list as CARGO_CRATES, and distinfo the checksum of each crate. The
# framework's own awk script is what `make cargo-crates` runs; asking it is the
# reading, and comparing its answer with the port's is the check. distinfo has
# one SIZE line per distfile: the crates plus the GitHub tarball.
#
# Exit status: 0 they agree, or there is no ports tree to ask; 1 they differ.

top=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
port=$top/ports/ports-mgmt/optique
portsdir=${PORTSDIR:-/usr/ports}
bsdmake=${BSDMAKE:-make}
awkf=$portsdir/Mk/Scripts/cargo-crates.awk

if [ ! -f "$awkf" ] || [ ! -f "$portsdir/Mk/bsd.port.mk" ]; then
    printf '  crates: unchecked (no ports tree at %s)\n' "$portsdir"
    exit 0
fi

tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT

awk -f "$awkf" "$top/Cargo.lock" \
    | sed -e 's/^CARGO_CRATES=//' -e 's/\\$//' | tr '\t' ' ' | tr -s ' ' '\n' \
    | grep . | sort > "$tmp/lock"
$bsdmake -C "$port" PORTSDIR="$portsdir" -V CARGO_CRATES \
    | tr ' ' '\n' | grep . | sort > "$tmp/port"
sed -n 's|^SIZE (rust/crates/\(.*\)\.crate) = .*|\1|p' "$port/distinfo" \
    | sort > "$tmp/dist"

bad=0
if ! cmp -s "$tmp/lock" "$tmp/port"; then
    printf '  crates: CARGO_CRATES differs from Cargo.lock (< lock, > port):\n'
    diff "$tmp/lock" "$tmp/port" | grep '^[<>]' | sed 's/^/    /' || :
    printf '          (see ports/README.md, "Refreshing for a release", steps 2-3)\n'
    bad=1
fi
if ! cmp -s "$tmp/port" "$tmp/dist"; then
    printf '  crates: distinfo differs from CARGO_CRATES (< port, > distinfo):\n'
    diff "$tmp/port" "$tmp/dist" | grep '^[<>]' | sed 's/^/    /' || :
    printf '          (ports/README.md, "Refreshing for a release", step 3)\n'
    bad=1
fi

exit "$bad"
