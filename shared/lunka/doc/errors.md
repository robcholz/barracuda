# Notes on handling Lua errors
Lua state operations that may involve memory allocation or unprotected execution
of metamethods or other arbitrary code, can raise Lua errors.

By default, Lua uses `setjmp` and `longjmp` in C, and exceptions in C++.
These error mechanisms may or may not unwind the stack, depending on the implementation.
(See `ldo.c` for more information, specifically the macros `LUAI_THROW` and `LUAI_TRY`.)

Given that the Lua API functions are compatible with the
[`C-unwind` ABI](https://rust-lang.github.io/rfcs/2945-c-unwind-abi.html),
the stack may be unwound safely, without any Undefined Behavior.
