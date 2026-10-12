//! Build this Plugin's author-declared resources before compiling it.

fn main() -> Result<(), barracuda_plugin_build::TaskError> {
    barracuda_plugin_build::build()
}
