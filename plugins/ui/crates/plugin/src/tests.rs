#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

extern crate std;

use crate::service::{DEFAULT_VIEWPORT, UiAssets, UiService, UiSession};
use crate::view::{LuaView, ViewChannel};
use alloc::{format, string::String, vec::Vec};
use barracuda_vm_plugin::Lua;
use futures_lite::future;

const VIEW: &[u8] = include_bytes!("../../runtime/tests/fixtures/counter.bvir");
const STYLES: &[u8] = include_bytes!("../../runtime/tests/fixtures/counter.styles.bin");

fn assets() -> UiAssets {
    UiAssets {
        view: VIEW.to_vec(),
        styles: STYLES.to_vec(),
        fonts: Vec::new(),
    }
}

/// A Lua state whose global `view` is the session's view.
fn lua_with_view(session: &UiSession) -> Lua {
    let mut lua = Lua::new().expect("lua");
    let channel = session.channel().clone();
    lua.register_with("get_view", move |lua, (): ()| {
        Some(lua.create_userdata(LuaView(channel.clone())))
    })
    .expect("register");
    lua.load("view = get_view()").exec().expect("view");
    lua
}

fn eval<T: barracuda_lua::FromLuaMulti>(lua: &mut Lua, source: &str) -> T {
    future::block_on(lua.load(source).eval_async::<T>()).expect(source)
}

fn next_call(channel: &ViewChannel) -> Option<(String, Vec<String>)> {
    future::block_on(channel.next()).map(|call| {
        let args = call
            .args
            .into_iter()
            .map(|value| match value {
                barracuda_vm_plugin::LuaValue::Integer(value) => format!("{value}"),
                _ => String::from("?"),
            })
            .collect();
        (call.handler, args)
    })
}

/// Finds a point that presses `handler`; every probe that hits queues a call.
/// "Reset" sits right of "+1", so it is searched for from the right.
fn find(session: &UiSession, handler: &str) -> (f32, f32) {
    for y in (0..360).step_by(6) {
        for x in (0..360).step_by(6) {
            let x = if handler == "reset" { 354 - x } else { x };
            let (x, y) = (x as f32, y as f32);
            if session.press_at(x, y).as_deref() == Some(handler) {
                return (x, y);
            }
        }
    }
    panic!("no '{handler}' button")
}

#[test]
fn lua_writes_drive_the_view_and_taps_arrive_as_calls() {
    let service = UiService::new(DEFAULT_VIEWPORT);
    let session = service.open(&assets()).expect("view opens");
    let mut lua = lua_with_view(&session);
    assert_eq!(session.texts(), ["0", "+1", "Reset"]);

    eval::<()>(
        &mut lua,
        "assert(view:set('count', 2)) \
         assert(view:set('history', { { id = 2, label = 'two' }, { id = 1, label = 'one' } }))",
    );
    assert_eq!(session.texts(), ["2", "+1", "Reset", "two", "one"]);

    find(&session, "increment");
    let channel = session.channel().clone();
    assert_eq!(
        next_call(&channel),
        Some(("increment".into(), alloc::vec!["1".into()]))
    );

    // Integral floats are accepted for integer slots.
    eval::<()>(&mut lua, "assert(view:set('count', 0.0))");
    assert_eq!(session.texts(), ["0", "+1", "Reset"]);
}

#[test]
fn writes_are_checked_against_the_contract() {
    let service = UiService::new(DEFAULT_VIEWPORT);
    let session = service.open(&assets()).expect("view opens");
    let mut lua = lua_with_view(&session);
    for (source, message) in [
        ("view:set('count', 'two')", "'count': expected an integer"),
        ("view:set('count', 1.5)", "'count': expected an integer"),
        (
            "view:set('missing', 1)",
            "the view has no value named 'missing'",
        ),
        (
            "view:set('history', { { id = 'x', label = 'y' } })",
            "'history': expected an integer",
        ),
        (
            "view:set('history', 3)",
            "'history': expected an array table",
        ),
    ] {
        let (ok, error): (Option<bool>, String) = eval(&mut lua, &format!("return {source}"));
        assert_eq!(ok, None, "{source}");
        assert_eq!(error, message, "{source}");
    }
    assert_eq!(session.texts(), ["0", "+1", "Reset"]);
}

#[test]
fn closing_ends_the_view_for_lua() {
    let service = UiService::new(DEFAULT_VIEWPORT);
    let session = service.open(&assets()).expect("view opens");
    let mut lua = lua_with_view(&session);
    assert!(!session.is_closed());
    eval::<()>(&mut lua, "view:close()");
    assert!(session.is_closed());
    let (is_open, set): (bool, bool) = eval(
        &mut lua,
        "return view:is_open(), view:set('count', 1) ~= nil",
    );
    assert_eq!((is_open, set), (false, false));
    assert_eq!(next_call(session.channel()), None);
    assert_eq!(session.press_at(0.0, 0.0), None);

    // Dropping the session closes channels that outlive it.
    let other = service.open(&assets()).expect("view opens");
    let channel = other.channel().clone();
    drop(other);
    assert!(!channel.is_open());
}

#[test]
fn a_suspended_view_keeps_its_values_and_catches_up_on_resume() {
    let service = UiService::new(DEFAULT_VIEWPORT);
    let session = service.open(&assets()).expect("view opens");
    let mut lua = lua_with_view(&session);
    eval::<()>(
        &mut lua,
        "assert(view:set('count', 1)) \
         assert(view:set('history', { { id = 1, label = 'one' } }))",
    );
    assert_eq!(session.texts(), ["1", "+1", "Reset", "one"]);

    session.suspend();
    assert!(session.is_suspended());
    assert!(session.texts().is_empty());
    assert_eq!(session.press_at(180.0, 180.0), None);
    // Lua keeps writing while suspended; the writes wait for the resume.
    eval::<()>(&mut lua, "assert(view:set('count', 5))");

    session.resume(&assets()).expect("resume");
    assert!(!session.is_suspended());
    assert_eq!(session.texts(), ["5", "+1", "Reset", "one"]);

    // Rejected assets leave the view suspended with its values.
    session.suspend();
    let mut broken = assets();
    broken.view.truncate(10);
    assert!(session.resume(&broken).is_err());
    assert!(session.is_suspended());
    session.resume(&assets()).expect("resume");
    assert_eq!(session.texts(), ["5", "+1", "Reset", "one"]);
}

#[test]
fn clearing_drops_calls_lua_has_not_received() {
    let service = UiService::new(DEFAULT_VIEWPORT);
    let session = service.open(&assets()).expect("view opens");
    find(&session, "increment");
    session.clear_events();
    find(&session, "reset");
    assert_eq!(
        next_call(session.channel()).map(|(handler, _)| handler),
        Some("reset".into())
    );
}
