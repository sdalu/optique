PORTNAME=	flav
PORTVERSION=	1.0
CATEGORIES=	misc
MAINTAINER=	nobody@example.org
COMMENT=	Three flavors sharing one options file (editors/emacs)
WWW=		https://example.org/
NO_BUILD=	yes

FLAVORS=	full nox pgtk
FLAVOR?=	${FLAVORS:[1]}
nox_PKGNAMESUFFIX=	-nox
pgtk_PKGNAMESUFFIX=	-pgtk

OPTIONS_DEFINE=	A X11 Y
OPTIONS_DEFAULT=	A X11 Y

.if ${FLAVOR} == nox
OPTIONS_EXCLUDE=	X11 Y
.elif ${FLAVOR} == pgtk
OPTIONS_EXCLUDE=	X11
.endif

.include <bsd.port.mk>
