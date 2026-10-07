/*
 * Included before every musl source (build.rs). musl's internal global names
 * get a barracuda_lua_ prefix so they never meet a C library linked elsewhere
 * in the firmware, and musl's visibility and alias markers are dropped.
 */
#ifndef BARRACUDA_MUSL_H
#define BARRACUDA_MUSL_H

#define hidden
#define weak_alias(old, new)

#define __exp_data barracuda_lua_exp_data
#define __pow_log_data barracuda_lua_pow_log_data
#define __math_invalid barracuda_lua_math_invalid
#define __math_oflow barracuda_lua_math_oflow
#define __math_uflow barracuda_lua_math_uflow
#define __math_xflow barracuda_lua_math_xflow
#define __floatscan barracuda_lua_floatscan
#define __strchrnul barracuda_lua_strchrnul
#define __stpcpy barracuda_lua_stpcpy

char *__strchrnul(const char *text, int byte);
char *__stpcpy(char *restrict destination, const char *restrict source);

#endif
