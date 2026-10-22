#!/bin/sh
# check-port.sh — is the FreeBSD port, ports-mgmt/optique, up to date?
# usage: check-port.sh            (STRICT=1: exit 1 when it is not)
set -eu

# The port lives in the FreeBSD ports tree and is updated there after a
# release, by a ports commit that needs the tarball GitHub serves for the
# tag: it lags a release by design, so this is a report, not a gate, and
# `make check` and `make tag` do not run it. It reads the port in
# $PORTSDIR (the local checkout of the ports tree) and says:
#
#   the release   its DISTVERSION against the latest v* tag
#   the crates    its CARGO_CRATES (Makefile.crates) against Cargo.lock at
#                 the tag it builds, as `make cargo-crates` would write them
#   the checksums its distinfo against CARGO_CRATES
#   what is next  whether Cargo.lock has moved since that tag, so the next
#                 update needs `make cargo-crates` too
#
# Exit status: 0, unless STRICT=1 and something above disagrees.

top=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd -P)
portsdir=${PORTSDIR:-/usr/ports}
port=$portsdir/ports-mgmt/optique
bsdmake=${BSDMAKE:-make}
awkf=$portsdir/Mk/Scripts/cargo-crates.awk
lag=0

say() {
    printf '  port: %s\n' "$*"
}

if [ ! -f "$port/Makefile" ] || [ ! -f "$awkf" ]; then
    say "unchecked (no ports-mgmt/optique in $portsdir)"
    exit 0
fi

have=$($bsdmake -C "$port" PORTSDIR="$portsdir" -V DISTVERSION 2>/dev/null) || have=''
latest=$(git -C "$top" tag -l 'v*' --sort=-v:refname | head -n 1)
latest=${latest#v}
if [ -z "$have" ]; then
    say "unchecked ($bsdmake -V DISTVERSION answered nothing)"
    exit 0
elif [ -z "$latest" ]; then
    say "at $have; no v* tag here to compare with"
elif [ "$have" != "$latest" ]; then
    say "lags: at $have, the latest release is $latest (update DISTVERSION,"
    say "      make makesum, make cargo-crates > Makefile.crates; docs/port.md)"
    lag=1
else
    say "at $have, the latest release"
fi

tmp=$(mktemp -d)
trap 'rm -rf -- "$tmp"' EXIT

crates() {
    awk -f "$awkf" | sed -e 's/^CARGO_CRATES=//' -e 's/\\$//' | tr '\t' ' ' \
        | tr -s ' ' '\n' | grep . | sort
}

$bsdmake -C "$port" PORTSDIR="$portsdir" -V CARGO_CRATES \
    | tr ' ' '\n' | grep . | sort > "$tmp/port"
sed -n 's|^SIZE (rust/crates/\(.*\)\.crate) = .*|\1|p' "$port/distinfo" \
    | sort > "$tmp/dist"

# The crates are judged against the release the port builds, not HEAD.
if git -C "$top" show "v$have:Cargo.lock" > "$tmp/Cargo.lock" 2>/dev/null; then
    crates < "$tmp/Cargo.lock" > "$tmp/lock"
    if ! cmp -s "$tmp/lock" "$tmp/port"; then
        say "Makefile.crates differs from Cargo.lock at v$have (< lock, > port):"
        diff "$tmp/lock" "$tmp/port" | grep '^[<>]' | sed 's/^/    /' || :
        lag=1
    fi
    crates < "$top/Cargo.lock" > "$tmp/head"
    if ! cmp -s "$tmp/lock" "$tmp/head"; then
        say "Cargo.lock has changed since v$have: the next update needs"
        say "      make cargo-crates as well"
    fi
else
    say "crates: unchecked (no tag v$have here)"
fi
if ! cmp -s "$tmp/port" "$tmp/dist"; then
    say "distinfo differs from CARGO_CRATES (< port, > distinfo):"
    diff "$tmp/port" "$tmp/dist" | grep '^[<>]' | sed 's/^/    /' || :
    lag=1
fi

[ "$lag" -eq 0 ] && say "up to date"
if [ "${STRICT:-}" = 1 ]; then
    exit "$lag"
fi
exit 0
