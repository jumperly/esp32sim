# ESP8266 host layer (J29): measurements behind the choices

Jumperly patch J29 (`esp8266/src/{jumperly,pwm,soft_i2c}.rs`, `wasm/src/chips/esp8266.rs`). Source:
esp32sim `patch/esp8266-host` (2026-11-10), Arduino ESP8266 3.1.2 fixtures of the Jumperly monorepo
(`emulator/fixtures/bin/esp8266-*.bin`), Rust 1.99.0, Windows 11.

## Which edges are the waveform's

The ESP8266 has no PWM block: core 3.1.2 toggles `analogWrite`, `tone` and `Servo` pins in the FRC1
NMI (`core_esp8266_waveform_pwm.cpp`, `timer1Interrupt`). OneWire bit-banging can repeat a slot
shape (read scratchpad 0xBE has five 1 slots in a row: four 65 us periods at the same duty), so
the stability rule alone would take it for 15 kHz PWM and swallow its edges. The detector counts
only edges written soon after FRC1 ran out. Measured from the timer to the GPIO write (APB
cycles, a temporary `eprintln!` in `SocBus::pins_changed`, the wasm tests
`esp8266_pwm_fixture_yields_pwm_events_not_edges`, `esp8266_servo_fixture_pwm_50hz`,
`esp8266_blink_prints_on_uart0_and_toggles_gpio2`):

| Edges | Count | Min | Max |
| --- | --- | --- | --- |
| `analogWrite(4, ...)`, 1 kHz | 8024 | 96 | 576 |
| `tone(5, 440)` | 1762 | 150 | 704 |
| `Servo` on GPIO14, 50 Hz | 160 | 160 | 256 |
| Blink's `digitalWrite(2, ...)` | 5 | 343 168 | 120 366 336 |

The window is 20 us (1600 cycles), more than twice the largest.

## The TOUT pad

`analogRead(A0)` is `system_adc_read` (libmain, Blink image 0x40234990): `test_tout` (libphy,
0x4022699c) averages eight samples from `read_sar_dout` (0x4022c090), which reads the words at
0x60000D80..0x60000D9C (the literal 0x60000A00 + 0x380 + 4 i), takes `v = !word` and makes a sample
of `(v & 0x700) + min(255, max(0, (v & 0xff) - 21) * 0x117 >> 8)`; `test_tout` returns
`(sum + 8) >> 4`, `system_adc_read` multiplies by 12, divides by 11 (`__divsi3`) and caps at 1024.
With the registers at 0 (the model without a voltage) every reading is 1024. `sar_word` picks the
word whose reading is closest to the code; 500 mV gives 512.

## Wi-Fi trigger

Register writes to the WDEV block (0x3FF20000..) in 1.5 s of the release CLI (`--debug mmio`):
Blink writes 44 addresses, `esp8266-wifi-off` 56. Only the Wi-Fi sketch writes 0x3FF20DC0..0x3FF20DD4,
right after `WiFi.begin()`: 0x3FF20DD4 = 0x00100000, 0x3FF20DC4 = 0x0062F3DC, 0x3FF20DD0 = 0,
0x3FF20DC8 = 0x36, 0x3FF20DC0 = 0x1000, then 0x3FF20DC4 = 0xC062F3DC (bit 31: the transmit
queue starts). The note fires on that last write.

## CPU time of the other chips

Release CLI, 5 runs of each tree alternating, median wall time (the machine otherwise idle;
Windows has no `/usr/bin/time`); before = `bcc4fe65` (`jumperly` + `patch/esp8266`), after = the
J29 tree. The instruction counts match.

| Run | Before | After | Instructions |
| --- | --- | --- | --- |
| S3 `hello` (ROM boot, 10 s cap) | 0.125 s | 0.126 s | 2 257 786 |
| C3 `c3-hello`, 30 s | 3.192 s | 3.157 s | 4 800 000 000 |
| classic `esp32-blink`, 3 s | 0.210 s | 0.196 s | 3 931 139 |
| ESP8266 `esp8266-serial`, 3 s (no host layer on) | 4.218 s | 4.080 s | 239 999 849 |
| ESP8266 `esp8266-pwm`, 4 s (no host layer on) | 0.137 s | 0.138 s | 4 610 789 |

The S3, C3 and classic code is unchanged by J29 (it touches the `esp8266` and `esp32sim-wasm`
crates only); the ESP8266's bus gains a check of two `Option`s per register write and a store
when a timer falls due.
