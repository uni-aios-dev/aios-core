//! Minimal `no_std` parser for the Console Font (`PSF1` / `PSF2`) format.
//!
//! PSF is the classic Linux kernel console font format: a tiny header followed
//! by fixed-size glyph bitmaps. PNG or BDF fonts are parsed by userspace tooling
//! *before* reaching a kernel; a kernel only ever needs PSF1/PSF2, which is why
//! the format exists. This module reads both variants and can also synthesise a
//! valid PSF2 stream from the baked-in `font8x8` glyphs so the whole path can be
//! exercised at boot without shipping a separate font binary.
//!
//! PSF2 header layout (`width`, `height`, counts are little-endian `u32`):
//!   0..4   magic `72 B5 4A 86`
//!   4..8   version (0)
//!   8..12  header size in bytes (32)
//!   12..16 flags
//!   16..20 number of glyphs
//!   20..24 bytes per glyph
//!   24..28 glyph height in pixels
//!   28..32 glyph width in pixels
//!   32..   glyph bitmaps (MSB-first rows, no padding)
//!
//! PSF1 header layout:
//!   0..2   magic `36 04`
//!   2      mode (bit 0: 512 glyphs instead of 256)
//!   3      glyph height in pixels (rows; width is fixed at 8 bits)
//!   4..    glyph bitmaps (1 byte per row)
//! The "unicode table" that may follow the glyphs is ignored by this parser.

/// PSF1 magic bytes.
pub const PSF1_MAGIC: [u8; 2] = [0x36, 0x04];
/// PSF2 magic bytes.
pub const PSF2_MAGIC: [u8; 4] = [0x72, 0xb5, 0x4a, 0x86];
/// PSF1 `mode` flag: 512 glyphs instead of 256.
pub const PSF1_FLAG_512_GLYPHS: u8 = 0x01;
/// Size of the fixed PSF2 header.
pub const PSF2_HEADER_SIZE: usize = 32;

/// Which PSF variant a parsed font came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontVersion {
    /// Classic 256/512-glyph format.
    Psf1,
    /// Extended format with arbitrary width/height.
    Psf2,
}

/// A parsed console font: header metadata plus glyph bitmaps.
///
/// `data` points at the first glyph bitmap byte; rows within a glyph are
/// MSB-first and tightly packed (`bytes_per_glyph = ceil(width / 8) * height`).
#[derive(Clone, Copy, Debug)]
pub struct PsfFont<'a> {
    version: FontVersion,
    width: usize,
    height: usize,
    glyph_count: usize,
    bytes_per_glyph: usize,
    data: &'a [u8],
}

impl<'a> PsfFont<'a> {
    /// Parses a PSF1 or PSF2 font from raw bytes.
    pub fn parse(bytes: &'a [u8]) -> Option<Self> {
        if bytes.starts_with(&PSF2_MAGIC) {
            let header_size = u32::from_le_bytes(bytes.get(8..12)?.try_into().ok()?) as usize;
            let glyph_count = u32::from_le_bytes(bytes.get(16..20)?.try_into().ok()?) as usize;
            let bytes_per_glyph = u32::from_le_bytes(bytes.get(20..24)?.try_into().ok()?) as usize;
            let height = u32::from_le_bytes(bytes.get(24..28)?.try_into().ok()?) as usize;
            let width = u32::from_le_bytes(bytes.get(28..32)?.try_into().ok()?) as usize;
            let glyphs = bytes.get(header_size..)?;
            if glyphs.len() < glyph_count.saturating_mul(bytes_per_glyph) {
                return None;
            }
            Some(Self {
                version: FontVersion::Psf2,
                width,
                height,
                glyph_count,
                bytes_per_glyph,
                data: glyphs,
            })
        } else if bytes.starts_with(&PSF1_MAGIC) {
            let mode = *bytes.get(2)?;
            let height = usize::from(*bytes.get(3)?);
            let glyph_count: usize = if mode & PSF1_FLAG_512_GLYPHS != 0 {
                512
            } else {
                256
            };
            let bytes_per_glyph = height;
            let glyphs = bytes.get(4..)?;
            if glyphs.len() < glyph_count.saturating_mul(bytes_per_glyph) {
                return None;
            }
            Some(Self {
                version: FontVersion::Psf1,
                width: 8,
                height,
                glyph_count,
                bytes_per_glyph,
                data: glyphs,
            })
        } else {
            None
        }
    }

    /// The PSF variant this font was parsed from.
    pub fn version(&self) -> FontVersion {
        self.version
    }

    /// Glyph width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Glyph height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    /// Number of glyphs in the font.
    pub fn glyph_count(&self) -> usize {
        self.glyph_count
    }

    /// Returns the bitmap bytes of glyph `index`, or `None` when out of range.
    pub fn glyph(&self, index: usize) -> Option<&[u8]> {
        if index >= self.glyph_count {
            return None;
        }
        let start = index * self.bytes_per_glyph;
        self.data.get(start..start + self.bytes_per_glyph)
    }
}

/// Writes a valid PSF2 stream containing the baked-in `font8x8::BASIC` glyphs
/// (width 8, height 8, one glyph per byte) into `output`.
///
/// Returns the number of bytes written, or `None` if `output` is too small.
/// The needed size is [`PSF2_HEADER_SIZE`] + 128 x 8 bytes.
pub fn synth_psf2_basic(output: &mut [u8]) -> Option<usize> {
    use crate::font8x8::BASIC;
    let needed = PSF2_HEADER_SIZE + BASIC.len() * 8;
    let out = output.get_mut(..needed)?;
    out[0..4].copy_from_slice(&PSF2_MAGIC);
    out[4..8].copy_from_slice(&0u32.to_le_bytes());
    out[8..12].copy_from_slice(&(PSF2_HEADER_SIZE as u32).to_le_bytes());
    out[12..16].copy_from_slice(&0u32.to_le_bytes());
    out[16..20].copy_from_slice(&(BASIC.len() as u32).to_le_bytes());
    out[20..24].copy_from_slice(&8u32.to_le_bytes());
    out[24..28].copy_from_slice(&8u32.to_le_bytes());
    out[28..32].copy_from_slice(&8u32.to_le_bytes());
    let mut offset = PSF2_HEADER_SIZE;
    for glyph in BASIC.iter() {
        out[offset..offset + 8].copy_from_slice(glyph);
        offset += 8;
    }
    Some(needed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn u32_bytes(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    #[test]
    fn psf2_synthetic_roundtrip() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&PSF2_MAGIC);
        buf.extend_from_slice(&u32_bytes(0));
        buf.extend_from_slice(&u32_bytes(PSF2_HEADER_SIZE as u32));
        buf.extend_from_slice(&u32_bytes(0));
        buf.extend_from_slice(&u32_bytes(3));
        buf.extend_from_slice(&u32_bytes(8));
        buf.extend_from_slice(&u32_bytes(8));
        buf.extend_from_slice(&u32_bytes(8));
        buf.extend_from_slice(&[0x18, 0x3c, 0x66, 0xff, 0xff, 0x66, 0x3c, 0x18]);
        buf.extend_from_slice(&[0x10, 0x10, 0x7c, 0x10, 0x10, 0x10, 0x7c, 0x00]);
        buf.extend_from_slice(&[0x00; 8]);

        let font = PsfFont::parse(&buf).expect("PSF2 should parse");
        assert_eq!(font.version(), FontVersion::Psf2);
        assert_eq!(font.width(), 8);
        assert_eq!(font.height(), 8);
        assert_eq!(font.glyph_count(), 3);
        assert_eq!(
            font.glyph(0).unwrap(),
            &[0x18, 0x3c, 0x66, 0xff, 0xff, 0x66, 0x3c, 0x18]
        );
        assert!(font.glyph(2).is_some());
        assert!(font.glyph(3).is_none());
    }

    #[test]
    fn psf1_parses_with_256_glyphs() {
        let mut buf = vec![0x36, 0x04, 0x00, 0x02];
        for _ in 0..256 {
            buf.extend_from_slice(&[0xff, 0x00]);
        }
        let font = PsfFont::parse(&buf).expect("PSF1 should parse");
        assert_eq!(font.version(), FontVersion::Psf1);
        assert_eq!(font.width(), 8);
        assert_eq!(font.height(), 2);
        assert_eq!(font.glyph_count(), 256);
        assert_eq!(font.glyph(0).unwrap(), &[0xff, 0x00]);
        assert_eq!(font.glyph(65).unwrap(), &[0xff, 0x00]);
        assert!(font.glyph(256).is_none());
    }

    #[test]
    fn psf1_512_glyph_flag() {
        let mut buf = vec![0x36, 0x04, PSF1_FLAG_512_GLYPHS, 0x01];
        for _ in 0..512 {
            buf.push(0xaa);
        }
        let font = PsfFont::parse(&buf).expect("PSF1 512 should parse");
        assert_eq!(font.glyph_count(), 512);
        assert_eq!(font.glyph(511).unwrap(), &[0xaa]);
        assert!(font.glyph(512).is_none());
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(PsfFont::parse(&[0x11, 0x22, 0x33]).is_none());
        assert!(PsfFont::parse(&PSF2_MAGIC).is_none());
        let mut short = vec![0x36, 0x04, 0x00, 0x08];
        short.extend_from_slice(&[0x00; 100]);
        assert!(PsfFont::parse(&short).is_none());
    }

    #[test]
    fn synth_psf2_basic_roundtrips() {
        use crate::font8x8::BASIC;
        let mut buf = [0u8; 4096];
        let len = synth_psf2_basic(&mut buf).expect("buffer must be big enough");
        assert_eq!(len, PSF2_HEADER_SIZE + 128 * 8);
        let mut tiny = [0u8; PSF2_HEADER_SIZE - 1];
        assert!(synth_psf2_basic(&mut tiny).is_none());
        let font = PsfFont::parse(&buf[..len]).expect("synthesised PSF2 should parse");
        assert_eq!(font.width(), 8);
        assert_eq!(font.height(), 8);
        assert_eq!(font.glyph_count(), 128);
        let a = font.glyph(b'A' as usize).unwrap();
        assert_eq!(a.len(), 8);
        for i in 0..8 {
            assert_eq!(a[i], BASIC[b'A' as usize][i]);
        }
    }
}
