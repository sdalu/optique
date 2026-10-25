#!/bin/sh
# Time optique against `poudriere options -C` on one closure (docs/benchmark.md).
#
# poudriere need not be installed: what `poudriere options -C -z <set>
# <origin>...` runs is reproduced from its options.sh, setup_makeconf and
# setup_ports_env: each listed origin is resolved through MOVED, then gets
# `make config-conditional` and `make config-recursive`, under a make.conf
# holding the set's layer (empty here) and the ports_env.sh values with
# UID and GID 0, LOCALBASE=/nonexistent for the recursive run.
# dialog4ports is replaced by a stub that keeps the options shown, a user
# pressing Enter, and counts the dialogs. optique is given the same list
# and reads the same empty layer through a throwaway POUDRIERE_ETC.
# Nothing of the host's configuration is read or written.
#
# Usage: scripts/bench.sh [-b optique] [-P portsdir] [-f list] [-1] [-k] [-u "rev ..."] [origin...]
#   -f   origins from a package list (one per line, `#` comments), as -f
#        of poudriere and optique; repeatable
#   -1   also time a cold optique scan with one job
#   -k   keep the work dir (both options dirs, the dialog log), and say where
#   -u   on a git tree, also time a scan after moving from each rev to HEAD
#        (a shared clone in the work dir, the cache warmed at rev; the
#        checkout needs some 1.5 GB under TMPDIR, more than a small /tmp)

set -eu

bin=target/release/optique ports=/usr/ports single= revs= lists= keep=
while getopts b:P:f:1ku: opt; do
	case $opt in
	b) bin=$OPTARG ;;
	P) ports=$OPTARG ;;
	f) lists="$lists $(realpath "$OPTARG")" ;;
	1) single=1 ;;
	k) keep=1 ;;
	u) revs=$OPTARG ;;
	*) sed -n 's/^# Usage: /usage: /p' "$0" >&2; exit 2 ;;
	esac
done
shift $((OPTIND - 1))
bin=$(realpath "$bin")
ports=$(realpath "$ports")
[ -x "$bin" ] || { echo "bench: no $bin; run make build first" >&2; exit 1; }
[ -f "$ports/Mk/bsd.port.mk" ] || { echo "bench: no ports tree at $ports" >&2; exit 1; }

work=$(mktemp -d -t optique-bench)
if [ -n "$keep" ]; then
	trap 'echo; echo "work dir kept: $work"' EXIT
else
	trap 'rm -rf "$work"' EXIT
fi
path=/sbin:/bin:/usr/sbin:/usr/bin:/usr/local/sbin:/usr/local/bin

# The listed origins, read as poudriere's _listed_ports reads them: each
# line's first word, blank and `#` lines skipped, a missing last newline
# no matter, slashes trimmed, duplicates dropped (`sort -u`).
{
	for o in "$@"; do echo "$o"; done
	for l in $lists; do awk '$1 != "" && $1 !~ /^#/ { print $1 }' "$l"; done
} | sed -e 's,^/,,' -e 's,/$,,' | sort -u > "$work/listed"
[ -s "$work/listed" ] || { echo "bench: no origin given" >&2; exit 2; }
set -- $(cat "$work/listed")

# An origin no longer in the tree follows MOVED, chain and all, as
# poudriere's listed_ports does; a removed one is left out.
resolve() {
	o=${1%@*} f=
	case $1 in *@*) f=@${1#*@} ;; esac
	hops=0
	while [ ! -f "$ports/$o/Makefile" ] && [ $hops -lt 20 ]; do
		o=$(grep "^$o|" "$ports/MOVED" | tail -1 | cut -d'|' -f2)
		[ -n "$o" ] || { echo "bench: $1 was removed, left out" >&2; return; }
		hops=$((hops + 1))
	done
	echo "$o$f"
}

# The dialog: keep what is shown (PORT_OPTIONS, the defaults for a new
# port), and log the port.
cat > "$work/d4p" <<'EOF'
#!/bin/sh
[ "$1" = "-v" ] && exit 0
echo "$PKGNAME" >> "$D4P_LOG"
echo $PORT_OPTIONS >&2
EOF
chmod +x "$work/d4p"

# poudriere's make.conf for the set: the (empty) layer, then setup_ports_env.
conf=$work/poudriere-make.conf
env -i PATH=$path SCRIPTSDIR="$ports/Mk/Scripts" PORTSDIR="$ports" \
    MAKE=/usr/bin/make MAKE_OBJDIR_CHECK_WRITABLE=0 \
    /bin/sh "$ports/Mk/Scripts/ports_env.sh" |
    grep '^export [^;&]*' | sed -e 's,^export ,,' -e 's,=",=,' -e 's,"$,,' > "$conf.ports_env"
printf '.include "%s"\nGID=0\nUID=0\n' "$conf.ports_env" > "$conf"
resolved=$(for o in "$@"; do resolve "$o"; done)

# One `poudriere options -C` over the resolved origins into options dir $1.
# make gets a TMPDIR of its own: Uses/cargo.mk otherwise moves TMPDIR, which
# bmake writes its command scripts to, into the port's WRKDIR, and a work/
# left there by a build as root stops a run as anyone else (poudriere runs
# as root). A failing make stops that origin's whole walk, as it would
# poudriere's: each is logged, to be reported.
poudriere_options() {
	mkdir -p "$1" "$work/tmp"
	: > "$work/dialogs"
	: > "$work/failed"
	for o in $resolved; do
		flavor=
		case $o in *@*) flavor=${o#*@} ;; esac
		for target in config-conditional config-recursive; do
			lb=
			[ $target = config-recursive ] && lb=LOCALBASE=/nonexistent
			env -i PATH=$path HOME="$HOME" USER="${USER:-}" D4P_LOG="$work/dialogs" \
			    TMPDIR="$work/tmp" \
			    __MAKE_CONF="$conf" LOCALBASE=/usr/local PREFIX=/usr/local \
			    ${flavor:+FLAVOR=$flavor} \
			    make PORT_DBDIR="$1" PORTSDIR="$ports" DIALOG4PORTS="$work/d4p" \
			    PKG_BIN=/usr/local/sbin/pkg-static $lb \
			    -C "$ports/${o%@*}" $target < /dev/null > "$work/make.out" 2>&1 ||
			    echo "$o $target: $(grep -m1 -v '^===>' "$work/make.out")" >> "$work/failed"
		done
	done
}

# Wall-clock seconds a command takes, to a tenth; its output in $work/out.
seconds() {
	t0=$(date +%s.%N)
	"$@" > "$work/out" 2>&1 || true
	t1=$(date +%s.%N)
	printf '%.1f\n' "$(echo "$t1 - $t0" | bc)"
}

mkdir -p "$work/etc/poudriere.d"
: > "$work/etc/poudriere.d/bench-make.conf"
optique() {
	tree=$1
	shift
	env XDG_CACHE_HOME="$work/cache" POUDRIERE_ETC="$work/etc" \
	    "$bin" "$@" -z bench -P "$tree" -f "$work/listed"
}
# The `N queried, M cached` part of a scan's summary.
queried() {
	sed -n 's/.*· \([0-9]* queried, [0-9]* cached\).*/\1/p' "$work/out"
}
count() {
	ls "$1" | wc -l | tr -d ' '
}

rev=$(git -c safe.directory='*' -C "$ports" rev-parse --short=12 HEAD 2>/dev/null || echo 'no git')
echo "ports tree   $ports ($rev)"
echo "host         $(sysctl -n hw.model), $(sysctl -n hw.ncpu) threads, $(uname -r)"
echo "origins      $# listed"
echo

s=$(seconds poudriere_options "$work/poudriere")
dialogs=$(wc -l < "$work/dialogs" | tr -d ' ')
echo "poudriere options -C, empty options dir   ${s}s, $dialogs dialogs, $(count "$work/poudriere") files"
[ -s "$work/failed" ] && sed 's/^/  failed: /' "$work/failed"
s=$(seconds poudriere_options "$work/poudriere")
dialogs=$(wc -l < "$work/dialogs" | tr -d ' ')
echo "poudriere options -C, all configured      ${s}s, $dialogs dialogs"
[ -s "$work/failed" ] && sed 's/^/  failed: /' "$work/failed"

s=$(seconds optique "$ports" sync -o "$work/optique" --no-cache)
echo "optique sync, empty options dir           ${s}s, $(count "$work/optique") files"
s=$(seconds optique "$ports" scan -o "$work/poudriere" --no-cache)
echo "optique scan, cold                        ${s}s, $(queried)"
s=$(seconds optique "$ports" scan -o "$work/poudriere" --no-cache --no-framework-probe)
echo "optique scan, cold, no framework probe    ${s}s, $(queried)"
if [ -n "$single" ]; then
	s=$(seconds optique "$ports" scan -o "$work/poudriere" --no-cache -J 1)
	echo "optique scan, cold, -J 1                  ${s}s, $(queried)"
fi
optique "$ports" scan -o "$work/poudriere" > /dev/null 2>&1 || true
s=$(seconds optique "$ports" scan -o "$work/poudriere")
echo "optique scan, warm                        ${s}s, $(queried)"

# An option edit in make.conf: poudriere's walk is the same as any check
# (config-conditional looks for new options only); optique re-queries the
# ports the edit can reach.
echo 'OPTIONS_UNSET+=DOCS' > "$work/etc/poudriere.d/bench-make.conf"
s=$(seconds optique "$ports" scan -o "$work/poudriere")
echo "optique scan, make.conf OPTIONS_UNSET+=DOCS ${s}s, $(queried)"
: > "$work/etc/poudriere.d/bench-make.conf"

# Where the two disagree: options directories, then recorded values.
echo
ls "$work/poudriere" > "$work/p.ls"
ls "$work/optique" > "$work/o.ls"
comm -23 "$work/p.ls" "$work/o.ls" | sed 's/^/poudriere only: /'
comm -13 "$work/p.ls" "$work/o.ls" | sed 's/^/optique only:   /'
for d in $(comm -12 "$work/p.ls" "$work/o.ls"); do
	for side in poudriere optique; do
		[ -f "$work/$side/$d/options" ] || echo "no options file: $side $d"
	done
	[ -f "$work/poudriere/$d/options" ] && [ -f "$work/optique/$d/options" ] || continue
	grep -h '^OPTIONS_FILE_' "$work/poudriere/$d/options" | sort > "$work/p.v"
	grep -h '^OPTIONS_FILE_' "$work/optique/$d/options" | sort > "$work/o.v"
	cmp -s "$work/p.v" "$work/o.v" ||
	    echo "values differ: $d: poudriere has" $(comm -23 "$work/p.v" "$work/o.v")
done

[ -n "$revs" ] || exit 0
echo
git -c safe.directory='*' clone -q --shared --no-checkout "$ports" "$work/tree"
head=$(git -C "$work/tree" rev-parse HEAD)
for rev in $revs; do
	git -C "$work/tree" checkout -q "$rev"
	n=$(git -C "$work/tree" rev-list --count "$rev..$head")
	p=$(git -C "$work/tree" diff-tree -r --name-only "$rev" "$head" | wc -l | tr -d ' ')
	rm -rf "$work/cache"
	optique "$work/tree" scan -o "$work/poudriere" > /dev/null 2>&1 || true
	git -C "$work/tree" checkout -q "$head"
	s=$(seconds optique "$work/tree" scan -o "$work/poudriere")
	echo "optique scan after $n commits ($p paths)   ${s}s, $(queried)"
done
