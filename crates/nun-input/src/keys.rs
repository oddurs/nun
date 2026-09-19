//! Keys as nun sees them, independent of how the terminal encoded them.
//!
//! The same keystroke arrives in different shapes depending on whether the
//! Kitty keyboard protocol was negotiated: legacy encoding reports `Shift+a`
//! as a bare `A`, the protocol reports it as `a` with Shift held. [`Key::new`]
//! normalises both to one value so a binding written once matches either way.

use std::fmt;

/// Modifier keys held with a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Mods(u8);

impl Mods {
    /// No modifiers.
    pub const NONE: Self = Self(0);
    /// Control.
    pub const CTRL: Self = Self(1);
    /// Alt, or Option on a Mac.
    pub const ALT: Self = Self(1 << 1);
    /// Shift.
    pub const SHIFT: Self = Self(1 << 2);
    /// Cmd on a Mac, Super elsewhere. Only reported under the Kitty protocol.
    pub const CMD: Self = Self(1 << 3);

    /// Whether every modifier in `other` is held.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// These modifiers without `other`.
    #[must_use]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// True when nothing is held.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for Mods {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl std::ops::BitOrAssign for Mods {
    fn bitor_assign(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

/// Which key, ignoring modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Code {
    /// A key that produces a character. Letters are always lowercase here;
    /// Shift is recorded in [`Mods`].
    Char(char),
    /// A function key, `F1` to `F24`.
    F(u8),
    /// Enter or Return.
    Enter,
    /// Tab.
    Tab,
    /// Backspace.
    Backspace,
    /// Forward delete.
    Delete,
    /// Insert.
    Insert,
    /// Escape.
    Esc,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Home.
    Home,
    /// End.
    End,
    /// Page Up.
    PageUp,
    /// Page Down.
    PageDown,
}

/// One keystroke: a key and the modifiers held with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    /// Which key.
    pub code: Code,
    /// What was held.
    pub mods: Mods,
}

impl Key {
    /// A keystroke, normalised.
    ///
    /// An uppercase letter becomes the lowercase letter with Shift, so `A` and
    /// `Shift+a` are the same key. Shift is dropped from any other character,
    /// because the character already says it was held: `?` is `Shift+/` on one
    /// layout and something else on another, and the binding means the `?`.
    #[must_use]
    pub fn new(code: Code, mods: Mods) -> Self {
        match code {
            Code::Char(ch) if ch.is_uppercase() => {
                let lower = ch.to_lowercase().next().unwrap_or(ch);
                Self { code: Code::Char(lower), mods: mods | Mods::SHIFT }
            }
            Code::Char(ch) if !ch.is_lowercase() && ch != ' ' => {
                Self { code, mods: mods.without(Mods::SHIFT) }
            }
            _ => Self { code, mods },
        }
    }

    /// A key with no modifiers.
    #[must_use]
    pub fn plain(code: Code) -> Self {
        Self::new(code, Mods::NONE)
    }

    /// Whether typing this key should insert text rather than run a command.
    ///
    /// A character with nothing held but Shift is text. Alt is left out on
    /// purpose: on a Mac, Option is how people type `@`, `€` and `|`.
    #[must_use]
    pub fn is_text(&self) -> bool {
        matches!(self.code, Code::Char(_))
            && !self.mods.contains(Mods::CTRL)
            && !self.mods.contains(Mods::CMD)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (modifier, name) in
            [(Mods::CMD, "Cmd"), (Mods::CTRL, "Ctrl"), (Mods::ALT, "Alt"), (Mods::SHIFT, "Shift")]
        {
            if self.mods.contains(modifier) {
                write!(f, "{name}+")?;
            }
        }
        match self.code {
            Code::Char(' ') => f.write_str("Space"),
            Code::Char(ch) => write!(f, "{}", ch.to_uppercase()),
            Code::F(n) => write!(f, "F{n}"),
            Code::Enter => f.write_str("Enter"),
            Code::Tab => f.write_str("Tab"),
            Code::Backspace => f.write_str("Backspace"),
            Code::Delete => f.write_str("Delete"),
            Code::Insert => f.write_str("Insert"),
            Code::Esc => f.write_str("Esc"),
            Code::Left => f.write_str("Left"),
            Code::Right => f.write_str("Right"),
            Code::Up => f.write_str("Up"),
            Code::Down => f.write_str("Down"),
            Code::Home => f.write_str("Home"),
            Code::End => f.write_str("End"),
            Code::PageUp => f.write_str("PageUp"),
            Code::PageDown => f.write_str("PageDown"),
        }
    }
}

/// A key sequence, displayed the way a menu shows it: `Ctrl+K Ctrl+T`.
#[derive(Debug, Clone, Copy)]
pub struct Sequence<'a>(pub &'a [Key]);

impl fmt::Display for Sequence<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, key) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{key}")?;
        }
        Ok(())
    }
}

/// Why a binding could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

/// Read a key sequence as written in `nun.toml`: `"ctrl+s"`, `"cmd+shift+p"`,
/// `"ctrl+k ctrl+t"`.
///
/// Case does not matter, and the common aliases are accepted — `control`,
/// `option`, `super`, `return`, `escape` — because the one thing worse than a
/// strict parser is one that rejects the name the user's own keyboard prints.
///
/// # Errors
///
/// When a key or modifier name is not recognised, or the sequence is empty.
pub fn parse_sequence(text: &str) -> Result<Vec<Key>, ParseError> {
    let keys = text.split_whitespace().map(parse_key).collect::<Result<Vec<_>, _>>()?;
    if keys.is_empty() {
        return Err(ParseError("an empty key binding".into()));
    }
    Ok(keys)
}

/// Read one keystroke, such as `ctrl+shift+z`.
///
/// # Errors
///
/// When a name is not recognised.
pub fn parse_key(text: &str) -> Result<Key, ParseError> {
    // `ctrl++` binds the plus key: split on the last '+' that has something
    // after it.
    let (modifiers, key) = match text.strip_suffix("++") {
        Some(head) => (head, "+"),
        None if text == "+" => ("", "+"),
        None => text.rsplit_once('+').unwrap_or(("", text)),
    };

    let mut mods = Mods::NONE;
    for name in modifiers.split('+').filter(|name| !name.is_empty()) {
        mods |= match name.to_lowercase().as_str() {
            "ctrl" | "control" => Mods::CTRL,
            "alt" | "option" | "opt" => Mods::ALT,
            "shift" => Mods::SHIFT,
            "cmd" | "command" | "super" | "win" => Mods::CMD,
            other => return Err(ParseError(format!("`{other}` is not a modifier in `{text}`"))),
        };
    }

    let lower = key.to_lowercase();
    let code = match lower.as_str() {
        "enter" | "return" => Code::Enter,
        "tab" => Code::Tab,
        "backspace" => Code::Backspace,
        "delete" | "del" => Code::Delete,
        "insert" | "ins" => Code::Insert,
        "esc" | "escape" => Code::Esc,
        "left" => Code::Left,
        "right" => Code::Right,
        "up" => Code::Up,
        "down" => Code::Down,
        "home" => Code::Home,
        "end" => Code::End,
        "pageup" | "pgup" => Code::PageUp,
        "pagedown" | "pgdn" => Code::PageDown,
        "space" => Code::Char(' '),
        "plus" => Code::Char('+'),
        _ => {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                // Case never implies Shift in a binding: `ctrl+S` is `ctrl+s`,
                // and Shift is written out when it is meant.
                (Some(ch), None) => Code::Char(ch.to_lowercase().next().unwrap_or(ch)),
                _ => match lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                    Some(n @ 1..=24) => Code::F(n),
                    _ => return Err(ParseError(format!("`{key}` is not a key in `{text}`"))),
                },
            }
        }
    };
    Ok(Key::new(code, mods))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn key(text: &str) -> Key {
        parse_key(text).unwrap()
    }

    #[test]
    fn an_uppercase_letter_is_the_letter_with_shift() {
        assert_eq!(
            Key::new(Code::Char('Z'), Mods::CTRL),
            Key::new(Code::Char('z'), Mods::CTRL | Mods::SHIFT)
        );
    }

    #[test]
    fn shift_on_a_symbol_is_already_in_the_symbol() {
        assert_eq!(Key::new(Code::Char('?'), Mods::SHIFT), Key::plain(Code::Char('?')));
    }

    #[test]
    fn shift_on_a_non_character_key_is_kept() {
        assert!(Key::new(Code::Left, Mods::SHIFT).mods.contains(Mods::SHIFT));
    }

    #[test]
    fn non_ascii_letters_normalise_too() {
        assert_eq!(Key::plain(Code::Char('Ö')), Key::new(Code::Char('ö'), Mods::SHIFT));
    }

    #[test]
    fn bindings_parse_in_any_case_with_the_usual_aliases() {
        assert_eq!(key("Ctrl+S"), Key::new(Code::Char('s'), Mods::CTRL));
        assert_eq!(key("control+s"), key("ctrl+s"));
        assert_eq!(key("option+left"), Key::new(Code::Left, Mods::ALT));
        assert_eq!(key("super+p"), Key::new(Code::Char('p'), Mods::CMD));
        assert_eq!(key("cmd+shift+p"), Key::new(Code::Char('p'), Mods::CMD | Mods::SHIFT));
        assert_eq!(key("return"), Key::plain(Code::Enter));
        assert_eq!(key("f12"), Key::plain(Code::F(12)));
        assert_eq!(key("ctrl+space"), Key::new(Code::Char(' '), Mods::CTRL));
    }

    #[test]
    fn the_plus_key_can_be_bound() {
        assert_eq!(key("ctrl++"), Key::new(Code::Char('+'), Mods::CTRL));
        assert_eq!(key("ctrl+plus"), key("ctrl++"));
    }

    #[test]
    fn a_chord_is_several_keys() {
        let chord = parse_sequence("ctrl+k  ctrl+t").unwrap();
        assert_eq!(chord, vec![key("ctrl+k"), key("ctrl+t")]);
    }

    #[test]
    fn a_misspelled_name_says_which_part_is_wrong() {
        let error = parse_key("ctrl+sift+s").unwrap_err();
        assert!(error.0.contains("`sift`"), "{error}");
        assert!(parse_key("ctrl+nope").unwrap_err().0.contains("`nope`"));
        assert!(parse_key("f25").is_err());
        assert!(parse_sequence("   ").is_err());
    }

    #[test]
    fn keys_display_the_way_menus_show_them() {
        assert_eq!(key("cmd+shift+p").to_string(), "Cmd+Shift+P");
        assert_eq!(key("ctrl+space").to_string(), "Ctrl+Space");
        let chord = parse_sequence("ctrl+k ctrl+t").unwrap();
        assert_eq!(Sequence(&chord).to_string(), "Ctrl+K Ctrl+T");
    }

    #[test]
    fn typing_is_text_and_a_chord_is_not() {
        assert!(Key::plain(Code::Char('a')).is_text());
        assert!(Key::new(Code::Char('a'), Mods::SHIFT).is_text());
        assert!(Key::new(Code::Char('@'), Mods::ALT).is_text(), "Option types symbols on a Mac");
        assert!(!key("ctrl+a").is_text());
        assert!(!key("cmd+a").is_text());
        assert!(!Key::plain(Code::Enter).is_text());
    }

    fn any_key() -> impl Strategy<Value = Key> {
        let code = prop_oneof![
            prop::char::range('!', '~').prop_map(Code::Char),
            prop::char::range('à', 'ÿ').prop_map(Code::Char),
            (1u8..=24).prop_map(Code::F),
            Just(Code::Enter),
            Just(Code::Tab),
            Just(Code::Left),
            Just(Code::PageDown),
            Just(Code::Char(' ')),
        ];
        (code, 0u8..16).prop_map(|(code, bits)| Key::new(code, Mods(bits)))
    }

    proptest! {
        /// Whatever a binding displays as, it parses back to the same key, so
        /// the palette can show a binding the user can copy into their config.
        #[test]
        fn display_round_trips_through_parse(key in any_key()) {
            prop_assert_eq!(parse_key(&key.to_string()).unwrap(), key);
        }

        #[test]
        fn normalising_twice_changes_nothing(key in any_key()) {
            prop_assert_eq!(Key::new(key.code, key.mods), key);
        }
    }
}
