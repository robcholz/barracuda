#include <stddef.h>

struct _reent;

extern void *malloc(size_t size);
extern void *calloc(size_t count, size_t size);
extern void *realloc(void *pointer, size_t size);
extern void free(void *pointer);

void *_malloc_r(struct _reent *reent, size_t size) {
    (void)reent;
    return malloc(size);
}

void *_calloc_r(struct _reent *reent, size_t count, size_t size) {
    (void)reent;
    return calloc(count, size);
}

void *_realloc_r(struct _reent *reent, void *pointer, size_t size) {
    (void)reent;
    return realloc(pointer, size);
}

void _free_r(struct _reent *reent, void *pointer) {
    (void)reent;
    free(pointer);
}
