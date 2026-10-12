/* The one classification vfprintf.c uses, as musl defines it. */
#ifndef BARRACUDA_TLS_CTYPE_H
#define BARRACUDA_TLS_CTYPE_H

#define isdigit(c) ((unsigned)(c)-'0' < 10)

#endif
