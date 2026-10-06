#![allow(clippy::expect_used)]
#![allow(missing_docs)]

use barracuda_lua::{Lua, Result};
use barracuda_vm_builtin_packages::BuiltinPackages;
use barracuda_vm_builtin_packages::math::SeedSource;

fn lua_with(seeds: SeedSource) -> Result<Lua> {
    let mut lua = Lua::new()?;
    BuiltinPackages::new(seeds).install(&mut lua)?;
    Ok(lua)
}

/// Fills seeds with 0, 1, 2, ... so the drawn seeds are predictable.
fn counting_seeds() -> SeedSource {
    SeedSource::new(|bytes| {
        for (byte, value) in bytes.iter_mut().zip(0_u8..) {
            *byte = value;
        }
        true
    })
}

fn eval(lua: &mut Lua, code: &str) -> Result<String> {
    lua.load(code).eval()
}

// Expected values below come from the reference Lua 5.4.8 interpreter.

#[test]
fn a_seed_gives_standard_lua_sequences() -> Result<()> {
    let mut lua = lua_with(SeedSource::unavailable())?;
    assert_eq!(
        eval(
            &mut lua,
            "math.randomseed(42) \
             return table.concat({ math.random(1, 100), math.random(1, 100), math.random(0), \
             math.random(), math.random(-5, 5) }, ' ')",
        )?,
        "50 76 -3807604385970496171 0.61731763595847 1"
    );
    assert_eq!(
        eval(
            &mut lua,
            "local first, second = math.randomseed(7, 9) \
             return table.concat({ first, second, math.random(1, 6), math.random(10) }, ' ')",
        )?,
        "7 9 1 8"
    );
    Ok(())
}

#[test]
fn functions_follow_standard_lua() -> Result<()> {
    let mut lua = lua_with(SeedSource::unavailable())?;
    assert_eq!(
        eval(
            &mut lua,
            "return table.concat({ math.floor(3.7), math.ceil(-3.2), math.floor(2^70), \
             math.abs(math.mininteger), math.fmod(7, -3), math.fmod(-7.5, 2) }, ' ')",
        )?,
        "3 -3 1.1805916207174e+21 -9223372036854775808 1 -1.5"
    );
    assert_eq!(
        eval(
            &mut lua,
            "return table.concat({ math.max(1, 2.5, 2), math.min(3, 1.0, 1), math.type(1), \
             math.type(1.0), tostring(math.type('1')), math.tointeger(3.0), \
             tostring(math.tointeger(3.5)), math.tointeger('8') }, ' ')",
        )?,
        "2.5 1.0 integer float nil 3 nil 8"
    );
    assert_eq!(
        eval(
            &mut lua,
            "return table.concat({ math.modf(3.7), math.modf(-3.7), math.modf(5), \
             tostring(math.ult(1, -1)), math.log(8, 2), math.log(100, 10) }, ' ')",
        )?,
        "3 -3 5 true 3.0 2.0"
    );
    assert_eq!(
        eval(
            &mut lua,
            "return table.concat({ math.pi, math.huge, math.maxinteger, math.mininteger, \
             math.sqrt(16), math.atan(1, 1) * 4 == math.pi and 'atan' }, ' ')",
        )?,
        "3.1415926535898 inf 9223372036854775807 -9223372036854775808 4.0 atan"
    );
    assert!(lua.load("return math.fmod(1, 0)").exec().is_err());
    assert!(lua.load("return math.random(3, 1)").exec().is_err());
    assert!(lua.load("return math.random(1.5)").exec().is_err());
    assert!(lua.load("return math.max()").exec().is_err());
    Ok(())
}

#[test]
fn unseeded_random_needs_platform_entropy() -> Result<()> {
    let mut lua = lua_with(SeedSource::unavailable())?;
    for code in ["return math.random()", "return math.randomseed()"] {
        let error = lua.load(code).exec().expect_err("no entropy to seed from");
        assert!(error.message().contains("entropy"), "{}", error.message());
    }
    assert_eq!(
        eval(
            &mut lua,
            "math.randomseed(1) return tostring(math.random(1, 10))"
        )?,
        "8"
    );
    Ok(())
}

#[test]
fn seeds_come_from_platform_entropy() -> Result<()> {
    let mut lua = lua_with(counting_seeds())?;
    assert_eq!(
        eval(
            &mut lua,
            "local first, second = math.randomseed() \
             return string.format('%x %x', first, second)",
        )?,
        "706050403020100 f0e0d0c0b0a0908"
    );
    let unseeded: String = eval(&mut lua, "return tostring(math.random(1, 1000))")?;
    let mut reseeded = lua_with(counting_seeds())?;
    assert_eq!(
        eval(
            &mut reseeded,
            "math.randomseed(0x0706050403020100, 0x0f0e0d0c0b0a0908) \
             math.random(1, 1000) \
             return tostring(math.random(1, 1000))",
        )?,
        eval(&mut lua, "return tostring(math.random(1, 1000))")?
    );
    assert!(!unseeded.is_empty());
    Ok(())
}
