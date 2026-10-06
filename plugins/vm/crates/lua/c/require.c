/*
 * Barracuda's require: modules come only from the registry's preload table,
 * which the embedder fills. There are no files or shared libraries to search,
 * so Lua's package library is not built.
 */
#include "lua.h"
#include "lauxlib.h"

static int require(lua_State *L) {
	const char *name = luaL_checkstring(L, 1);
	lua_settop(L, 1);
	luaL_getsubtable(L, LUA_REGISTRYINDEX, LUA_LOADED_TABLE);
	if (lua_getfield(L, 2, name) != LUA_TNIL && lua_toboolean(L, -1))
		return 1;
	lua_pop(L, 1);
	luaL_getsubtable(L, LUA_REGISTRYINDEX, LUA_PRELOAD_TABLE);
	if (lua_getfield(L, -1, name) == LUA_TNIL)
		return luaL_error(L, "module '%s' not found:\n\tno field package.preload['%s']", name, name);
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

/* Installs the global require and creates the preload table. */
LUALIB_API void barracuda_lua_open_require(lua_State *L) {
	luaL_getsubtable(L, LUA_REGISTRYINDEX, LUA_PRELOAD_TABLE);
	lua_pop(L, 1);
	lua_pushcfunction(L, require);
	lua_setglobal(L, "require");
}
