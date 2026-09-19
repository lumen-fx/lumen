//! Reading an image's own size out of its first few bytes.
//!
//! An `<img>` that says how big its file is lets a browser hold the image's
//! place in the layout before a byte of it has arrived, so the text below it
//! does not jump down when it lands. The build has the bytes in hand already,
//! from naming the asset after their hash, so the size is read there.
//!
//! This reads headers, it does not decode. A decoder would answer for one
//! more format at the price of carrying every format's decompressor into
//! every build that links this crate, and it would still answer nothing for
//! SVG, which has no pixels to count. A file in a format nothing here knows
//! keeps the emitted document it has today: a `src`, an `alt` and no size.

use lumen_html::PixelSize;

/// The size `bytes` declares, where the format is one this knows.
///
/// `path` is the file's own path, which is what tells an SVG apart: it is
/// the one format with no bytes at a fixed offset to recognise it by.
#[must_use]
pub fn intrinsic_size(bytes: &[u8], path: &str) -> Option<PixelSize> {
    let size = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        png(bytes)
    } else if bytes.starts_with(b"\xff\xd8") {
        jpeg(bytes)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        gif(bytes)
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        webp(bytes)
    } else if is_svg(bytes, path) {
        svg(bytes)
    } else {
        None
    }?;
    (size.width > 0 && size.height > 0).then_some(size)
}

/// A big-endian `u32` at `at`.
fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// A big-endian `u16` at `at`.
fn be16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes(
        bytes.get(at..at + 2)?.try_into().ok()?,
    )))
}

/// A little-endian `u16` at `at`.
fn le16(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes(
        bytes.get(at..at + 2)?.try_into().ok()?,
    )))
}

/// A little-endian `u24` at `at`, which is how WebP writes a canvas size.
fn le24(bytes: &[u8], at: usize) -> Option<u32> {
    let part = bytes.get(at..at + 3)?;
    Some(u32::from(part[0]) | u32::from(part[1]) << 8 | u32::from(part[2]) << 16)
}

/// PNG: the IHDR chunk is first and always in the same place.
fn png(bytes: &[u8]) -> Option<PixelSize> {
    Some(PixelSize {
        width: be32(bytes, 16)?,
        height: be32(bytes, 20)?,
    })
}

/// JPEG: the size lives in the frame header, which sits behind however many
/// metadata segments the encoder wrote, so the segments are walked by their
/// own length fields to the first one that starts a frame.
fn jpeg(bytes: &[u8]) -> Option<PixelSize> {
    let mut at = 2;
    loop {
        // A segment may be padded with any number of fill bytes before its
        // marker.
        while bytes.get(at) == Some(&0xff) && bytes.get(at + 1) == Some(&0xff) {
            at += 1;
        }
        if bytes.get(at) != Some(&0xff) {
            return None;
        }
        let marker = *bytes.get(at + 1)?;
        // `C4`, `C8` and `CC` share the range but are a Huffman table, an
        // extension and an arithmetic coding table, not frame headers.
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            return Some(PixelSize {
                width: be16(bytes, at + 7)?,
                height: be16(bytes, at + 5)?,
            });
        }
        // The markers that carry no payload, and so no length to skip by.
        if matches!(marker, 0x01 | 0xd0..=0xd9) {
            at += 2;
            continue;
        }
        let length = be16(bytes, at + 2)? as usize;
        if length < 2 {
            return None;
        }
        at += 2 + length;
    }
}

/// GIF: the logical screen descriptor follows the signature.
fn gif(bytes: &[u8]) -> Option<PixelSize> {
    Some(PixelSize {
        width: le16(bytes, 6)?,
        height: le16(bytes, 8)?,
    })
}

/// WebP: three formats under one container, each writing its size its own
/// way. `VP8X` is the extended form and carries the canvas size the others
/// only imply, so it is read where it is there.
fn webp(bytes: &[u8]) -> Option<PixelSize> {
    match bytes.get(12..16)? {
        b"VP8X" => Some(PixelSize {
            width: le24(bytes, 24)? + 1,
            height: le24(bytes, 27)? + 1,
        }),
        b"VP8L" => {
            if bytes.get(20) != Some(&0x2f) {
                return None;
            }
            // Two 14-bit fields packed into the four bytes after the
            // signature byte, each holding one less than the real value.
            let packed = u32::from_le_bytes(bytes.get(21..25)?.try_into().ok()?);
            Some(PixelSize {
                width: (packed & 0x3fff) + 1,
                height: ((packed >> 14) & 0x3fff) + 1,
            })
        }
        b"VP8 " => {
            if bytes.get(23..26)? != b"\x9d\x01\x2a" {
                return None;
            }
            // The top two bits of each field are a scale, not part of the
            // size.
            Some(PixelSize {
                width: le16(bytes, 26)? & 0x3fff,
                height: le16(bytes, 28)? & 0x3fff,
            })
        }
        _ => None,
    }
}

/// SVG is text, so it has no magic number to recognise it by. The extension
/// is what names it; a file that opens as XML or as an `svg` element is taken
/// as one whatever it is called.
fn is_svg(bytes: &[u8], path: &str) -> bool {
    path.rsplit('.')
        .next()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
        || bytes.starts_with(b"<?xml")
        || bytes.starts_with(b"<svg")
}

/// SVG: the `width` and `height` on the root element where they are lengths
/// in pixels, and the `viewBox` otherwise.
///
/// A size in `em` or `%` is a size in something the emitter has no value
/// for, so those fall through to the `viewBox`, which is always in user
/// units. A fraction rounds up: the attribute pair is a box to reserve and
/// an aspect ratio, and a box one pixel large is the safer of the two errors.
fn svg(bytes: &[u8]) -> Option<PixelSize> {
    let text = String::from_utf8_lossy(bytes);
    let open = text.find("<svg")?;
    let tag = &text[open..];
    let tag = &tag[..tag.find('>')?];
    let width = tag_attr(tag, "width").and_then(svg_length);
    let height = tag_attr(tag, "height").and_then(svg_length);
    if let (Some(width), Some(height)) = (width, height) {
        return Some(PixelSize { width, height });
    }
    let numbers: Vec<f64> = tag_attr(tag, "viewBox")?
        .split([' ', ',', '\t', '\n', '\r'])
        .filter(|part| !part.is_empty())
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let [.., width, height] = numbers.as_slice() else {
        return None;
    };
    Some(PixelSize {
        width: round_up(*width)?,
        height: round_up(*height)?,
    })
}

/// The value of `name` on an opening tag, quoted either way.
///
/// The name has to stand on its own: `stroke-width` ends in `width` and is
/// not it.
fn tag_attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = tag;
    while let Some(at) = rest.find(name) {
        let before = rest[..at].chars().next_back();
        let after = &rest[at + name.len()..];
        rest = after;
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let after = after.trim_start();
        let Some(after) = after.strip_prefix('=') else {
            continue;
        };
        let after = after.trim_start();
        let quote = after.chars().next()?;
        if quote != '"' && quote != '\'' {
            continue;
        }
        let value = &after[quote.len_utf8()..];
        return Some(&value[..value.find(quote)?]);
    }
    None
}

/// An SVG length the emitter can write as a pixel count: a bare number, or
/// one in `px`.
fn svg_length(value: &str) -> Option<u32> {
    let value = value.trim();
    let value = value.strip_suffix("px").unwrap_or(value);
    round_up(value.trim().parse().ok()?)
}

/// A user unit as a whole number of pixels.
fn round_up(value: f64) -> Option<u32> {
    let value = value.ceil();
    (value.is_finite() && value >= 1.0 && value <= f64::from(u32::MAX)).then_some(value as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(width: u32, height: u32) -> Option<PixelSize> {
        Some(PixelSize { width, height })
    }

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        out.extend_from_slice(&13u32.to_be_bytes());
        out.extend_from_slice(b"IHDR");
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&height.to_be_bytes());
        out
    }

    /// A JPEG with one metadata segment ahead of the frame header, which is
    /// what makes the walk worth having.
    fn jpeg_bytes(width: u16, height: u16) -> Vec<u8> {
        let mut out = vec![0xff, 0xd8];
        out.extend_from_slice(&[0xff, 0xe0, 0x00, 0x06, b'J', b'F', b'I', b'F']);
        out.extend_from_slice(&[0xff, 0xc0, 0x00, 0x11, 0x08]);
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out
    }

    fn gif_bytes(width: u16, height: u16) -> Vec<u8> {
        let mut out = b"GIF89a".to_vec();
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out
    }

    fn riff(chunk: &[u8]) -> Vec<u8> {
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(chunk.len() as u32 + 4).to_le_bytes());
        out.extend_from_slice(b"WEBP");
        out.extend_from_slice(chunk);
        out
    }

    fn webp_extended(width: u32, height: u32) -> Vec<u8> {
        let mut chunk = b"VP8X".to_vec();
        chunk.extend_from_slice(&10u32.to_le_bytes());
        chunk.extend_from_slice(&[0x10, 0, 0, 0]);
        chunk.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        chunk.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        riff(&chunk)
    }

    fn webp_lossless(width: u32, height: u32) -> Vec<u8> {
        let mut chunk = b"VP8L".to_vec();
        chunk.extend_from_slice(&5u32.to_le_bytes());
        chunk.push(0x2f);
        let packed = (width - 1) | (height - 1) << 14;
        chunk.extend_from_slice(&packed.to_le_bytes());
        riff(&chunk)
    }

    fn webp_lossy(width: u16, height: u16) -> Vec<u8> {
        let mut chunk = b"VP8 ".to_vec();
        chunk.extend_from_slice(&13u32.to_le_bytes());
        chunk.extend_from_slice(&[0, 0, 0]);
        chunk.extend_from_slice(b"\x9d\x01\x2a");
        chunk.extend_from_slice(&width.to_le_bytes());
        chunk.extend_from_slice(&height.to_le_bytes());
        riff(&chunk)
    }

    #[test]
    fn a_png_says_its_size_in_the_first_chunk() {
        assert_eq!(
            intrinsic_size(&png_bytes(120, 80), "logo.png"),
            size(120, 80)
        );
    }

    #[test]
    fn a_jpeg_says_it_behind_its_metadata() {
        assert_eq!(
            intrinsic_size(&jpeg_bytes(640, 480), "photo.jpg"),
            size(640, 480)
        );
    }

    #[test]
    fn a_gif_says_it_in_the_screen_descriptor() {
        assert_eq!(intrinsic_size(&gif_bytes(32, 24), "spin.gif"), size(32, 24));
    }

    #[test]
    fn every_shape_of_webp_answers() {
        assert_eq!(
            intrinsic_size(&webp_extended(300, 200), "hero.webp"),
            size(300, 200)
        );
        assert_eq!(
            intrinsic_size(&webp_lossless(300, 200), "hero.webp"),
            size(300, 200)
        );
        assert_eq!(
            intrinsic_size(&webp_lossy(300, 200), "hero.webp"),
            size(300, 200)
        );
    }

    #[test]
    fn an_svg_in_pixels_is_read_off_its_root() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="16px" height="16">"#;
        assert_eq!(intrinsic_size(svg, "icon.svg"), size(16, 16));
    }

    #[test]
    fn an_svg_sized_in_anything_else_falls_back_to_its_view_box() {
        let svg = br#"<svg width="100%" height="4em" viewBox="0 0 24 18" stroke-width="2">"#;
        assert_eq!(intrinsic_size(svg, "icon.svg"), size(24, 18));
        let fractional = br#"<svg viewBox="0 0 23.5 17.2">"#;
        assert_eq!(intrinsic_size(fractional, "icon.svg"), size(24, 18));
    }

    #[test]
    fn an_svg_that_declares_no_size_answers_nothing() {
        assert_eq!(intrinsic_size(br#"<svg fill="red">"#, "icon.svg"), None);
    }

    #[test]
    fn a_file_cut_short_answers_nothing_rather_than_panicking() {
        for full in [
            png_bytes(120, 80),
            jpeg_bytes(640, 480),
            gif_bytes(32, 24),
            webp_extended(300, 200),
            webp_lossless(300, 200),
            webp_lossy(300, 200),
            br#"<svg width="16" height="16">"#.to_vec(),
        ] {
            for cut in 0..full.len() {
                let _ = intrinsic_size(&full[..cut], "cut.png");
            }
            assert_eq!(intrinsic_size(&full[..full.len() - 1], "cut.bin"), None);
        }
    }

    #[test]
    fn a_format_this_does_not_know_answers_nothing() {
        assert_eq!(intrinsic_size(b"not an image at all", "notes.txt"), None);
        assert_eq!(intrinsic_size(&[], "empty.png"), None);
    }
}
