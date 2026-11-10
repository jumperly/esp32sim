//! The SPI NOR flash on the module (4 MB on an ESP-12F). Program operations only clear bits, so
//! a write ANDs into what is there and only an erase sets bytes back to 0xFF, as on the chip.
//! Geometry: Arduino ESP8266 3.1.2 tools/sdk/include/spi_flash_geometry.h:4-6.

/// FLASH_SECTOR_SIZE, the erase unit of `SPIEraseSector`.
pub const SECTOR: usize = 0x1000;
/// FLASH_BLOCK_SIZE, the erase unit of `SPIEraseBlock`.
pub const BLOCK: usize = 0x1_0000;

pub struct Flash { pub bytes: Vec<u8> }

impl Flash {
    /// An erased chip of `size` bytes.
    pub fn new(size: usize) -> Self { Flash { bytes: vec![0xff; size] } }
    pub fn len(&self) -> usize { self.bytes.len() }
    pub fn is_empty(&self) -> bool { self.bytes.is_empty() }

    /// `out.len()` bytes from `off`; false (and `out` untouched) past the end.
    pub fn read(&self, off: u32, out: &mut [u8]) -> bool {
        let Some(src) = (off as usize).checked_add(out.len()).and_then(|end| self.bytes.get(off as usize..end)) else { return false };
        out.copy_from_slice(src);
        true
    }

    /// Program `data` at `off`: each byte ANDs into the cell. False (nothing written) past the end.
    pub fn write(&mut self, off: u32, data: &[u8]) -> bool {
        let Some(dst) = (off as usize).checked_add(data.len()).and_then(|end| self.bytes.get_mut(off as usize..end)) else { return false };
        for (d, s) in dst.iter_mut().zip(data) { *d &= s; }
        true
    }

    /// Erase 4 KB sector `n`; false past the end.
    pub fn erase_sector(&mut self, n: u32) -> bool { self.erase(n as usize * SECTOR, SECTOR) }

    /// Erase 64 KB block `n`; false past the end.
    pub fn erase_block(&mut self, n: u32) -> bool { self.erase(n as usize * BLOCK, BLOCK) }

    fn erase(&mut self, off: usize, len: usize) -> bool {
        let Some(cells) = off.checked_add(len).and_then(|end| self.bytes.get_mut(off..end)) else { return false };
        cells.fill(0xff);
        true
    }

    /// What a programmer leaves (esptool erases, then writes): `data` replaces the bytes at `off`.
    pub fn load(&mut self, off: usize, data: &[u8]) -> Result<(), String> {
        let end = off.checked_add(data.len()).filter(|&e| e <= self.bytes.len())
            .ok_or_else(|| format!("{} bytes at flash {off:#x} do not fit in {} bytes of flash", data.len(), self.bytes.len()))?;
        self.bytes[off..end].copy_from_slice(data);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_clear_bits_and_only_erases_set_them() {
        let mut f = Flash::new(2 * BLOCK);
        assert!(f.write(0x1000, &[0x0f, 0xf0]));
        assert!(f.write(0x1000, &[0xf3, 0xff]));
        let mut b = [0; 2];
        assert!(f.read(0x1000, &mut b));
        assert_eq!(b, [0x03, 0xf0]);
        assert!(f.erase_sector(1));
        assert!(f.read(0x1000, &mut b));
        assert_eq!(b, [0xff, 0xff]);
        f.write(BLOCK as u32 + 5, &[0]);
        assert!(f.erase_block(1));
        assert_eq!(f.bytes[BLOCK + 5], 0xff);
        assert!(!f.erase_block(2));
        assert!(!f.erase_sector(32));
        assert!(!f.read(2 * BLOCK as u32 - 1, &mut b));
        assert!(!f.write(u32::MAX, &[0]));
        assert!(f.load(2 * BLOCK - 1, &[1, 2]).is_err());
        f.load(0, &[1, 2]).unwrap();
        assert_eq!(&f.bytes[..2], &[1, 2]);
    }
}
