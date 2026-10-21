use esp_periph::{Gpio, RegRam};
use esp_soc::pins::ChipPins;

#[test]
fn software_drive_respects_mux_route_enable_and_inversion() {
    for chip in [ChipPins::S3, ChipPins::C3, ChipPins::C6, ChipPins::ESP32] {
        let mut gpio = Gpio::new();
        let mut mux = RegRam::new();
        let signal = (chip.output_mask + 1) / 4;
        for pin in [4u8, 21] {
            let bit = 1u64 << pin;
            gpio.enable = bit;
            gpio.out = bit;
            assert_eq!(chip.routes(&gpio, &mux).software_output(pin), None);
            mux.write(4 + 4 * u32::from(pin), 1 << 12);
            for oen in [0, signal * 4] {
                gpio.func_out_sel[usize::from(pin)] = signal | oen;
                assert_eq!(chip.routes(&gpio, &mux).software_output(pin), Some(true));
                gpio.out = 0;
                assert_eq!(chip.routes(&gpio, &mux).software_output(pin), Some(false));
                gpio.func_out_sel[usize::from(pin)] |= signal * 2;
                assert_eq!(chip.routes(&gpio, &mux).software_output(pin), Some(true));
                gpio.enable = 0;
                assert_eq!(chip.routes(&gpio, &mux).software_output(pin), None);
                gpio.func_out_sel[usize::from(pin)] |= signal * 8;
                assert_eq!(chip.routes(&gpio, &mux).software_output(pin), Some(true));
                gpio.enable = bit;
                assert_eq!(chip.routes(&gpio, &mux).software_output(pin), None);
                gpio.out = bit;
            }
            gpio.func_out_sel[usize::from(pin)] = 12;
            assert_eq!(chip.routes(&gpio, &mux).software_output(pin), None);
        }
        for pin in [49, 63, 64, 255] {
            assert_eq!(chip.routes(&gpio, &mux).software_output(pin), None);
        }
    }
    let mut gpio = Gpio::new();
    let mut mux = RegRam::new();
    gpio.enable = 1u64 << 48;
    gpio.out = gpio.enable;
    mux.write(4 + 4 * 48, 1 << 12);
    assert_eq!(ChipPins::S3.routes(&gpio, &mux).software_output(48), Some(true));
    assert_eq!(ChipPins::C3.routes(&gpio, &mux).software_output(48), None);
}
