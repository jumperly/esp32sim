#!/bin/sh
# Build the demo's MicroPython filesystem: a LittleFS image of this directory's app for the vfs
# partition of the official ESP32_GENERIC image (2 MiB at flash 0x200000), trimmed to its used
# blocks, into web/wasm/fw/public/tdisplay-vfs.bin.
#   examples/ttgo-tdisplay/build-vfs.sh [OUT.bin]
#   MPY_CROSS=path    mpy-cross for MicroPython v1.29 (pip install mpy-cross==1.29.0; default: on
#                     PATH). The modules are precompiled so the board never compiles the 66 KB font
#                     source on its heap; st7789py has @micropython.viper/native functions, so the
#                     compiler's version (.mpy sub-version) and -march=xtensawin must match the image
#   MKLITTLEFS=path   mklittlefs (default: on PATH, else PlatformIO's tool-mklittlefs)
# The image is 2 MiB with 4 KiB blocks and 32-byte pages, MicroPython's vfs geometry for the
# ESP32_GENERIC build (ports/esp32: lfs2, block 4096, lookahead 32). mklittlefs writes every unused
# block as 0xff, which is also the emulator's erased flash, so the image is cut after the last
# block that is not all 0xff; the listing (mklittlefs -l) of the trimmed file is unchanged.
# A rebuild differs from the committed image only in mklittlefs's image creation timestamp (a
# root attribute in block 1) — the file mtimes are fixed below so the per-file timestamps agree.
set -e
cd "$(dirname "$0")"
OUT=${1:-../../web/wasm/fw/public/tdisplay-vfs.bin}
MPY_CROSS=${MPY_CROSS:-mpy-cross}
MKLITTLEFS=${MKLITTLEFS:-$(command -v mklittlefs || echo "$HOME/.platformio/packages/tool-mklittlefs/mklittlefs")}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/fs"
for m in app st7789py vga1_8x16 vga2_16x32; do "$MPY_CROSS" -march=xtensawin -o "$tmp/fs/$m.mpy" "$m.py"; done
cp main.py config.json demo_prices.json "$tmp/fs/"
touch -t 202601010000 "$tmp/fs" "$tmp/fs"/*
"$MKLITTLEFS" -c "$tmp/fs" -b 4096 -p 32 -s 2097152 "$tmp/vfs.bin" >/dev/null
python3 -I - "$tmp/vfs.bin" "$OUT" <<'PY'
import sys
data = open(sys.argv[1], 'rb').read()
block = 4096
used = [i for i in range(len(data) // block) if data[i * block:(i + 1) * block] != b'\xff' * block]
end = (used[-1] + 1) * block
open(sys.argv[2], 'wb').write(data[:end])
print(f"{sys.argv[2]}: {end} bytes, blocks {used} of {len(data) // block} used (the rest are 0xff)")
PY
"$MKLITTLEFS" -l "$OUT"
