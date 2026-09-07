use barracuda_lua::{
    Environment, Error, ErrorKind, Function, Lua, LuaReturn, MetaMethod, Package, Result, Table,
    UserData, UserDataHandle, UserDataMethods, Variadic,
};

#[test]
fn lua_new_has_only_the_allowlist_sandbox_environment() -> Result<()> {
    let mut lua = Lua::new()?;
    let sandboxed: bool = lua
        .load(
            "return _G == _ENV and package == nil and io == nil and os == nil \
             and debug == nil and load == nil and loadfile == nil and dofile == nil \
             and collectgarbage == nil and warn == nil and print == nil \
             and getmetatable == nil and setmetatable == nil \
             and rawget == nil and rawset == nil and rawlen == nil and rawequal == nil \
             and not pcall(require, '_G') and not pcall(require, 'package') \
             and type(require) == 'function' and type(pcall) == 'function' \
             and type(tostring) == 'function' and type(select) == 'function'",
        )
        .eval()?;

    assert!(sandboxed);
    Ok(())
}

#[test]
fn lua_new_includes_safe_computation_standard_libraries() -> Result<()> {
    let mut lua = Lua::new()?;
    let libraries_work: bool = lua
        .load(
            "return string == require('string') \
             and table == require('table') \
             and math == require('math') \
             and utf8 == require('utf8') \
             and string.upper('barracuda') == 'BARRACUDA' \
             and table.concat({'bar', 'rac', 'uda'}) == 'barracuda' \
             and math.floor(4.2) == 4 \
             and utf8.len('汉字') == 2",
        )
        .eval()?;

    assert!(libraries_work);
    Ok(())
}

struct FlagPackage {
    name: &'static str,
}

impl Package for FlagPackage {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        lua.register_lib(self.name, |package| package.set("installed", true))
    }
}

const TEST_IO_INSTALL: &str = r##"
local io = require("io")
local emit = io.__emit
io.__emit = nil
function io.print(...)
    local line = ""
    for index = 1, select("#", ...) do
        if index > 1 then
            line = line .. "\t"
        end
        line = line .. tostring(select(index, ...))
    end
    emit(line)
end
"##;

struct TestIo {
    input: async_channel::Receiver<String>,
    output: async_channel::Sender<String>,
}

impl Package for TestIo {
    fn install(&self, lua: &mut Lua) -> Result<()> {
        let input = self.input.clone();
        let output = self.output.clone();
        lua.register_lib("io", move |package| {
            package.register_async("input", move |(): ()| {
                let input = input.clone();
                async move { Some(Ok(input.recv().await.ok())) }
            })?;
            package.register_async("__emit", move |line: String| {
                let output = output.clone();
                async move {
                    let _ = output.send(line).await;
                    None::<Result<()>>
                }
            })
        })?;
        lua.load(TEST_IO_INSTALL).exec()
    }
}

struct TestInput(async_channel::Sender<String>);

impl TestInput {
    async fn send(&self, value: impl Into<String>) -> Result<()> {
        self.0
            .send(value.into())
            .await
            .map_err(|_| Error::runtime("Lua input is closed"))
    }

    fn close(&self) {
        self.0.close();
    }
}

struct TestOutput(async_channel::Receiver<String>);

impl TestOutput {
    async fn next(&mut self) -> Option<String> {
        self.0.recv().await.ok()
    }
}

fn install_test_io(lua: &mut Lua) -> Result<(TestInput, TestOutput)> {
    let (input_sender, input_receiver) = async_channel::bounded(16);
    let (output_sender, output_receiver) = async_channel::bounded(16);
    let io = TestIo {
        input: input_receiver,
        output: output_sender,
    };
    Environment::new().with_package(io).install(lua)?;
    Ok((TestInput(input_sender), TestOutput(output_receiver)))
}

#[test]
fn environment_composes_external_packages_in_a_chain() -> Result<()> {
    let environment = Environment::new()
        .with_package(FlagPackage { name: "first" })
        .with_package(FlagPackage { name: "second" });
    let mut lua = Lua::new()?;

    environment.install(&mut lua)?;

    assert!(
        lua.load(
            "local first = require('first') \
             local second = require('second') \
             return first.installed and second.installed",
        )
        .eval::<bool>()?
    );
    Ok(())
}

#[test]
fn execution_environment_is_injected_after_sandbox_creation() -> Result<()> {
    let mut lua = Lua::new()?;
    assert!(
        lua.load("return input == nil and print == nil and not pcall(require, 'io')")
            .eval::<bool>()?
    );

    let (input, mut output) = install_test_io(&mut lua)?;
    assert!(
        lua.load(
            "local io = require('io') \
             return _G.io == nil \
                 and input == nil \
                 and print == nil \
                 and io == require('io') \
                 and io.__emit == nil \
                 and type(io.input) == 'function' \
                 and type(io.print) == 'function' \
                 and io.read == nil \
                 and io.write == nil",
        )
        .eval::<bool>()?
    );

    let completion = lua.run(
        "local io = require('io'); \
         local value = io.input(); \
         io.print('received', value)",
    );
    futures_lite::future::block_on(input.send("message"))?;
    input.close();
    futures_lite::future::block_on(completion)?;

    assert_eq!(
        futures_lite::future::block_on(output.next()),
        Some("received\tmessage".into())
    );
    assert_eq!(futures_lite::future::block_on(output.next()), None);
    Ok(())
}
use core::{
    alloc::{GlobalAlloc, Layout},
    cell::Cell,
    future::{Future, pending},
    pin::Pin,
    task::{Context, Poll, Waker},
};
use std::alloc::System;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingAllocator {
    allocations: AtomicUsize,
    deallocations: AtomicUsize,
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.allocations.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        self.deallocations.fetch_add(1, Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) };
    }
}

static LUA_ALLOCATOR: CountingAllocator = CountingAllocator {
    allocations: AtomicUsize::new(0),
    deallocations: AtomicUsize::new(0),
};

#[test]
fn instruction_hook_yields_cpu_bound_lua_back_to_the_executor() -> Result<()> {
    let hook_calls = Rc::new(Cell::new(0_u32));
    let mut lua = Lua::new()?;
    lua.set_instruction_hook(100, {
        let hook_calls = Rc::clone(&hook_calls);
        move || {
            hook_calls.set(hook_calls.get().saturating_add(1));
        }
    })?;

    let total = futures_lite::future::block_on(
        lua.load("local n=0; for i=1,10000 do n=n+i end; return n")
            .eval_async::<i64>(),
    )?;

    assert_eq!(total, 50_005_000);
    assert_ne!(hook_calls.get(), 0);
    Ok(())
}

#[test]
fn lua_uses_the_external_rust_allocator() -> Result<()> {
    let allocations_before = LUA_ALLOCATOR.allocations.load(Ordering::Relaxed);
    let deallocations_before = LUA_ALLOCATOR.deallocations.load(Ordering::Relaxed);
    let mut lua = unsafe { Lua::new_with_allocator(&raw const LUA_ALLOCATOR) }?;

    assert_eq!(lua.load("return 42").eval::<i64>()?, 42);
    drop(lua);

    assert!(LUA_ALLOCATOR.allocations.load(Ordering::Relaxed) > allocations_before);
    assert!(LUA_ALLOCATOR.deallocations.load(Ordering::Relaxed) > deallocations_before);
    Ok(())
}

#[test]
fn binds_sync_rust_function_with_mlua_shaped_api() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.register("add", |(a, b): (i64, i64)| Some(Ok(a + b)))?;

    let result: i64 = lua.load("return add(20, 22)").eval()?;
    assert_eq!(result, 42);
    Ok(())
}

#[test]
fn binds_native_async_rust_function_and_drives_it_from_lua() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.register_async("double", |value: i64| async move {
        futures_lite::future::yield_now().await;
        Some(Ok(value * 2))
    })?;

    let result: i64 = futures_lite::future::block_on(lua.load("return double(21)").eval_async())?;
    assert_eq!(result, 42);
    Ok(())
}

#[test]
fn supports_zero_arguments_and_multiple_returns() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.register_async("pair", |(): ()| async { Some(Ok((true, 7_i64))) })?;

    let result: (bool, i64) =
        futures_lite::future::block_on(lua.load("return pair()").eval_async())?;
    assert_eq!(result, (true, 7));
    Ok(())
}

#[test]
fn maps_async_native_errors_to_nil_and_error_values() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.register_async("fail", |(): ()| async {
        Some(Err::<(), _>(Error::runtime("native failed")))
    })?;

    let result: (bool, bool, String) = futures_lite::future::block_on(
        lua.load("local ok, value, err = pcall(fail); return ok, value == nil, err")
            .eval_async(),
    )?;
    assert_eq!(result, (true, true, "native failed".into()));
    Ok(())
}

#[test]
fn maps_sync_native_errors_to_values_but_raises_argument_errors() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.register("fail", |(): ()| -> LuaReturn<()> {
        Some(Err(Error::runtime("sync failed")))
    })?;
    lua.register("integer", |value: i64| Some(Ok(value)))?;

    let result: (bool, bool, String, bool, String) = lua
        .load(
            "local ok1, value1, err1 = pcall(fail); \
             local ok2, err2 = pcall(integer, 'wrong'); \
             return ok1, value1 == nil, err1, ok2, err2",
        )
        .eval()?;
    assert_eq!(
        result,
        (
            true,
            true,
            "sync failed".into(),
            false,
            "expected integer".into()
        )
    );
    Ok(())
}

#[test]
fn dropping_execution_cancels_the_native_future() -> Result<()> {
    struct DropFlag(Rc<Cell<bool>>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    let dropped = Rc::new(Cell::new(false));
    let mut lua = Lua::new()?;
    lua.register_async("wait", {
        let dropped = Rc::clone(&dropped);
        move |(): ()| {
            let guard = DropFlag(Rc::clone(&dropped));
            async move {
                let _guard = guard;
                pending::<()>().await;
                None::<core::result::Result<(), Error>>
            }
        }
    })?;

    let mut execution = lua.load("return wait()").eval_async::<()>();
    assert!(matches!(
        Pin::new(&mut execution).poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    assert!(!dropped.get());
    drop(execution);
    assert!(dropped.get());
    Ok(())
}

#[test]
fn registers_values_and_functions_directly() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.set("answer", 40_i64)?;
    lua.set("name", "barracuda")?;
    lua.register("add", |(a, b): (i64, i64)| Some(Ok(a + b)))?;
    lua.register_async("double", |value: i64| async move { Some(Ok(value * 2)) })?;

    let result: (String, i64) = futures_lite::future::block_on(
        lua.load("return name, double(add(answer, 2))").eval_async(),
    )?;
    assert_eq!(result, ("barracuda".into(), 84));
    Ok(())
}

#[test]
fn registers_a_lazy_require_only_library() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.register_lib("native", |lib| {
        lib.set("name", "barracuda")?;
        lib.register("add", |(a, b): (i64, i64)| Some(Ok(a + b)))?;
        lib.register_async("double", |value: i64| async move { Some(Ok(value * 2)) })?;
        Ok(())
    })?;

    let before: (bool, bool) = lua.load("return native == nil, package == nil").eval()?;
    assert_eq!(before, (true, true));

    let result: (bool, String, i64) = futures_lite::future::block_on(
        lua.load(
            "local first = require('native'); \
             local second = require('native'); \
             return first == second, first.name, first.double(first.add(20, 1))",
        )
        .eval_async(),
    )?;
    assert_eq!(result, (true, "barracuda".into(), 42));
    Ok(())
}

#[test]
fn libraries_can_contain_nested_library_tables() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.register_lib("thread", |lib| {
        lib.table("sync", |sync| {
            sync.register("ready", |(): ()| Some(Ok(true)))
        })
    })?;

    let result: bool = lua
        .load("local thread = require('thread'); return thread.sync.ready()")
        .eval()?;
    assert!(result);
    Ok(())
}

#[test]
fn distinguishes_no_return_from_nil_and_success_values() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.register("no_return", |(): ()| -> LuaReturn<()> { None })?;
    lua.register("nil_value", |(): ()| -> LuaReturn<Option<i64>> {
        Some(Ok(None))
    })?;
    lua.register("answer", |(): ()| Some(Ok(42_i64)))?;

    let result: (i64, i64, bool, i64) = lua
        .load(
            "return select('#', no_return()), select('#', nil_value()), \
                    nil_value() == nil, answer()",
        )
        .eval()?;
    assert_eq!(result, (0, 1, true, 42));
    Ok(())
}

#[test]
fn receives_creates_and_returns_tables_without_a_value_enum() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.register("read_options", |options: Table| {
        Some(options.get::<_, i64>("timeout"))
    })?;
    lua.register_with("make_info", |lua, name: String| {
        Some(lua.create_table().and_then(|table| {
            table.set("name", name)?;
            table.set("ready", true)?;
            Ok(table)
        }))
    })?;

    let result: (i64, String, bool) = lua
        .load(
            "local info = make_info('barracuda'); \
             return read_options({ timeout = 250 }), info.name, info.ready",
        )
        .eval()?;
    assert_eq!(result, (250, "barracuda".into(), true));
    Ok(())
}

#[test]
fn stores_lua_functions_in_the_registry_and_calls_them_later() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.register_with("save", |lua, callback: Function| {
        Some(lua.create_registry_value(callback).and_then(|key| {
            let callback: Function = lua.registry_value(&key)?;
            let result: i64 = callback.call((20_i64, 22_i64))?;
            lua.remove_registry_value(key)?;
            Ok(result)
        }))
    })?;

    let result: i64 = lua
        .load("return save(function(a, b) return a + b end)")
        .eval()?;
    assert_eq!(result, 42);
    Ok(())
}

#[test]
fn saved_lua_functions_can_yield_through_native_async_calls() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.register_async("double", |value: i64| async move {
        futures_lite::future::yield_now().await;
        Some(Ok(value * 2))
    })?;
    let callback: Function = lua
        .load("return function(value) return double(value) end")
        .eval()?;

    let result: i64 = futures_lite::future::block_on(callback.call_async(21_i64))?;
    assert_eq!(result, 42);
    Ok(())
}

#[test]
fn pending_function_calls_fail_cleanly_after_the_lua_state_is_dropped() -> Result<()> {
    let mut lua = Lua::new()?;
    let callback: Function = lua.load("return function() return 42 end").eval()?;
    let call = callback.call_async::<(), i64>(());
    drop(lua);

    let error = futures_lite::future::block_on(call).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Runtime);
    assert_eq!(error.message(), "Lua state has been dropped");
    Ok(())
}

#[test]
fn rejects_registry_values_from_another_lua_state() -> Result<()> {
    let mut first = Lua::new()?;
    let mut second = Lua::new()?;
    let key = first.create_registry_value("first")?;

    let error = second.registry_value::<String>(&key).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Runtime);
    Ok(())
}

#[test]
fn exposes_typed_rust_userdata_with_methods_and_drop() -> Result<()> {
    struct Counter {
        value: i64,
        dropped: Rc<Cell<bool>>,
    }

    impl Drop for Counter {
        fn drop(&mut self) {
            self.dropped.set(true);
        }
    }

    impl UserData for Counter {
        fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
            methods.add_method("get", |counter, (): ()| Some(Ok(counter.value)));
            methods.add_method_mut("add", |counter, amount: i64| {
                counter.value += amount;
                Some(Ok(counter.value))
            });
            methods.add_meta_method(MetaMethod::ToString, |counter, (): ()| {
                Some(Ok(format!("counter:{}", counter.value)))
            });
        }
    }

    let dropped = Rc::new(Cell::new(false));
    let mut lua = Lua::new()?;
    lua.register_with("new_counter", {
        let dropped = Rc::clone(&dropped);
        move |lua, value: i64| {
            Some(lua.create_userdata(Counter {
                value,
                dropped: Rc::clone(&dropped),
            }))
        }
    })?;

    let result: (i64, i64, String) = lua
        .load(
            "local counter = new_counter(10); \
             return counter:get(), counter:add(5), tostring(counter)",
        )
        .eval()?;
    assert_eq!(result, (10, 15, "counter:15".into()));
    assert!(!dropped.get());

    drop(lua);
    assert!(dropped.get());
    Ok(())
}

#[test]
fn async_userdata_methods_use_the_same_lua_return_contract() -> Result<()> {
    struct Device(i64);

    impl UserData for Device {
        fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
            methods.add_async_method("read", |device, amount: i64| async move {
                futures_lite::future::yield_now().await;
                let value = device.with(|device| device.0 + amount);
                Some(value)
            });
        }
    }

    let mut lua = Lua::new()?;
    lua.register_with("new_device", |lua, base: i64| {
        Some(lua.create_userdata(Device(base)))
    })?;

    let result: i64 = futures_lite::future::block_on(
        lua.load("local device = new_device(40); return device:read(2)")
            .eval_async(),
    )?;
    assert_eq!(result, 42);
    Ok(())
}

#[test]
fn userdata_close_metamethod_can_mutate_the_resource() -> Result<()> {
    struct Resource(bool);

    impl UserData for Resource {
        fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
            methods.add_method("is_closed", |resource, (): ()| Some(Ok(resource.0)));
            methods.add_meta_method_mut(MetaMethod::Close, |resource, _error: Option<String>| {
                resource.0 = true;
                None::<Result<()>>
            });
        }
    }

    let mut lua = Lua::new()?;
    lua.register_with("new_resource", |lua, (): ()| {
        Some(lua.create_userdata(Resource(false)))
    })?;

    let closed: bool = lua
        .load(
            "local resource; do local scoped <close> = new_resource(); resource = scoped end; \
             return resource:is_closed()",
        )
        .eval()?;
    assert!(closed);
    Ok(())
}

#[test]
fn failed_userdata_type_registration_is_atomic() -> Result<()> {
    struct Broken;

    impl UserData for Broken {
        fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
            methods.add_method("bad\0name", |_value, (): ()| None::<Result<()>>);
            methods.add_method("ignored", |_value, (): ()| None::<Result<()>>);
        }
    }

    let mut lua = Lua::new()?;
    lua.register_with("new_broken", |lua, (): ()| {
        Some(lua.create_userdata(Broken))
    })?;

    let result: (bool, String, bool, String) = lua
        .load(
            "local first, first_err = new_broken(); \
             local second, second_err = new_broken(); \
             return first == nil, first_err, second == nil, second_err",
        )
        .eval()?;
    assert!(result.0);
    assert!(result.1.contains("contains NUL"));
    assert!(result.2);
    assert!(result.3.contains("contains NUL"));
    Ok(())
}

#[test]
fn async_functions_can_prepare_owned_lua_values_through_context() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.register_async_with("async_info", |lua, value: i64| {
        let table = lua.create_table();
        async move {
            futures_lite::future::yield_now().await;
            Some(table.and_then(|table| {
                table.set("value", value)?;
                Ok(table)
            }))
        }
    })?;

    let result: i64 = futures_lite::future::block_on(
        lua.load("local info = async_info(42); return info.value")
            .eval_async(),
    )?;
    assert_eq!(result, 42);
    Ok(())
}

#[test]
fn supports_typed_variadic_arguments_and_results() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.register("sum", |values: Variadic<i64>| {
        Some(Ok(values.iter().copied().sum::<i64>()))
    })?;
    lua.register("sequence", |(): ()| {
        Some(Ok(Variadic::from(vec![1_i64, 2, 3])))
    })?;

    let result: (i64, i64, i64, i64) = lua
        .load("local a, b, c = sequence(); return sum(10, 20, 12), a, b, c")
        .eval()?;
    assert_eq!(result, (42, 1, 2, 3));
    Ok(())
}

#[test]
fn failed_library_registration_is_atomic() -> Result<()> {
    let mut lua = Lua::new()?;
    let result = lua.register_lib("broken", |lib| {
        lib.set("partial", true)?;
        Err(barracuda_lua::Error::runtime("stop"))
    });
    assert!(result.is_err());

    assert!(lua.load("return broken == nil").eval::<bool>()?);
    Ok(())
}

#[test]
fn converts_supported_scalars_and_tuples_directly_on_the_stack() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.set("flag", true)?;
    lua.set("ratio", 1.5_f64)?;
    lua.set("text", String::from("hello"))?;
    lua.set("bytes", vec![0xff, 0x00, 0x7f])?;
    lua.register("echo3", |values: (bool, f64, String)| Some(Ok(values)))?;
    lua.register("echo4", |values: (bool, i64, f64, Vec<u8>)| {
        Some(Ok(values))
    })?;

    let first: (bool, f64, String) = lua.load("return echo3(flag, ratio, text)").eval()?;
    assert_eq!(first, (true, 1.5, "hello".into()));

    let second: (bool, i64, f64, Vec<u8>) =
        lua.load("return echo4(false, 7, 2.5, bytes)").eval()?;
    assert_eq!(second, (false, 7, 2.5, vec![0xff, 0x00, 0x7f]));
    Ok(())
}

#[test]
fn reports_load_runtime_conversion_and_unexpected_yield_errors() -> Result<()> {
    let mut lua = Lua::new()?;

    let load = lua.load("this is not lua").eval::<()>().unwrap_err();
    assert_eq!(load.kind(), ErrorKind::Load);
    assert!(!load.message().is_empty());
    assert_eq!(load.to_string(), load.message());

    let runtime = lua.load("error('boom')").eval::<()>().unwrap_err();
    assert_eq!(runtime.kind(), ErrorKind::Runtime);
    assert!(runtime.message().contains("boom"));

    let conversion = lua
        .load("return 'not utf8: \\255'")
        .eval::<String>()
        .unwrap_err();
    assert_eq!(conversion.kind(), ErrorKind::Conversion);

    lua.register_async("wait_forever", |(): ()| async {
        pending::<()>().await;
        None::<core::result::Result<(), Error>>
    })?;
    let yielded = lua.load("return wait_forever()").eval::<()>().unwrap_err();
    assert_eq!(yielded.kind(), ErrorKind::UnexpectedYield);
    Ok(())
}

#[test]
fn rejects_nul_in_registered_names() -> Result<()> {
    let mut lua = Lua::new()?;
    assert!(lua.set("bad\0name", true).is_err());
    assert!(
        lua.register("bad\0name", |(): ()| -> LuaReturn<()> { None })
            .is_err()
    );
    assert!(
        lua.register_async("bad\0name", |(): ()| async { None::<Result<()>> })
            .is_err()
    );
    assert!(lua.register_lib("bad\0name", |_| Ok(())).is_err());
    assert!(
        lua.register_lib("native", |lib| {
            lib.set("bad\0name", true)?;
            Ok(())
        })
        .is_err()
    );
    Ok(())
}

#[test]
fn exec_variants_and_completed_execution_are_well_defined() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.load("local value = 1; return value").exec()?;
    let async_result: Result<()> =
        futures_lite::future::block_on(lua.load("local value = 2; return value").exec_async());
    async_result?;

    let error = lua.load("return 1").eval::<()>().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Conversion);

    lua.register_async("wait_forever", |(): ()| async {
        pending::<()>().await;
        None::<Result<()>>
    })?;
    let error = lua.load("wait_forever()").exec().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UnexpectedYield);

    let mut execution = lua.load("return 7").eval_async::<i64>();
    let mut execution = Pin::new(&mut execution);
    assert!(matches!(
        execution
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(Ok(7))
    ));
    let second = execution
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()));
    assert!(matches!(second, Poll::Ready(Err(error)) if error.kind() == ErrorKind::Runtime));

    let lua = Lua::new()?;
    let mut execution = lua.run("return 7");
    assert!(matches!(
        Pin::new(&mut execution).poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(Ok(()))
    ));
    assert!(matches!(
        Pin::new(&mut execution).poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(Err(error)) if error.kind() == ErrorKind::Runtime
    ));
    Ok(())
}

#[test]
fn lua_exposes_input_output_and_execution_as_separate_flows() -> Result<()> {
    let mut lua = Lua::new()?;
    let (input, mut output) = install_test_io(&mut lua)?;
    let execution =
        lua.run("local io = require('io'); local name = io.input(); io.print('hello', name); return 'ignored top-level value'");

    futures_lite::future::block_on(input.send("agent"))?;
    input.close();
    futures_lite::future::block_on(execution)?;

    assert_eq!(
        futures_lite::future::block_on(output.next()),
        Some("hello\tagent".into())
    );
    assert_eq!(futures_lite::future::block_on(output.next()), None);
    Ok(())
}

#[test]
fn input_waits_asynchronously_and_closed_input_becomes_nil() -> Result<()> {
    let mut lua = Lua::new()?;
    let (input, mut output) = install_test_io(&mut lua)?;
    let mut execution =
        lua.run("local io = require('io'); local first = io.input(); io.print(first); local eof = io.input(); io.print(eof == nil)");

    assert!(matches!(
        Pin::new(&mut execution).poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));

    futures_lite::future::block_on(input.send(String::from("first")))?;
    input.close();
    futures_lite::future::block_on(execution)?;

    assert_eq!(
        futures_lite::future::block_on(output.next()),
        Some("first".into())
    );
    assert_eq!(
        futures_lite::future::block_on(output.next()),
        Some("true".into())
    );
    assert_eq!(futures_lite::future::block_on(output.next()), None);
    Ok(())
}

#[test]
fn execution_errors_do_not_discard_buffered_output() -> Result<()> {
    let mut lua = Lua::new()?;
    let (_input, mut output) = install_test_io(&mut lua)?;
    let execution =
        lua.run("local io = require('io'); io.print('before failure'); error('script failed')");

    let error = futures_lite::future::block_on(execution).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Runtime);
    assert!(error.message().contains("script failed"));
    assert_eq!(
        futures_lite::future::block_on(output.next()),
        Some("before failure".into())
    );
    assert_eq!(futures_lite::future::block_on(output.next()), None);
    Ok(())
}

#[test]
fn lua_runs_with_registered_libraries() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.register_lib("native", |lib| {
        lib.register_async("double", |value: i64| async move { Some(Ok(value * 2)) })
    })?;
    let (_input, mut output) = install_test_io(&mut lua)?;
    let execution = lua.run(
        "local io = require('io'); local native = require('native'); io.print(native.double(21))",
    );

    futures_lite::future::block_on(execution)?;
    assert_eq!(
        futures_lite::future::block_on(output.next()),
        Some("42".into())
    );
    Ok(())
}

#[test]
fn dropping_output_discards_prints_without_failing_the_script() -> Result<()> {
    let mut lua = Lua::new()?;
    let (_input, output) = install_test_io(&mut lua)?;
    let execution =
        lua.run("local io = require('io'); io.print('ignored'); io.print('also ignored')");
    drop(output);

    futures_lite::future::block_on(execution)?;
    Ok(())
}

#[test]
fn dropping_execution_closes_both_data_flows() -> Result<()> {
    let mut lua = Lua::new()?;
    let (input, mut output) = install_test_io(&mut lua)?;
    let execution = lua.run("local io = require('io'); io.input()");
    drop(execution);

    let send_error = futures_lite::future::block_on(input.send("late")).unwrap_err();
    assert_eq!(send_error.kind(), ErrorKind::Runtime);
    assert_eq!(futures_lite::future::block_on(output.next()), None);
    Ok(())
}

#[test]
fn lua_defers_script_load_errors_to_execution() -> Result<()> {
    let mut lua = Lua::new()?;
    let (_input, mut output) = install_test_io(&mut lua)?;
    let execution = lua.run("this is not lua");

    let error = futures_lite::future::block_on(execution).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Load);
    assert_eq!(futures_lite::future::block_on(output.next()), None);
    Ok(())
}

#[test]
fn full_output_buffer_yields_until_the_host_consumes_a_message() -> Result<()> {
    let mut lua = Lua::new()?;
    let (_input, mut output) = install_test_io(&mut lua)?;
    let mut execution =
        lua.run("local io = require('io'); for value = 1, 17 do io.print(value) end");

    assert!(matches!(
        Pin::new(&mut execution).poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    assert_eq!(
        futures_lite::future::block_on(output.next()),
        Some("1".into())
    );
    assert!(matches!(
        Pin::new(&mut execution).poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(Ok(()))
    ));

    for value in 2..=17 {
        assert_eq!(
            futures_lite::future::block_on(output.next()),
            Some(value.to_string())
        );
    }
    assert_eq!(futures_lite::future::block_on(output.next()), None);
    Ok(())
}

#[test]
fn conversion_errors_cover_each_supported_shape() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.register("boolean", |value: bool| Some(Ok(value)))?;
    lua.register("number", |value: f64| Some(Ok(value)))?;
    lua.register("text", |value: String| Some(Ok(value)))?;
    lua.register("bytes", |value: Vec<u8>| Some(Ok(value)))?;
    lua.register("pair", |value: (i64, i64)| Some(Ok(value)))?;
    lua.register("nothing", |(): ()| None::<Result<()>>)?;
    lua.register("optional", |value: Option<i64>| {
        Some(Ok(value.unwrap_or_default()))
    })?;

    for source in [
        "boolean(1)",
        "number(false)",
        "text({})",
        "bytes({})",
        "pair(1)",
        "nothing(1)",
    ] {
        let error = lua.load(source).exec().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Runtime);
    }
    assert_eq!(lua.load("return optional(7)").eval::<i64>()?, 7);
    assert_eq!(
        lua.load("return").eval::<i64>().unwrap_err().kind(),
        ErrorKind::Conversion
    );

    let mut values = Variadic::from(vec![1_i64, 2]);
    values[0] = 3;
    let values: Vec<_> = values.into();
    assert_eq!(values, vec![3, 2]);
    Ok(())
}

#[test]
fn every_tuple_arity_rejects_the_wrong_number_of_values() -> Result<()> {
    let mut lua = Lua::new()?;
    macro_rules! rejects_one_value {
        ($tuple:ty) => {
            assert_eq!(
                lua.load("return 1").eval::<$tuple>().unwrap_err().kind(),
                ErrorKind::Conversion
            );
        };
    }

    rejects_one_value!((i64, i64, i64));
    rejects_one_value!((i64, i64, i64, i64));
    rejects_one_value!((i64, i64, i64, i64, i64));
    rejects_one_value!((i64, i64, i64, i64, i64, i64));
    rejects_one_value!((i64, i64, i64, i64, i64, i64, i64));
    rejects_one_value!((i64, i64, i64, i64, i64, i64, i64, i64));
    rejects_one_value!((i64, i64, i64, i64, i64, i64, i64, i64, i64));
    rejects_one_value!((i64, i64, i64, i64, i64, i64, i64, i64, i64, i64));
    rejects_one_value!((i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64));
    rejects_one_value!((i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64));
    Ok(())
}

#[test]
fn scalar_results_reject_zero_values_for_each_core_type() -> Result<()> {
    let mut lua = Lua::new()?;
    assert!(lua.load("return").eval::<bool>().is_err());
    assert!(lua.load("return").eval::<f64>().is_err());
    assert!(lua.load("return").eval::<String>().is_err());
    assert!(lua.load("return").eval::<Vec<u8>>().is_err());
    assert!(lua.load("return").eval::<Table>().is_err());
    assert!(lua.load("return").eval::<Function>().is_err());
    Ok(())
}

#[test]
fn tables_report_length_emptiness_and_type_errors() -> Result<()> {
    let mut lua = Lua::new()?;
    let table: Table = lua.load("return { 10, 20 }").eval()?;
    assert_eq!(table.len()?, 2);
    assert!(!table.is_empty()?);

    let empty: Table = lua.load("return {}").eval()?;
    assert!(empty.is_empty()?);

    lua.register("table_only", |table: Table| {
        Some(table.len().map(|length| length as i64))
    })?;
    let error = lua.load("table_only(42)").exec().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Runtime);
    Ok(())
}

#[test]
fn functions_report_type_runtime_and_completed_call_errors() -> Result<()> {
    let mut lua = Lua::new()?;
    lua.register("function_only", |function: Function| {
        Some(function.call::<_, ()>(()))
    })?;
    assert_eq!(
        lua.load("function_only(42)").exec().unwrap_err().kind(),
        ErrorKind::Runtime
    );

    let failing: Function = lua
        .load("return function() error('callback failed') end")
        .eval()?;
    let error = failing.call::<_, ()>(()).unwrap_err();
    assert!(error.message().contains("callback failed"));

    let callback: Function = lua.load("return function() return 42 end").eval()?;
    let mut call = callback.call_async::<_, i64>(());
    assert!(matches!(
        Pin::new(&mut call).poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(Ok(42))
    ));
    assert!(matches!(
        Pin::new(&mut call).poll(&mut Context::from_waker(Waker::noop())),
        Poll::Ready(Err(error)) if error.kind() == ErrorKind::Runtime
    ));
    Ok(())
}

#[test]
fn function_call_created_after_lua_drop_fails_cleanly() -> Result<()> {
    let mut lua = Lua::new()?;
    let callback: Function = lua.load("return function() return 42 end").eval()?;
    drop(lua);

    let error = futures_lite::future::block_on(callback.call_async::<_, i64>(())).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Runtime);
    Ok(())
}

#[test]
fn async_function_call_rejects_arguments_from_another_vm() -> Result<()> {
    let mut lua = Lua::new()?;
    let callback: Function = lua.load("return function(value) return value end").eval()?;
    let mut foreign = Lua::new()?;
    let table: Table = foreign.load("return {}").eval()?;

    let error = match futures_lite::future::block_on(callback.call_async::<_, Table>(table)) {
        Ok(_) => panic!("foreign argument unexpectedly accepted"),
        Err(error) => error,
    };
    assert!(error.message().contains("different state"));
    Ok(())
}

#[test]
fn registry_removal_rejects_foreign_state() -> Result<()> {
    let mut first = Lua::new()?;
    let mut second = Lua::new()?;
    let key = first.create_registry_value("first")?;

    let error = second.remove_registry_value(key).unwrap_err();
    assert!(error.message().contains("different Lua state"));
    Ok(())
}

#[test]
fn nested_library_failures_are_atomic_and_with_variants_work() -> Result<()> {
    let mut lua = Lua::new()?;
    let error = lua
        .register_lib("broken", |lib| {
            lib.table("nested", |nested| {
                nested.register("partial", |(): ()| Some(Ok(true)))?;
                Err(Error::runtime("nested failed"))
            })
        })
        .unwrap_err();
    assert_eq!(error.message(), "nested failed");

    lua.register_lib("native", |lib| {
        lib.register_with("table", |lua, value: i64| {
            Some(lua.create_table().and_then(|table| {
                table.set("value", value)?;
                Ok(table)
            }))
        })?;
        lib.register_async_with("async_table", |lua, value: i64| {
            let table = lua.create_table();
            async move {
                Some(table.and_then(|table| {
                    table.set("value", value)?;
                    Ok(table)
                }))
            }
        })
    })?;
    let values: (i64, i64) = futures_lite::future::block_on(
        lua.load("local n = require('native'); return n.table(20).value, n.async_table(22).value")
            .eval_async(),
    )?;
    assert_eq!(values, (20, 22));
    Ok(())
}

#[test]
fn userdata_checks_self_type_equality_and_runtime_borrows() -> Result<()> {
    struct Counter(i64);

    impl UserData for Counter {
        fn add_methods(methods: &mut UserDataMethods<'_, Self>) {
            methods.add_method("get", |counter, (): ()| Some(Ok(counter.0)));
            methods.add_method_mut("set", |counter, value: i64| {
                counter.0 = value;
                None::<Result<()>>
            });
            methods.add_async_method("async_get", |counter, (): ()| async move {
                Some(counter.with(|counter| counter.0))
            });
            methods.add_meta_method(MetaMethod::Eq, |left, right: UserDataHandle<Counter>| {
                Some(right.with(|right| left.0 == right.0))
            });
            methods.add_meta_method_mut(MetaMethod::Close, |counter, _error: Option<String>| {
                counter.0 = 0;
                None::<Result<()>>
            });
        }
    }

    struct DefaultMethods;
    impl UserData for DefaultMethods {}

    let mut lua = Lua::new()?;
    lua.register_with("counter", |lua, value: i64| {
        Some(lua.create_userdata(Counter(value)))
    })?;
    lua.register_with("default_userdata", |lua, (): ()| {
        Some(lua.create_userdata(DefaultMethods))
    })?;

    let handle: UserDataHandle<Counter> = lua.load("return counter(1)").eval()?;
    {
        let mut value = handle.borrow_mut()?;
        value.0 = 2;
        assert!(handle.borrow().is_err());
        assert!(handle.borrow_mut().is_err());
    }
    handle.with_mut(|value| value.0 = 3)?;
    let shared = handle.borrow()?;
    assert!(handle.borrow_mut().is_err());
    drop(shared);
    assert_eq!(handle.with(|value| value.0)?, 3);

    let result: (bool, bool) = lua
        .load(
            "local a, b = counter(3), counter(3); default_userdata(); return a == b, a:get() == 3",
        )
        .eval()?;
    assert_eq!(result, (true, true));

    for source in [
        "local c = counter(1); c.get()",
        "local c = counter(1); c.set()",
        "local c = counter(1); c.async_get()",
        "local c = counter(1); c.get(42)",
    ] {
        let error = futures_lite::future::block_on(lua.load(source).exec_async()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Runtime);
    }

    Ok(())
}

#[test]
fn async_function_calls_cover_error_cancellation_and_foreign_values() -> Result<()> {
    let mut lua = Lua::new()?;
    let failing: Function = lua
        .load("return function() error('async callback failed') end")
        .eval()?;
    let error = futures_lite::future::block_on(failing.call_async::<_, ()>(())).unwrap_err();
    assert!(error.message().contains("async callback failed"));

    lua.register_async("wait_forever", |(): ()| async {
        pending::<()>().await;
        None::<Result<()>>
    })?;
    let pending_callback: Function = lua
        .load("return function() return wait_forever() end")
        .eval()?;
    let mut call = pending_callback.call_async::<_, ()>(());
    assert!(matches!(
        Pin::new(&mut call).poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    ));
    drop(call);

    let mut foreign = Lua::new()?;
    let table: Table = foreign.load("return { value = 42 }").eval()?;
    let table = Rc::new(core::cell::RefCell::new(Some(table)));
    lua.register_async("foreign_table", {
        let table = Rc::clone(&table);
        move |(): ()| {
            let table = table.borrow_mut().take();
            async move { Some(table.ok_or_else(|| Error::runtime("already taken"))) }
        }
    })?;
    let callback: Function = lua
        .load("return function() return foreign_table() end")
        .eval()?;
    let error = match futures_lite::future::block_on(callback.call_async::<_, Table>(())) {
        Ok(_) => panic!("foreign table unexpectedly returned"),
        Err(error) => error,
    };
    assert!(error.message().contains("different state"));
    Ok(())
}

#[test]
fn execution_rejects_async_values_owned_by_another_vm() -> Result<()> {
    let mut lua = Lua::new()?;
    let mut foreign = Lua::new()?;
    let table: Table = foreign.load("return {}").eval()?;
    let table = Rc::new(core::cell::RefCell::new(Some(table)));
    lua.register_async("foreign_table", {
        let table = Rc::clone(&table);
        move |(): ()| {
            let table = table.borrow_mut().take();
            async move { Some(table.ok_or_else(|| Error::runtime("already taken"))) }
        }
    })?;

    let error = futures_lite::future::block_on(lua.load("return foreign_table()").exec_async())
        .unwrap_err();
    assert!(error.message().contains("different state"));
    Ok(())
}
