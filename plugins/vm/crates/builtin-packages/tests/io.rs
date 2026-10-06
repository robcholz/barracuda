#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use barracuda_lua::{Lua, Package, Result};
use barracuda_vm_builtin_packages::BuiltinPackages;
use barracuda_vm_builtin_packages::math::SeedSource;

struct Marker;

impl Package for Marker {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        lua.register_lib("marker", |package| package.set("installed", true))
    }
}

#[test]
fn io_exposes_the_standard_message_io_surface() -> Result<()> {
    let mut lua = Lua::new()?;
    Marker.install(&mut lua)?;
    let (input, mut output) = BuiltinPackages::new(SeedSource::unavailable())
        .install(&mut lua)?
        .into_io();

    assert!(
        lua.load(
            "local marker = require('marker') \
             return io == require('io') \
                 and os == require('os') \
                 and next(os) == nil \
                 and input == nil \
                 and type(print) == 'function' \
                 and io.__read == nil \
                 and io.__emit == nil \
                 and io.input() == io.stdin \
                 and io.input(nil) == io.stdin \
                 and io.output() == io.stdout \
                 and io.output(nil) == io.stdout \
                 and io.print == nil \
                 and type(io.read) == 'function' \
                 and type(io.lines) == 'function' \
                 and type(io.write) == 'function' \
                 and type(io.flush) == 'function' \
                 and io.type(io.stdin) == 'file' \
                 and io.type(io.stdout) == 'file' \
                 and io.type(io.stderr) == 'file' \
                 and io.type({}) == nil \
                 and not pcall(io.lines, 'file.lua') \
                 and marker.installed",
        )
        .eval::<bool>()?
    );

    let execution = lua.run(
        "local value = io.read('*l'); \
         io.write('received', '\\t'); \
         io.stdout:write(value, '\\n'); \
         print('done')",
    );
    futures_lite::future::block_on(input.send("message"))?;
    input.close();
    let (execution, chunks) =
        futures_lite::future::block_on(futures_lite::future::zip(execution, async {
            [
                output.next().await,
                output.next().await,
                output.next().await,
            ]
        }));
    execution?;
    assert_eq!(
        chunks,
        [
            Some("received\t".into()),
            Some("message\n".into()),
            Some("done\n".into()),
        ]
    );
    assert_eq!(futures_lite::future::block_on(output.next()), None);
    assert!(futures_lite::future::block_on(input.send("late")).is_err());
    Ok(())
}

#[test]
fn io_read_supports_standard_message_stream_formats() -> Result<()> {
    let mut lua = Lua::new()?;
    let (input, mut output) = BuiltinPackages::new(SeedSource::unavailable())
        .install(&mut lua)?
        .into_io();
    let execution = lua.run(
        "local first = io.read(3) \
         local rest = io.read('*L') \
         local all = io.read('*a') \
         print(first, rest, all)",
    );

    futures_lite::future::block_on(async {
        let send_input = async {
            assert!(input.next_request().await);
            input.send("hello").await?;
            assert!(input.next_request().await);
            input.send("second").await?;
            input.close();
            Result::<()>::Ok(())
        };
        let (execution, input) = futures_lite::future::zip(execution, send_input).await;
        input?;
        execution
    })?;

    assert_eq!(
        futures_lite::future::block_on(output.next()),
        Some("hel\tlo\n\tsecond\n\n".into())
    );
    assert_eq!(futures_lite::future::block_on(output.next()), None);
    Ok(())
}

#[test]
fn io_lines_iterates_input_messages_until_eof() -> Result<()> {
    let mut lua = Lua::new()?;
    let (input, mut output) = BuiltinPackages::new(SeedSource::unavailable())
        .install(&mut lua)?
        .into_io();
    let execution = lua.run(
        "local values = {} \
         for value in io.lines(nil, '*L') do \
             table.insert(values, value) \
         end \
         print(table.concat(values, ','))",
    );

    futures_lite::future::block_on(async {
        let send_input = async {
            assert!(input.next_request().await);
            input.send("first").await?;
            assert!(input.next_request().await);
            input.send("second").await?;
            input.close();
            Result::<()>::Ok(())
        };
        let (execution, input) = futures_lite::future::zip(execution, send_input).await;
        input?;
        execution
    })?;

    assert_eq!(
        futures_lite::future::block_on(output.next()),
        Some("first\n,second\n\n".into())
    );
    assert_eq!(futures_lite::future::block_on(output.next()), None);
    Ok(())
}
