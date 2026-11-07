//! 字符 ↔ 物理键码（macOS US 布局）。
//!
//! 触发判定只认键码：Shift+`;` 打出的是 `:`，中文输入法组合期也拿不到字符，
//! 只有物理键码在这些情况下仍然稳定。

/// (字符, 是否需要 Shift, 虚拟键码)
const KEYS: &[(char, bool, u32)] = &[
    ('a', false, 0),
    ('b', false, 11),
    ('c', false, 8),
    ('d', false, 2),
    ('e', false, 14),
    ('f', false, 3),
    ('g', false, 5),
    ('h', false, 4),
    ('i', false, 34),
    ('j', false, 38),
    ('k', false, 40),
    ('l', false, 37),
    ('m', false, 46),
    ('n', false, 45),
    ('o', false, 31),
    ('p', false, 35),
    ('q', false, 12),
    ('r', false, 15),
    ('s', false, 1),
    ('t', false, 17),
    ('u', false, 32),
    ('v', false, 9),
    ('w', false, 13),
    ('x', false, 7),
    ('y', false, 16),
    ('z', false, 6),
    ('1', false, 18),
    ('2', false, 19),
    ('3', false, 20),
    ('4', false, 21),
    ('5', false, 23),
    ('6', false, 22),
    ('7', false, 26),
    ('8', false, 28),
    ('9', false, 25),
    ('0', false, 29),
    ('!', true, 18),
    ('@', true, 19),
    ('#', true, 20),
    ('$', true, 21),
    ('%', true, 23),
    ('^', true, 22),
    ('&', true, 26),
    ('*', true, 28),
    ('(', true, 25),
    (')', true, 29),
    ('-', false, 27),
    ('_', true, 27),
    ('=', false, 24),
    ('+', true, 24),
    ('[', false, 33),
    ('{', true, 33),
    (']', false, 30),
    ('}', true, 30),
    ('\\', false, 42),
    ('|', true, 42),
    (';', false, 41),
    (':', true, 41),
    ('\'', false, 39),
    ('"', true, 39),
    (',', false, 43),
    ('<', true, 43),
    ('.', false, 47),
    ('>', true, 47),
    ('/', false, 44),
    ('?', true, 44),
    ('`', false, 50),
    ('~', true, 50),
    (' ', false, 49),
    (',', false, 43),
    ('.', false, 47),
];

// ---- 特殊键 ----
pub const BACKSPACE: u32 = 51;
pub const TAB: u32 = 48;
pub const RETURN: u32 = 36;
pub const ESCAPE: u32 = 53;
pub const LEFT: u32 = 123;
pub const RIGHT: u32 = 124;
pub const DOWN: u32 = 125;
pub const UP: u32 = 126;
pub const KEY_V: u32 = 9;
pub const KEY_1: u32 = 18;

/// 返回 (键码, 是否需要 Shift)。测试与粘贴兜底里用来合成任意 ASCII。
pub fn keycode_and_shift(c: char) -> Option<(u32, bool)> {
    if c.is_ascii_uppercase() {
        let lower = c.to_ascii_lowercase();
        let code = KEYS.iter().find(|k| k.0 == lower && !k.1)?.2;
        return Some((code, true));
    }
    KEYS.iter().find(|k| k.0 == c).map(|k| (k.2, k.1))
}

pub fn keycode_for_char(c: char) -> Option<u32> {
    let lowered = c.to_ascii_lowercase();
    KEYS.iter()
        .find(|k| k.0 == c || (k.0 == lowered && !k.1))
        .map(|k| k.2)
}

/// 键码 + Shift 还原字符（把面板里的按键拼成查询串）。
pub fn char_for_keycode(code: u32, shift: bool) -> Option<char> {
    let mut fallback: Option<char> = None;
    for k in KEYS {
        if k.2 != code {
            continue;
        }
        if k.1 == shift {
            return Some(k.0);
        }
        if fallback.is_none() {
            fallback = Some(if k.0.is_ascii_lowercase() && shift {
                k.0.to_ascii_uppercase()
            } else {
                k.0
            });
        }
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semicolon_maps_to_41() {
        assert_eq!(keycode_for_char(';'), Some(41));
    }

    #[test]
    fn shift_variant_resolves() {
        assert_eq!(char_for_keycode(41, false), Some(';'));
        assert_eq!(char_for_keycode(41, true), Some(':'));
    }

    #[test]
    fn uppercase_letter_resolves() {
        assert_eq!(char_for_keycode(0, true), Some('A'));
        assert_eq!(char_for_keycode(0, false), Some('a'));
    }

    #[test]
    fn non_ascii_has_no_keycode() {
        assert!(keycode_for_char('中').is_none());
    }
}
