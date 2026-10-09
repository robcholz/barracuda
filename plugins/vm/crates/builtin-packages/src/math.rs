//! The standard `math` library, in Rust.
//!
//! It follows `lmathlib.c` from Lua 5.4.8, including its xoshiro256**
//! generator, so a seed gives the same `math.random` sequence as standard Lua.
//! Seeds come from the Platform's entropy source, never from the clock: until a
//! script calls `math.randomseed(n)`, `math.random` draws its seed there and
//! raises an error on a Platform without one.

use alloc::rc::Rc;
use core::cell::RefCell;

use barracuda_lua::{Error, Lua, LuaReturn, Number, Package, Result, Variadic};

const INSTALL_PACKAGE: &str = r#"
local math = require("math")
local native_type = math.__type
local native_tointeger = math.__tointeger
math.__type = nil
math.__tointeger = nil

-- Native functions report failure as nil plus a message; math raises it.
local function raising(native)
    return function(...)
        local first, second = native(...)
        if first == nil and type(second) == "string" then
            error(second, 2)
        end
        if second == nil then
            return first
        end
        return first, second
    end
end
for _, name in ipairs({ "fmod", "ult", "max", "min", "random", "randomseed" }) do
    math[name] = raising(math[name])
end

-- Native optional parameters need an explicit nil, so pass both arguments.
local native_log = math.log
local native_atan = math.atan
function math.log(x, base)
    return native_log(x, base)
end
function math.atan(y, x)
    return native_atan(y, x)
end

function math.type(value)
    if type(value) ~= "number" then
        return nil
    end
    return native_type(value)
end

function math.tointeger(value)
    if type(value) == "string" then
        value = tonumber(value)
    end
    if type(value) ~= "number" then
        return nil
    end
    return native_tointeger(value)
end

_G.math = math
"#;

/// Fills a buffer with entropy and reports whether it could.
type Fill = dyn Fn(&mut [u8]) -> bool;

/// Unpredictable bytes for seeding `math.random`, from the Platform's entropy
/// source with its type erased.
#[derive(Clone)]
pub struct SeedSource(Rc<Fill>);

impl SeedSource {
    /// Wraps a fill function that reports whether it produced entropy.
    #[must_use]
    pub fn new(fill: impl Fn(&mut [u8]) -> bool + 'static) -> Self {
        Self(Rc::new(fill))
    }

    /// A source for Platforms without entropy.
    #[must_use]
    pub fn unavailable() -> Self {
        Self::new(|_bytes| false)
    }

    fn fill(&self, bytes: &mut [u8]) -> bool {
        (self.0)(bytes)
    }
}

/// The `math` library for one Lua state.
pub struct Math {
    seeds: SeedSource,
}

impl Math {
    /// Creates `math` with its own generator, seeded from `seeds` on demand.
    #[must_use]
    pub fn new(seeds: SeedSource) -> Self {
        Self { seeds }
    }
}

impl Package for Math {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let generator = Rc::new(RefCell::new(Generator {
            state: None,
            seeds: self.seeds.clone(),
        }));
        lua.register_lib("math", |math| {
            math.set("pi", core::f64::consts::PI)?;
            math.set("huge", f64::INFINITY)?;
            math.set("maxinteger", i64::MAX)?;
            math.set("mininteger", i64::MIN)?;

            math.register("abs", |x: Number| {
                Some(Ok(match x {
                    Number::Integer(value) => Number::Integer(value.wrapping_abs()),
                    Number::Float(value) => Number::Float(libm::fabs(value)),
                }))
            })?;
            math.register("ceil", |x: Number| {
                Some(Ok(match x {
                    Number::Integer(value) => Number::Integer(value),
                    Number::Float(value) => integer_if_exact(libm::ceil(value)),
                }))
            })?;
            math.register("floor", |x: Number| {
                Some(Ok(match x {
                    Number::Integer(value) => Number::Integer(value),
                    Number::Float(value) => integer_if_exact(libm::floor(value)),
                }))
            })?;
            math.register("fmod", |(a, b): (Number, Number)| Some(fmod(a, b)))?;
            math.register("modf", |x: Number| {
                Some(Ok(match x {
                    Number::Integer(value) => (Number::Integer(value), 0.0),
                    Number::Float(value) => {
                        let integral = if value < 0.0 {
                            libm::ceil(value)
                        } else {
                            libm::floor(value)
                        };
                        let fraction = if value == integral {
                            0.0
                        } else {
                            value - integral
                        };
                        (integer_if_exact(integral), fraction)
                    }
                }))
            })?;
            math.register("sqrt", |x: Number| Some(Ok(libm::sqrt(float(x)))))?;
            math.register("exp", |x: Number| Some(Ok(libm::exp(float(x)))))?;
            math.register("log", |(x, base): (Number, Option<Number>)| {
                let x = float(x);
                Some(Ok(match base.map(float) {
                    None => libm::log(x),
                    Some(2.0) => libm::log2(x),
                    Some(10.0) => libm::log10(x),
                    Some(base) => libm::log(x) / libm::log(base),
                }))
            })?;
            math.register("sin", |x: Number| Some(Ok(libm::sin(float(x)))))?;
            math.register("cos", |x: Number| Some(Ok(libm::cos(float(x)))))?;
            math.register("tan", |x: Number| Some(Ok(libm::tan(float(x)))))?;
            math.register("asin", |x: Number| Some(Ok(libm::asin(float(x)))))?;
            math.register("acos", |x: Number| Some(Ok(libm::acos(float(x)))))?;
            math.register("atan", |(y, x): (Number, Option<Number>)| {
                Some(Ok(libm::atan2(float(y), x.map_or(1.0, float))))
            })?;
            math.register("ult", |(m, n): (Number, Number)| {
                Some(integer_argument(m, 1, "ult").and_then(|m| {
                    integer_argument(n, 2, "ult").map(|n| m.cast_unsigned() < n.cast_unsigned())
                }))
            })?;
            math.register("max", |values: Variadic<Number>| {
                Some(extreme(&values, "max", |candidate, best| {
                    less(best, candidate)
                }))
            })?;
            math.register("min", |values: Variadic<Number>| {
                Some(extreme(&values, "min", |candidate, best| {
                    less(candidate, best)
                }))
            })?;
            math.register("__type", |x: Number| {
                Some(Ok(match x {
                    Number::Integer(_) => "integer",
                    Number::Float(_) => "float",
                }))
            })?;
            math.register("__tointeger", |x: Number| {
                Some(Ok(match x {
                    Number::Integer(value) => Some(value),
                    Number::Float(value) => exact_integer(value),
                }))
            })?;

            let random = Rc::clone(&generator);
            math.register(
                "random",
                move |arguments: Variadic<Number>| -> LuaReturn<Number> {
                    Some(random.borrow_mut().random(&arguments))
                },
            )?;
            math.register("randomseed", move |arguments: Variadic<Number>| {
                Some(generator.borrow_mut().randomseed(&arguments))
            })
        })?;
        lua.load(INSTALL_PACKAGE).exec()
    }
}

/// `math.random`'s xoshiro256** state, seeded on first use.
struct Generator {
    state: Option<[u64; 4]>,
    seeds: SeedSource,
}

impl Generator {
    fn random(&mut self, arguments: &[Number]) -> Result<Number> {
        // Like Lua, draw before checking the arguments.
        let value = self.next()?;
        let (low, high) = match arguments {
            [] => return Ok(Number::Float(to_unit_float(value))),
            [high] => {
                let high = integer_argument(*high, 1, "random")?;
                if high == 0 {
                    return Ok(Number::Integer(value.cast_signed()));
                }
                (1, high)
            }
            [low, high] => (
                integer_argument(*low, 1, "random")?,
                integer_argument(*high, 2, "random")?,
            ),
            _ => return Err(Error::runtime("wrong number of arguments")),
        };
        if low > high {
            return Err(Error::runtime(
                "bad argument #1 to 'random' (interval is empty)",
            ));
        }
        let span = high.cast_unsigned().wrapping_sub(low.cast_unsigned());
        let offset = self.project(value, span)?;
        Ok(Number::Integer(
            offset.wrapping_add(low.cast_unsigned()).cast_signed(),
        ))
    }

    fn randomseed(&mut self, arguments: &[Number]) -> Result<(i64, i64)> {
        let (first, second) = match arguments {
            [] => return self.seed_from_entropy(),
            [first] => (integer_argument(*first, 1, "randomseed")?, 0),
            [first, second, ..] => (
                integer_argument(*first, 1, "randomseed")?,
                integer_argument(*second, 2, "randomseed")?,
            ),
        };
        self.seed(first.cast_unsigned(), second.cast_unsigned());
        Ok((first, second))
    }

    fn seed_from_entropy(&mut self) -> Result<(i64, i64)> {
        let mut bytes = [0_u8; 16];
        if !self.seeds.fill(&mut bytes) {
            return Err(Error::runtime(
                "math.random needs an entropy source, which this Platform does not have",
            ));
        }
        let [a, b, c, d, e, f, g, h, rest @ ..] = bytes;
        let first = u64::from_le_bytes([a, b, c, d, e, f, g, h]);
        let second = u64::from_le_bytes(rest);
        self.seed(first, second);
        Ok((first.cast_signed(), second.cast_signed()))
    }

    fn seed(&mut self, first: u64, second: u64) {
        // Lua's setseed: a nonzero state, then 16 discarded values.
        let mut state = [first, 0xff, second, 0];
        for _ in 0..16 {
            next_value(&mut state);
        }
        self.state = Some(state);
    }

    fn next(&mut self) -> Result<u64> {
        if self.state.is_none() {
            self.seed_from_entropy()?;
        }
        let state = self
            .state
            .as_mut()
            .ok_or_else(|| Error::runtime("unseeded"))?;
        Ok(next_value(state))
    }

    /// Lua's `project`: a uniform value in `[0, span]` without modulo bias.
    fn project(&mut self, mut value: u64, span: u64) -> Result<u64> {
        if span & span.wrapping_add(1) == 0 {
            return Ok(value & span);
        }
        let mut limit = span;
        for shift in [1, 2, 4, 8, 16, 32] {
            limit |= limit >> shift;
        }
        loop {
            value &= limit;
            if value <= span {
                return Ok(value);
            }
            value = self.next()?;
        }
    }
}

/// One xoshiro256** step.
fn next_value(state: &mut [u64; 4]) -> u64 {
    let [state0, state1, state2, state3] = *state;
    let state2 = state2 ^ state0;
    let state3 = state3 ^ state1;
    let result = state1.wrapping_mul(5).rotate_left(7).wrapping_mul(9);
    *state = [
        state0 ^ state3,
        state1 ^ state2,
        state2 ^ (state1 << 17),
        state3.rotate_left(45),
    ];
    result
}

/// The top 53 bits as a float in `[0, 1)`.
#[allow(clippy::cast_precision_loss)]
fn to_unit_float(value: u64) -> f64 {
    (value >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
}

#[allow(clippy::cast_precision_loss)]
fn float(x: Number) -> f64 {
    match x {
        Number::Integer(value) => value as f64,
        Number::Float(value) => value,
    }
}

/// The integer equal to `value`, if there is one.
#[allow(clippy::cast_possible_truncation)]
fn exact_integer(value: f64) -> Option<i64> {
    // 2^63, the first float past i64::MAX.
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    (libm::floor(value) == value && (-LIMIT..LIMIT).contains(&value)).then_some(value as i64)
}

fn integer_if_exact(value: f64) -> Number {
    exact_integer(value).map_or(Number::Float(value), Number::Integer)
}

/// `luaL_checkinteger`: an integer, or a float with an integral value.
fn integer_argument(x: Number, position: u8, function: &str) -> Result<i64> {
    match x {
        Number::Integer(value) => Ok(value),
        Number::Float(value) => exact_integer(value).ok_or_else(|| {
            Error::runtime(alloc::format!(
                "bad argument #{position} to '{function}' (number has no integer representation)"
            ))
        }),
    }
}

fn fmod(a: Number, b: Number) -> Result<Number> {
    match (a, b) {
        (Number::Integer(_), Number::Integer(0)) => {
            Err(Error::runtime("bad argument #2 to 'fmod' (zero)"))
        }
        // Avoids overflowing i64::MIN % -1.
        (Number::Integer(_), Number::Integer(-1)) => Ok(Number::Integer(0)),
        (Number::Integer(a), Number::Integer(b)) => Ok(Number::Integer(a.wrapping_rem(b))),
        (a, b) => Ok(Number::Float(libm::fmod(float(a), float(b)))),
    }
}

fn extreme(
    values: &[Number],
    function: &str,
    better: impl Fn(Number, Number) -> bool,
) -> Result<Number> {
    let (&first, rest) = values.split_first().ok_or_else(|| {
        Error::runtime(alloc::format!(
            "bad argument #1 to '{function}' (value expected)"
        ))
    })?;
    Ok(rest.iter().fold(first, |best, &candidate| {
        if better(candidate, best) {
            candidate
        } else {
            best
        }
    }))
}

/// Lua's `<` on numbers, exact across integers and floats.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn less(a: Number, b: Number) -> bool {
    // 2^63, the first float past i64::MAX.
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    match (a, b) {
        (Number::Integer(a), Number::Integer(b)) => a < b,
        (Number::Float(a), Number::Float(b)) => a < b,
        // i < f exactly when i < ceil(f).
        (Number::Integer(a), Number::Float(b)) => {
            if b.is_nan() {
                false
            } else if b >= LIMIT {
                true
            } else if b > -LIMIT {
                a < libm::ceil(b) as i64
            } else {
                false
            }
        }
        // f < i exactly when floor(f) < i.
        (Number::Float(a), Number::Integer(b)) => {
            if a.is_nan() {
                false
            } else if a < -LIMIT {
                true
            } else if a < LIMIT {
                (libm::floor(a) as i64) < b
            } else {
                false
            }
        }
    }
}
