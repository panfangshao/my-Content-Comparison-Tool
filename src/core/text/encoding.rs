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

    // We are decoding local files, not untrusted web content, so ISO-2022-JP
    // is allowed. UTF-8 is denied because the exhaustive check above already
    // ruled it out - letting the detector pick it here would only produce a
    // lossy decode of bytes we know are not UTF-8.
    let mut detector =
        chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Allow);
    // Feeding the head is enough and keeps huge files fast; the `last` flag
    // tells the detector not to wait for more input.
    let sample = &bytes[..bytes.len().min(64 * 1024)];
    detector.feed(sample, sample.len() == bytes.len());
    let encoding = detector.guess(None, chardetng::Utf8Detection::Deny);

    let (text, had_errors) = encoding.decode_without_bom_handling(bytes);
    finish(text.into_owned(), encoding, false, had_errors)
}

fn finish(text: String, encoding: &'static Encoding, bom: bool, lossy: bool) -> DecodedFile {
    let newline = Newline::detect(&text);
    DecodedFile {
        text: text.replace("\r\n", "\n"),
        encoding: FileEncoding {
            encoding,
            bom,
            newline,
            lossy,
        },
    }
}

/// Encode text back to bytes using the encoding it was read with.
pub fn encode(text: &str, enc: &FileEncoding) -> Vec<u8> {
    let text = match enc.newline {
        Newline::Lf => std::borrow::Cow::Borrowed(text),
        Newline::Crlf => std::borrow::Cow::Owned(text.replace('\n', "\r\n")),
    };

    let mut out = Vec::with_capacity(text.len() + 3);
    if enc.bom {
        // `encoding_rs` does not emit BOMs itself.
        match enc.encoding.name() {
            "UTF-8" => out.extend_from_slice(&[0xEF, 0xBB, 0xBF]),
            "UTF-16LE" => out.extend_from_slice(&[0xFF, 0xFE]),
            "UTF-16BE" => out.extend_from_slice(&[0xFE, 0xFF]),
            _ => {}
        }
    }

    // encoding_rs cannot *encode* UTF-16; those files are written back as
    // UTF-8, which is lossless and what every editor does today.
    let encoder = match enc.encoding.name() {
        "UTF-16LE" | "UTF-16BE" => encoding_rs::UTF_8,
        _ => enc.encoding,
    };
    let (bytes, _, _) = encoder.encode(&text);
    out.extend_from_slice(&bytes);
    out
}

/// Write text to disk in its original encoding and line ending.
pub fn write_file(path: &Path, text: &str, enc: &FileEncoding) -> Result<()> {
    std::fs::write(path, encode(text, enc))
        .with_context(|| format!("cannot write {}", path.display()))
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
            let back = encode(&d.text, &d.encoding);
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
        assert_eq!(encode(&d.text, &d.encoding), bytes.to_vec());
    }

    #[test]
    fn utf8_bom_round_trips() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("x\n".as_bytes());
        let d = decode(&bytes);
        assert_eq!(encode(&d.text, &d.encoding), bytes);
    }

    #[test]
    fn empty_input_is_safe() {
        let d = decode(b"");
        assert_eq!(d.text, "");
        assert!(encode(&d.text, &d.encoding).is_empty());
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
}
