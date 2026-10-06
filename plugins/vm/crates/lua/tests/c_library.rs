//! The copied musl number conversions against glibc, the reference C library.
#![cfg(all(feature = "vendored", target_os = "linux", target_env = "gnu"))]
#![allow(unsafe_code, missing_docs)]

use std::ffi::{CString, c_char, c_int};

// Links the crate, and with it the C libraries its build script compiles.
use barracuda_lua as _;

unsafe extern "C" {
    fn barracuda_lua_snprintf(
        buffer: *mut c_char,
        size: usize,
        format: *const c_char,
        ...
    ) -> c_int;
    fn barracuda_lua_strtod(text: *const c_char, end: *mut *mut c_char) -> f64;
    fn snprintf(buffer: *mut c_char, size: usize, format: *const c_char, ...) -> c_int;
    fn strtod(text: *const c_char, end: *mut *mut c_char) -> f64;
}

fn values() -> Vec<f64> {
    let mut values = vec![
        0.0,
        -0.0,
        0.5,
        1.5,
        2.5,
        0.125,
        0.375,
        1e-5,
        // The largest double below 1e-4, where `%g` switches notation.
        f64::from_bits(1e-4_f64.to_bits() - 1),
        1e21,
        1e-300,
        5e-324,
        f64::MAX,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ];
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..2000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        values.push(f64::from_bits(state));
        #[allow(clippy::cast_precision_loss)]
        values.push((state % 2_000_000) as f64 / 64.0 - 15_000.0);
    }
    values
}

fn formatted(
    format: &CString,
    value: f64,
    print: unsafe extern "C" fn(*mut c_char, usize, *const c_char, ...) -> c_int,
) -> String {
    let mut buffer = [0_u8; 600];
    let length = unsafe {
        print(
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            format.as_ptr(),
            value,
        )
    };
    let length = usize::try_from(length).unwrap_or(0);
    String::from_utf8_lossy(&buffer[..length]).into_owned()
}

#[test]
fn snprintf_matches_the_host_c_library() {
    let formats = [
        "%.14g", "%g", "%.3g", "%#g", "%e", "%.0e", "%E", "%f", "%.1f", "%.0f", "%12.4f",
        "%-12.4e|", "%+.5g", "%010.2f", "%G", "%.17g", "%.20f", "%a", "%A", "%.3a",
    ];
    for value in values() {
        for spec in formats {
            // C leaves %a of subnormals to the implementation: musl normalizes
            // them (0x1p-1074) where glibc does not (0x0.0000000000001p-1022).
            if (spec.contains('a') || spec.contains('A')) && value.is_subnormal() {
                continue;
            }
            let format = CString::new(spec).unwrap_or_default();
            assert_eq!(
                formatted(&format, value, barracuda_lua_snprintf),
                formatted(&format, value, snprintf),
                "{spec} {value:e}"
            );
        }
    }
}

#[test]
fn snprintf_formats_integers_strings_and_truncates() {
    let mut buffer = [0_u8; 8];
    let format = CString::new("%lld|%5s|%-3c|%x").unwrap_or_default();
    let text = CString::new("ab").unwrap_or_default();
    let length = unsafe {
        barracuda_lua_snprintf(
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            format.as_ptr(),
            -42_i64,
            text.as_ptr(),
            c_int::from(b'z'),
            255_u32,
        )
    };
    assert_eq!(length, 16);
    assert_eq!(&buffer, b"-42|   \0");
}

#[test]
fn strtod_matches_the_host_c_library() {
    let mut texts: Vec<String> = values().iter().map(|value| format!("{value:e}")).collect();
    texts.extend(
        [
            "3.25",
            "  -1e3x",
            "+.5",
            "5.",
            "1e",
            "1e+",
            "0x10",
            "0x1.8p1",
            "0x.8",
            "-0x1p-2",
            "0x",
            "abc",
            "1e400",
            "0x1p-1074",
            "inf",
            "nan",
            "4.9406564584124654e-324",
            "2.2250738585072011e-308",
            "123456789012345678901234567890",
        ]
        .map(String::from),
    );
    for text in texts {
        let text = CString::new(text).unwrap_or_default();
        let (mut ours_end, mut host_end) = (std::ptr::null_mut(), std::ptr::null_mut());
        let ours = unsafe { barracuda_lua_strtod(text.as_ptr(), &raw mut ours_end) };
        let host = unsafe { strtod(text.as_ptr(), &raw mut host_end) };
        assert_eq!(ours.to_bits(), host.to_bits(), "{text:?}");
        assert_eq!(ours_end, host_end, "{text:?}");
    }
}
