//! Number and string conversions for the vendored Lua sources.
//!
//! `include/barracuda_lua_user.h` points Lua's `l_sprintf`,
//! `lua_str2number` and related configuration macros here, so Lua never
//! calls the C library's `snprintf` or `strtod`. Those allocate internally
//! and keep per-thread state, which a bare-metal build cannot provide; these
//! conversions write only into the caller's buffer.
//!
//! Formatting follows C99 `printf` for the conversions Lua emits: `d i u o x
//! X c` on integers, `e E f F g G` on numbers, `s` and `p`, with the `- + #
//! 0` and space flags, width and precision. Parsing follows `strtod` for
//! decimal and hexadecimal numbers.

use core::ffi::{c_char, c_int, c_longlong, c_void};
use core::fmt::{self, Write as _};

/// Formats one `long long` argument.
///
/// # Safety
///
/// `format` is a NUL-terminated string and `buffer` is valid for `size`
/// bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn barracuda_lua_format_integer(
    buffer: *mut c_char,
    size: usize,
    format: *const c_char,
    value: c_longlong,
) -> c_int {
    unsafe { format_one(buffer, size, format, Argument::Integer(value)) }
}

/// Formats one `int` argument, used for `%c`.
///
/// # Safety
///
/// As for [`barracuda_lua_format_integer`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn barracuda_lua_format_int(
    buffer: *mut c_char,
    size: usize,
    format: *const c_char,
    value: c_int,
) -> c_int {
    unsafe {
        format_one(
            buffer,
            size,
            format,
            Argument::Integer(c_longlong::from(value)),
        )
    }
}

/// Formats one `double` argument.
///
/// # Safety
///
/// As for [`barracuda_lua_format_integer`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn barracuda_lua_format_number(
    buffer: *mut c_char,
    size: usize,
    format: *const c_char,
    value: f64,
) -> c_int {
    unsafe { format_one(buffer, size, format, Argument::Number(value)) }
}

/// Formats one string argument.
///
/// # Safety
///
/// As for [`barracuda_lua_format_integer`]; `value` is NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn barracuda_lua_format_string(
    buffer: *mut c_char,
    size: usize,
    format: *const c_char,
    value: *const c_char,
) -> c_int {
    let bytes = unsafe { c_bytes(value) };
    unsafe { format_one(buffer, size, format, Argument::String(bytes)) }
}

/// Formats one pointer argument.
///
/// # Safety
///
/// As for [`barracuda_lua_format_integer`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn barracuda_lua_format_pointer(
    buffer: *mut c_char,
    size: usize,
    format: *const c_char,
    value: *const c_void,
) -> c_int {
    unsafe { format_one(buffer, size, format, Argument::Pointer(value as usize)) }
}

/// Parses the longest number prefix of `text`, like `strtod`.
///
/// # Safety
///
/// `text` is NUL-terminated and `end`, when not null, is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn barracuda_lua_str2number(
    text: *const c_char,
    end: *mut *mut c_char,
) -> f64 {
    let bytes = unsafe { c_bytes(text) };
    let (value, consumed) = parse_number(bytes);
    if !end.is_null() {
        unsafe { end.write(text.add(consumed).cast_mut()) };
    }
    value
}

unsafe fn c_bytes<'a>(text: *const c_char) -> &'a [u8] {
    if text.is_null() {
        return &[];
    }
    unsafe { core::ffi::CStr::from_ptr(text) }.to_bytes()
}

enum Argument<'a> {
    Integer(c_longlong),
    Number(f64),
    String(&'a [u8]),
    Pointer(usize),
}

unsafe fn format_one(
    buffer: *mut c_char,
    size: usize,
    format: *const c_char,
    argument: Argument<'_>,
) -> c_int {
    let mut output = Output {
        buffer: buffer.cast::<u8>(),
        size,
        length: 0,
    };
    format_into(&mut output, unsafe { c_bytes(format) }, &argument);
    output.finish()
}

/// `snprintf` output: counts every byte, stores those that fit.
struct Output {
    buffer: *mut u8,
    size: usize,
    length: usize,
}

impl Output {
    fn push(&mut self, byte: u8) {
        if self.length.saturating_add(1) < self.size {
            // SAFETY: the caller made `buffer` valid for `size` bytes and
            // `length + 1 < size`.
            unsafe { self.buffer.add(self.length).write(byte) };
        }
        self.length = self.length.saturating_add(1);
    }

    fn extend(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.push(byte);
        }
    }

    fn repeat(&mut self, byte: u8, count: usize) {
        for _ in 0..count {
            self.push(byte);
        }
    }

    fn finish(self) -> c_int {
        if self.size > 0 {
            let end = self.length.min(self.size.saturating_sub(1));
            // SAFETY: `end < size`.
            unsafe { self.buffer.add(end).write(0) };
        }
        c_int::try_from(self.length).unwrap_or(c_int::MAX)
    }
}

#[derive(Clone, Copy, Default)]
struct Spec {
    left: bool,
    plus: bool,
    space: bool,
    alternate: bool,
    zero: bool,
    width: usize,
    precision: Option<usize>,
    conversion: u8,
}

fn format_into(output: &mut Output, format: &[u8], argument: &Argument<'_>) {
    let mut index = 0;
    while let Some(&byte) = format.get(index) {
        index += 1;
        if byte != b'%' {
            output.push(byte);
            continue;
        }
        if format.get(index) == Some(&b'%') {
            index += 1;
            output.push(b'%');
            continue;
        }
        let (spec, next) = parse_spec(format, index);
        index = next;
        write_conversion(output, &spec, argument);
    }
}

fn parse_spec(format: &[u8], mut index: usize) -> (Spec, usize) {
    let mut spec = Spec::default();
    while let Some(&flag) = format.get(index) {
        match flag {
            b'-' => spec.left = true,
            b'+' => spec.plus = true,
            b' ' => spec.space = true,
            b'#' => spec.alternate = true,
            b'0' => spec.zero = true,
            _ => break,
        }
        index += 1;
    }
    (spec.width, index) = parse_decimal(format, index);
    if format.get(index) == Some(&b'.') {
        let (precision, next) = parse_decimal(format, index + 1);
        spec.precision = Some(precision);
        index = next;
    }
    while matches!(
        format.get(index),
        Some(b'l' | b'h' | b'j' | b'z' | b't' | b'L')
    ) {
        index += 1;
    }
    spec.conversion = format.get(index).copied().unwrap_or(0);
    (spec, index.saturating_add(1))
}

fn parse_decimal(format: &[u8], mut index: usize) -> (usize, usize) {
    let mut value: usize = 0;
    while let Some(digit) = format.get(index).filter(|byte| byte.is_ascii_digit()) {
        value = value
            .saturating_mul(10)
            .saturating_add(usize::from(digit - b'0'));
        index += 1;
    }
    (value, index)
}

fn write_conversion(output: &mut Output, spec: &Spec, argument: &Argument<'_>) {
    match (spec.conversion, argument) {
        (b'd' | b'i', Argument::Integer(value)) => {
            let sign = sign_of(*value < 0, spec);
            write_unsigned(output, spec, sign, value.unsigned_abs(), 10, b"");
        }
        (b'u' | b'o' | b'x' | b'X', Argument::Integer(value)) => {
            #[allow(clippy::cast_sign_loss, reason = "C reinterprets the bits as unsigned")]
            let bits = *value as u64;
            let (base, prefix): (u64, &[u8]) = match spec.conversion {
                b'o' => (8, b""),
                b'x' => (
                    16,
                    if spec.alternate && bits != 0 {
                        b"0x"
                    } else {
                        b""
                    },
                ),
                b'X' => (
                    16,
                    if spec.alternate && bits != 0 {
                        b"0X"
                    } else {
                        b""
                    },
                ),
                _ => (10, b""),
            };
            write_unsigned(output, spec, b"", bits, base, prefix);
        }
        (b'c', Argument::Integer(value)) => {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "C converts to unsigned char"
            )]
            let byte = *value as u8;
            write_padded(output, spec, b"", &[byte], false);
        }
        (b'e' | b'E' | b'f' | b'F' | b'g' | b'G', Argument::Number(value)) => {
            write_number(output, spec, *value);
        }
        (b's', Argument::String(text)) => {
            let text = match spec.precision {
                Some(limit) => text.get(..limit).unwrap_or(text),
                None => text,
            };
            write_padded(output, spec, b"", text, false);
        }
        (b'p', Argument::Pointer(address)) => {
            let mut digits = Digits::new();
            let _ = write!(digits, "{address:x}");
            write_padded(output, spec, b"0x", digits.as_bytes(), false);
        }
        // Lua validates every format before formatting; anything else is a
        // mismatch between this file and the Lua sources.
        _ => {}
    }
}

fn sign_of(negative: bool, spec: &Spec) -> &'static [u8] {
    if negative {
        b"-"
    } else if spec.plus {
        b"+"
    } else if spec.space {
        b" "
    } else {
        b""
    }
}

fn write_unsigned(
    output: &mut Output,
    spec: &Spec,
    sign: &[u8],
    value: u64,
    base: u64,
    prefix: &[u8],
) {
    let mut digits = Digits::new();
    let precision = spec.precision.unwrap_or(1);
    if !(value == 0 && precision == 0) {
        let _ = match (base, spec.conversion) {
            (8, _) => write!(digits, "{value:o}"),
            (16, b'X') => write!(digits, "{value:X}"),
            (16, _) => write!(digits, "{value:x}"),
            _ => write!(digits, "{value}"),
        };
    }
    let mut leading_zeros = precision.saturating_sub(digits.len());
    if base == 8 && spec.alternate && leading_zeros == 0 && digits.as_bytes().first() != Some(&b'0')
    {
        leading_zeros = 1;
    }
    let body_length = leading_zeros.saturating_add(digits.len());
    let zero_pad = spec.zero && !spec.left && spec.precision.is_none();
    let fill = spec
        .width
        .saturating_sub(sign.len() + prefix.len() + body_length);
    if !spec.left && !zero_pad {
        output.repeat(b' ', fill);
    }
    output.extend(sign);
    output.extend(prefix);
    if zero_pad {
        output.repeat(b'0', fill);
    }
    output.repeat(b'0', leading_zeros);
    output.extend(digits.as_bytes());
    if spec.left {
        output.repeat(b' ', fill);
    }
}

fn write_padded(output: &mut Output, spec: &Spec, prefix: &[u8], body: &[u8], zero_pad: bool) {
    let fill = spec.width.saturating_sub(prefix.len() + body.len());
    if !spec.left && !zero_pad {
        output.repeat(b' ', fill);
    }
    output.extend(prefix);
    if zero_pad && !spec.left {
        output.repeat(b'0', fill);
    }
    output.extend(body);
    if spec.left {
        output.repeat(b' ', fill);
    }
}

fn write_number(output: &mut Output, spec: &Spec, value: f64) {
    let upper = spec.conversion.is_ascii_uppercase();
    let sign = sign_of(value.is_sign_negative(), spec);
    let mut digits = Digits::new();
    if !value.is_finite() {
        let text: &[u8] = match (value.is_nan(), upper) {
            (true, false) => b"nan",
            (true, true) => b"NAN",
            (false, false) => b"inf",
            (false, true) => b"INF",
        };
        let _ = digits.write_bytes(text);
        write_padded(output, spec, sign, digits.as_bytes(), false);
        return;
    }
    let magnitude = value.abs();
    let precision = spec.precision.unwrap_or(6);
    match spec.conversion.to_ascii_lowercase() {
        b'f' => write_fixed(&mut digits, magnitude, precision, spec.alternate),
        b'e' => {
            write_scientific(&mut digits, magnitude, precision, spec.alternate, upper);
        }
        _ => write_general(&mut digits, magnitude, precision, spec.alternate, upper),
    }
    write_padded(output, spec, sign, digits.as_bytes(), spec.zero);
}

fn write_fixed(digits: &mut Digits, magnitude: f64, precision: usize, alternate: bool) {
    let _ = write!(digits, "{magnitude:.precision$}");
    if alternate && precision == 0 {
        let _ = digits.write_char('.');
    }
}

/// Writes `d.ddde±XX` and returns the decimal exponent.
fn write_scientific(
    digits: &mut Digits,
    magnitude: f64,
    precision: usize,
    alternate: bool,
    upper: bool,
) -> i32 {
    let mut rust = Digits::new();
    let _ = write!(rust, "{magnitude:.precision$e}");
    let text = rust.as_bytes();
    let split = text
        .iter()
        .position(|&byte| byte == b'e')
        .unwrap_or(text.len());
    let (mantissa, exponent) = text.split_at(split);
    let exponent = core::str::from_utf8(exponent.get(1..).unwrap_or_default())
        .ok()
        .and_then(|exponent| exponent.parse::<i32>().ok())
        .unwrap_or(0);
    let _ = digits.write_bytes(mantissa);
    if alternate && precision == 0 {
        let _ = digits.write_char('.');
    }
    let _ = digits.write_char(if upper { 'E' } else { 'e' });
    let _ = digits.write_char(if exponent < 0 { '-' } else { '+' });
    let _ = write!(digits, "{:02}", exponent.unsigned_abs());
    exponent
}

/// `%g`: the shorter of `%e` and `%f` at `precision` significant digits.
fn write_general(
    digits: &mut Digits,
    magnitude: f64,
    precision: usize,
    alternate: bool,
    upper: bool,
) {
    let significant = precision.max(1);
    let mut scientific = Digits::new();
    let exponent = write_scientific(
        &mut scientific,
        magnitude,
        significant - 1,
        alternate,
        upper,
    );
    let fixed = i64::try_from(significant)
        .is_ok_and(|significant| significant > i64::from(exponent) && exponent >= -4);
    let start = digits.len();
    if fixed {
        let fraction =
            usize::try_from(i64::try_from(significant).unwrap_or(0) - 1 - i64::from(exponent))
                .unwrap_or(0);
        write_fixed(digits, magnitude, fraction, alternate);
    } else {
        let _ = digits.write_bytes(scientific.as_bytes());
    }
    if !alternate {
        digits.strip_fraction_zeros(start);
    }
}

/// Formatting scratch space large enough for any Lua item: `%.99f` of the
/// largest double is 409 bytes.
struct Digits {
    bytes: [u8; 512],
    length: usize,
}

impl Digits {
    const fn new() -> Self {
        Self {
            bytes: [0; 512],
            length: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..self.length).unwrap_or_default()
    }

    fn len(&self) -> usize {
        self.length
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> fmt::Result {
        let end = self.length.checked_add(bytes.len()).ok_or(fmt::Error)?;
        let target = self.bytes.get_mut(self.length..end).ok_or(fmt::Error)?;
        target.copy_from_slice(bytes);
        self.length = end;
        Ok(())
    }

    /// Removes trailing fraction zeros, and a bare decimal point, from the
    /// number written since `start`, keeping any exponent.
    fn strip_fraction_zeros(&mut self, start: usize) {
        let number = self.bytes.get(start..self.length).unwrap_or_default();
        let Some(point) = number.iter().position(|&byte| byte == b'.') else {
            return;
        };
        let exponent = number
            .iter()
            .position(|byte| byte.eq_ignore_ascii_case(&b'e'))
            .unwrap_or(number.len());
        let mut keep = exponent;
        while keep > point + 1 && number.get(keep - 1) == Some(&b'0') {
            keep -= 1;
        }
        if keep == point + 1 {
            keep = point;
        }
        let removed = exponent - keep;
        if removed == 0 {
            return;
        }
        let from = start + exponent;
        let to = start + keep;
        self.bytes.copy_within(from..self.length, to);
        self.length -= removed;
    }
}

impl fmt::Write for Digits {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.write_bytes(text.as_bytes())
    }
}

/// Parses like `strtod`: optional whitespace and sign, then a decimal number
/// with optional exponent or a `0x` hexadecimal number with optional binary
/// exponent. Returns the value and the bytes consumed, `0` when no number
/// was found. Lua rejects `inf` and `nan` before parsing.
fn parse_number(text: &[u8]) -> (f64, usize) {
    let mut index = 0;
    while text
        .get(index)
        .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r'))
    {
        index += 1;
    }
    let negative = text.get(index) == Some(&b'-');
    if matches!(text.get(index), Some(b'+' | b'-')) {
        index += 1;
    }
    let hexadecimal =
        text.get(index) == Some(&b'0') && matches!(text.get(index + 1), Some(b'x' | b'X'));
    let parsed = if hexadecimal {
        parse_hexadecimal(text, index + 2)
    } else {
        parse_decimal_number(text, index)
    };
    match parsed {
        Some((magnitude, end)) => (if negative { -magnitude } else { magnitude }, end),
        None if hexadecimal => (if negative { -0.0 } else { 0.0 }, index + 1),
        None => (0.0, 0),
    }
}

fn digits_from(text: &[u8], mut index: usize, radix: u32) -> usize {
    while text
        .get(index)
        .is_some_and(|&byte| char::from(byte).is_digit(radix))
    {
        index += 1;
    }
    index
}

fn parse_decimal_number(text: &[u8], start: usize) -> Option<(f64, usize)> {
    let integer_end = digits_from(text, start, 10);
    let mut end = integer_end;
    let mut fraction_digits = 0;
    if text.get(end) == Some(&b'.') {
        let fraction_end = digits_from(text, end + 1, 10);
        fraction_digits = fraction_end - end - 1;
        end = fraction_end;
    }
    if integer_end == start && fraction_digits == 0 {
        return None;
    }
    if matches!(text.get(end), Some(b'e' | b'E')) {
        let mut exponent_start = end + 1;
        if matches!(text.get(exponent_start), Some(b'+' | b'-')) {
            exponent_start += 1;
        }
        let exponent_end = digits_from(text, exponent_start, 10);
        if exponent_end > exponent_start {
            end = exponent_end;
        }
    }
    let number = core::str::from_utf8(text.get(start..end)?).ok()?;
    Some((number.parse::<f64>().ok()?, end))
}

fn parse_hexadecimal(text: &[u8], start: usize) -> Option<(f64, usize)> {
    // Keep 60 significant bits; any further nonzero digit only matters as a
    // sticky bit for rounding.
    let mut mantissa: u64 = 0;
    let mut exponent: i64 = 0;
    let mut sticky = false;
    let mut digits = 0;
    let mut index = start;
    let mut seen_point = false;
    loop {
        match text.get(index) {
            Some(&b'.') if !seen_point => seen_point = true,
            Some(&byte) if byte.is_ascii_hexdigit() => {
                let digit = u64::from(char::from(byte).to_digit(16).unwrap_or(0));
                digits += 1;
                if mantissa >> 56 == 0 {
                    mantissa = (mantissa << 4) | digit;
                    if seen_point {
                        exponent -= 4;
                    }
                } else {
                    sticky |= digit != 0;
                    if !seen_point {
                        exponent += 4;
                    }
                }
            }
            _ => break,
        }
        index += 1;
    }
    if digits == 0 {
        return None;
    }
    let mut end = index;
    if matches!(text.get(index), Some(b'p' | b'P')) {
        let mut exponent_start = index + 1;
        let negative = text.get(exponent_start) == Some(&b'-');
        if matches!(text.get(exponent_start), Some(b'+' | b'-')) {
            exponent_start += 1;
        }
        let exponent_end = digits_from(text, exponent_start, 10);
        if exponent_end > exponent_start {
            let mut binary: i64 = 0;
            for &byte in text.get(exponent_start..exponent_end)? {
                binary = binary
                    .saturating_mul(10)
                    .saturating_add(i64::from(byte - b'0'));
            }
            exponent = exponent.saturating_add(if negative { -binary } else { binary });
            end = exponent_end;
        }
    }
    if sticky {
        mantissa |= 1;
    }
    #[allow(clippy::cast_precision_loss, reason = "u64 to f64 rounds to nearest")]
    let value = scale_by_power_of_two(mantissa as f64, exponent);
    Some((value, end))
}

/// `value * 2^exponent`, exact except where the result is subnormal.
fn scale_by_power_of_two(mut value: f64, mut exponent: i64) -> f64 {
    const STEP: i64 = 1000;
    while exponent > STEP {
        value *= power_of_two(STEP);
        exponent -= STEP;
    }
    while exponent < -STEP {
        value *= power_of_two(-STEP);
        exponent += STEP;
    }
    value * power_of_two(exponent)
}

fn power_of_two(exponent: i64) -> f64 {
    // `exponent` is within the normal range, so the biased exponent fits.
    #[allow(clippy::cast_sign_loss, reason = "the biased exponent is positive")]
    f64::from_bits(((exponent + 1023) as u64) << 52)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::ffi::CString;
    use std::string::String;
    use std::vec::Vec;

    use super::*;

    fn format(spec: &str, argument: Argument<'_>) -> String {
        let format = CString::new(spec).unwrap_or_default();
        let mut buffer = [0_u8; 600];
        let length = unsafe {
            format_one(
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                format.as_ptr(),
                argument,
            )
        };
        let length = usize::try_from(length).unwrap_or(0);
        String::from_utf8_lossy(&buffer[..length]).into_owned()
    }

    fn number(spec: &str, value: f64) -> String {
        format(spec, Argument::Number(value))
    }

    fn integer(spec: &str, value: i64) -> String {
        format(spec, Argument::Integer(value))
    }

    #[test]
    fn lua_default_number_format_matches_c() {
        let cases = [
            (0.1, "0.1"),
            (1.0, "1"),
            (-0.0, "-0"),
            (1e100, "1e+100"),
            (123456789012345.0, "1.2345678901234e+14"),
            (12345678901234.0, "12345678901234"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (3.14159265358979, "3.1415926535898"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (2.5, "2.5"),
            (1.0 / 3.0, "0.33333333333333"),
        ];
        for (value, expected) in cases {
            assert_eq!(number("%.14g", value), expected, "{value}");
        }
    }

    #[test]
    fn number_conversions_follow_c() {
        assert_eq!(number("%f", 1.5), "1.500000");
        assert_eq!(number("%.2f", 0.125), "0.12");
        assert_eq!(number("%.0f", 2.5), "2");
        assert_eq!(number("%#.0f", 2.0), "2.");
        assert_eq!(number("%e", 1234.5), "1.234500e+03");
        assert_eq!(number("%.3E", -0.00012345), "-1.234E-04");
        assert_eq!(number("%10.3f", 3.14159), "     3.142");
        assert_eq!(number("%-10.3f|", 3.14159), "3.142     |");
        assert_eq!(number("%010.3f", -3.14159), "-00003.142");
        assert_eq!(number("%+g", 1.0), "+1");
        assert_eq!(number("% g", 1.0), " 1");
        assert_eq!(number("%g", 100000.0), "100000");
        assert_eq!(number("%g", 1000000.0), "1e+06");
        assert_eq!(number("%#g", 1.0), "1.00000");
        assert_eq!(number("%G", 1e-10), "1E-10");
        assert_eq!(number("%.0g", 0.5), "0.5");
        assert_eq!(
            number("%5.1f", f64::NAN)
                .trim_start_matches(' ')
                .trim_start_matches('-'),
            "nan"
        );
        assert_eq!(number("%08f", f64::INFINITY), "     inf");
        assert_eq!(number("%.99f", f64::MAX).len(), 309 + 1 + 99);
    }

    #[test]
    fn integer_conversions_follow_c() {
        assert_eq!(integer("%lld", -42), "-42");
        assert_eq!(integer("%lld", i64::MIN), "-9223372036854775808");
        assert_eq!(integer("%5lld", 42), "   42");
        assert_eq!(integer("%-5lld|", 42), "42   |");
        assert_eq!(integer("%05lld", -42), "-0042");
        assert_eq!(integer("%.3lld", 7), "007");
        assert_eq!(integer("%.0lld", 0), "");
        assert_eq!(integer("%+lld", 7), "+7");
        assert_eq!(integer("%llx", 255), "ff");
        assert_eq!(integer("%#llX", 255), "0XFF");
        assert_eq!(integer("%llx", -1), "ffffffffffffffff");
        assert_eq!(integer("%#llo", 8), "010");
        assert_eq!(integer("%llu", -1), "18446744073709551615");
        assert_eq!(integer("%c", 65), "A");
        assert_eq!(integer("%3c", 65), "  A");
    }

    #[test]
    fn strings_and_pointers_follow_c() {
        assert_eq!(format("%s", Argument::String(b"hello")), "hello");
        assert_eq!(format("%.3s", Argument::String(b"hello")), "hel");
        assert_eq!(format("%7s", Argument::String(b"hi")), "     hi");
        assert_eq!(format("%-4s|", Argument::String(b"hi")), "hi  |");
        assert_eq!(format("%p", Argument::Pointer(0x1234)), "0x1234");
    }

    #[test]
    fn output_is_truncated_like_snprintf() {
        let format = CString::new("%.14g").unwrap_or_default();
        let mut buffer = [0xff_u8; 4];
        let length = unsafe {
            barracuda_lua_format_number(
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                format.as_ptr(),
                0.125,
            )
        };
        assert_eq!(length, 5);
        assert_eq!(&buffer, b"0.1\0");
    }

    #[test]
    fn parsing_follows_strtod() {
        let cases: [(&str, f64, usize); 12] = [
            ("3.25", 3.25, 4),
            ("  -1e3x", -1000.0, 6),
            ("+.5", 0.5, 3),
            ("5.", 5.0, 2),
            ("1e", 1.0, 1),
            ("1e+", 1.0, 1),
            ("0x10", 16.0, 4),
            ("0x1.8p1", 3.0, 7),
            ("0x.8", 0.5, 4),
            ("-0x1p-2", -0.25, 7),
            ("0x", 0.0, 1),
            ("abc", 0.0, 0),
        ];
        for (text, expected, consumed) in cases {
            assert_eq!(
                parse_number(text.as_bytes()),
                (expected, consumed),
                "{text}"
            );
        }
        assert_eq!(parse_number(b"1e400").0, f64::INFINITY);
        assert_eq!(parse_number(b"0x1p-1074").0, f64::from_bits(1));
    }

    /// Compares with the host C library for many values and formats.
    #[test]
    fn matches_the_host_c_library() {
        unsafe extern "C" {
            fn snprintf(buffer: *mut c_char, size: usize, format: *const c_char, ...) -> c_int;
            fn strtod(text: *const c_char, end: *mut *mut c_char) -> f64;
        }
        let formats = [
            "%.14g", "%g", "%.3g", "%#g", "%e", "%.0e", "%E", "%f", "%.1f", "%.0f", "%12.4f",
            "%-12.4e|", "%+.5g", "%010.2f", "%G", "%.17g", "%.20f",
        ];
        let mut values: Vec<f64> = Vec::new();
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..2000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let candidate = f64::from_bits(state);
            if candidate.is_finite() {
                values.push(candidate);
            }
            #[allow(clippy::cast_precision_loss)]
            values.push((state % 2_000_000) as f64 / 64.0 - 15_000.0);
        }
        values.extend([
            0.5,
            1.5,
            2.5,
            0.125,
            0.375,
            1e-5,
            9.9999999999999995e-5,
            1e21,
            1e-300,
            5e-324,
        ]);
        for value in values {
            for spec in formats {
                let format = CString::new(spec).unwrap_or_default();
                let mut expected = [0_u8; 600];
                let length = unsafe {
                    snprintf(
                        expected.as_mut_ptr().cast(),
                        expected.len(),
                        format.as_ptr(),
                        value,
                    )
                };
                let length = usize::try_from(length).unwrap_or(0);
                let expected = String::from_utf8_lossy(&expected[..length]).into_owned();
                assert_eq!(number(spec, value), expected, "{spec} {value:e}");
            }
            let text = CString::new(std::format!("{value:e}")).unwrap_or_default();
            let parsed = unsafe { strtod(text.as_ptr(), core::ptr::null_mut()) };
            assert_eq!(parse_number(text.as_bytes()).0.to_bits(), parsed.to_bits());
        }
    }
}
