# optique -- build, check and install driver around cargo.
#
# Runs unchanged under BSD make and GNU make: no conditionals, no GNU-only
# functions; ?=, != and ${} are common syntax. `make` alone prints the help.

# Build where the sources are. bmake otherwise runs the recipes from
# /usr/obj${.CURDIR} as soon as that directory exists — and it does here, left
# behind by a ports build under ports/ — where cargo finds no Cargo.toml.
# GNU make ignores this: it defines no .CURDIR and never asks for .OBJDIR.
.OBJDIR: ${.CURDIR}

NAME=		optique

# The release lives in Cargo.toml and nowhere else that is not checked
# against it: check-version asks cargo, Cargo.lock, the port and the tags.
VERSION!=	sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n 1

PREFIX?=	/usr/local
DESTDIR?=
BINDIR?=	${PREFIX}/bin
MANDIR?=	${PREFIX}/share/man/man8

CARGO?=		cargo
INSTALL?=	install
GZIP?=		gzip
MANDOC?=	mandoc
# Gate on warnings and above. The STYLE messages below that level are about
# this host, not the page: "referenced manual not found" only says the ports
# providing poudriere(8), synth(1) or portconfig(1) are not installed here.
# `make check-man MANDOCFLAGS="-T lint"` shows them anyway.
MANDOCFLAGS?=	-T lint -W warning
# The ports tree the port skeleton is evaluated against by check-version and
# check-crates; without one they report themselves unchecked.
PORTSDIR?=	/usr/ports
# The port is a ports-framework Makefile and needs BSD make, whichever make
# runs this one.
BSDMAKE?=	make
YES?=

MAN8=		optique.8
MANSRC=		man/${MAN8}
RELEASE=	target/release/${NAME}

help:						## show this help (the default)
	@echo '${NAME} -- fast FreeBSD ports options configurator for poudriere and synth'
	@echo ''
	@echo 'Targets:'
	@awk -F':.*## ' '/^[a-z][a-z0-9-]*:.*## /{ \
	    pre = sprintf("  %-16s ", $$1); n = split($$2, w, / /); line = ""; \
	    for (i = 1; i <= n; i++) { \
	        cand = (line == "" ? w[i] : line " " w[i]); \
	        if (length(pre) + length(cand) > 80 && line != "") { \
	            print pre line; pre = "                   "; line = w[i]; \
	        } else line = cand; \
	    } \
	    if (line != "") print pre line; \
	}' Makefile
	@echo ''
	@echo 'Variables (current value):'
	@printf '  %-16s %s\n' \
	    PREFIX      '${PREFIX}' \
	    DESTDIR     '${DESTDIR}  (staging prefix for packaging)' \
	    CARGO       '${CARGO}' \
	    MANDOC      '${MANDOC}' \
	    MANDOCFLAGS '${MANDOCFLAGS}' \
	    PORTSDIR    '${PORTSDIR}  (ports tree check-version and check-crates use)' \
	    BSDMAKE     '${BSDMAKE}  (BSD make, for the port skeleton)' \
	    YES         '${YES}  (1 answers yes to `make tag`)'
	@echo ''
	@echo 'This Makefile works with both GNU make and BSD make.'

# --- preflight: runs none of optique's code --------------------------------

check: check-man check-version check-crates	## preflight: this tree is fit to build and release

check-man:					## the man page lints clean (mandoc, warnings and above)
	${MANDOC} ${MANDOCFLAGS} ${MANSRC}

check-version:					## Cargo.toml, cargo, Cargo.lock, the port and the tags name one release
	@PORTSDIR='${PORTSDIR}' BSDMAKE='${BSDMAKE}' sh tests/check-version.sh '${VERSION}'

check-crates:					## the port's CARGO_CRATES and distinfo match Cargo.lock
	@PORTSDIR='${PORTSDIR}' BSDMAKE='${BSDMAKE}' sh tests/check-crates.sh

# --- building and the suite ------------------------------------------------

build:						## build the release binary (cargo build --release)
	${CARGO} build --release

tests:						## build and run the unit and hermetic CLI tests (cargo test)
	${CARGO} test

tests-live:					## the live tests against /usr/ports on a FreeBSD host; tests does not run them
	${CARGO} test -- --ignored

# --- installing ------------------------------------------------------------

# -s strips the binary, 555 for the executable and 444 for the man page are the
# FreeBSD conventions; the man page is installed gzipped, as pkg does.
install:					## install the binary and the man page under PREFIX (needs build)
	${INSTALL} -d ${DESTDIR}${BINDIR}
	${INSTALL} -d ${DESTDIR}${MANDIR}
	${INSTALL} -s -m 555 ${RELEASE} ${DESTDIR}${BINDIR}/${NAME}
	${GZIP} -cn ${MANSRC} > ${DESTDIR}${MANDIR}/${MAN8}.gz
	chmod 444 ${DESTDIR}${MANDIR}/${MAN8}.gz

uninstall:					## remove what install put down
	rm -f ${DESTDIR}${BINDIR}/${NAME}
	rm -f ${DESTDIR}${MANDIR}/${MAN8}.gz

# --- the release -----------------------------------------------------------

# Bare, so a script can use it:  v=`make -s version`
version:					## print the release, read from Cargo.toml
	@echo '${VERSION}'

tag:						## tag v<release> from Cargo.toml, after check and tests; pushes nothing
	@if [ -n "`git status --porcelain --untracked-files=no`" ]; then \
	    echo 'make: uncommitted changes; commit them before tagging' >&2; \
	    exit 1; fi
	@if git rev-parse -q --verify 'refs/tags/v${VERSION}' >/dev/null; then \
	    echo 'make: v${VERSION} exists already; bump Cargo.toml first' >&2; \
	    exit 1; fi
	@if [ "${YES}" != 1 ]; then \
	    printf 'tag v%s at %s? (check and tests run first) [y/N] ' \
		'${VERSION}' "`git rev-parse --short HEAD`"; \
	    read -r ans || ans=; \
	    case "$$ans" in \
		y|Y|yes|YES) ;; \
		*) echo 'make: not tagged'; exit 1 ;; \
	    esac; \
	fi
	@${MAKE} check && ${MAKE} tests
	git tag -a -m '${NAME} ${VERSION}' 'v${VERSION}'
	@sh scripts/checktag.sh '${VERSION}'
	@echo 'tagged v${VERSION} -- push it with: git push origin v${VERSION}'

# --- housekeeping ----------------------------------------------------------

clean:						## remove target/ (cargo clean)
	${CARGO} clean

.PHONY: help check check-man check-version check-crates build tests \
	tests-live install uninstall version tag clean
