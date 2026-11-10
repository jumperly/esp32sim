//! Boards around the ESP8266. One file per board.
//!
//! - `Esp12f`: the Ai-Thinker ESP-12F module, the chip, 4 MB of flash and a blue LED on GPIO2,
//!   nothing else on the pins (no button, no pull resistor)
//! - `NoBoard`: nothing on the pins
pub use esp_soc::board::{Board, BoardModel, NoBoard};

pub mod esp12f;

pub use esp12f::{Esp12f, PIN_LED};

/// Board by name: `esp12f` (`esp-12f`, `esp12`) or `none` (`bare`).
pub fn make_board(name: &str) -> Option<Board> {
    match name {
        "none" | "bare" => Some(Box::new(NoBoard)),
        "esp12f" | "esp-12f" | "esp12" => Some(Box::new(Esp12f)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_esp8266_boards_are_named_and_the_others_are_not() {
        for name in ["none", "bare", "esp12f", "esp-12f", "esp12"] { assert!(super::make_board(name).is_some(), "{name}"); }
        assert_eq!(super::make_board("esp12f").unwrap().name(), "esp12f");
        assert_eq!(super::make_board("esp12f").unwrap().named_pin("led"), Some(2));
        for name in ["atech14", "esp32dev", "xiao"] { assert!(super::make_board(name).is_none(), "{name}"); }
    }
}
