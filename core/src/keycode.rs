//! Physical key positions: the DOM `KeyboardEvent.code` set.
//!
//! A key event carries two answers to "which key": [`crate::input::Key`] is
//! what the key means under the user's layout (`q` on QWERTY, `a` on AZERTY
//! for the same key), and [`KeyCode`] is where the key sits, named after the
//! US layout (`KeyQ` either way). Text and shortcuts read the key; a game or
//! a 3D viewport binding movement to the WASD block reads the code.
//!
//! The names are the DOM's. Each code also has a fixed number
//! ([`KeyCode::number`]), so a process that receives key events over a wire
//! maps them with a table rather than strings; numbers are never reused.

macro_rules! key_codes {
    ($($name:ident = $n:literal),* $(,)?) => {
        /// Where a key sits on the keyboard, by its DOM `code` name.
        ///
        /// [`KeyCode::Unidentified`] is a key the platform did not place.
        #[derive(Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[repr(u16)]
        pub enum KeyCode {
            /// A key the platform did not place.
            #[default]
            Unidentified = 0,
            $(
                #[doc = concat!("The DOM's `", stringify!($name), "`.")]
                $name = $n,
            )*
        }

        /// Every name, back to back, in number order from `Unidentified`.
        /// One string and a byte per name keep the table small where every
        /// byte is paid for (a page downloads it).
        const JOINED: &str = concat!("Unidentified", $(stringify!($name)),*);
        /// The length of each name in [`JOINED`].
        const LENGTHS: &[u8] = &[12, $(stringify!($name).len() as u8),*];

        impl KeyCode {
            /// Every code but [`KeyCode::Unidentified`], in number order.
            pub const ALL: &'static [KeyCode] = &[$(KeyCode::$name),*];

            /// The DOM `code` name: `KeyW`, `ShiftLeft`, `Numpad1`.
            pub fn as_str(self) -> &'static str {
                let i = self as usize;
                let start: usize = LENGTHS[..i].iter().map(|&n| usize::from(n)).sum();
                &JOINED[start..start + usize::from(LENGTHS[i])]
            }

            /// The code's fixed number; 0 is [`KeyCode::Unidentified`].
            pub fn number(self) -> u16 {
                self as u16
            }

            /// The code numbered `n`, or `None` for a number no code has.
            pub fn from_number(n: u16) -> Option<KeyCode> {
                match n {
                    0 => Some(KeyCode::Unidentified),
                    // Numbered densely from 1, in the order of `ALL`.
                    n => KeyCode::ALL.get(usize::from(n) - 1).copied(),
                }
            }
        }

        impl std::str::FromStr for KeyCode {
            type Err = UnknownKeyCode;

            /// The code a DOM `code` name names.
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                if s.is_empty() {
                    return Ok(KeyCode::Unidentified);
                }
                let mut start = 0;
                for (i, &n) in LENGTHS.iter().enumerate() {
                    let end = start + usize::from(n);
                    if &JOINED[start..end] == s {
                        return Ok(match i {
                            0 => KeyCode::Unidentified,
                            i => KeyCode::ALL[i - 1],
                        });
                    }
                    start = end;
                }
                Err(UnknownKeyCode)
            }
        }
    };
}

key_codes! {
    Backquote = 1,
    Backslash = 2,
    BracketLeft = 3,
    BracketRight = 4,
    Comma = 5,
    Digit0 = 6,
    Digit1 = 7,
    Digit2 = 8,
    Digit3 = 9,
    Digit4 = 10,
    Digit5 = 11,
    Digit6 = 12,
    Digit7 = 13,
    Digit8 = 14,
    Digit9 = 15,
    Equal = 16,
    IntlBackslash = 17,
    IntlRo = 18,
    IntlYen = 19,
    KeyA = 20,
    KeyB = 21,
    KeyC = 22,
    KeyD = 23,
    KeyE = 24,
    KeyF = 25,
    KeyG = 26,
    KeyH = 27,
    KeyI = 28,
    KeyJ = 29,
    KeyK = 30,
    KeyL = 31,
    KeyM = 32,
    KeyN = 33,
    KeyO = 34,
    KeyP = 35,
    KeyQ = 36,
    KeyR = 37,
    KeyS = 38,
    KeyT = 39,
    KeyU = 40,
    KeyV = 41,
    KeyW = 42,
    KeyX = 43,
    KeyY = 44,
    KeyZ = 45,
    Minus = 46,
    Period = 47,
    Quote = 48,
    Semicolon = 49,
    Slash = 50,
    AltLeft = 51,
    AltRight = 52,
    Backspace = 53,
    CapsLock = 54,
    ContextMenu = 55,
    ControlLeft = 56,
    ControlRight = 57,
    Enter = 58,
    MetaLeft = 59,
    MetaRight = 60,
    ShiftLeft = 61,
    ShiftRight = 62,
    Space = 63,
    Tab = 64,
    Convert = 65,
    KanaMode = 66,
    Lang1 = 67,
    Lang2 = 68,
    Lang3 = 69,
    Lang4 = 70,
    Lang5 = 71,
    NonConvert = 72,
    Delete = 73,
    End = 74,
    Help = 75,
    Home = 76,
    Insert = 77,
    PageDown = 78,
    PageUp = 79,
    ArrowDown = 80,
    ArrowLeft = 81,
    ArrowRight = 82,
    ArrowUp = 83,
    NumLock = 84,
    Numpad0 = 85,
    Numpad1 = 86,
    Numpad2 = 87,
    Numpad3 = 88,
    Numpad4 = 89,
    Numpad5 = 90,
    Numpad6 = 91,
    Numpad7 = 92,
    Numpad8 = 93,
    Numpad9 = 94,
    NumpadAdd = 95,
    NumpadBackspace = 96,
    NumpadClear = 97,
    NumpadClearEntry = 98,
    NumpadComma = 99,
    NumpadDecimal = 100,
    NumpadDivide = 101,
    NumpadEnter = 102,
    NumpadEqual = 103,
    NumpadHash = 104,
    NumpadMemoryAdd = 105,
    NumpadMemoryClear = 106,
    NumpadMemoryRecall = 107,
    NumpadMemoryStore = 108,
    NumpadMemorySubtract = 109,
    NumpadMultiply = 110,
    NumpadParenLeft = 111,
    NumpadParenRight = 112,
    NumpadStar = 113,
    NumpadSubtract = 114,
    Escape = 115,
    F1 = 116,
    F2 = 117,
    F3 = 118,
    F4 = 119,
    F5 = 120,
    F6 = 121,
    F7 = 122,
    F8 = 123,
    F9 = 124,
    F10 = 125,
    F11 = 126,
    F12 = 127,
    F13 = 128,
    F14 = 129,
    F15 = 130,
    F16 = 131,
    F17 = 132,
    F18 = 133,
    F19 = 134,
    F20 = 135,
    F21 = 136,
    F22 = 137,
    F23 = 138,
    F24 = 139,
    Fn = 140,
    FnLock = 141,
    PrintScreen = 142,
    ScrollLock = 143,
    Pause = 144,
    BrowserBack = 145,
    BrowserFavorites = 146,
    BrowserForward = 147,
    BrowserHome = 148,
    BrowserRefresh = 149,
    BrowserSearch = 150,
    BrowserStop = 151,
    Eject = 152,
    LaunchApp1 = 153,
    LaunchApp2 = 154,
    LaunchMail = 155,
    MediaPlayPause = 156,
    MediaSelect = 157,
    MediaStop = 158,
    MediaTrackNext = 159,
    MediaTrackPrevious = 160,
    Power = 161,
    Sleep = 162,
    AudioVolumeDown = 163,
    AudioVolumeMute = 164,
    AudioVolumeUp = 165,
    WakeUp = 166,
    Hyper = 167,
    Super = 168,
    Turbo = 169,
    Abort = 170,
    Resume = 171,
    Suspend = 172,
    Again = 173,
    Copy = 174,
    Cut = 175,
    Find = 176,
    Open = 177,
    Paste = 178,
    Props = 179,
    Select = 180,
    Undo = 181,
    Hiragana = 182,
    Katakana = 183,
}

/// A string that is not a DOM `code` name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownKeyCode;

impl std::fmt::Display for UnknownKeyCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("not a DOM key code name")
    }
}

impl std::error::Error for UnknownKeyCode {}

/// The DOM name, which is also the variant's name: one table serves both.
impl std::fmt::Debug for KeyCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::fmt::Display for KeyCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every code survives the round trip through its name and its number,
    /// and numbers are dense from 1.
    #[test]
    fn names_and_numbers_round_trip() {
        for (i, code) in KeyCode::ALL.iter().enumerate() {
            assert_eq!(code.number() as usize, i + 1);
            assert_eq!(code.as_str().parse::<KeyCode>(), Ok(*code));
            assert_eq!(KeyCode::from_number(code.number()), Some(*code));
        }
        assert_eq!("".parse::<KeyCode>(), Ok(KeyCode::Unidentified));
        assert_eq!("NotAKey".parse::<KeyCode>(), Err(UnknownKeyCode));
        assert_eq!(KeyCode::from_number(u16::MAX), None);
        assert_eq!(KeyCode::KeyW.to_string(), "KeyW");
    }
}
