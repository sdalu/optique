PORTNAME=	model-implied-prevents
PORTVERSION=	1.0
CATEGORIES=	misc
MAINTAINER=	nobody@example.org
COMMENT=	An excluded option implied, with a PREVENTS of its own
WWW=		https://example.org/
NO_BUILD=	yes

OPTIONS_DEFINE=	B1 FOO
OPTIONS_GROUP=	G
OPTIONS_GROUP_G=	C
OPTIONS_EXCLUDE=	FOO
OPTIONS_DEFAULT=	B1 C
C_IMPLIES=	FOO
FOO_PREVENTS=	B1

.include <bsd.port.mk>
