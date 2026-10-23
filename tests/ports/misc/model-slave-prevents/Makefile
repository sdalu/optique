PORTNAME=	model-slave-prevents
PORTVERSION=	1.0
CATEGORIES=	misc
MAINTAINER=	nobody@example.org
COMMENT=	An OPTIONS_SLAVE option preventing a default one
WWW=		https://example.org/
NO_BUILD=	yes

OPTIONS_DEFINE=	X Y Z
OPTIONS_SLAVE=	X
OPTIONS_DEFAULT=	Y
X_PREVENTS=	Y
X_IMPLIES=	Z

.include <bsd.port.mk>
