/*
 * Included before every source here (build.rs). Each C library function is
 * weak, so a definition the target already links (chip ROM, the allocator,
 * a vendor library) takes precedence, and these only fill what is missing.
 * musl's visibility and alias markers are dropped.
 */
#ifndef BARRACUDA_TLS_WEAK_H
#define BARRACUDA_TLS_WEAK_H

#define hidden
#define weak_alias(old, new)

#pragma weak memchr
#pragma weak strcmp
#pragma weak vsnprintf
#pragma weak snprintf
#pragma weak calloc
#pragma weak free
#pragma weak printf
#pragma weak puts

#endif
