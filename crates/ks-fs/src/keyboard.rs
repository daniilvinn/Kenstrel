//! Keys by position: the US-layout character of a Windows virtual-key code.

/// The character a US layout types with virtual key `vk` (no shift): letters, digits and the
/// punctuation keys. `None` for every other key.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn us_char_for_vk(vk: u8) -> Option<char> {
    Some(match vk {
        b'A'..=b'Z' => char::from(vk.to_ascii_lowercase()),
        b'0'..=b'9' => char::from(vk),
        0xBA => ';',  // VK_OEM_1
        0xBB => '=',  // VK_OEM_PLUS
        0xBC => ',',  // VK_OEM_COMMA
        0xBD => '-',  // VK_OEM_MINUS
        0xBE => '.',  // VK_OEM_PERIOD
        0xBF => '/',  // VK_OEM_2
        0xC0 => '`',  // VK_OEM_3
        0xDB => '[',  // VK_OEM_4
        0xDC => '\\', // VK_OEM_5
        0xDD => ']',  // VK_OEM_6
        0xDE => '\'', // VK_OEM_7
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_and_punctuation_keys_map_to_us_characters() {
        assert_eq!(us_char_for_vk(b'P'), Some('p'));
        assert_eq!(us_char_for_vk(b'7'), Some('7'));
        assert_eq!(us_char_for_vk(0xBE), Some('.'));
        assert_eq!(us_char_for_vk(0xDC), Some('\\'));
        assert_eq!(us_char_for_vk(0x0D), None, "enter has its own key code");
        assert_eq!(us_char_for_vk(0x70), None, "F1");
    }
}
