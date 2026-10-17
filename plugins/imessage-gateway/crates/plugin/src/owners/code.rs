//! Six-digit pairing codes: minting from an entropy source, parsing an
//! inbound message, and constant-time comparison.

use alloc::rc::Rc;
use core::fmt;

use barracuda_platform::{Entropy, EntropyUnavailable};

/// Number of decimal digits in a pairing code.
pub const PAIRING_CODE_DIGITS: usize = 6;

/// Number of distinct pairing codes.
const CODE_SPACE: u32 = 1_000_000;

/// Largest multiple of [`CODE_SPACE`] that fits a `u32` draw; draws at or
/// above it are rejected so every code is equally likely.
const UNBIASED_LIMIT: u32 = 4_294_000_000;

/// Draws before minting gives up; one draw is rejected with probability
/// below 0.03 %, so reaching this limit means the source is broken.
const MINT_DRAWS: usize = 8;

/// Type-erased Platform entropy used to mint pairing codes.
///
/// Build it from the Platform's [`Entropy`] with [`PairingEntropy::new`]. A
/// Platform without an entropy source gives [`PairingEntropy::unavailable`],
/// and pairing is then reported as unavailable instead of using a weaker
/// generator.
#[derive(Clone)]
pub struct PairingEntropy(Rc<FillFn>);

type FillFn = dyn Fn(&mut [u8]) -> Result<(), EntropyUnavailable>;

impl PairingEntropy {
    /// Erases the Platform's entropy source.
    #[must_use]
    pub fn new<E: Entropy>(entropy: E) -> Self {
        Self(Rc::new(move |bytes: &mut [u8]| entropy.fill(bytes)))
    }

    /// A source that never yields bytes; pairing codes cannot be minted.
    #[must_use]
    pub fn unavailable() -> Self {
        Self(Rc::new(|_bytes: &mut [u8]| Err(EntropyUnavailable)))
    }

    fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
        (self.0)(bytes)
    }
}

impl fmt::Debug for PairingEntropy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PairingEntropy")
    }
}

/// One six-digit pairing code, stored as ASCII digits.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PairingCode([u8; PAIRING_CODE_DIGITS]);

impl PairingCode {
    /// Mints a uniformly distributed code from `entropy`.
    ///
    /// # Errors
    ///
    /// Returns [`EntropyUnavailable`] when the source yields no bytes.
    pub fn mint(entropy: &PairingEntropy) -> Result<Self, EntropyUnavailable> {
        for _draw in 0..MINT_DRAWS {
            let mut bytes = [0_u8; 4];
            entropy.fill(&mut bytes)?;
            let draw = u32::from_le_bytes(bytes);
            if draw < UNBIASED_LIMIT {
                return Ok(Self::from_value(draw.checked_rem(CODE_SPACE).unwrap_or(0)));
            }
        }
        Err(EntropyUnavailable)
    }

    /// Builds the code whose decimal value is `value`, which is below `10^6`.
    fn from_value(mut value: u32) -> Self {
        let mut digits = [b'0'; PAIRING_CODE_DIGITS];
        for digit in digits.iter_mut().rev() {
            let low = u8::try_from(value.checked_rem(10).unwrap_or(0)).unwrap_or(0);
            *digit = b'0'.saturating_add(low);
            value = value.checked_div(10).unwrap_or(0);
        }
        Self(digits)
    }

    /// Returns the code as six ASCII digits.
    #[must_use]
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.0).unwrap_or("000000")
    }

    /// Reads the code a pairing message carries: its trimmed text is exactly
    /// six ASCII digits, or `/start <code>` (also `/start@bot <code>`), the
    /// form a Telegram deep link sends.
    #[must_use]
    pub fn parse_message(text: &str) -> Option<Self> {
        let text = text.trim();
        let candidate = match text.strip_prefix("/start") {
            Some(rest) => {
                let rest = match rest.strip_prefix('@') {
                    Some(bot) => bot.split_once(char::is_whitespace)?.1,
                    None if rest.starts_with(char::is_whitespace) => rest,
                    None => return None,
                };
                rest.trim()
            }
            None => text,
        };
        let digits: [u8; PAIRING_CODE_DIGITS] = candidate.as_bytes().try_into().ok()?;
        digits
            .iter()
            .all(u8::is_ascii_digit)
            .then_some(Self(digits))
    }

    /// Compares two codes in time independent of where they differ.
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        let difference = self
            .0
            .iter()
            .zip(other.0.iter())
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            });
        core::hint::black_box(difference) == 0
    }
}

impl fmt::Debug for PairingCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PairingCode(******)")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing)]

    use super::*;
    use core::cell::Cell;

    #[derive(Clone)]
    struct Counter(Rc<Cell<u32>>);

    impl Entropy for Counter {
        fn fill(&self, bytes: &mut [u8]) -> Result<(), EntropyUnavailable> {
            let value = self.0.get();
            self.0.set(value.wrapping_add(1));
            bytes.copy_from_slice(&value.to_le_bytes()[..bytes.len()]);
            Ok(())
        }
    }

    #[test]
    fn mints_six_digits_with_leading_zeros() {
        let entropy = PairingEntropy::new(Counter(Rc::new(Cell::new(42))));
        let code = PairingCode::mint(&entropy).expect("code");
        assert_eq!(code.as_str(), "000042");
        let code = PairingCode::mint(&entropy).expect("code");
        assert_eq!(code.as_str(), "000043");
    }

    #[test]
    fn rejects_biased_draws() {
        let entropy = PairingEntropy::new(Counter(Rc::new(Cell::new(u32::MAX))));
        // u32::MAX is rejected, the counter wraps to 0, which is accepted.
        assert_eq!(
            PairingCode::mint(&entropy).expect("code").as_str(),
            "000000"
        );
    }

    #[test]
    fn unavailable_entropy_mints_nothing() {
        assert_eq!(
            PairingCode::mint(&PairingEntropy::unavailable()),
            Err(EntropyUnavailable)
        );
    }

    #[test]
    fn parses_plain_and_telegram_start_messages() {
        let code = PairingCode::from_value(123_456);
        for text in [
            "123456",
            "  123456\n",
            "/start 123456",
            "/start   123456 ",
            "/start@barracuda_bot 123456",
        ] {
            let parsed = PairingCode::parse_message(text).expect(text);
            assert!(parsed.matches(&code), "{text}");
        }
        for text in [
            "12345",
            "1234567",
            "12345a",
            "/start",
            "/start123456",
            "/startx 123456",
            "/start@bot",
            "code 123456",
            "１２３４５６",
        ] {
            assert!(PairingCode::parse_message(text).is_none(), "{text}");
        }
    }

    #[test]
    fn compares_every_digit() {
        let code = PairingCode::from_value(654_321);
        assert!(code.matches(&PairingCode::from_value(654_321)));
        assert!(!code.matches(&PairingCode::from_value(654_320)));
        assert!(!code.matches(&PairingCode::from_value(54_321)));
    }

    #[test]
    fn debug_hides_the_code() {
        let code = PairingCode::from_value(1);
        assert_eq!(alloc::format!("{code:?}"), "PairingCode(******)");
    }
}
