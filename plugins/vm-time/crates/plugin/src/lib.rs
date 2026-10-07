//! Lua adapter for Barracuda's network-synchronized UTC clock.

#![no_std]

extern crate alloc;

use alloc::{format, rc::Rc, string::String};
use core::fmt::Write as _;

use async_channel::{Receiver, Sender};
use barracuda_plugin::api::PluginContext;
use barracuda_plugin::manager::{
    Plugin, PluginError, PluginRegisterContext, PluginResult, PluginStartContext, PluginTaskToken,
};
use barracuda_runtime_utils::oneshot;
use barracuda_time_plugin::{ClockError, UtcClock};
use barracuda_vm_plugin::{
    Context, Error, Lua, LuaPackage, LuaPackageRegistry, Package, Result, Table,
};
use embassy_futures::select::{Either, select};
use portable_atomic::{AtomicBool, Ordering};
use portable_atomic_util::Arc;
use time::{Date, Duration, Month, OffsetDateTime, PrimitiveDateTime, Time};

const REQUEST_QUEUE_DEPTH: usize = 4;
const MAX_FORMAT_BYTES: usize = 256;

const INSTALL_ADAPTER: &str = r#"
local os = require("os")
local native_now = os.__time_now
local native_from_table = os.__time_from_table
local native_table = os.__time_table
local native_format = os.__time_format
os.__time_now = nil
os.__time_from_table = nil
os.__time_table = nil
os.__time_format = nil

function os.time(value)
    local timestamp, message
    if value == nil then
        timestamp, message = native_now()
    else
        timestamp, message = native_from_table(value)
    end
    if timestamp == nil then error(message, 2) end
    return timestamp
end

function os.date(format, timestamp)
    format = format or "%c"
    if type(format) ~= "string" then error("bad argument #1 to 'date' (string expected)", 2) end
    if string.sub(format, 1, 1) == "!" then format = string.sub(format, 2) end
    if timestamp == nil then timestamp = os.time() end
    local value, message
    if format == "*t" then
        value, message = native_table(timestamp)
    else
        value, message = native_format(format, timestamp)
    end
    if value == nil then error(message, 2) end
    return value
end

function os.difftime(second, first)
    return second - first
end
"#;

/// Installs UTC-backed standard `os` time functions into every Lua VM.
#[barracuda_plugin::macros::plugin]
pub struct VmTimePlugin {
    runtime: Option<VmTimeRuntime>,
}

struct VmTimeRuntime {
    clock: Rc<UtcClock>,
    requests: Receiver<TimeRequest>,
    state: Arc<TimePackageState>,
}

impl VmTimePlugin {
    /// Creates an unregistered VM Time adapter.
    #[must_use]
    pub const fn new<Builtins, Io>(_context: &mut PluginContext<Builtins, Io>) -> Self {
        Self { runtime: None }
    }
}

impl Plugin for VmTimePlugin {
    fn register<Storage>(
        &mut self,
        context: &mut PluginRegisterContext<'_, Storage>,
    ) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let clock = context.require::<UtcClock>("time")?;
        let packages = context.require::<LuaPackageRegistry>("vm")?;
        let (package, requests) = TimePackage::new();
        let state = Arc::clone(&package.state);
        let registration = packages
            .register(package)
            .map_err(PluginError::registration)?;
        context.retain(registration);
        self.runtime = Some(VmTimeRuntime {
            clock,
            requests,
            state,
        });
        Ok(())
    }

    fn start<Storage>(&mut self, context: &mut PluginStartContext<'_, Storage>) -> PluginResult<()>
    where
        Storage: barracuda_plugin::manager::PluginStorage,
    {
        let runtime = self
            .runtime
            .take()
            .ok_or_else(|| PluginError::registration(TimeRuntimeUnavailable))?;
        let cancellation = context.task_token();
        let task = vm_time_task(runtime, cancellation).map_err(PluginError::registration)?;
        context.task_spawner()?.spawn(task);
        Ok(())
    }
}

#[embassy_executor::task]
async fn vm_time_task(runtime: VmTimeRuntime, cancellation: PluginTaskToken) {
    let VmTimeRuntime {
        clock,
        requests,
        state,
    } = runtime;
    loop {
        match select(cancellation.cancelled(), requests.recv()).await {
            Either::First(()) | Either::Second(Err(_)) => break,
            Either::Second(Ok(request)) => {
                let result = clock.now().and_then(|value| {
                    i64::try_from(u64::from(value) / 1_000).map_err(|_| ClockError::OutOfRange)
                });
                request.respond(result.map_err(ClockError::code));
            }
        }
    }
    state.revoke();
    requests.close();
    while let Ok(request) = requests.try_recv() {
        request.respond(Err("runtime_stopped"));
    }
}

struct TimeRequest {
    response: oneshot::Sender<core::result::Result<i64, &'static str>>,
}

impl TimeRequest {
    fn respond(self, response: core::result::Result<i64, &'static str>) {
        let _ignored = self.response.send(response);
    }
}

struct TimePackage {
    state: Arc<TimePackageState>,
}

impl TimePackage {
    fn new() -> (Self, Receiver<TimeRequest>) {
        let (requests, receiver) = async_channel::bounded(REQUEST_QUEUE_DEPTH);
        let state = Arc::new(TimePackageState {
            requests,
            active: AtomicBool::new(true),
        });
        (Self { state }, receiver)
    }
}

/// Creates the package without its clock bridge for cross-Plugin memory tests.
#[cfg(feature = "test-fixture")]
pub fn package_for_test() -> impl LuaPackage {
    TimePackage::new().0
}

impl Package for TimePackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let state = Arc::clone(&self.state);
        lua.extend_loaded_lib("os", move |os| {
            os.register_async("__time_now", move |(): ()| {
                let state = Arc::clone(&state);
                async move { Some(request_time(state).await) }
            })?;
            os.register_with("__time_from_table", time_from_table)?;
            os.register_with("__time_table", time_table)?;
            os.register("__time_format", |(format, timestamp): (String, i64)| {
                Some(format_time(&format, timestamp))
            })
        })?;
        lua.load(INSTALL_ADAPTER).exec()
    }
}

impl LuaPackage for TimePackage {
    fn name(&self) -> &'static str {
        "vm-time"
    }

    fn revoke(&self) {
        self.state.revoke();
    }
}

struct TimePackageState {
    requests: Sender<TimeRequest>,
    active: AtomicBool,
}

impl TimePackageState {
    fn revoke(&self) {
        self.active.store(false, Ordering::Release);
        self.requests.close();
    }
}

async fn request_time(state: Arc<TimePackageState>) -> Result<i64> {
    if !state.active.load(Ordering::Acquire) {
        return Err(Error::runtime("VM Time package has been revoked"));
    }
    let (response, receive) = oneshot::channel();
    state
        .requests
        .try_send(TimeRequest { response })
        .map_err(|error| {
            if error.is_closed() {
                Error::runtime("VM Time runtime is not available")
            } else {
                Error::runtime("VM Time request capacity is busy")
            }
        })?;
    match receive.await {
        Ok(Ok(timestamp)) => Ok(timestamp),
        Ok(Err(code)) => Err(Error::runtime(format!("UTC clock is {code}"))),
        Err(_) => Err(Error::runtime("VM Time runtime is not available")),
    }
}

fn time_from_table(_lua: &mut Context<'_>, value: Table) -> Option<Result<i64>> {
    Some(normalize_time_table(&value))
}

fn normalize_time_table(value: &Table) -> Result<i64> {
    let year = required_field(value, "year")?;
    let month = required_field(value, "month")?;
    let day = required_field(value, "day")?;
    let hour = optional_field(value, "hour", 12)?;
    let minute = optional_field(value, "min", 0)?;
    let second = optional_field(value, "sec", 0)?;

    let zero_based_month = month
        .checked_sub(1)
        .ok_or_else(|| Error::runtime("date value is out of range"))?;
    let total_months = year
        .checked_mul(12)
        .and_then(|months| months.checked_add(zero_based_month))
        .ok_or_else(|| Error::runtime("date value is out of range"))?;
    let normalized_year = total_months.div_euclid(12);
    let normalized_month = total_months.rem_euclid(12) + 1;
    let normalized_year =
        i32::try_from(normalized_year).map_err(|_| Error::runtime("date value is out of range"))?;
    let normalized_month = u8::try_from(normalized_month)
        .ok()
        .and_then(|month| Month::try_from(month).ok())
        .ok_or_else(|| Error::runtime("date value is out of range"))?;
    let date = Date::from_calendar_date(normalized_year, normalized_month, 1)
        .map_err(|_| Error::runtime("date value is out of range"))?;
    let offset_seconds = day
        .checked_sub(1)
        .and_then(|days| days.checked_mul(86_400))
        .and_then(|seconds| {
            hour.checked_mul(3_600)
                .and_then(|hour| seconds.checked_add(hour))
        })
        .and_then(|seconds| {
            minute
                .checked_mul(60)
                .and_then(|minute| seconds.checked_add(minute))
        })
        .and_then(|seconds| seconds.checked_add(second))
        .ok_or_else(|| Error::runtime("date value is out of range"))?;
    let date_time = PrimitiveDateTime::new(date, Time::MIDNIGHT)
        .checked_add(Duration::seconds(offset_seconds))
        .ok_or_else(|| Error::runtime("date value is out of range"))?;
    let calendar = date_time.assume_utc();
    update_time_table(value, calendar)?;
    Ok(calendar.unix_timestamp())
}

fn required_field(value: &Table, name: &'static str) -> Result<i64> {
    value
        .get::<_, Option<i64>>(name)?
        .ok_or_else(|| Error::runtime(format!("field '{name}' is missing in date table")))
}

fn optional_field(value: &Table, name: &'static str, default: i64) -> Result<i64> {
    Ok(value.get::<_, Option<i64>>(name)?.unwrap_or(default))
}

fn time_table(lua: &mut Context<'_>, timestamp: i64) -> Option<Result<Table>> {
    Some((|| {
        let calendar = calendar(timestamp)?;
        let table = lua.create_table()?;
        update_time_table(&table, calendar)?;
        Ok(table)
    })())
}

fn update_time_table(table: &Table, calendar: OffsetDateTime) -> Result<()> {
    table.set("year", i64::from(calendar.year()))?;
    table.set("month", i64::from(u8::from(calendar.month())))?;
    table.set("day", i64::from(calendar.day()))?;
    table.set("hour", i64::from(calendar.hour()))?;
    table.set("min", i64::from(calendar.minute()))?;
    table.set("sec", i64::from(calendar.second()))?;
    table.set(
        "wday",
        i64::from(calendar.weekday().number_days_from_sunday()) + 1,
    )?;
    table.set("yday", i64::from(calendar.ordinal()))?;
    table.set("isdst", false)
}

fn calendar(timestamp: i64) -> Result<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp(timestamp)
        .map_err(|_| Error::runtime("date value is out of range"))
}

fn format_time(format: &str, timestamp: i64) -> Result<String> {
    if format.len() > MAX_FORMAT_BYTES {
        return Err(Error::runtime("date format is too long"));
    }
    let calendar = calendar(timestamp)?;
    let mut output = String::new();
    let mut characters = format.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            output.push(character);
            continue;
        }
        let mut conversion = characters
            .next()
            .ok_or_else(|| Error::runtime("invalid date conversion specifier"))?;
        if conversion == 'E' || conversion == 'O' {
            conversion = characters
                .next()
                .ok_or_else(|| Error::runtime("invalid date conversion specifier"))?;
        }
        write_conversion(&mut output, conversion, calendar)?;
    }
    Ok(output)
}

fn write_conversion(output: &mut String, conversion: char, calendar: OffsetDateTime) -> Result<()> {
    let date = calendar.date();
    let hour12 = match calendar.hour() % 12 {
        0 => 12,
        hour => hour,
    };
    let (iso_year, iso_week, _) = date.to_iso_week_date();
    match conversion {
        '%' => {
            output.push('%');
            Ok(())
        }
        'a' => {
            output.push_str(short_weekday(calendar));
            Ok(())
        }
        'A' => {
            output.push_str(long_weekday(calendar));
            Ok(())
        }
        'b' | 'h' => {
            output.push_str(short_month(calendar));
            Ok(())
        }
        'B' => {
            output.push_str(long_month(calendar));
            Ok(())
        }
        'c' => write!(
            output,
            "{} {} {:2} {:02}:{:02}:{:02} {:04}",
            short_weekday(calendar),
            short_month(calendar),
            calendar.day(),
            calendar.hour(),
            calendar.minute(),
            calendar.second(),
            calendar.year()
        ),
        'C' => write!(output, "{:02}", calendar.year().div_euclid(100)),
        'd' => write!(output, "{:02}", calendar.day()),
        'D' | 'x' => write!(
            output,
            "{:02}/{:02}/{:02}",
            u8::from(calendar.month()),
            calendar.day(),
            calendar.year().rem_euclid(100)
        ),
        'e' => write!(output, "{:2}", calendar.day()),
        'F' => write!(
            output,
            "{:04}-{:02}-{:02}",
            calendar.year(),
            u8::from(calendar.month()),
            calendar.day()
        ),
        'g' => write!(output, "{:02}", iso_year.rem_euclid(100)),
        'G' => write!(output, "{iso_year:04}"),
        'H' => write!(output, "{:02}", calendar.hour()),
        'I' => write!(output, "{hour12:02}"),
        'j' => write!(output, "{:03}", calendar.ordinal()),
        'm' => write!(output, "{:02}", u8::from(calendar.month())),
        'M' => write!(output, "{:02}", calendar.minute()),
        'n' => {
            output.push('\n');
            Ok(())
        }
        'p' => {
            output.push_str(if calendar.hour() < 12 { "AM" } else { "PM" });
            Ok(())
        }
        'r' => write!(
            output,
            "{hour12:02}:{:02}:{:02} {}",
            calendar.minute(),
            calendar.second(),
            if calendar.hour() < 12 { "AM" } else { "PM" }
        ),
        'R' => write!(output, "{:02}:{:02}", calendar.hour(), calendar.minute()),
        'S' => write!(output, "{:02}", calendar.second()),
        't' => {
            output.push('\t');
            Ok(())
        }
        'T' | 'X' => write!(
            output,
            "{:02}:{:02}:{:02}",
            calendar.hour(),
            calendar.minute(),
            calendar.second()
        ),
        'u' => write!(output, "{}", calendar.weekday().number_from_monday()),
        'U' => write!(output, "{:02}", date.sunday_based_week()),
        'V' => write!(output, "{iso_week:02}"),
        'w' => write!(output, "{}", calendar.weekday().number_days_from_sunday()),
        'W' => write!(output, "{:02}", date.monday_based_week()),
        'y' => write!(output, "{:02}", calendar.year().rem_euclid(100)),
        'Y' => write!(output, "{:04}", calendar.year()),
        'z' => {
            output.push_str("+0000");
            Ok(())
        }
        'Z' => {
            output.push_str("UTC");
            Ok(())
        }
        _ => return Err(Error::runtime("invalid date conversion specifier")),
    }
    .map_err(|_| Error::runtime("failed to format date"))
}

fn short_weekday(calendar: OffsetDateTime) -> &'static str {
    match calendar.weekday().number_days_from_sunday() {
        0 => "Sun",
        1 => "Mon",
        2 => "Tue",
        3 => "Wed",
        4 => "Thu",
        5 => "Fri",
        _ => "Sat",
    }
}

fn long_weekday(calendar: OffsetDateTime) -> &'static str {
    match calendar.weekday().number_days_from_sunday() {
        0 => "Sunday",
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        _ => "Saturday",
    }
}

fn short_month(calendar: OffsetDateTime) -> &'static str {
    match u8::from(calendar.month()) {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        _ => "Dec",
    }
}

fn long_month(calendar: OffsetDateTime) -> &'static str {
    match u8::from(calendar.month()) {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        _ => "December",
    }
}

#[derive(Debug, thiserror::Error)]
#[error("VM Time runtime was not prepared during Plugin registration")]
struct TimeRuntimeUnavailable;

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::TimePackage;
    use barracuda_plugin::manager::PluginDeclaration;
    use barracuda_vm_plugin::{LuaPackage, Package, Result};
    use barracuda_vm_runtime::FixedMemoryLua;

    use super::VmTimePlugin;

    #[test]
    fn plugin_declares_time_and_vm_dependencies() {
        assert_eq!(VmTimePlugin::DEPENDS_ON, ["time", "vm"]);
    }

    #[test]
    fn exposes_standard_os_time_in_fixed_utc() -> Result<()> {
        futures_lite::future::block_on(async {
            let (package, requests) = TimePackage::new();
            let mut fixed = FixedMemoryLua::new(64 * 1024)
                .map_err(|error| barracuda_vm_plugin::Error::runtime(error.to_string()))?;
            let _io = barracuda_vm_builtin_packages::BuiltinPackages::new(
                barracuda_vm_builtin_packages::math::SeedSource::unavailable(),
            )
            .install(fixed.lua_mut())?;
            package.install(fixed.lua_mut())?;

            let execution = fixed
                .lua_mut()
                .load(
                    "assert(os == require('os')) \
                 assert(os.clock == nil and os.execute == nil) \
                 assert(os.time() == 1700000000) \
                 assert(os.date('%Y-%m-%d %H:%M:%S %z %Z', 0) == '1970-01-01 00:00:00 +0000 UTC') \
                 local value = os.date('*t', 0) \
                 assert(value.year == 1970 and value.month == 1 and value.day == 1) \
                 assert(value.hour == 0 and value.min == 0 and value.sec == 0) \
                 assert(value.wday == 5 and value.yday == 1 and value.isdst == false) \
                 assert(os.time({ year=1970, month=1, day=2, hour=0 }) == 86400) \
                 assert(os.time({ year=1970, month=1, day=32, hour=0 }) == 2678400) \
                 assert(not pcall(os.time, {year=1970, month=1, day=9223372036854775807})) \
                 assert(os.difftime(10, 3) == 7)",
                )
                .exec_async();
            let service = async {
                let request = requests
                    .recv()
                    .await
                    .map_err(|_| barracuda_vm_plugin::Error::runtime("missing clock request"))?;
                request.respond(Ok(1_700_000_000));
                Result::<()>::Ok(())
            };
            let (execution, service) = futures_lite::future::zip(execution, service).await;
            service?;
            execution
        })
    }

    #[test]
    fn revoked_time_package_rejects_existing_callbacks() -> Result<()> {
        futures_lite::future::block_on(async {
            let (package, _requests) = TimePackage::new();
            let mut fixed = FixedMemoryLua::new(64 * 1024)
                .map_err(|error| barracuda_vm_plugin::Error::runtime(error.to_string()))?;
            let _io = barracuda_vm_builtin_packages::BuiltinPackages::new(
                barracuda_vm_builtin_packages::math::SeedSource::unavailable(),
            )
            .install(fixed.lua_mut())?;
            package.install(fixed.lua_mut())?;
            package.revoke();
            fixed
                .lua_mut()
                .load("local ok = pcall(os.time); assert(not ok)")
                .exec_async()
                .await
        })
    }
}
