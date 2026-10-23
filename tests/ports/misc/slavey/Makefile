PORTNAME=	slavey
PORTVERSION=	1.0
CATEGORIES=	misc
MAINTAINER=	nobody@example.org
COMMENT=	Flavor turning an option on through OPTIONS_SLAVE (databases/sqlite3)
WWW=		https://example.org/
NO_BUILD=	yes

FLAVORS=	default icu
FLAVOR?=	${FLAVORS:[1]}
icu_PKGNAMESUFFIX=	-icu

OPTIONS_DEFINE=	DOCS ICU
OPTIONS_DEFAULT=	DOCS

.if ${FLAVOR} == icu
OPTIONS_SLAVE=	ICU
.endif

.include <bsd.port.mk>
