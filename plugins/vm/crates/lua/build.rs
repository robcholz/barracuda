#[cfg(feature = "vendored")]
struct BareMetal;

#[cfg(feature = "vendored")]
impl lunka_src::platforms::Platform for BareMetal {
    fn defines(&self) -> &[&str] {
        &[]
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

fn main() {
    #[cfg(feature = "vendored")]
    {
        let target = std::env::var("TARGET").expect("Cargo must provide TARGET");
        if target.contains("-none-") {
            lunka_src::Build::new(BareMetal)
                .add_lunka_src()
                .compile("lua");
        } else {
            lunka_src::Build::for_current()
                .add_lunka_src()
                .compile("lua");
        }
    }
}
