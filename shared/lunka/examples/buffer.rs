use lunka::prelude::*;

fn main() {
    let mut lua = Lua::new();

    let mut lua = lua.managed();
    macro_rules! check_buffer {
        ($fill:expr, $target:expr) => {{
            lua.push_buffer($fill);
            assert_eq!(lua.to_string(-1), Some($target.as_ref()));
            unsafe { lua.pop(1) };
        }};
    }

    check_buffer!(|_| {}, b"");
    check_buffer!(|b| b.add_c_str(c"not your business"), b"not your business");
    check_buffer!(
        |b| {
            b.add_string("not your ");
            b.add_byte(b'b');
            b.add_c_str(c"usiness");
        },
        b"not your business"
    );
    check_buffer!(
        |b| {
            b.thread().push_number(1.2345);
            b.add_value();
        },
        b"1.2345"
    );
}
