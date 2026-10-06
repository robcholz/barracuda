/*
 * C library functions the Lua core calls on bare-metal targets, linked into
 * Lua's own object so the firmware's C library is never involved.
 *
 * Lua only uses time and clock as seeds for hashing and sort pivots; the
 * sandbox fixes math.randomseed, so a constant keeps runs reproducible.
 */
#include <time.h>

time_t time(time_t *result) {
	if (result != NULL) {
		*result = 0;
	}
	return 0;
}

clock_t clock(void) {
	return 0;
}
