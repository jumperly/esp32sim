use crate::periph::Peripherals;
use esp_soc::{board::SpiPins, pins::ChipPins};

impl Peripherals {
    pub(crate) fn i2c_pin(&self, signal: usize) -> Option<u8> { ChipPins::S3.routes(&self.gpio, &self.io_mux).i2c_pin(signal) }
    pub(crate) fn spi2_pins(&self) -> SpiPins {
        ChipPins::S3.routes(&self.gpio, &self.io_mux).spi_pins(&self.spi2, [101, 102, 103, 110], 256, &[(4, 12, 11, 13, 10), (2, 36, 35, 37, 34)])
    }
    /// Jumperly patch J18: SPI3 has no IO_MUX pins on the S3, only the matrix: SPI3_CLK 66, SPI3_Q 67,
    /// SPI3_D 68, SPI3_CS0..2 71, 72, 127 (`soc/gpio_sig_map.h`).
    pub(crate) fn spi3_pins(&self) -> SpiPins {
        ChipPins::S3.routes(&self.gpio, &self.io_mux).spi_pins_cs(&self.spi3, [66, 67, 68], &[71, 72, 127], 256, &[])
    }
    /// Jumperly patches J19 / J21: the (TX, RX) pads of UART `port` (0..2) from upstream's route
    /// (the GPIO matrix or the UART's IO_MUX pads; the lowest TX pad when it fans out).
    pub fn uart_pins(&self, port: u8) -> (Option<u8>, Option<u8>) {
        if port > 2 { return (None, None); }
        let r = self.uart_route(port as usize);
        ((r.tx_pins != 0).then(|| r.tx_pins.trailing_zeros() as u8), r.rx_pin)
    }
    /// Jumperly patch J21: the baud UART `port` (0..2) is set to, if its clock and divider are.
    pub fn uart_baud(&self, port: u8) -> Option<u32> {
        if port > 2 { return None; }
        self.uart_route(port as usize).baud
    }
    /// Jumperly patch J18: GP-SPI `host` 2 or 3.
    pub fn gpspi(&self, host: u8) -> &esp_periph::GpSpi { if host == 3 { &self.spi3 } else { &self.spi2 } }
    pub fn gpspi_mut(&mut self, host: u8) -> &mut esp_periph::GpSpi { if host == 3 { &mut self.spi3 } else { &mut self.spi2 } }
}
