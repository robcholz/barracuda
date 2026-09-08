//! Builds the existing `vendored` Lua feature for the selected target.

#[cfg(feature = "vendored")]
use std::env;

fn main() {
    #[cfg(feature = "vendored")]
    build_vendored();
}

#[cfg(feature = "vendored")]
fn build_vendored() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("wasi") {
        build_wasi();
    } else {
        lunka_src::Build::for_current()
            .add_lunka_src()
            .compile("lua");
    }
}

#[cfg(feature = "vendored")]
fn build_wasi() {
    configure_wasi_cflags();

    cc::Build::new()
        .file("c/lunka_abi.c")
        .flag("-std=gnu99")
        .warnings(true)
        .extra_warnings(true)
        .compile("lua-abi");

    lunka_src::Build::new(Wasi).add_lunka_src().compile("lua");
}

#[cfg(feature = "vendored")]
struct Wasi;

#[cfg(feature = "vendored")]
impl lunka_src::platforms::Platform for Wasi {
    fn defines(&self) -> &[&str] {
        &[
            "_WASI_EMULATED_SIGNAL",
            "lua_error=barracuda_lua_error_impl",
            "lua_yieldk=barracuda_lua_yieldk_impl",
            "luaL_argerror=barracuda_luaL_argerror_impl",
            "luaL_typeerror=barracuda_luaL_typeerror_impl",
            "luaL_error=barracuda_luaL_error_impl",
            "L_tmpnam=32",
        ]
    }

    fn standards(&self) -> &lunka_src::platforms::Standards<'_> {
        static STANDARDS: lunka_src::platforms::Standards<'static> =
            lunka_src::platforms::Standards {
                gnu: Some("gnu99"),
                clang: Some("gnu99"),
                msvc: None,
                clang_cl: None,
            };
        &STANDARDS
    }
}

#[cfg(feature = "vendored")]
fn configure_wasi_cflags() {
    const NAME: &str = "CFLAGS_wasm32_wasip1";
    const REQUIRED: &str = "-mllvm -wasm-enable-sjlj -Wno-deprecated-declarations";
    let flags = env::var(NAME).unwrap_or_default();
    if flags
        .split_ascii_whitespace()
        .any(|flag| flag == "-wasm-enable-sjlj")
    {
        return;
    }
    let flags = if flags.is_empty() {
        REQUIRED.to_owned()
    } else {
        format!("{flags} {REQUIRED}")
    };

    // Cargo executes each build script in its own process. No other thread in
    // this script reads or writes the environment before `cc` consumes it.
    #[allow(unsafe_code)]
    unsafe {
        env::set_var(NAME, flags);
    }
}
