#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use barracuda_lua::{Environment, Lua, Package, Result};
use barracuda_lua_io::Io;

struct Marker;

impl Package for Marker {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        lua.register_lib("marker", |package| package.set("installed", true))
    }
}

#[test]
fn io_is_an_external_require_only_package_in_a_composed_environment() -> Result<()> {
    let (io, input, mut output) = Io::new();
    let environment = Environment::new().with_package(io).with_package(Marker);
    let mut lua = Lua::new()?;

    environment.install(&mut lua)?;
    assert!(
        lua.load(
            "local io = require('io') \
             local marker = require('marker') \
             return _G.io == nil \
                 and input == nil \
                 and print == nil \
                 and io == require('io') \
                 and io.__emit == nil \
                 and type(io.input) == 'function' \
                 and type(io.print) == 'function' \
                 and io.read == nil \
                 and io.write == nil \
                 and marker.installed",
        )
        .eval::<bool>()?
    );

    let execution = lua.run(
        "local io = require('io'); \
         local value = io.input(); \
         io.print('received', value)",
    );
    futures_lite::future::block_on(input.send("message"))?;
    input.close();
    futures_lite::future::block_on(execution)?;

    assert_eq!(
        futures_lite::future::block_on(output.next()),
        Some("received\tmessage".into())
    );
    assert_eq!(futures_lite::future::block_on(output.next()), None);
    assert!(futures_lite::future::block_on(input.send("late")).is_err());
    Ok(())
}
