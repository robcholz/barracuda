//! The Cardputer's 4 by 14 keymap and its modifier rules.
//!
//! The layout is M5Cardputer's `_key_value_map` (`utility/Keyboard/Keyboard.h`):
//! each key has a first legend, a second ("Aa" layer) legend, and a `fn`
//! layer. As in M5Cardputer's `Keyboard_Class::getKey`, Shift or Control
//! selects the second legend, and Fn selects the `fn` layer where it has a
//! key. The Cardputer-ADV's controller reports the same positions.

use barracuda_peripheral::keyboard::{Key, KeyEvent, KeyState, Modifiers};

/// Rows of the layout, top to bottom.
pub const ROWS: usize = 4;
/// Columns of the layout, left to right.
pub const COLUMNS: usize = 14;
/// Keys on the layout.
pub const KEYS: usize = ROWS * COLUMNS;

#[derive(Clone, Copy)]
enum Legend {
    /// A printable key: its first and second legends.
    Text(char, char),
    /// A key that types nothing.
    Key(Key),
}

#[derive(Clone, Copy)]
struct Cap {
    legend: Legend,
    function: Option<Key>,
}

const fn text(first: char, second: char) -> Cap {
    Cap {
        legend: Legend::Text(first, second),
        function: None,
    }
}

const fn text_fn(first: char, second: char, function: Key) -> Cap {
    Cap {
        legend: Legend::Text(first, second),
        function: Some(function),
    }
}

const fn key(key: Key) -> Cap {
    Cap {
        legend: Legend::Key(key),
        function: None,
    }
}

const KEYMAP: [[Cap; COLUMNS]; ROWS] = [
    [
        text_fn('`', '~', Key::Escape),
        text_fn('1', '!', Key::F(1)),
        text_fn('2', '@', Key::F(2)),
        text_fn('3', '#', Key::F(3)),
        text_fn('4', '$', Key::F(4)),
        text_fn('5', '%', Key::F(5)),
        text_fn('6', '^', Key::F(6)),
        text_fn('7', '&', Key::F(7)),
        text_fn('8', '*', Key::F(8)),
        text_fn('9', '(', Key::F(9)),
        text_fn('0', ')', Key::F(10)),
        text_fn('-', '_', Key::F(11)),
        text_fn('=', '+', Key::F(12)),
        Cap {
            legend: Legend::Key(Key::Backspace),
            function: Some(Key::Delete),
        },
    ],
    [
        key(Key::Tab),
        text('q', 'Q'),
        text('w', 'W'),
        text('e', 'E'),
        text('r', 'R'),
        text('t', 'T'),
        text('y', 'Y'),
        text('u', 'U'),
        text('i', 'I'),
        text('o', 'O'),
        text('p', 'P'),
        text('[', '{'),
        text(']', '}'),
        text('\\', '|'),
    ],
    [
        key(Key::Function),
        key(Key::Shift),
        text('a', 'A'),
        text('s', 'S'),
        text('d', 'D'),
        text('f', 'F'),
        text('g', 'G'),
        text('h', 'H'),
        text('j', 'J'),
        text('k', 'K'),
        text('l', 'L'),
        text_fn(';', ':', Key::Up),
        text('\'', '"'),
        key(Key::Enter),
    ],
    [
        key(Key::Control),
        key(Key::Option),
        key(Key::Alt),
        text('z', 'Z'),
        text('x', 'X'),
        text('c', 'C'),
        text('v', 'V'),
        text('b', 'B'),
        text('n', 'N'),
        text('m', 'M'),
        text_fn(',', '<', Key::Left),
        text_fn('.', '>', Key::Down),
        text_fn('/', '?', Key::Right),
        text(' ', ' '),
    ],
];

/// Returns whether the key at `row`, `column` is a modifier.
#[must_use]
pub fn is_modifier(row: usize, column: usize) -> bool {
    KEYMAP
        .get(row)
        .and_then(|keys| keys.get(column))
        .is_some_and(|cap| matches!(cap.legend, Legend::Key(key) if Modifiers::of(key).is_some()))
}

/// Translates key positions into events, tracking the held modifiers and
/// the key each held position reported when it went down.
pub struct Layout {
    modifiers: Modifiers,
    held: [Option<Key>; KEYS],
}

impl Layout {
    /// Starts with no key held.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            modifiers: Modifiers::empty(),
            held: [None; KEYS],
        }
    }

    /// Translates the key at `row`, `column` going down or coming up; `None`
    /// for a position outside the layout.
    pub fn translate(&mut self, row: usize, column: usize, pressed: bool) -> Option<KeyEvent> {
        let cap = *KEYMAP.get(row)?.get(column)?;
        let index = row.checked_mul(COLUMNS)?.checked_add(column)?;
        let code = u16::try_from(index).ok()?;
        let state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        let first = match cap.legend {
            Legend::Text(first, _) => Key::Character(first),
            Legend::Key(key) => key,
        };
        if let Some(modifier) = Modifiers::of(first) {
            self.modifiers = if pressed {
                self.modifiers.union(modifier)
            } else {
                self.modifiers.difference(modifier)
            };
            return Some(KeyEvent::new(first, state, self.modifiers, None, code));
        }
        let slot = self.held.get_mut(index)?;
        if !pressed {
            // A release reports the key its press did, whatever the
            // modifiers did since.
            let key = slot.take().unwrap_or(first);
            return Some(KeyEvent::new(key, state, self.modifiers, None, code));
        }
        let (key, text) = match cap.function {
            Some(function) if self.modifiers.contains(Modifiers::FUNCTION) => (function, None),
            _ => {
                let second = self.modifiers.contains(Modifiers::SHIFT)
                    || self.modifiers.contains(Modifiers::CONTROL);
                let text = match cap.legend {
                    Legend::Text(first, other) => Some(if second { other } else { first }),
                    Legend::Key(_) => None,
                };
                (first, text)
            }
        };
        *slot = Some(key);
        Some(KeyEvent::new(key, state, self.modifiers, text, code))
    }
}

impl Default for Layout {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use barracuda_peripheral::keyboard::{Key, Modifiers};

    use super::{Layout, is_modifier};

    #[test]
    fn types_first_and_second_legends() {
        let mut layout = Layout::new();
        let a = layout.translate(2, 2, true).expect("a");
        assert_eq!((a.key(), a.text()), (Key::Character('a'), Some('a')));
        assert_eq!(layout.translate(2, 2, false).expect("a").text(), None);
        let shift = layout.translate(2, 1, true).expect("shift");
        assert_eq!(shift.key(), Key::Shift);
        assert!(shift.modifiers().contains(Modifiers::SHIFT));
        assert_eq!(layout.translate(0, 1, true).expect("1").text(), Some('!'));
        layout.translate(2, 1, false).expect("shift up");
        assert_eq!(
            layout.translate(3, 13, true).expect("space").text(),
            Some(' ')
        );
        assert_eq!(
            layout.translate(2, 13, true).expect("enter").key(),
            Key::Enter
        );
    }

    #[test]
    fn function_layer_selects_arrows_and_function_keys() {
        let mut layout = Layout::new();
        layout.translate(2, 0, true).expect("fn");
        let up = layout.translate(2, 11, true).expect("up");
        assert_eq!((up.key(), up.text()), (Key::Up, None));
        assert_eq!(layout.translate(0, 5, true).expect("f5").key(), Key::F(5));
        assert_eq!(
            layout.translate(0, 13, true).expect("del").key(),
            Key::Delete
        );
        // A key without a fn legend types as usual.
        assert_eq!(layout.translate(1, 1, true).expect("q").text(), Some('q'));
        layout.translate(2, 0, false).expect("fn up");
        // The release still names the key the press reported.
        assert_eq!(layout.translate(2, 11, false).expect("up").key(), Key::Up);
    }

    #[test]
    fn knows_its_modifiers_and_bounds() {
        assert!(is_modifier(3, 0));
        assert!(is_modifier(2, 1));
        assert!(!is_modifier(2, 2));
        assert!(Layout::new().translate(4, 0, true).is_none());
        assert!(Layout::new().translate(0, 14, true).is_none());
        assert_eq!(
            Layout::new().translate(3, 13, true).expect("space").code(),
            55
        );
    }
}
