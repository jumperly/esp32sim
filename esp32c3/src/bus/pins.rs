use crate::periph::Peripherals;
use esp_soc::{board::SpiPins, pins::ChipPins};

impl Peripherals {
    pub(crate) fn i2c_pin(&self, signal: usize) -> Option<u8> { ChipPins::C3.routes(&self.gpio, &self.io_mux).i2c_pin(signal) }
    pub(crate) fn spi2_pins(&self) -> SpiPins {
        ChipPins::C3.routes(&self.gpio, &self.io_mux).spi_pins(&self.spi2, [63, 64, 65, 68, 69, 70, 71, 72, 73], 128, &[(2, 6, 7, 2, &[10])])
    }
    /// Jumperly patches J19 / J21: the (TX, RX) pads of UART `port` (0, 1) from upstream's route.
    pub fn uart_pins(&self, port: u8) -> (Option<u8>, Option<u8>) {
        if port > 1 { return (None, None); }
        let r = self.uart_route(port as usize);
        ((r.tx_pins != 0).then(|| r.tx_pins.trailing_zeros() as u8), r.rx_pin)
    }
    /// Jumperly patch J21: the baud UART `port` (0, 1) is set to, if its clock and divider are.
    pub fn uart_baud(&self, port: u8) -> Option<u32> {
        if port > 1 { return None; }
        self.uart_route(port as usize).baud
    }
}
