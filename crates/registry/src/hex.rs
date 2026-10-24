// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Lowercase hex, for the places a key has to be text: environment variables,
//! an operator's terminal, and the public name page.
//!
//! Deliberately not a dependency. It is thirty lines, and the one subtlety —
//! `from_str_radix` accepts a leading `+`, so `+f` used to parse as a byte — is
//! already documented in `tiamat_core::release::hex_to_array`, where the release
//! manifest's fuzz target found it. Repeating it here would be repeating the
//! bug.

/// Encodes bytes as lowercase hex.
#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        // Writing into a `String` cannot fail.
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Reads exactly `N` bytes of lowercase-or-uppercase hex, or nothing.
///
/// **Hex digits only**: `from_str_radix` takes a leading `+`, so a caller that
/// sliced pairs without this check would accept `+f` as a byte.
#[must_use]
pub fn decode<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; N];
    for (index, slot) in out.iter_mut().enumerate() {
        let pair = text.get(index * 2..index * 2 + 2)?;
        *slot = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let bytes = [0x00u8, 0x0f, 0xa5, 0xff];
        let text = encode(&bytes);
        assert_eq!(text, "000fa5ff");
        assert_eq!(decode::<4>(&text), Some(bytes));
        assert_eq!(decode::<4>("000FA5FF"), Some(bytes));
    }

    #[test]
    fn hex_refuses_everything_that_is_not_a_byte() {
        assert_eq!(decode::<1>("0"), None);
        assert_eq!(decode::<1>("000"), None);
        // The `+` lesson from the release manifest's fuzz target.
        assert_eq!(decode::<1>("+f"), None);
        assert_eq!(decode::<1>("zz"), None);
        assert_eq!(decode::<1>(" f"), None);
        // Non-ASCII digits are not hex digits.
        assert_eq!(decode::<1>("０f"), None);
    }
}
