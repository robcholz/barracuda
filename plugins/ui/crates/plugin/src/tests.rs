#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

extern crate std;

use crate::mailbox::Mailbox;
use crate::package::UiPackage;
use crate::service::{DEFAULT_VIEWPORT, UiAssets, UiService, UiSession};
use alloc::{format, string::String, sync::Arc, vec::Vec};
use barracuda_vm_plugin::{Lua, LuaPackage, Package};
use futures_lite::future;

const VIEW: &[u8] = include_bytes!("../../runtime/tests/fixtures/counter.bvir");
const STYLES: &[u8] = include_bytes!("../../runtime/tests/fixtures/counter.styles.bin");

fn setup() -> (Lua, UiService, UiPackage) {
    let mailbox = Arc::new(Mailbox::new());
    let service = UiService::new(Arc::clone(&mailbox), DEFAULT_VIEWPORT);
    let package = UiPackage::new(mailbox);
    let mut lua = Lua::new().expect("lua");
    package.install(&mut lua).expect("install ui");
    (lua, service, package)
}

fn open(service: &UiService) -> UiSession {
    service
        .open(&UiAssets {
            view: VIEW.to_vec(),
            styles: STYLES.to_vec(),
            fonts: Vec::new(),
        })
        .expect("view opens")
}

/// Finds a point that presses `handler`; every probe that hits queues an event.
fn find(session: &UiSession, handler: &str) -> (f32, f32) {
    for y in (0..360).step_by(6) {
        for x in (0..360).step_by(6) {
            let (x, y) = (x as f32, y as f32);
            if session.press_at(x, y).as_deref() == Some(handler) {
                return (x, y);
            }
        }
    }
    panic!("no '{handler}' button")
}

fn eval<T: barracuda_lua::FromLuaMulti>(lua: &mut Lua, source: &str) -> T {
    future::block_on(lua.load(source).eval_async::<T>()).expect(source)
}

#[test]
fn lua_drives_the_view_and_receives_taps() {
    let (mut lua, service, _package) = setup();
    let session = open(&service);
    assert_eq!(session.texts(), ["0", "+1", "Reset"]);

    eval::<()>(
        &mut lua,
        &format!(
            "view = assert(require('ui').attach('{}')) \
             assert(view:set('count', 2)) \
             assert(view:set('history', {{ {{ id = 2, label = 'two' }}, {{ id = 1, label = 'one' }} }}))",
            session.ticket()
        ),
    );
    assert_eq!(session.texts(), ["2", "+1", "Reset", "two", "one"]);

    // Probing for buttons taps them; drain those taps before checking new ones.
    let plus = find(&session, "increment");
    let reset = find(&session, "reset");
    let (name, by): (String, i64) = eval(&mut lua, "return view:next()");
    assert_eq!((name.as_str(), by), ("increment", 1));
    eval::<()>(&mut lua, "repeat until view:next() == 'reset'");

    assert_eq!(session.press_at(reset.0, reset.1).as_deref(), Some("reset"));
    assert_eq!(
        session.press_at(plus.0, plus.1).as_deref(),
        Some("increment")
    );
    let names: (String, String) = eval(
        &mut lua,
        "local a = view:next() local b = view:next() return a, b",
    );
    assert_eq!((names.0.as_str(), names.1.as_str()), ("reset", "increment"));

    // Integral floats are accepted for integer slots.
    eval::<()>(&mut lua, "view:set('count', 0.0)");
    assert_eq!(session.texts(), ["0", "+1", "Reset"]);
}

#[test]
fn writes_are_checked_against_the_contract() {
    let (mut lua, service, _package) = setup();
    let session = open(&service);
    eval::<()>(
        &mut lua,
        &format!(
            "view = assert(require('ui').attach('{}'))",
            session.ticket()
        ),
    );
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
fn a_ticket_attaches_once() {
    let (mut lua, service, _package) = setup();
    let session = open(&service);
    let results: (bool, bool, bool) = eval(
        &mut lua,
        &format!(
            "local ui = require('ui') \
             return ui.attach('{0}') ~= nil, \
                 ui.attach('{0}') ~= nil, \
                 ui.attach('view-999') ~= nil",
            session.ticket()
        ),
    );
    assert_eq!(results, (true, false, false));
}

#[test]
fn closing_ends_the_view_for_both_sides() {
    let (mut lua, service, package) = setup();
    let session = open(&service);
    eval::<()>(
        &mut lua,
        &format!(
            "view = assert(require('ui').attach('{}'))",
            session.ticket()
        ),
    );
    assert!(!session.is_closed());
    eval::<()>(&mut lua, "view:close()");
    assert!(session.is_closed());
    let (is_open, next, set): (bool, Option<String>, bool) = eval(
        &mut lua,
        "return view:is_open(), (view:next()), view:set('count', 1) ~= nil",
    );
    assert_eq!((is_open, next, set), (false, None, false));
    assert_eq!(session.press_at(0.0, 0.0), None);

    // Revoking the package closes every view and refuses new attaches.
    let other = open(&service);
    package.revoke();
    assert!(other.is_closed());
    let attached: bool = eval(
        &mut lua,
        &format!("return require('ui').attach('{}') ~= nil", other.ticket()),
    );
    assert!(!attached);
}

#[test]
fn next_waits_for_a_tap() {
    let (mut lua, service, _package) = setup();
    let session = open(&service);
    let ticket = String::from(session.ticket());
    let script = format!(
        "local view = assert(require('ui').attach('{ticket}')) \
         local count = 0 \
         while count < 3 do \
             local event, by = view:next() \
             count = count + by \
             view:set('count', count) \
         end \
         return count"
    );
    // Finding the button queues the first tap; two more arrive while Lua waits.
    let plus = find(&session, "increment");
    let lua_side = lua.load(&script).eval_async::<i64>();
    let taps = async {
        for _ in 0..2 {
            future::yield_now().await;
            assert_eq!(
                session.press_at(plus.0, plus.1).as_deref(),
                Some("increment")
            );
        }
    };
    let (count, ()) = future::block_on(future::zip(lua_side, taps));
    assert_eq!(count.expect("script"), 3);
    assert_eq!(session.texts(), ["3", "+1", "Reset"]);
}
