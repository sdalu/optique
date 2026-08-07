# optique — BSD make (bmake) build/install driver around cargo.
# Deliberately trivial so GNU make handles it too; ?= and ${} are common syntax.

# Build where the sources are. bmake otherwise runs the recipes from
# /usr/obj${.CURDIR} as soon as that directory exists — and it does here, left
# behind by a ports build under ports/ — where cargo finds no Cargo.toml.
# GNU make ignores this: it defines no .CURDIR and never asks for .OBJDIR.
.OBJDIR: ${.CURDIR}

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
# `make lint MANDOCFLAGS="-T lint"` shows them anyway.
MANDOCFLAGS?=	-T lint -W warning

PROG=		optique
MAN8=		optique.8
RELEASE=	target/release/${PROG}

all: build

build:
	${CARGO} build --release

test:
	${CARGO} test

# -s strips the binary, 555 for the executable and 444 for the man page are the
# FreeBSD conventions; the man page is installed gzipped, as pkg does.
install:
	${INSTALL} -d ${DESTDIR}${BINDIR}
	${INSTALL} -d ${DESTDIR}${MANDIR}
	${INSTALL} -s -m 555 ${RELEASE} ${DESTDIR}${BINDIR}/${PROG}
	${GZIP} -cn ${MAN8} > ${DESTDIR}${MANDIR}/${MAN8}.gz
	chmod 444 ${DESTDIR}${MANDIR}/${MAN8}.gz

deinstall:
	rm -f ${DESTDIR}${BINDIR}/${PROG}
	rm -f ${DESTDIR}${MANDIR}/${MAN8}.gz

lint:
	${MANDOC} ${MANDOCFLAGS} ${MAN8}

clean:
	${CARGO} clean

.PHONY: all build test install deinstall lint clean
