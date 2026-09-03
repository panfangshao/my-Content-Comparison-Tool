//! Reading and writing text files without asking the user what encoding they
//! are in.
//!
//! Order of evidence, strongest first:
//!
//! 1. A byte-order mark. Unambiguous, so it wins outright.
//! 2. Valid UTF-8. Guessing anything else for a file that decodes cleanly as
//!    UTF-8 is almost always wrong in 2026.
//! 3. `chardetng`, Firefox's detector - good on GBK/Big5/Shift_JIS/legacy
//!    Windows code pages, which is exactly the long tail that matters here.
//!
//! Nothing is ever sent anywhere: detection and decoding are entirely local.

use std::path::Path;

use anyhow::{Context, Result};
use encoding_rs::Encoding;

/// What a file turned out to be, so we can round-trip it on save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileEncoding {
    pub encoding: &'static Encoding,
    pub bom: bool,
    /// The dominant line ending, so saving does not rewrite every line.
    pub newline: Newline,
    /// True when decoding needed replacement characters - the guess was wrong
    /// somewhere, and saving will not round-trip byte for byte.
    pub lossy: bool,
}

impl Default for FileEncoding {
    fn default() -> Self {
        Self {
            encoding: encoding_rs::UTF_8,
            bom: false,
            newline: Newline::platform_default(),
            lossy: false,
        }
    }
}

impl FileEncoding {
    pub fn label(&self) -> String {
        let mut s = self.encoding.name().to_owned();
        if self.bom {
            s.push_str(" BOM");
        }
        s
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Newline {
    Lf,
    Crlf,
}

impl Newline {
    pub const fn platform_default() -> Self {
        if cfg!(windows) { Self::Crlf } else { Self::Lf }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Lf => "LF",
            Self::Crlf => "CRLF",
        }
    }

    /// Pick the dominant ending in `text`. Ties go to LF.
    pub fn detect(text: &str) -> Self {
        let crlf = text.matches("\r\n").count();
        let lf = text.matches('\n').count() - crlf;
        if crlf > lf { Self::Crlf } else { Self::Lf }
    }
}

/// A decoded file plus everything needed to write it back unchanged.
pub struct DecodedFile {
    /// Text with all line endings normalized to `\n`. The buffer only ever
    /// sees LF; the original ending is restored on save.
    pub text: String,
    pub encoding: FileEncoding,
}

/// Read and decode a file from disk.
pub fn read_file(path: &Path) -> Result<DecodedFile> {
    let bytes =
        std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(decode(&bytes))
}

/// Decode a byte buffer, detecting the encoding.
pub fn decode(bytes: &[u8]) -> DecodedFile {
    if let Some((encoding, bom_len)) = Encoding::for_bom(bytes) {
        let (text, _, lossy) = encoding.decode(&bytes[bom_len..]);
        return finish(text.into_owned(), encoding, true, lossy);
    }

    // A clean UTF-8 decode is decisive - no detector needed.
    if let Ok(text) = std::str::from_utf8(bytes) {
        return finish(text.to_owned(), encoding_rs::UTF_8, false, false);
    }

    // UTF-16 without a BOM: the strict UTF-8 check above already failed, and
    // a statistical detector reads NULs as junk, so it must be caught here.
    if let Some(encoding) = sniff_utf16(bytes) {
        let (text, _, lossy) = encoding.decode(bytes);
        return finish(text.into_owned(), encoding, false, lossy);
    }

    // We are decoding local files, not untrusted web content, so ISO-2022-JP
    // is allowed. UTF-8 is denied because the exhaustive check above already
    // ruled it out - letting the detector pick it here would only produce a
    // lossy decode of bytes we know are not UTF-8.
    let mut detector =
        chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Allow);
    // Sample from the first byte that carries any information.
    //
    // Feeding the head keeps huge files fast, but a head of pure ASCII tells
    // the detector nothing: every legacy encoding agrees about those bytes.
    // A file with a long ASCII preamble - a licence header, a run of
    // `CREATE TABLE` statements - and GBK text starting past 64 KB was guessed
    // as windows-1252, and the tail came out as mojibake *without* the lossy
    // flag, since windows-1252 maps every byte. Starting the window where the
    // ASCII stops costs one linear scan and looks at the bytes that decide it.
    //
    // The scan always finds something: this point is only reached when the
    // exhaustive UTF-8 check above failed, which needs a non-ASCII byte.
    let start = bytes.iter().position(|b| !b.is_ascii()).unwrap_or(0);
    let end = (start + 64 * 1024).min(bytes.len());
    let sample = &bytes[start..end];
    detector.feed(sample, end == bytes.len());
    let encoding = detector.guess(None, chardetng::Utf8Detection::Deny);

    let (text, had_errors) = encoding.decode_without_bom_handling(bytes);
    finish(text.into_owned(), encoding, false, had_errors)
}

/// Guess UTF-16 endianness, without a BOM, from where the NUL bytes sit.
///
/// UTF-16LE text keeps the high (zero) byte of ASCII at odd offsets, BE at
/// even ones, so a strong skew towards one parity is decisive. Anything with
/// too few NULs, or no clear skew, is left for the statistical detector.
fn sniff_utf16(bytes: &[u8]) -> Option<&'static Encoding> {
    let sample = &bytes[..bytes.len().min(64 * 1024)];
    let mut even_nul = 0usize;
    let mut odd_nul = 0usize;
    for (i, &b) in sample.iter().enumerate() {
        if b == 0 {
            if i % 2 == 0 { even_nul += 1 } else { odd_nul += 1 }
        }
    }
    let total = even_nul + odd_nul;
    if total < 4 {
        return None;
    }
    if odd_nul * 4 >= total * 3 {
        Some(encoding_rs::UTF_16LE)
    } else if even_nul * 4 >= total * 3 {
        Some(encoding_rs::UTF_16BE)
    } else {
        None
    }
}

fn finish(text: String, encoding: &'static Encoding, bom: bool, lossy: bool) -> DecodedFile {
    let newline = Newline::detect(&text);
    // A pure-LF file - the common case - passes through with no copy.
    let text = if text.contains('\r') {
        text.replace("\r\n", "\n")
    } else {
        text
    };
    DecodedFile {
        text,
        encoding: FileEncoding {
            encoding,
            bom,
            newline,
            lossy,
        },
    }
}

/// Encode text back to bytes using the encoding it was read with.
///
/// Returns the bytes and whether any characters had no mapping in the target
/// encoding and were written as replacements - the caller should warn the
/// user, because the file on disk then no longer holds what was on screen.
pub fn encode(text: &str, enc: &FileEncoding) -> (Vec<u8>, bool) {
    let text = match enc.newline {
        Newline::Lf => std::borrow::Cow::Borrowed(text),
        Newline::Crlf => std::borrow::Cow::Owned(text.replace('\n', "\r\n")),
    };

    // encoding_rs cannot *encode* UTF-16; those files are written back as
    // UTF-8, which is lossless and what every editor does today. The encoder
    // must be settled before the BOM so the two always agree - writing a
    // UTF-16 BOM in front of UTF-8 bytes corrupts the file.
    let encoder = match enc.encoding.name() {
        "UTF-16LE" | "UTF-16BE" => encoding_rs::UTF_8,
        _ => enc.encoding,
    };

    let mut out = Vec::with_capacity(text.len() + 3);
    if enc.bom {
        // `encoding_rs` does not emit BOMs itself.
        match encoder.name() {
            "UTF-8" => out.extend_from_slice(&[0xEF, 0xBB, 0xBF]),
            _ => {}
        }
    }

    let (bytes, _, had_unmappables) = encoder.encode(&text);
    out.extend_from_slice(&bytes);
    (out, had_unmappables)
}

/// Write text to disk in its original encoding and line ending.
///
/// The write is atomic: the bytes go to a sibling temporary file first and
/// are renamed over the target, so a crash mid-write cannot leave a
/// truncated file behind. The process id in the name keeps two instances
/// saving the same file from clashing.
///
/// Returns whether the encoding dropped unmappable characters (see
/// [`encode`]).
pub fn write_file(path: &Path, text: &str, enc: &FileEncoding) -> Result<bool> {
    let (bytes, had_unmappables) = encode(text, enc);
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, &bytes)
        .with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(had_unmappables)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_utf8() {
        let d = decode("hello".as_bytes());
        assert_eq!(d.text, "hello");
        assert_eq!(d.encoding.encoding, encoding_rs::UTF_8);
        assert!(!d.encoding.bom);
        assert!(!d.encoding.lossy);
    }

    #[test]
    fn utf8_bom_is_stripped() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("hi".as_bytes());
        let d = decode(&bytes);
        assert_eq!(d.text, "hi");
        assert!(d.encoding.bom);
        assert_eq!(d.encoding.label(), "UTF-8 BOM");
    }

    #[test]
    fn utf16le_bom() {
        let mut bytes = vec![0xFF, 0xFE];
        for u in "hi".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let d = decode(&bytes);
        assert_eq!(d.text, "hi");
        assert!(d.encoding.bom);
    }

    #[test]
    fn gbk_is_detected() {
        let (bytes, _, _) = encoding_rs::GBK.encode("对比工具，本地运行，绝不上传");
        let d = decode(&bytes);
        assert!(!d.encoding.lossy, "GBK text decoded with replacements");
        assert_eq!(d.text, "对比工具，本地运行，绝不上传");
    }

    #[test]
    fn utf8_wins_over_the_detector() {
        // This is valid UTF-8 and also valid GBK; UTF-8 must win.
        let d = decode("中文测试".as_bytes());
        assert_eq!(d.encoding.encoding, encoding_rs::UTF_8);
        assert_eq!(d.text, "中文测试");
    }

    #[test]
    fn crlf_is_normalized_but_remembered() {
        let d = decode("a\r\nb\r\nc".as_bytes());
        assert_eq!(d.text, "a\nb\nc", "the buffer only ever sees LF");
        assert_eq!(d.encoding.newline, Newline::Crlf);
    }

    #[test]
    fn mixed_endings_pick_the_majority() {
        assert_eq!(Newline::detect("a\r\nb\r\nc\nd"), Newline::Crlf);
        assert_eq!(Newline::detect("a\nb\nc\r\nd"), Newline::Lf);
        assert_eq!(Newline::detect("no endings"), Newline::Lf);
    }

    #[test]
    fn round_trip_preserves_encoding_and_endings() {
        for original in [
            "line one\r\nline two\r\n",
            "line one\nline two\n",
            "中文\r\n内容\r\n",
        ] {
            let d = decode(original.as_bytes());
            let (back, _) = encode(&d.text, &d.encoding);
            assert_eq!(
                String::from_utf8_lossy(&back),
                original,
                "round trip changed the file"
            );
        }
    }

    #[test]
    fn gbk_round_trips_byte_for_byte() {
        let (bytes, _, _) = encoding_rs::GBK.encode("对比工具\r\n第二行\r\n");
        let d = decode(&bytes);
        assert_eq!(encode(&d.text, &d.encoding).0, bytes.to_vec());
    }

    #[test]
    fn utf8_bom_round_trips() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("x\n".as_bytes());
        let d = decode(&bytes);
        assert_eq!(encode(&d.text, &d.encoding).0, bytes);
    }

    #[test]
    fn utf16le_bom_round_trips_through_utf8() {
        // encoding_rs cannot encode UTF-16, so saving produces UTF-8 - but
        // the BOM must match the UTF-8 bytes, not the original encoding.
        let mut bytes = vec![0xFF, 0xFE];
        for u in "hi 对比\n第二行\n".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let d = decode(&bytes);
        let (back, lossy) = encode(&d.text, &d.encoding);
        assert!(!lossy, "UTF-16 -> UTF-8 is lossless");
        let again = decode(&back);
        assert_eq!(again.text, d.text, "saved file must decode to the same text");
    }

    #[test]
    fn utf16_without_bom_is_sniffed() {
        // Pure-ASCII UTF-16 is valid UTF-8 (NUL is a legal codepoint), so
        // this only gets interesting once a non-ASCII byte breaks the strict
        // UTF-8 check above the sniff.
        let text = "café müller, 对比测试\n";
        for endian in ["LE", "BE"] {
            let mut bytes = Vec::new();
            for u in text.encode_utf16() {
                let [lo, hi] = u.to_le_bytes();
                let pair = if endian == "LE" { [lo, hi] } else { [hi, lo] };
                bytes.extend_from_slice(&pair);
            }
            let d = decode(&bytes);
            assert_eq!(d.text, text, "{endian}");
            assert!(!d.encoding.bom);
            assert!(!d.encoding.lossy, "{endian} decoded with replacements");
        }
    }

    #[test]
    fn unmappable_characters_are_reported() {
        let enc = FileEncoding {
            encoding: encoding_rs::WINDOWS_1252,
            bom: false,
            newline: Newline::Lf,
            lossy: false,
        };
        let (_, lossy) = encode("héllo 中文", &enc);
        assert!(lossy, "CJK has no mapping in windows-1252");
        let (_, lossy) = encode("plain ascii", &enc);
        assert!(!lossy);
    }

    #[test]
    fn empty_input_is_safe() {
        let d = decode(b"");
        assert_eq!(d.text, "");
        assert!(encode(&d.text, &d.encoding).0.is_empty());
    }

    #[test]
    fn binary_input_never_panics() {
        // Dropping a .png on the window should show mojibake, not crash.
        let junk: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
        let d = decode(&junk);
        assert!(!d.text.is_empty());
        // Whatever we decoded, re-encoding it must still succeed.
        let _ = encode(&d.text, &d.encoding);
    }

    /// The detector's window starts where the ASCII stops.
    ///
    /// A file with a long ASCII preamble and GBK text past 64 KB used to be
    /// guessed as windows-1252, and the tail came out as `¶Ô±È¹¤¾ß` - mojibake
    /// with no lossy flag to warn about it, because windows-1252 maps every
    /// byte. The leading ASCII carries no information about which legacy
    /// encoding this is, so it is skipped.
    #[test]
    fn a_long_ascii_preamble_does_not_hide_the_encoding() {
        let text = "对比工具，本地运行";
        for preamble in [0usize, 1024, 64 * 1024, 512 * 1024] {
            let mut bytes = vec![b'a'; preamble];
            let (gbk, _, _) = encoding_rs::GBK.encode(text);
            bytes.extend_from_slice(&gbk);

            let d = decode(&bytes);
            assert_eq!(
                d.encoding.encoding,
                encoding_rs::GBK,
                "{preamble} bytes of ASCII hid the encoding"
            );
            assert!(!d.encoding.lossy);
            assert_eq!(&d.text[preamble..], text);
        }
    }
}
