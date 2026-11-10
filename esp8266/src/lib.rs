//! ESP8266 (ESP8266EX): one Xtensa LX106 core at 80 MHz (`xtensa_lx7::Lx106`), 96 KB DRAM,
//! 64 KB IRAM, a 1 MB window into the flash through the cache, and a mask ROM modelled in Rust
//! (`rom`), because Espressif publishes no ROM ELF for this chip.
pub mod bus;
pub mod flash;
pub mod periph;
pub mod rom;
pub mod soc;

pub use esp_soc::Stop;
pub use soc::{machine, Esp8266, Machine};
