/*
 * Barracuda's require: modules come only from the registry's preload table,
 * which the embedder fills, either before the script runs or the first time
 * the script requires a name. There are no files or shared libraries to
 * search, so Lua's package library is not built.
 */
#include "lua.h"
#include "lauxlib.h"
#include "ldo.h"
#include "lmem.h"

static int not_found(lua_State *L, const char *name) {
	return luaL_error(L, "module '%s' not found:\n\tno field package.preload['%s']", name, name);
}

/*
 * Pushes package.loaded[name] and returns 1 when it is set; otherwise leaves
 * the stack as it was and returns 0.
 */
static int push_loaded(lua_State *L, const char *name) {
	if (lua_getfield(L, 2, name) != LUA_TNIL && lua_toboolean(L, -1))
		return 1;
	lua_pop(L, 1);
	return 0;
}

static int require(lua_State *L) {
	const char *name = luaL_checkstring(L, 1);
	lua_settop(L, 1);
	luaL_getsubtable(L, LUA_REGISTRYINDEX, LUA_LOADED_TABLE);
	if (push_loaded(L, name))
		return 1;
	luaL_getsubtable(L, LUA_REGISTRYINDEX, LUA_PRELOAD_TABLE);
	if (lua_getfield(L, 3, name) == LUA_TNIL) {
		/*
		 * Nothing registered under the name yet: the upvalue may install a
		 * package by that name, which registers it in the preload table,
		 * and may load it while installing.
		 */
		lua_pop(L, 1);
		if (lua_isnoneornil(L, lua_upvalueindex(1)))
			return not_found(L, name);
		lua_pushvalue(L, lua_upvalueindex(1));
		lua_pushvalue(L, 1);
		lua_call(L, 1, 0);
		if (push_loaded(L, name))
			return 1;
		if (lua_getfield(L, 3, name) == LUA_TNIL)
			return not_found(L, name);
	}
	lua_pushvalue(L, 1);
	lua_pushliteral(L, ":preload:");
	lua_call(L, 2, 1);
	if (!lua_isnil(L, -1))
		lua_setfield(L, 2, name);
	else
		lua_pop(L, 1);
	if (lua_getfield(L, 2, name) == LUA_TNIL) {
		lua_pushboolean(L, 1);
		lua_copy(L, -1, -2);
		lua_setfield(L, 2, name);
	}
	lua_pushliteral(L, ":preload:");
	return 2;
}

/*
 * Installs the global require and creates the preload table. `install`, when
 * not NULL, is called with a name nothing is registered under yet.
 */
LUALIB_API void barracuda_lua_open_require(lua_State *L, lua_CFunction install) {
	luaL_getsubtable(L, LUA_REGISTRYINDEX, LUA_PRELOAD_TABLE);
	lua_pop(L, 1);
	if (install != NULL)
		lua_pushcfunction(L, install);
	else
		lua_pushnil(L);
	lua_pushcclosure(L, require, 1);
	lua_setglobal(L, "require");
}

/* Raises Lua's own out-of-memory error, as a failed allocation does. */
LUALIB_API l_noret barracuda_lua_memory_error(lua_State *L) {
	luaM_error(L);
}
