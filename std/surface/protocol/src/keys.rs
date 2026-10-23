//! The key-code table: the number each DOM `KeyboardEvent.code` name has
//! on the wire.
//!
//! A key input carries where the key sits as a number from this table, so a
//! producer maps it with a lookup rather than strings. Numbers are fixed and
//! never reused; 0 is a key the platform did not place. The table is the same
//! one Lumen numbers its own key codes with.

macro_rules! table {
    ($($name:ident = $n:literal),* $(,)?) => {
        /// Every named code with its number, in number order.
        pub const KEY_CODES: &[(u16, &str)] = &[$(($n, stringify!($name))),*];
    };
}

table! {
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

/// The DOM `code` name numbered `n`, `Unidentified` for 0 and for a number
/// the table does not have.
pub fn key_code_name(n: u16) -> &'static str {
    KEY_CODES
        .get(usize::from(n).wrapping_sub(1))
        .filter(|(m, _)| *m == n)
        .map_or("Unidentified", |(_, name)| name)
}

/// The number of the DOM `code` name, 0 for one the table does not have.
pub fn key_code_number(name: &str) -> u16 {
    KEY_CODES
        .iter()
        .find(|(_, n)| *n == name)
        .map_or(0, |(number, _)| *number)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Numbers are dense from 1 and both lookups agree.
    #[test]
    fn the_table_is_dense_and_reversible() {
        for (i, (n, name)) in KEY_CODES.iter().enumerate() {
            assert_eq!(usize::from(*n), i + 1);
            assert_eq!(key_code_name(*n), *name);
            assert_eq!(key_code_number(name), *n);
        }
        assert_eq!(key_code_name(0), "Unidentified");
        assert_eq!(key_code_number("NoSuchKey"), 0);
    }
}
