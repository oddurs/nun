//! What to send a program for a key, the mouse, a paste or focus.
//!
//! Keys go in the legacy xterm encoding, which every program reads: the
//! emulator tells programs there is no Kitty keyboard protocol, so none of
//! them expect it. A key that encoding cannot say — Cmd anything, which
//! never reaches a program in a real terminal either — has no bytes, and
//! [`key`] says so with `None`, leaving it for the editor.

use crate::emulator::Modes;

/// A key, as far as a program in a terminal can be told about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A character key.
    Char(char),
    /// Return.
    Enter,
    /// Tab.
    Tab,
    /// Shift+Tab, as terminals report it.
    BackTab,
    /// Backspace.
    Backspace,
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
    /// Insert.
    Insert,
    /// Forward delete.
    Delete,
    /// A function key, from 1.
    F(u8),
}

/// Modifiers held with a key or the mouse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods(u8);

impl Mods {
    /// None held.
    pub const NONE: Self = Self(0);
    /// Shift.
    pub const SHIFT: Self = Self(1);
    /// Alt, or Option, or Meta.
    pub const ALT: Self = Self(2);
    /// Control.
    pub const CTRL: Self = Self(4);

    /// Whether every modifier in `other` is held.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// These and `other`.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The xterm modifier parameter: one more than the bits.
    const fn parameter(self) -> u8 {
        1 + self.0
    }
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// The left one.
    Left,
    /// The middle one.
    Middle,
    /// The right one.
    Right,
}

impl Button {
    const fn code(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
        }
    }
}

/// What the pointer did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pointer {
    /// A button went down.
    Press(Button),
    /// A button came up.
    Release(Button),
    /// It moved with a button held.
    Drag(Button),
    /// It moved with nothing held.
    Move,
    /// The wheel turned up a notch.
    WheelUp,
    /// The wheel turned down a notch.
    WheelDown,
}

/// The bytes for `key` with the modifiers `held`, in the program's current modes.
/// `None` for a key the legacy encoding cannot say.
#[must_use]
pub fn key(key: Key, held: Mods, modes: Modes) -> Option<Vec<u8>> {
    let alt = held.contains(Mods::ALT);
    let ctrl = held.contains(Mods::CTRL);
    let mut out = Vec::new();
    match key {
        Key::Char(ch) => {
            if alt {
                out.push(0x1b);
            }
            if ctrl {
                out.push(control(ch)?);
            } else {
                let mut buffer = [0; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
            }
            return Some(out);
        }
        Key::Enter | Key::Tab | Key::Backspace | Key::Esc | Key::BackTab => {
            if alt {
                out.push(0x1b);
            }
            out.extend_from_slice(match key {
                Key::Enter => b"\r",
                Key::Tab if held.contains(Mods::SHIFT) => b"\x1b[Z",
                Key::Tab => b"\t",
                Key::BackTab => b"\x1b[Z",
                // Ctrl+Backspace is the old BS; Backspace itself is DEL.
                Key::Backspace if ctrl => b"\x08",
                Key::Backspace => b"\x7f",
                _ => b"\x1b",
            });
            return Some(out);
        }
        _ => {}
    }

    let plain = held == Mods::NONE;
    let app = modes.contains(Modes::APP_CURSOR);
    // The cursor keys and Home and End: `ESC [ A`, `ESC O A` in application
    // mode, and `ESC [ 1 ; m A` with modifiers.
    let letter = match key {
        Key::Up => Some(b'A'),
        Key::Down => Some(b'B'),
        Key::Right => Some(b'C'),
        Key::Left => Some(b'D'),
        Key::Home => Some(b'H'),
        Key::End => Some(b'F'),
        Key::F(n @ 1..=4) => Some(b'P' + (n - 1)),
        _ => None,
    };
    if let Some(letter) = letter {
        let function = matches!(key, Key::F(_));
        return Some(if plain && (app || function) {
            vec![0x1b, b'O', letter]
        } else if plain {
            vec![0x1b, b'[', letter]
        } else {
            format!("\x1b[1;{}{}", held.parameter(), char::from(letter)).into_bytes()
        });
    }

    // The rest are `ESC [ n ~`, with `; m` before the tilde for modifiers.
    let number = match key {
        Key::Insert => 2,
        Key::Delete => 3,
        Key::PageUp => 5,
        Key::PageDown => 6,
        Key::F(5) => 15,
        Key::F(6) => 17,
        Key::F(7) => 18,
        Key::F(8) => 19,
        Key::F(9) => 20,
        Key::F(10) => 21,
        Key::F(11) => 23,
        Key::F(12) => 24,
        _ => return None,
    };
    Some(if plain {
        format!("\x1b[{number}~").into_bytes()
    } else {
        format!("\x1b[{number};{}~", held.parameter()).into_bytes()
    })
}

/// The control character Ctrl sends with `ch`, where there is one.
fn control(ch: char) -> Option<u8> {
    let byte = u8::try_from(ch).ok()?;
    match byte {
        b'@' | b' ' | b'2' => Some(0),
        b'a'..=b'z' => Some(byte - b'a' + 1),
        b'A'..=b'Z' => Some(byte - b'A' + 1),
        b'[' | b'3' => Some(0x1b),
        b'\\' | b'4' => Some(0x1c),
        b']' | b'5' => Some(0x1d),
        b'^' | b'6' => Some(0x1e),
        b'_' | b'/' | b'7' => Some(0x1f),
        b'?' | b'8' => Some(0x7f),
        _ => None,
    }
}

/// The bytes that report what the pointer did at `(col, row)` — zero-based
/// within the terminal — with the modifiers `held`, in the program's mouse
/// modes. `None` when the
/// program has not asked for this.
#[must_use]
pub fn pointer(event: Pointer, col: u16, row: u16, held: Mods, modes: Modes) -> Option<Vec<u8>> {
    let wanted = match event {
        Pointer::Press(_) | Pointer::Release(_) | Pointer::WheelUp | Pointer::WheelDown => {
            modes.mouse()
        }
        Pointer::Drag(_) => {
            modes.contains(Modes::MOUSE_DRAG) || modes.contains(Modes::MOUSE_MOTION)
        }
        Pointer::Move => modes.contains(Modes::MOUSE_MOTION),
    };
    if !wanted {
        return None;
    }
    let sgr = modes.contains(Modes::SGR_MOUSE);
    let mut code = match event {
        Pointer::Press(button) | Pointer::Drag(button) => button.code(),
        // Only SGR can say which button came up.
        Pointer::Release(button) if sgr => button.code(),
        Pointer::Release(_) | Pointer::Move => 3,
        Pointer::WheelUp => 64,
        Pointer::WheelDown => 65,
    };
    if matches!(event, Pointer::Drag(_) | Pointer::Move) {
        code += 32;
    }
    if held.contains(Mods::SHIFT) {
        code += 4;
    }
    if held.contains(Mods::ALT) {
        code += 8;
    }
    if held.contains(Mods::CTRL) {
        code += 16;
    }
    let (x, y) = (u32::from(col) + 1, u32::from(row) + 1);
    if sgr {
        let end = if matches!(event, Pointer::Release(_)) { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes());
    }
    let mut out = vec![0x1b, b'[', b'M', 32 + code];
    for position in [x, y] {
        if modes.contains(Modes::UTF8_MOUSE) {
            let ch = char::from_u32(32 + position)?;
            let mut buffer = [0; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
        } else {
            // The original encoding has one byte a coordinate, and stops at
            // 223. Past that there is nothing true to send.
            out.push(u8::try_from(32 + position).ok()?);
        }
    }
    Some(out)
}

/// What a turn of the wheel sends a full-screen program that did not ask for
/// the mouse but reads the arrows: `lines` presses of Up or Down, as xterm's
/// alternate scroll mode has it. `None` where it does not apply.
#[must_use]
pub fn wheel_as_arrows(up: bool, lines: usize, modes: Modes) -> Option<Vec<u8>> {
    let applies = modes.contains(Modes::ALT_SCREEN)
        && modes.contains(Modes::ALTERNATE_SCROLL)
        && !modes.mouse();
    if !applies {
        return None;
    }
    let arrow = key(if up { Key::Up } else { Key::Down }, Mods::NONE, modes)?;
    Some(arrow.repeat(lines))
}

/// The bytes for pasting `text`. Bracketed when the program asked for it,
/// with anything that could close the bracket early taken out; line endings
/// become the carriage return Enter sends, either way.
#[must_use]
pub fn paste(text: &str, modes: Modes) -> Vec<u8> {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if modes.contains(Modes::BRACKETED_PASTE) {
        let inner = text.replace('\x1b', "");
        format!("\x1b[200~{inner}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

/// What to send when the terminal gains or loses the keyboard, if the program
/// asked to be told.
#[must_use]
pub fn focus(focused: bool, modes: Modes) -> Option<Vec<u8>> {
    modes
        .contains(Modes::FOCUS)
        .then(|| if focused { b"\x1b[I".to_vec() } else { b"\x1b[O".to_vec() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(k: Key) -> Vec<u8> {
        key(k, Mods::NONE, Modes::default()).unwrap()
    }

    #[test]
    fn characters_are_their_utf8() {
        assert_eq!(plain(Key::Char('a')), b"a");
        assert_eq!(plain(Key::Char('é')), "é".as_bytes());
        assert_eq!(plain(Key::Char('😀')), "😀".as_bytes());
    }

    #[test]
    fn control_sends_the_control_character() {
        assert_eq!(key(Key::Char('c'), Mods::CTRL, Modes::default()).unwrap(), [3]);
        assert_eq!(key(Key::Char('a'), Mods::CTRL, Modes::default()).unwrap(), [1]);
        assert_eq!(key(Key::Char(' '), Mods::CTRL, Modes::default()).unwrap(), [0]);
        assert_eq!(key(Key::Char('\\'), Mods::CTRL, Modes::default()).unwrap(), [0x1c]);
        assert_eq!(key(Key::Char('é'), Mods::CTRL, Modes::default()), None, "no control form");
    }

    #[test]
    fn alt_prefixes_escape() {
        assert_eq!(key(Key::Char('b'), Mods::ALT, Modes::default()).unwrap(), b"\x1bb");
        assert_eq!(key(Key::Backspace, Mods::ALT, Modes::default()).unwrap(), b"\x1b\x7f");
    }

    #[test]
    fn the_arrows_follow_application_cursor_mode() {
        assert_eq!(plain(Key::Up), b"\x1b[A");
        assert_eq!(key(Key::Up, Mods::NONE, Modes::APP_CURSOR).unwrap(), b"\x1bOA");
        assert_eq!(key(Key::Left, Mods::CTRL, Modes::APP_CURSOR).unwrap(), b"\x1b[1;5D");
        assert_eq!(
            key(Key::Right, Mods::SHIFT.union(Mods::ALT), Modes::default()).unwrap(),
            b"\x1b[1;4C"
        );
    }

    #[test]
    fn editing_and_function_keys_are_xterms() {
        assert_eq!(plain(Key::Enter), b"\r");
        assert_eq!(plain(Key::Backspace), b"\x7f");
        assert_eq!(plain(Key::BackTab), b"\x1b[Z");
        assert_eq!(plain(Key::Delete), b"\x1b[3~");
        assert_eq!(plain(Key::PageDown), b"\x1b[6~");
        assert_eq!(plain(Key::F(1)), b"\x1bOP");
        assert_eq!(plain(Key::F(5)), b"\x1b[15~");
        assert_eq!(plain(Key::F(12)), b"\x1b[24~");
        assert_eq!(key(Key::F(5), Mods::SHIFT, Modes::default()).unwrap(), b"\x1b[15;2~");
        assert_eq!(key(Key::F(20), Mods::NONE, Modes::default()), None);
    }

    #[test]
    fn the_mouse_is_reported_only_when_asked_for() {
        let press = Pointer::Press(Button::Left);
        assert_eq!(pointer(press, 0, 0, Mods::NONE, Modes::default()), None);
        assert_eq!(pointer(Pointer::Move, 0, 0, Mods::NONE, Modes::MOUSE_CLICK), None);
        assert_eq!(
            pointer(Pointer::Drag(Button::Left), 0, 0, Mods::NONE, Modes::MOUSE_CLICK),
            None
        );
        assert!(
            pointer(Pointer::Drag(Button::Left), 0, 0, Mods::NONE, Modes::MOUSE_DRAG).is_some()
        );
    }

    #[test]
    fn sgr_reports_say_where_and_which_button() {
        let modes = Modes::MOUSE_CLICK.union(Modes::SGR_MOUSE);
        assert_eq!(
            pointer(Pointer::Press(Button::Left), 4, 2, Mods::NONE, modes).unwrap(),
            b"\x1b[<0;5;3M"
        );
        assert_eq!(
            pointer(Pointer::Release(Button::Right), 4, 2, Mods::NONE, modes).unwrap(),
            b"\x1b[<2;5;3m"
        );
        assert_eq!(pointer(Pointer::WheelDown, 0, 0, Mods::CTRL, modes).unwrap(), b"\x1b[<81;1;1M");
        let drag = Modes::MOUSE_DRAG.union(Modes::SGR_MOUSE);
        assert_eq!(
            pointer(Pointer::Drag(Button::Left), 1, 1, Mods::NONE, drag).unwrap(),
            b"\x1b[<32;2;2M"
        );
    }

    #[test]
    fn the_original_encoding_is_offset_by_32_and_stops_at_223() {
        let modes = Modes::MOUSE_CLICK;
        assert_eq!(
            pointer(Pointer::Press(Button::Left), 0, 0, Mods::NONE, modes).unwrap(),
            [0x1b, b'[', b'M', 32, 33, 33]
        );
        assert_eq!(
            pointer(Pointer::Release(Button::Left), 0, 0, Mods::NONE, modes).unwrap()[3],
            35
        );
        assert_eq!(pointer(Pointer::Press(Button::Left), 300, 0, Mods::NONE, modes), None);
        let utf8 = Modes::MOUSE_CLICK.union(Modes::UTF8_MOUSE);
        let far = pointer(Pointer::Press(Button::Left), 300, 0, Mods::NONE, utf8).unwrap();
        assert_eq!(&far[4..], "ō!".as_bytes(), "{far:?}");
    }

    #[test]
    fn the_wheel_sends_arrows_to_a_full_screen_program_that_reads_them() {
        let pager = Modes::ALT_SCREEN.union(Modes::ALTERNATE_SCROLL);
        assert_eq!(wheel_as_arrows(true, 3, pager).unwrap(), b"\x1b[A\x1b[A\x1b[A");
        assert_eq!(wheel_as_arrows(false, 1, pager.union(Modes::APP_CURSOR)).unwrap(), b"\x1bOB");
        assert_eq!(
            wheel_as_arrows(true, 3, Modes::ALTERNATE_SCROLL),
            None,
            "the shell has scrollback"
        );
        assert_eq!(
            wheel_as_arrows(true, 3, pager.union(Modes::MOUSE_CLICK)),
            None,
            "it wants the wheel itself"
        );
    }

    #[test]
    fn a_paste_is_bracketed_when_asked_and_cannot_close_the_bracket_itself() {
        assert_eq!(paste("a\nb", Modes::default()), b"a\rb");
        assert_eq!(paste("a\r\nb", Modes::default()), b"a\rb");
        assert_eq!(paste("x\x1b[201~y", Modes::BRACKETED_PASTE), b"\x1b[200~x[201~y\x1b[201~");
    }

    #[test]
    fn focus_is_reported_only_when_asked_for() {
        assert_eq!(focus(true, Modes::default()), None);
        assert_eq!(focus(true, Modes::FOCUS).unwrap(), b"\x1b[I");
        assert_eq!(focus(false, Modes::FOCUS).unwrap(), b"\x1b[O");
    }
}
