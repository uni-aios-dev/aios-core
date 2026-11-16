//! Little-endian byte reader with the LEB128 and UTF-8 helpers the WebAssembly
//! binary format needs.

use crate::module::ModuleError;

/// A cursor over a WebAssembly byte slice.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Starts at the first byte.
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// Whether every byte was consumed.
    pub const fn eof(&self) -> bool {
        self.pos == self.bytes.len()
    }

    /// The bytes that have not been read yet.
    pub fn rest(&self) -> &'a [u8] {
        &self.bytes[self.pos..]
    }

    /// Reads one byte.
    pub fn u8(&mut self) -> Result<u8, ModuleError<'a>> {
        let byte = *self.bytes.get(self.pos).ok_or(ModuleError::Truncated)?;
        self.pos += 1;
        Ok(byte)
    }

    /// Reads exactly `count` bytes without copying.
    pub fn bytes(&mut self, count: usize) -> Result<&'a [u8], ModuleError<'a>> {
        let end = self.pos.checked_add(count).ok_or(ModuleError::Truncated)?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or(ModuleError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    /// Reads an unsigned LEB128 `u32` (at most five bytes).
    pub fn uleb32(&mut self) -> Result<u32, ModuleError<'a>> {
        let mut result = 0u32;
        for shift in [0, 7, 14, 21, 28] {
            let byte = self.u8()?;
            let low = u32::from(byte & 0x7F);
            if shift == 28 && byte & 0x70 != 0 {
                return Err(ModuleError::LebOverflow);
            }
            result |= low << shift;
            if byte & 0x80 == 0 {
                return Ok(result);
            }
        }
        Err(ModuleError::LebOverflow)
    }

    /// Reads a signed LEB128 `i32` (at most five bytes).
    pub fn sleb32(&mut self) -> Result<i32, ModuleError<'a>> {
        let mut result = 0i32;
        let mut shift = 0u32;
        loop {
            let byte = self.u8()?;
            if shift == 28 {
                // Bits 4..=6 are the sign extension of bit 3; anything else is
                // an overlong encoding or a value that does not fit in 32 bits.
                let extension = (byte >> 4) & 0x07;
                let negative = byte & 0x08 != 0;
                let expected = if negative { 0x07 } else { 0x00 };
                if extension != expected || byte & 0x80 != 0 {
                    return Err(ModuleError::LebOverflow);
                }
                result |= i32::from(byte & 0x0F) << 28;
                return Ok(result);
            }
            result |= i32::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                if byte & 0x40 != 0 {
                    result |= !0 << (shift + 7);
                }
                return Ok(result);
            }
            shift += 7;
        }
    }

    /// Reads a length-prefixed UTF-8 name and borrows it from the input.
    pub fn name(&mut self) -> Result<&'a str, ModuleError<'a>> {
        let len = self.uleb32()? as usize;
        let bytes = self.bytes(len)?;
        core::str::from_utf8(bytes).map_err(|_| ModuleError::Utf8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_unsigned_leb128() {
        assert_eq!(Reader::new(&[0x00]).uleb32(), Ok(0));
        assert_eq!(Reader::new(&[0x7F]).uleb32(), Ok(127));
        assert_eq!(Reader::new(&[0x80, 0x01]).uleb32(), Ok(128));
        assert_eq!(
            Reader::new(&[0xFF, 0xFF, 0xFF, 0xFF, 0x0F]).uleb32(),
            Ok(u32::MAX)
        );
        assert_eq!(
            Reader::new(&[0xFF, 0xFF, 0xFF, 0xFF, 0x1F]).uleb32(),
            Err(ModuleError::LebOverflow)
        );
        assert_eq!(Reader::new(&[0x80]).uleb32(), Err(ModuleError::Truncated));
    }

    #[test]
    fn reads_signed_leb128() {
        assert_eq!(Reader::new(&[0x00]).sleb32(), Ok(0));
        assert_eq!(Reader::new(&[0x7F]).sleb32(), Ok(-1));
        assert_eq!(Reader::new(&[0x40]).sleb32(), Ok(-64));
        assert_eq!(Reader::new(&[0xBF, 0x7F]).sleb32(), Ok(-65));
        assert_eq!(
            Reader::new(&[0xFF, 0xFF, 0xFF, 0xFF, 0x07]).sleb32(),
            Ok(i32::MAX)
        );
        assert_eq!(
            Reader::new(&[0x80, 0x80, 0x80, 0x80, 0x78]).sleb32(),
            Ok(i32::MIN)
        );
        assert_eq!(
            Reader::new(&[0x80, 0x80, 0x80, 0x80, 0x10]).sleb32(),
            Err(ModuleError::LebOverflow)
        );
    }

    #[test]
    fn reads_names_and_stops_at_the_end() {
        let mut reader = Reader::new(&[0x04, b'a', b'i', b'o', b's']);
        assert_eq!(reader.name(), Ok("aios"));
        assert!(reader.eof());
        assert_eq!(
            Reader::new(&[0x02, 0xFF, 0xFE]).name(),
            Err(ModuleError::Utf8)
        );
    }
}
