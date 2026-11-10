# ESP8266: SDK start-up through the HLE ROM

The `esp8266` machine boots unmodified Arduino ESP8266 3.1.2 images (NONOS SDK 2.2.2-dev,
`generic`, 4M1M) from its modelled mask ROM: the ROM loader starts eboot, eboot loads the app,
the SDK initialises (PHY, RTC, Wi-Fi in station mode) and the sketch's `setup()` and `loop()`
run. There is no mask-ROM image: every ROM function the SDK calls is a host-side model (HLE),
and an unmodelled one stops the run in strict mode (`RomState::strict`). This is a
software-compatibility claim for four images, not a hardware-timing or radio claim: there is
no RF, the Wi-Fi MAC never sends or receives a frame, and the CPU runs at one cycle per
instruction at 80 MHz.

## What was checked

All runs are strict: an unmodelled ROM entry fails the test with its name and call site, and
each test asserts `rom.unknown` is empty at the end.

| Image | Device time | Checked |
| --- | --- | --- |
| `esp8266-blink` | 3 s | `Ready`, `t=<ms> led=1` and `led=0`; GPIO2's output toggles every 500 000 +- 1 000 us (`delay(500)` is an `os_timer` on FRC2 at 1 ms granularity, plus one `loop()` pass, tens of us) |
| `esp8266-serial` | 6 s | host `hello\n` into UART0 gives `rx=hello`; a 36-character line (above the core's RX threshold 16) arrives through interrupt 5 and RXFIFO_FULL; UART1 carries `one 1` .. `one 50` and UART0 none of them |
| `esp8266-strap` | 6 s | `ESP.restart()` jumps to the ROM's reset vector; one reboot; RTC user memory survives it (the second start finds its magic word and only prints) |
| `esp8266-wifi-off` | 6 s | `WiFi.begin("Jumperly")` with no radio: ten `status=7` lines (WL_DISCONNECTED, `cores/esp8266/wl_definitions.h:59`), then `alive`; no reset, one ROM banner |

Plus unit tests of the peripherals (UART RX interrupt and RXFIFO_TOUT gating, FRC1 down-count
and NMI routing, FRC2 alarm edges, WDEV counters and RNG, SPI0 commands), the scheduler-free
pieces of the ROM (PHY I2C fields, soft float, MD5/HMAC/RC4/AES against RFC and FIPS vectors,
descriptor chains) and `tests/xtos.rs` (interrupt dispatcher and C exception wrapper around a
guest handler that clobbers every register and rewrites the frame).

Fixture hashes and build options are in [inputs.json](inputs.json), outcomes in
[checks.json](checks.json).

## Commands

```sh
cargo +1.99.0 test -p esp8266
cargo +1.99.0 test -p xtensa-lx7 --test lx106
cargo +1.99.0 clippy -p esp8266 -p xtensa-lx7 --all-targets -- -D warnings
cargo +1.99.0 build --release -p esp32sim
target/release/esp32sim --chip esp8266 --flash-image ../fixtures/bin/esp8266-blink.bin --max-seconds 2
target/release/esp32sim --chip esp8266 --flash-image ../fixtures/bin/esp8266-serial.bin --max-seconds 2
```

The images are read from `../../fixtures/bin` next to the workspace or from
`JUMPERLY_FIXTURES_DIR`.

## Speed

Release CLI on Windows 11, three runs of each image (the three within 3 % of each other):

| Image | Device time | Instructions | Wall | Rate |
| --- | --- | --- | --- | --- |
| `esp8266-blink` | 2 s | 421 451 | < 0.05 s | idle: the scheduler waits in WAITI and the machine skips to the next FRC2 alarm |
| `esp8266-serial` | 2 s | 159 999 900 | 2.7 s | 59 Minsn/s, 0.74x real time |

The serial sketch never idles: `loop()` ends in `yield()`, which posts the loop task again, so
the CPU executes an instruction every cycle, as it does on the chip. Its 6 s debug test
therefore executes 480 M instructions (about 67 s at about 7 Minsn/s unoptimised). Sketches that
wait in `delay()` (Blink, Wi-Fi off, strap) spend almost all device time in the scheduler's
WAITI and finish 6 s in well under a second. Real-time speed for busy sketches depends on the
interpreter-only LX106 core, not on this ROM model.

## ROM entries implemented

127 table names (both names of the `ets_`/plain memory and string functions count).

- Boot and exceptions: `_ResetVector`, `_xtos_unhandled_exception`, `_xtos_l1int_handler`,
  `_xtos_c_wrapper_handler`, `_xtos_set_exception_handler`, `_xtos_p_none`.
- Interrupts: `ets_isr_attach`, `ets_isr_mask`, `ets_isr_unmask`, `_xtos_ints_on`,
  `_xtos_ints_off`, `_xtos_set_intlevel`.
- Scheduler: `ets_task`, `ets_post_rom`, `ets_run`, `ets_set_idle_cb`.
- PHY: `phy_get_romfuncs` (a 256-slot table of stubs), `rom_i2c_readReg(_Mask)`,
  `rom_i2c_writeReg(_Mask)`, `rom_chip_v5_enable_cca`, `rom_chip_v5_disable_cca`, `rom_sar_init`.
- GPIO and helpers: `gpio_output_set`, `gpio_input_get`, `gpio_pin_wakeup_enable/disable`,
  `lldesc_build_chain`, `lldesc_num2link`, `lldesc_set_owner`, `roundup2`,
  `ets_update_cpu_frequency`, `ets_get_cpu_frequency`, `ets_rtc_int_register`.
- Crypto (hostap prototypes): `MD5Init/Update/Final`, `md5_vector`, `hmac_md5(_vector)`,
  `sha1_vector`, `hmac_sha1(_vector)`, `pbkdf2_sha1`, `sha1_prf`, `rc4_skip`, `aes_decrypt`,
  `rijndaelKeySetupDec`.
- Soft float: `__addsf3 __subsf3 __mulsf3 __fixunssfsi __floatsisf __floatunsisf __adddf3
  __subdf3 __muldf3 __divdf3 __fixdfsi __fixunsdfsi __truncdfsf2 __extendsfdf2 __floatsidf
  __floatunsidf`.
- Output: `ets_printf`, `ets_vprintf`, `ets_uart_printf`, `ets_putc`, `ets_write_char`,
  `ets_uart_putc1`, `uart_tx_one_char`, `ets_install_putc1`, `ets_install_uart_printf`,
  `rom_uart_div_modify`, `Uart_Init`, `uart_buff_switch`. A guest character function installed
  with `ets_install_putc1` is called once per character.
- From the previous step: `ets_delay_us`, memory and string functions, 64-bit integer helpers,
  `SPIRead/Write/EraseSector/EraseBlock` and the SPI status functions, `Cache_Read_Enable/Disable`,
  `rtc_get_reset_reason`, `software_reset`, `ets_wdt_*`.

## Concessions

Places where the model answers what the SDK waits for instead of modelling the hardware behind
it. Each is cited in the source next to the code.

| Where | What the model returns | Why | Source |
| --- | --- | --- | --- |
| RF I2C block 98 reg 7 bit 7 | always set | libphy `wait_rfpll_cal_end` polls it for "PLL calibrated" | digitake esp8266-emulator `src/rom.c:622-633` (rev `c4c840d5`) |
| `phy_get_romfuncs` slots other than 36..39 | 0, one log line | no radio; which ROM function each slot is is not published | table and stubs: same, `src/rom.c:30,39` |
| `rom_chip_v5_*_cca`, `rom_sar_init` | no-op | radio and its ADC are not modelled | - |
| RTC 0x60000728 bit 0 | always set | libmain `pm_wait4wakeup` polls bits 1..0 after the PHY start | digitake `src/soc.c:559` |
| RTC 0x6000071C | device time at about 150 kHz | the RTC slow counter | digitake `src/soc.c:558` |
| GPIO 0x60000370 (RTC_CALIB_VALUE) | `1 << 31 \| 0x4E20` at once | libmain `pm_rtc_clock_cali` polls bit 31 | digitake `src/soc.c:218` |
| GPIO_IN bits 16.. | strapping 3 (flash boot) | `Updater.cpp:84` reads `(GPI >> 16) & 0xf` | digitake `src/soc.c:216` |
| RF 0x60009B60 bit 1 | always clear | libphy `do_noisefloor` polls it with a timeout; clear is "measurement done" | Blink image 0x402277e0 (disassembly) |
| WDEV 0x3FF20C00, 0x3FF21048/4C | device time in us (TSF 64-bit) | `system_get_time`, beacon scheduling | digitake `src/soc.c:500-504` |
| WDEV 0x3FF20E44 (RANDOM_REG32) | deterministic LCG seeded from the MAC | reproducible runs | `esp8266_peri.h:851` |
| WDEV MAC queues | plain memory | no frame completes, no MAC frame interrupt; WiFi stays "connecting" | - |
| WDEV MAC timer 1 (0x3FF210C8..D0, 0x3FF21058) | status bit 26 on interrupt 0 at the target microsecond | libpp's soft watchdog (`pp_soft_wdt_feed_local`) runs on it and feeds the hardware watchdog | digitake `src/soc.c:473-493,753-759`; Blink image 0x4022f580, 0x40101944 |
| SPI0 CMD | the command completes at once, CMD reads 0 | `spi_flash_get_id` and SDK flash reads poll CMD | `esp8266_peri.h:301-320,382-391`; digitake `src/soc.c:424-436` |
| UART RXFIFO_TOUT | masked unless CONF1 RX_TOUT_EN | the shared UART raises it whenever bytes wait; the core leaves it disabled | `uart_register.h:17,103`; `uart.cpp:430-433,457` |
| Scheduler idle callback | once per wake-up, then WAITI | the ROM calls it on every empty pass; once lets the machine skip idle time | digitake `src/rom.c:390-402` |
| Task priorities | 0..32 | libpp posts at 32 | digitake `src/rom.c:48` |
| FRC2 alarm equal to the count | fires on this count | `frc2_sched` | digitake `src/soc.c:283-293` |
| `_xtos_c_wrapper_handler` | handler runs at INTLEVEL 1 | the ROM's original lowers to 0 (`rsil a13, 0`), the core's copy does not | `exc-c-wrapper-handler.S` |
| Level-1 dispatch order | lowest pending line first | XTOS uses a priority table not in the SDK | - |
| `ets_update_cpu_frequency` default | 80 MHz | the CPU runs at 80 MHz from reset here (CPU2X recorded, not modelled) | `core_esp8266_main.cpp:96-102` |
| `ets_rtc_int_register` | nothing attached | no RTC interrupt is raised | - |

## Limits

- Boot modes other than flash (strapping `(GPI >> 16) & 0xf` != 3, e.g. GPIO0 low = UART download,
  mode (1,x)): the ROM waits for the download tool (Arduino ESP8266 `doc/boards.rst` "Boot
  Messages and Modes"; esp8266/Arduino issue #1722). Its protocols are not modelled, so the core
  parks with every interrupt masked and waits for a reset instead of stopping the run; a reset
  with GPIO0 released boots from flash.
- Reset reasons: a software restart reports `Power On` and the banner `rst cause:1` again
  (cause 4 and the WDT come with the next step's RTC/WDT model).
- No CPU-time comparison on the hello demos: the shared crates change only by
  `Lx106::waiti`, which no other chip calls.
