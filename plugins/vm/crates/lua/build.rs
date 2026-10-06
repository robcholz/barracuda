#[cfg(feature = "vendored")]
mod vendored {
    use std::env;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use lunka_src::platforms::{Platform, Standards};

    /// Header that `lua.h` includes last, through `LUA_USER_H`.
    const USER_HEADER: &str = "LUA_USER_H=\"barracuda_lua_user.h\"";

    struct BareMetal;

    impl Platform for BareMetal {
        fn defines(&self) -> &[&str] {
            &[USER_HEADER]
        }

        fn standards(&self) -> &Standards<'_> {
            static STANDARDS: Standards<'static> = Standards {
                gnu: Some("gnu99"),
                clang: Some("gnu99"),
                msvc: None,
                clang_cl: None,
            };
            &STANDARDS
        }
    }

    /// The host's own Lua platform, plus the user header.
    struct Host<P> {
        platform: P,
        defines: Vec<&'static str>,
    }

    impl<P: Platform> Platform for Host<P> {
        fn defines(&self) -> &[&str] {
            &self.defines
        }

        fn standards(&self) -> &Standards<'_> {
            self.platform.standards()
        }
    }

    pub(crate) fn build() {
        let manifest = PathBuf::from(
            env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"),
        );
        let c = manifest.join("c");
        println!("cargo:rerun-if-changed={}", c.display());
        let target = env::var("TARGET").expect("Cargo sets TARGET");
        if target.contains("-none-") {
            lunka_src::Build::new(BareMetal)
                .include(&c)
                .add_lunka_src()
                .compile("lua");
            isolate_c_library(&c);
        } else {
            let platform = lunka_src::platforms::from_current_triple()
                .expect("lunka-src supports the host platform");
            let defines = platform
                .defines()
                .iter()
                .map(|define| &*String::leak((*define).to_owned()))
                .chain([USER_HEADER])
                .collect();
            lunka_src::Build::new(Host { platform, defines })
                .include(&c)
                .add_lunka_src()
                .compile("lua");
        }
    }

    /// Replaces `liblua.a` with one object holding Lua and the parts of the
    /// toolchain's C library Lua uses, exporting only the Lua API.
    ///
    /// The firmware then links no C library: nothing else can resolve a C
    /// function, such as a Wi-Fi driver's `usleep`, to this one, and the C
    /// library's symbols cannot clash with the firmware's own.
    fn isolate_c_library(c: &Path) {
        let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
        let compiler = cc::Build::new().get_compiler();
        let tool = |name: &str| toolchain_tool(compiler.path(), name);
        let library = |name: &str| {
            let output = Command::new(compiler.path())
                .args(compiler.args())
                .arg(format!("-print-file-name={name}"))
                .output()
                .expect("the C compiler runs");
            PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
        };

        let bare_metal = out.join("barracuda_lua_bare_metal.o");
        run(Command::new(compiler.path())
            .args(compiler.args())
            .arg("-c")
            .arg(c.join("bare_metal.c"))
            .arg("-o")
            .arg(&bare_metal));

        let archive = out.join("liblua.a");
        let combined = out.join("lua-isolated.o");
        // Lua's own replacements come before the C library so the archive
        // supplies only what is still undefined.
        run(Command::new(tool("ld"))
            .arg("-r")
            .arg("-o")
            .arg(&combined)
            .arg(&bare_metal)
            .arg("--whole-archive")
            .arg(&archive)
            .arg("--no-whole-archive")
            .arg("--start-group")
            .arg(library("libc.a"))
            .arg(library("libm.a"))
            .arg(library("libgcc.a"))
            .arg("--end-group"));
        run(Command::new(tool("objcopy"))
            .arg("--wildcard")
            .args([
                "--keep-global-symbol=lua_*",
                "--keep-global-symbol=luaL_*",
                "--keep-global-symbol=luaopen_*",
            ])
            .arg(&combined));
        std::fs::remove_file(&archive).expect("liblua.a can be replaced");
        run(Command::new(tool("ar"))
            .arg("crs")
            .arg(&archive)
            .arg(&combined));
    }

    /// The `ld`, `objcopy` or `ar` beside a `<prefix>-gcc` cross compiler.
    fn toolchain_tool(compiler: &Path, name: &str) -> PathBuf {
        let file = compiler
            .file_name()
            .and_then(|file| file.to_str())
            .unwrap_or_default();
        let prefix = file.strip_suffix("gcc").unwrap_or_default();
        compiler.with_file_name(format!("{prefix}{name}"))
    }

    fn run(command: &mut Command) {
        let status = command
            .status()
            .unwrap_or_else(|error| panic!("failed to run {command:?}: {error}"));
        assert!(status.success(), "{command:?} failed with {status}");
    }
}

fn main() {
    #[cfg(feature = "vendored")]
    vendored::build();
}
