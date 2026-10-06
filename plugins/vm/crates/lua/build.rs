#[cfg(feature = "vendored")]
mod vendored {
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// The Lua core and the libraries the sandbox opens. Lua's io, os, math,
    /// debug, coroutine and package libraries and `luaL_openlibs` are not
    /// built; `c/require.c` replaces `require`.
    const SOURCES: [&str; 25] = [
        "lapi", "lcode", "lctype", "ldebug", "ldo", "ldump", "lfunc", "lgc", "llex", "lmem",
        "lobject", "lopcodes", "lparser", "lstate", "lstring", "ltable", "ltm", "lundump", "lvm",
        "lzio", "lauxlib", "lbaselib", "lstrlib", "ltablib", "lutf8lib",
    ];

    /// The C library functions Lua calls, copied from musl into `c/musl`.
    const MUSL_SOURCES: [&str; 30] = [
        "__math_invalid",
        "__math_oflow",
        "__math_uflow",
        "__math_xflow",
        "copysign",
        "ctype",
        "exp_data",
        "floatscan",
        "floor",
        "fmod",
        "frexp",
        "ldexp",
        "memchr",
        "pow",
        "pow_data",
        "scalbn",
        "stpcpy",
        "strchr",
        "strchrnul",
        "strcmp",
        "strcpy",
        "strcspn",
        "strlen",
        "strncmp",
        "strnlen",
        "strpbrk",
        "strspn",
        "strstr",
        "strtod",
        "vfprintf",
    ];

    pub(crate) fn build() {
        let manifest = PathBuf::from(
            env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"),
        );
        let upstream = manifest.join("lua");
        let patches = manifest.join("patches");
        let c = manifest.join("c");
        for path in [&upstream, &patches, &c] {
            println!("cargo:rerun-if-changed={}", path.display());
        }
        assert!(
            upstream.join("lua.h").is_file(),
            "the Lua sources are missing; run `git submodule update --init {}`",
            upstream.display()
        );

        let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
        let source = out.join("lua");
        if source.exists() {
            fs::remove_dir_all(&source).expect("the previous Lua sources can be removed");
        }
        fs::create_dir_all(&source).expect("the Lua source directory can be created");
        copy_sources(&upstream, &source);
        apply_patches(&patches, &source);

        cc::Build::new()
            .std("c11")
            // Lua-only C library headers come before the system's.
            .include(c.join("libc"))
            .include(&c)
            .include(&source)
            .define("LUA_USER_H", "\"barracuda_lua_user.h\"")
            .files(SOURCES.map(|name| source.join(format!("{name}.c"))))
            .file(c.join("require.c"))
            .compile("lua");
        // On Xtensa, GCC lowers Lua's error jumps (`__builtin_longjmp`) to
        // libgcc's `__xtensa_nonlocal_goto`. Rust links no C runtime by
        // default, so Lua names the one its compiler needs.
        if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("xtensa") {
            println!("cargo:rustc-link-lib=gcc");
        }

        let musl = c.join("musl");
        cc::Build::new()
            .std("c11")
            // musl is written for its own warning flags, not -Wall -Wextra.
            .warnings(false)
            .extra_warnings(false)
            .include(c.join("libc"))
            .include(&musl)
            .flag("-include")
            .flag(musl.join("barracuda_musl.h").to_string_lossy().as_ref())
            .files(MUSL_SOURCES.map(|name| musl.join(format!("{name}.c"))))
            .compile("lua_musl");
    }

    /// Copies the built sources and every header out of the submodule.
    fn copy_sources(upstream: &Path, source: &Path) {
        for entry in fs::read_dir(upstream).expect("the Lua submodule can be read") {
            let path = entry.expect("the Lua submodule can be listed").path();
            let is = |extension: &str| path.extension().is_some_and(|found| found == extension);
            let built = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| SOURCES.contains(&stem));
            if is("h") || (is("c") && built) {
                let name = path.file_name().expect("a Lua source has a file name");
                fs::copy(&path, source.join(name)).expect("a Lua source can be copied");
            }
        }
    }

    /// Applies `patches/*.patch` in name order.
    fn apply_patches(patches: &Path, source: &Path) {
        let mut files: Vec<PathBuf> = fs::read_dir(patches)
            .expect("the Lua patches can be read")
            .map(|entry| entry.expect("the Lua patches can be listed").path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "patch")
            })
            .collect();
        files.sort();
        for patch in files {
            let status = Command::new("patch")
                .args(["-p1", "-s", "-t", "-d"])
                .arg(source)
                .arg("-i")
                .arg(&patch)
                .status()
                .unwrap_or_else(|error| panic!("failed to run patch: {error}"));
            assert!(status.success(), "{} does not apply", patch.display());
        }
    }
}

fn main() {
    #[cfg(feature = "vendored")]
    vendored::build();
}
