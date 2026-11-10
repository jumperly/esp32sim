//! ESP8266 (ESP8266EX): one Xtensa LX106 core (`xtensa_lx7::Lx106`) at 80 MHz, 160 with CPU2X,
//! 96 KB DRAM, 64 KB IRAM, a 1 MB window into the flash through the cache, and a mask ROM modelled
//! in Rust (`rom`), because Espressif publishes no ROM ELF for this chip. `docs/esp8266.md`.
pub mod board;
pub mod bus;
pub mod flash;
pub mod periph;
pub mod rom;
pub mod soc;

pub use esp_soc::Stop;
pub use soc::{machine, Esp8266, Machine};
