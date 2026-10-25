PORTNAME=	dove
PORTVERSION=	1.0
CATEGORIES=	misc
MAINTAINER=	nobody@example.org
COMMENT=	A flavor whose Makefile names its own options file (mail/dovecot24)
WWW=		https://example.org/
NO_BUILD=	yes

FLAVORS=	default mysql
FLAVOR?=	default
mysql_PKGNAMESUFFIX=	-mysql

OPTIONS_DEFINE=	A
OPTIONS_DEFAULT=	A

.if ${FLAVOR} == mysql
DOVE_SUFFIX=	-mysql
.endif
OPTIONS_FILE=	${PORT_DBDIR}/misc_dove${DOVE_SUFFIX}/options

.include <bsd.port.mk>
