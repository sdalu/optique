PORTNAME=	flavfile
PORTVERSION=	1.0
CATEGORIES=	misc
MAINTAINER=	nobody@example.org
COMMENT=	Flavors reading options files of their own (devel/git)
WWW=		https://example.org/
NO_BUILD=	yes

FLAVORS=	default lite
FLAVOR?=	${FLAVORS:[1]}
lite_PKGNAMESUFFIX=	-lite

OPTIONS_DEFINE=	A X11
OPTIONS_DEFAULT=	A X11

.if ${FLAVOR} == lite
OPTIONS_EXCLUDE=	X11
.endif

.if !empty(FLAVOR) && ${FLAVOR:U} != default
OPTIONS_FILE=	${PORT_DBDIR}/${OPTIONS_NAME}/${FLAVOR}-options
.endif

.include <bsd.port.mk>
