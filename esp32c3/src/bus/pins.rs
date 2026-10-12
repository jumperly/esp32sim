use crate::periph::Peripherals;
use esp_soc::{board::SpiPins, pins::ChipPins};

impl Peripherals {
    pub(crate) fn i2c_pin(&self, signal: usize) -> Option<u8> { ChipPins::C3.routes(&self.gpio, &self.io_mux).i2c_pin(signal) }
    pub(crate) fn spi2_pins(&self) -> SpiPins {
        ChipPins::C3.routes(&self.gpio, &self.io_mux).spi_pins(&self.spi2, [63, 64, 65, 68], 128, &[(2, 6, 7, 2, 10)])
    }
    /// Jumperly patch J19: the (TX, RX) pads of UART `port`: U0 signal 6 with IO_MUX pads 21 / 20
    /// (function 0), U1 signal 9 on the matrix only (`soc/gpio_sig_map.h`, `soc/uart_pins.h`).
    pub fn uart_pins(&self, port: u8) -> (Option<u8>, Option<u8>) {
        let routes = ChipPins::C3.routes(&self.gpio, &self.io_mux);
        match port {
            0 => routes.uart_pins(6, Some((21, 20, 0))),
            1 => routes.uart_pins(9, None),
            _ => (None, None),
        }
    }
}
