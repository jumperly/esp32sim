//! The Ai-Thinker ESP-12F: an ESP8266EX with a 26 MHz crystal, 4 MB of SPI flash and a PCB
//! antenna on a 16 x 24 mm module (Ai-Thinker ESP-12F datasheet v1.0).
//!
//! The module puts nothing on the chip's pins but the flash (GPIO6..11, not on the edge pads):
//! no pull resistors on EN, RST, GPIO0, GPIO2 or GPIO15, no button and no USB bridge. The blue
//! LED next to the antenna is on GPIO2 (the Arduino core's `led=2` board option), lit while the
//! pin is low; it is the only part this model names. The strapping levels at reset are the
//! chip's own (`Peripherals::strap`): GPIO0 and GPIO2 pulled up by the ROM, GPIO15 floating,
//! which reads low here. The schematic around the module (the RST wire from GPIO16 for deep
//! sleep, buttons, pull resistors) is the host's: `SocBus::gpio_set_input`, `request_reset`.
use esp_soc::BoardModel;

/// The module's LED: GPIO2, lit while low.
pub const PIN_LED: u8 = 2;

/// The Ai-Thinker ESP-12F module.
pub struct Esp12f;

impl BoardModel for Esp12f {
    fn name(&self) -> &'static str { "esp12f" }
    fn named_pin(&self, name: &str) -> Option<u8> { (name == "led").then_some(PIN_LED) }
}
