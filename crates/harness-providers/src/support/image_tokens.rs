//! What an image costs a request, in tokens.
//!
//! A provider bills an image by its pixels, not by the bytes that carry it:
//! `OpenAI` by 512-pixel tiles of the image fitted to 2048 and 768, Anthropic by
//! its area over 750 once fitted to 1568. Counted as its base64 text, a pasted
//! 153 KiB screenshot was 58k tokens over (bug report 20261119-115208), and
//! an 8 MiB one - which an attachment may be - would have been 2.8M, more than
//! any window.

use base64::Engine as _;

/// The estimate when the size cannot be read: a remote link, or bytes in a
/// form the reader does not know. It is about the most either provider bills
/// one image.
pub const UNREAD_IMAGE_TOKENS: u64 = 1_600;

/// How much of the base64 is decoded to find the size: the header, and a JPEG's
/// metadata before its frame.
const HEADER_BASE64_CHARS: usize = 256 * 1024;

/// Tokens the image costs: the larger of the two providers' counts for its
/// size, or [`UNREAD_IMAGE_TOKENS`].
#[must_use]
pub fn estimate(data_base64: &str) -> u64 {
    dimensions_of_base64(data_base64).map_or(UNREAD_IMAGE_TOKENS, |(width, height)| {
        for_size(width, height)
    })
}

/// Tokens for an image of `width` by `height` pixels.
#[must_use]
pub fn for_size(width: u32, height: u32) -> u64 {
    if width == 0 || height == 0 {
        return UNREAD_IMAGE_TOKENS;
    }
    tiles(f64::from(width), f64::from(height)).max(area(f64::from(width), f64::from(height)))
}

/// `OpenAI`'s high detail: fitted inside 2048 x 2048, its short side brought
/// down to 768, then 170 per 512-pixel tile and 85 more.
fn tiles(width: f64, height: f64) -> u64 {
    let fit = (2048.0 / width.max(height)).min(1.0);
    let (width, height) = (width * fit, height * fit);
    let short = (768.0 / width.min(height)).min(1.0);
    let (width, height) = (width * short, height * short);
    let across = (width / 512.0).ceil();
    let down = (height / 512.0).ceil();
    to_tokens(85.0 + 170.0 * across * down)
}

/// Anthropic's: fitted to a long side of 1568 and 1.15 megapixels, then its
/// area over 750.
fn area(width: f64, height: f64) -> u64 {
    let fit = (1568.0 / width.max(height)).min(1.0);
    let (width, height) = (width * fit, height * fit);
    let pixels = (width * height).min(1_150_000.0);
    to_tokens((pixels / 750.0).ceil())
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a positive count well inside u64, rounded up already"
)]
fn to_tokens(value: f64) -> u64 {
    value.max(1.0) as u64
}

/// The width and height the image's header gives, from the start of its base64.
/// A PNG, GIF or WebP says it in its first 30 bytes; only a JPEG, whose frame
/// may follow its metadata, is read further. The estimate runs for every image
/// a request carries, at every step.
fn dimensions_of_base64(data_base64: &str) -> Option<(u32, u32)> {
    let decode = |chars: usize| {
        let mut end = data_base64.len().min(chars);
        end -= end % 4;
        base64::engine::general_purpose::STANDARD
            .decode(data_base64.get(..end)?)
            .ok()
    };
    let head = decode(SHORT_HEADER_BASE64_CHARS)?;
    if !head.starts_with(&[0xff, 0xd8]) {
        return dimensions(&head);
    }
    dimensions(&decode(HEADER_BASE64_CHARS)?)
}

/// The base64 that holds a PNG's, GIF's or WebP's size: 48 bytes.
const SHORT_HEADER_BASE64_CHARS: usize = 64;

/// The width and height of a PNG, GIF, WebP or JPEG, from its first bytes.
#[must_use]
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let big_short = |at: usize| Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
    let little_short =
        |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
    let big_word = |at: usize| Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let little_three = |at: usize| {
        let part = bytes.get(at..at + 3)?;
        Some(u32::from(part[0]) | u32::from(part[1]) << 8 | u32::from(part[2]) << 16)
    };
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some((big_word(16)?, big_word(20)?));
    }
    if bytes.starts_with(b"GIF8") {
        return Some((u32::from(little_short(6)?), u32::from(little_short(8)?)));
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return match bytes.get(12..16)? {
            b"VP8 " => Some((
                u32::from(little_short(26)? & 0x3fff),
                u32::from(little_short(28)? & 0x3fff),
            )),
            b"VP8L" => {
                let bits = u32::from_le_bytes(bytes.get(21..25)?.try_into().ok()?);
                Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
            }
            b"VP8X" => Some((little_three(24)? + 1, little_three(27)? + 1)),
            _ => None,
        };
    }
    if bytes.starts_with(&[0xff, 0xd8]) {
        let mut at = 2;
        while at + 4 <= bytes.len() {
            if bytes[at] != 0xff {
                return None;
            }
            let marker = bytes[at + 1];
            // Fill bytes and the markers that carry no length.
            if marker == 0xff {
                at += 1;
                continue;
            }
            if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
                at += 2;
                continue;
            }
            let length = usize::from(big_short(at + 2)?);
            let frame = (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc);
            if frame {
                return Some((u32::from(big_short(at + 7)?), u32::from(big_short(at + 5)?)));
            }
            at += 2 + length;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        // The pixels: a large file of a small image.
        bytes.resize(160 * 1024, 0x55);
        bytes
    }

    /// Bug report 20261119-115208: a 153 KiB PNG raised the estimate by 58k
    /// tokens, its base64 counted as text. Its pixels cost about a thousand.
    #[test]
    fn an_image_costs_its_pixels_not_its_bytes() {
        let data = base64::engine::general_purpose::STANDARD.encode(png(1920, 1080));
        assert!(
            data.len() / 4 > 50_000,
            "the bytes alone read as 50k tokens"
        );
        let tokens = super::estimate(&data);
        assert!((700..=2_000).contains(&tokens), "{tokens}");
        // One tile.
        assert_eq!(
            super::estimate(&base64::engine::general_purpose::STANDARD.encode(png(64, 64))),
            255
        );
    }

    #[test]
    fn the_sizes_of_gif_webp_and_jpeg_are_read() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&300u16.to_le_bytes());
        gif.extend_from_slice(&200u16.to_le_bytes());
        assert_eq!(super::dimensions(&gif), Some((300, 200)));

        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\0\0\0\0\0\0\0\0".to_vec();
        webp.extend_from_slice(&[0x7f, 0x02, 0x00, 0xdf, 0x01, 0x00]);
        assert_eq!(super::dimensions(&webp), Some((640, 480)));

        // An APP0 segment before the frame, as a camera writes it.
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00];
        jpeg.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
        jpeg.extend_from_slice(&768u16.to_be_bytes());
        jpeg.extend_from_slice(&1024u16.to_be_bytes());
        assert_eq!(super::dimensions(&jpeg), Some((1024, 768)));

        // Its frame past the short header the other formats need.
        let mut far = vec![0xff, 0xd8, 0xff, 0xe1, 0x01, 0x02];
        far.resize(far.len() + 0x0100, 0);
        far.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
        far.extend_from_slice(&768u16.to_be_bytes());
        far.extend_from_slice(&1024u16.to_be_bytes());
        far.resize(far.len() + 64, 0);
        let data = base64::engine::general_purpose::STANDARD.encode(&far);
        assert_eq!(super::estimate(&data), super::for_size(1024, 768));
    }

    #[test]
    fn an_unread_image_is_the_most_one_image_costs() {
        assert_eq!(super::estimate("not base64!"), super::UNREAD_IMAGE_TOKENS);
        assert!(super::for_size(8000, 8000) <= super::UNREAD_IMAGE_TOKENS);
    }
}
