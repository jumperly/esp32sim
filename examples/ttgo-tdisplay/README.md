# LilyGO TTGO T-Display: spot prices in MicroPython

A pure-Python application for the T-Display's ST7789 135×240 panel on stock
MicroPython v1.29.0 (the official `ESP32_GENERIC` image): the day's electricity
spot prices for a Swedish area as a bar chart, the current hour's price large,
fetched from elprisetjustnu.se over Wi-Fi when `config.json` has an `ssid`, or
drawn from `demo_prices.json` when it has none. It is the firmware of the
`tdisplay-prices` browser demo (`web/wasm/fw/tdisplay-prices.json`) and runs
unchanged on the board.

Files:

- `app.py` — the application (this repository, MIT); `main.py` is `import app`.
- `st7789py.py`, `vga1_8x16.py`, `vga2_16x32.py` — Russ Hughes's
  [st7789py_mpy](https://github.com/russhughes/st7789py_mpy) driver and two of
  its bitmap fonts (MIT; notice in `st7789py.py` and
  `web/wasm/fw/public/tdisplay-NOTICE.txt`).
- `demo_prices.json` — one day (2026-10-14, SE3, 96 quarter-hour slots) in the
  API's format, for the no-network path.
- `config.json` — `{"area": "SE3"}`: the browser configuration, no `ssid`, so
  the app skips Wi-Fi and draws the demo data at once (the browser build has
  no sockets). On a board, add `"ssid"` and `"psk"`.

## The filesystem image

MicroPython's `ESP32_GENERIC` partition table puts the 2 MiB `vfs` partition
(LittleFS) at flash offset 0x200000. `build-vfs.sh` compiles the modules with
`mpy-cross` (v1.29, `-march=xtensawin`: `st7789py` has `@micropython.viper`
and `@micropython.native` functions, so the compiler's `.mpy` sub-version and
architecture must match the image), packs the directory with `mklittlefs
-b 4096 -p 32 -s 2097152` (MicroPython's geometry), and trims the image to its
used blocks: `mklittlefs` writes every unused block as 0xff, which is also what
erased flash and the emulator's flash read, so everything after the last block
that is not all 0xff is cut. The result is
`web/wasm/fw/public/tdisplay-vfs.bin`, 647,168 bytes (158 of 512 blocks; the
allocator starts the files at block 144) instead of 2,097,152, with the same
`mklittlefs -l` listing. A rebuild differs only in the image creation time
`mklittlefs` stamps into block 1 (the file mtimes are fixed by the script).

```sh
pip install mpy-cross==1.29.0          # or any mpy-cross built from MicroPython v1.29
MPY_CROSS=mpy-cross MKLITTLEFS=~/.platformio/packages/tool-mklittlefs/mklittlefs \
  examples/ttgo-tdisplay/build-vfs.sh
```

## Running

Natively, with the fetched ROM and MicroPython image (`tools/fetch-rom-elfs.sh
web/wasm/fw`, `tools/fetch-micropython.sh`):

```sh
./target/release/esp32sim --chip esp32 --rom web/wasm/fw/esp32_rev300_rom.elf \
  --board ttgo-tdisplay --boot rom --console uart0 --flash-mb 4 \
  --flash-at 0x1000=web/wasm/fw/ESP32_GENERIC-20260824-v1.29.0.bin \
  --flash-at 0x200000=web/wasm/fw/public/tdisplay-vfs.bin --max-seconds 8 --tft-png screen.png
```

The console prints `WLAN no connection`, `PRICES demo 96 slots` and `DRAWN demo
local 12:00 now 0.4933525` within the first 1.2 s of emulated time; the
`[emu] tdisplay:` report line counts 201 RAMWR and 119,536 pixels. With
`--wifi "ssid=demo,chan=6,psk=demo-password"` and a `config.json` naming that
network the app joins the virtual access point, sets the clock by SNTP and
fetches the real prices over HTTPS (`PRICES live`, `DRAWN live`).

In the browser: `run.html?wasm&fw=tdisplay-prices`; under Node,
`ESP32SIM_ROM_DIR=roms node tools/wasm-test.mjs tdisplay-prices`.
