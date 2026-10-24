PORTNAME=	model-patterns
PORTVERSION=	1.0
CATEGORIES=	misc
MAINTAINER=	nobody@example.org
COMMENT=	A PREVENTS pattern, and options implying an excluded one
WWW=		https://example.org/
NO_BUILD=	yes

OPTIONS_DEFINE=	A B1 B2 D E FOO
OPTIONS_GROUP=	G
OPTIONS_GROUP_G=	C
OPTIONS_EXCLUDE=	FOO
OPTIONS_DEFAULT=	B1
A_PREVENTS=	B*
# E is excluded with FOO; C, a group member, escapes that pass.
C_IMPLIES=	FOO
E_IMPLIES=	FOO
D_PREVENTS=	FOO

.include <bsd.port.mk>
