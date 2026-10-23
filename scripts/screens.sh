#!/bin/sh
# Regenerate the text screenshots embedded in the docs.
#
# Every block between a `<!-- screen NAME -->` line and the next
# `<!-- /screen -->` line in docs/*.md and README.md is replaced by a fresh
# capture of the screen NAME, drawn by the real TUI through `tui --drive`
# (the headless driver: same keymap, same drawing code). The captures come
# from the www/nginx closure of the host's /usr/ports, staged into a
# throwaway options dir and cache, so nothing of the host's configuration
# is read or written; they change with the ports tree, which is the point.
#
# Usage: scripts/screens.sh [optique-binary]   (default: target/release/optique)

set -eu

bin=$(realpath "${1:-target/release/optique}" 2>/dev/null || echo "${1:-target/release/optique}")
cols=78
rows=24

[ -x "$bin" ] || { echo "screens: no $bin; run make build first" >&2; exit 1; }
[ -f /usr/ports/Mk/bsd.port.mk ] || { echo "screens: needs /usr/ports" >&2; exit 1; }

# A fixed directory, not mktemp's: optique shows the options dir absolute,
# and the captures must not change with a random name.
work=/tmp/optique-screens
rm -rf "$work"
mkdir -p "$work"
trap 'rm -rf "$work"' EXIT
export XDG_CACHE_HOME="$work/cache"
# Relative to $work, where the captures run; shown as $work/poudriere.d/...
opt=poudriere.d/server-options
here=$(pwd)

optique() {
	# The action first, then /usr/ports, whatever the host's own poudriere
	# trees point at.
	action=$1
	shift
	"$bin" "$action" -P /usr/ports "$@"
}

# The fixture: the closure configured, then one file left behind by a tree
# update (stale), one never written (unconfigured).
cd "$work"
optique sync -q -o "$opt" www/nginx >/dev/null 2>&1
sed -i '' -e 's/^_FILE_COMPLETE_OPTIONS_LIST=.*/_FILE_COMPLETE_OPTIONS_LIST=DOCS OLDOPT/' \
	-e '$a\
OPTIONS_FILE_SET+=OLDOPT' "$opt/ports-mgmt_pkg/options"
rm -r "$opt/devel_gmake"

# capture NAME KEYS...: start the TUI on the fixture, press the keys, dump.
capture() {
	name=$1
	shift
	{
		printf 'resize %sx%s\n' "$cols" "$rows"
		for k in "$@"; do printf '%s\n' "$k"; done
		printf 'dump\nquit\n'
	} | optique tui --drive -o "$opt" www/nginx 2>/dev/null |
		awk '/^screen [0-9]+x[0-9]+$/ { on = 1; next } /^end$/ { on = 0 } on' \
			>"$work/$name.txt"
	[ -s "$work/$name.txt" ] || { echo "screens: $name came out empty" >&2; exit 1; }
}

# Main view: problems first (one unconfigured, one stale), nginx, last in
# the list, under edit with one option toggled.
capture main 'key G' 'key enter' 'key j' 'key space' 'settle'
# The option pane, the cursor on an option: its description below.
capture options 'key G' 'key enter' 'key j' 'key j' 'key j'
# Why is this port here: the dependency chain up to a root.
capture why 'key /' 'keys libunistring' 'key enter' 'key r'
# The port's own description, above its options: at start only from 30
# rows, so on these 24 V shows it.
capture about 'key /' 'keys pcre2' 'key enter' 'key V'
# The apply preview: every file that would change, as a diff.
capture apply 'key G' 'key enter' 'key j' 'key space' 'key esc' 'settle' 'key a'

# Splice every capture into the blocks that name it.
cd "$here"
for doc in README.md docs/*.md; do
	[ -f "$doc" ] || continue
	grep -q '^<!-- screen ' "$doc" || continue
	awk -v dir="$work" '
		/^<!-- screen [a-z]+ -->$/ {
			print
			name = $3
			print "```text"
			while ((getline line < (dir "/" name ".txt")) > 0) print line
			close(dir "/" name ".txt")
			print "```"
			skip = 1
			next
		}
		/^<!-- \/screen -->$/ { skip = 0 }
		!skip
	' "$doc" >"$work/doc.tmp"
	if ! cmp -s "$work/doc.tmp" "$doc"; then
		cat "$work/doc.tmp" >"$doc"
		echo "screens: updated $doc"
	fi
done
