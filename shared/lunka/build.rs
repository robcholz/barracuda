fn main() {
    #[cfg(feature = "link-vendored")]
    {
        struct BareMetal;

        impl ::lunka_src::platforms::Platform for BareMetal {
            fn defines(&self) -> &[&str] {
                &[]
            }

            fn standards(&self) -> &::lunka_src::platforms::Standards<'_> {
                static STANDARDS: ::lunka_src::platforms::Standards<'static> =
                    ::lunka_src::platforms::Standards {
                        gnu: Some("gnu99"),
                        clang: Some("gnu99"),
                        msvc: Some("c99"),
                        clang_cl: Some("gnu99"),
                    };
                &STANDARDS
            }
        }

        let bare_metal = ::std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("none");
        let mut build = if bare_metal {
            ::lunka_src::Build::new(BareMetal)
        } else {
            ::lunka_src::Build::for_current()
        };
        build.add_lunka_src().compile("lua");

        if bare_metal {
            link_embedded_c_runtime();
        }
    }
}

#[cfg(feature = "link-vendored")]
fn link_embedded_c_runtime() {
    // Do not bundle newlib into Lunka's Rust rlib. ESP HAL, esp-alloc, and the
    // selected Platform provide the bare-metal C ABI; bundling another libc
    // would duplicate the allocator, ROM, formatting, and stack-check symbols.
}
