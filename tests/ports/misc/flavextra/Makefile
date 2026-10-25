PORTNAME=	flavextra
PORTVERSION=	1.0
CATEGORIES=	misc
MAINTAINER=	nobody@example.org
COMMENT=	A flavor with an option the default one lacks (multimedia/audacious-plugins)
WWW=		https://example.org/
NO_BUILD=	yes

FLAVORS=	qt gtk3
FLAVOR?=	${FLAVORS:[1]}
gtk3_PKGNAMESUFFIX=	-gtk3

OPTIONS_DEFINE=	A
OPTIONS_DEFAULT=	A

.if ${FLAVOR} == gtk3
OPTIONS_DEFINE+=	AOSD
.endif

.include <bsd.port.mk>
