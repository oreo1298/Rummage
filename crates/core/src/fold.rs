//! Case folding, normalization and collation for file names.
//!
//! File names are raw bytes. Valid UTF-8 sequences are folded with Unicode *simple* case folding
//! (one code point to one code point, so folding never changes how many characters a name has),
//! and bytes that are not valid UTF-8 are carried through unchanged and only ever match
//! themselves.

use std::cmp::Ordering;

use icu_casemap::{CaseMapper, CaseMapperBorrowed};
use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};

const CASE_MAPPER: CaseMapperBorrowed<'static> = CaseMapper::new();
const NFC: ComposingNormalizerBorrowed<'static> = ComposingNormalizerBorrowed::new_nfc();
const NFD: DecomposingNormalizerBorrowed<'static> = DecomposingNormalizerBorrowed::new_nfd();

/// UTF-8 encoding of U+212A KELVIN SIGN, which simple-folds to ASCII `k`.
const KELVIN: &[u8] = "\u{212A}".as_bytes();
/// UTF-8 encoding of U+017F LATIN SMALL LETTER LONG S, which simple-folds to ASCII `s`.
const LONG_S: &[u8] = "\u{17F}".as_bytes();

/// Simple case folding of one character.
#[inline]
#[must_use]
pub fn fold_char(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        CASE_MAPPER.simple_fold(c)
    }
}

/// Returns true if `name` contains a character outside ASCII whose simple case folding is an
/// ASCII character (U+212A → `k`, U+017F → `s`). Such names need the slow path even for ASCII
/// needles.
#[must_use]
pub fn has_fold_special(name: &[u8]) -> bool {
    memchr::memmem::find(name, KELVIN).is_some() || memchr::memmem::find(name, LONG_S).is_some()
}

/// Appends the simple case folding of `name` to `out`. Invalid UTF-8 bytes are copied as-is.
pub fn fold_into(name: &[u8], out: &mut Vec<u8>) {
    if name.is_ascii() {
        out.extend(name.iter().map(u8::to_ascii_lowercase));
        return;
    }
    for chunk in name.utf8_chunks() {
        for c in chunk.valid().chars() {
            push_char(out, fold_char(c));
        }
        out.extend_from_slice(chunk.invalid());
    }
}

/// Returns the simple case folding of `name`.
#[must_use]
pub fn fold(name: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len());
    fold_into(name, &mut out);
    out
}

/// NFC-normalizes the valid UTF-8 parts of `name`, then case folds (when `fold_case`).
pub fn normalize_into(name: &[u8], fold_case: bool, out: &mut Vec<u8>) {
    for chunk in name.utf8_chunks() {
        let nfc = NFC.normalize(chunk.valid());
        for c in nfc.chars() {
            push_char(out, if fold_case { fold_char(c) } else { c });
        }
        out.extend_from_slice(chunk.invalid());
    }
}

/// Removes diacritics: NFD-decomposes the valid UTF-8 parts, drops combining diacritical marks
/// (the Latin/Greek/Cyrillic combining-mark blocks only, so marks that carry meaning in other
/// scripts are kept), maps a few letters that do not decompose (`ø`, `ł`, `đ`, `ħ`, …) to their
/// base letter, and case folds when `fold_case`.
pub fn strip_diacritics_into(name: &[u8], fold_case: bool, out: &mut Vec<u8>) {
    for chunk in name.utf8_chunks() {
        let nfd = NFD.normalize(chunk.valid());
        for c in nfd.chars() {
            if is_combining_diacritic(c) {
                continue;
            }
            let c = base_letter(c);
            push_char(out, if fold_case { fold_char(c) } else { c });
        }
        out.extend_from_slice(chunk.invalid());
    }
}

fn is_combining_diacritic(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

fn base_letter(c: char) -> char {
    match c {
        'ø' => 'o',
        'Ø' => 'O',
        'ł' => 'l',
        'Ł' => 'L',
        'đ' => 'd',
        'Đ' => 'D',
        'ħ' => 'h',
        'Ħ' => 'H',
        'ŧ' => 't',
        'Ŧ' => 'T',
        'ı' => 'i',
        _ => c,
    }
}

#[inline]
fn push_char(out: &mut Vec<u8>, c: char) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

/// One collation unit: a folded code point, or an invalid byte mapped above the Unicode range so
/// that invalid sequences sort after all valid characters.
#[inline]
fn units(name: &[u8]) -> impl Iterator<Item = u32> + '_ {
    name.utf8_chunks().flat_map(|chunk| {
        chunk
            .valid()
            .chars()
            .map(|c| fold_char(c) as u32)
            .chain(chunk.invalid().iter().map(|&b| 0x11_0000 + u32::from(b)))
    })
}

/// Collation order for names: case-insensitive (simple folding) code-point order, ties broken
/// by the raw bytes so that distinct names never compare equal.
#[must_use]
pub fn collate(a: &[u8], b: &[u8]) -> Ordering {
    if a.is_ascii() && b.is_ascii() {
        let folded = a
            .iter()
            .map(u8::to_ascii_lowercase)
            .cmp(b.iter().map(u8::to_ascii_lowercase));
        return folded.then_with(|| a.cmp(b));
    }
    units(a).cmp(units(b)).then_with(|| a.cmp(b))
}

/// Appends a byte string whose plain byte order equals [`collate`]’s primary (folded) order:
/// folded characters as UTF-8 (UTF-8 preserves code-point order), and each invalid byte `b` as
/// `[0xF8, b]` (0xF8 is never a UTF-8 lead byte, so these sort after every valid character).
/// Ties between different raw names must still be broken by comparing the raw bytes.
pub fn collation_key_into(name: &[u8], out: &mut Vec<u8>) {
    if name.is_ascii() {
        out.extend(name.iter().map(u8::to_ascii_lowercase));
        return;
    }
    for chunk in name.utf8_chunks() {
        for c in chunk.valid().chars() {
            push_char(out, fold_char(c));
        }
        for &b in chunk.invalid() {
            out.extend_from_slice(&[0xF8, b]);
        }
    }
}

/// Case-insensitive equality under simple folding.
#[must_use]
pub fn eq_folded(a: &[u8], b: &[u8]) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    units(a).eq(units(b))
}

/// Number of characters in `name` (invalid bytes count one each).
#[must_use]
pub fn char_len(name: &[u8]) -> usize {
    if name.is_ascii() {
        return name.len();
    }
    name.utf8_chunks()
        .map(|c| c.valid().chars().count() + c.invalid().len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_ascii_and_unicode() {
        assert_eq!(fold(b"HeLLo.TXT"), b"hello.txt");
        assert_eq!(fold("ÄÖÜ".as_bytes()), "äöü".as_bytes());
        assert_eq!(fold("ΣΑΣ".as_bytes()), "σασ".as_bytes());
        assert_eq!(fold("ς".as_bytes()), "σ".as_bytes());
        // Simple folding: sharp s stays sharp s.
        assert_eq!(fold("Straße".as_bytes()), "straße".as_bytes());
        assert_eq!(fold("\u{212A}".as_bytes()), b"k");
    }

    #[test]
    fn invalid_bytes_pass_through() {
        assert_eq!(fold(b"A\xFFB"), b"a\xFFb");
        assert_eq!(char_len(b"a\xFF\xFEb"), 4);
    }

    #[test]
    fn collation_is_case_insensitive_with_raw_tiebreak() {
        assert_eq!(collate(b"apple", b"Banana"), Ordering::Less);
        assert_eq!(collate(b"B", b"a"), Ordering::Greater);
        assert_eq!(collate(b"A", b"a"), Ordering::Less);
        assert_eq!(collate(b"a", b"a"), Ordering::Equal);
        assert_eq!(collate("é".as_bytes(), b"f"), Ordering::Greater);
        assert_eq!(collate(b"\xFF", "z".as_bytes()), Ordering::Greater);
        assert_eq!(
            collate("Ärger".as_bytes(), "ärger".as_bytes()),
            Ordering::Less
        );
    }

    #[test]
    fn diacritics() {
        let mut out = Vec::new();
        strip_diacritics_into("Crème Brûlée Øre Łódź".as_bytes(), true, &mut out);
        assert_eq!(out, b"creme brulee ore lodz");
    }

    #[test]
    fn nfc_normalization() {
        let mut a = Vec::new();
        normalize_into("e\u{301}".as_bytes(), true, &mut a);
        let mut b = Vec::new();
        normalize_into("É".as_bytes(), true, &mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn fold_special_detection() {
        assert!(has_fold_special("\u{212A}elvin".as_bytes()));
        assert!(has_fold_special("ma\u{17F}t".as_bytes()));
        assert!(!has_fold_special("plain".as_bytes()));
    }
}
