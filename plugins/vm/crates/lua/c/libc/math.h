/*
 * The math Lua's core (and the copied musl) needs: the functions from c/musl,
 * the rest from compiler builtins.
 */
#ifndef BARRACUDA_LUA_MATH_H
#define BARRACUDA_LUA_MATH_H

typedef double double_t;
typedef float float_t;

#define HUGE_VAL (__builtin_huge_val())
#define HUGE_VALF (__builtin_huge_valf())
#define INFINITY (__builtin_inff())
#define NAN (__builtin_nanf(""))

#define isnan(x) __builtin_isnan(x)
#define isinf(x) __builtin_isinf(x)
#define isfinite(x) __builtin_isfinite(x)
#define signbit(x) __builtin_signbit(x)
#define fabs(x) __builtin_fabs(x)

#define copysign barracuda_lua_copysign
#define floor barracuda_lua_floor
#define fmod barracuda_lua_fmod
#define frexp barracuda_lua_frexp
#define ldexp barracuda_lua_ldexp
#define pow barracuda_lua_pow
#define scalbn barracuda_lua_scalbn

double copysign(double x, double y);
double floor(double x);
double fmod(double x, double y);
double frexp(double x, int *exponent);
double ldexp(double x, int exponent);
double pow(double x, double y);
double scalbn(double x, int exponent);

#endif
