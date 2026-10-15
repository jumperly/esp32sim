#!/bin/sh
# The official MicroPython image the T-Display demo boots (web/wasm/fw/tdisplay-prices.json):
# ESP32_GENERIC v1.29.0 from micropython.org (MIT; the image also links ESP-IDF v5.5.2 and Espressif's
# Wi-Fi/PHY libraries, see web/wasm/fw/public/tdisplay-NOTICE.txt), 1.8 MB, not committed, fetched
# into web/wasm/fw/ (git-ignored) and pinned by SHA-256. The file is a combined bootloader, partition
# table and application for flash offset 0x1000; the demo's filesystem is public/tdisplay-vfs.bin.
# A file already present with the right hash is kept. tools/fetch-demo-assets.sh and CI run this.
#   tools/fetch-micropython.sh [DIR]      default web/wasm/fw
set -e
cd "$(dirname "$0")/.."
DIR=${1:-web/wasm/fw}
NAME=ESP32_GENERIC-20260824-v1.29.0.bin
URL=https://micropython.org/resources/firmware/$NAME
SHA=e67ad6015a0a504c1fec9aa9bbf589d0432ed28e62546f4f8dd8a147f8bd95f6
sha256_ok() {  # FILE HASH; -c without GNU-only --status, so macOS shasum and sha256sum both work
  if command -v shasum >/dev/null 2>&1; then echo "$2  $1" | shasum -a 256 -c >/dev/null 2>&1
  else echo "$2  $1" | sha256sum -c >/dev/null 2>&1; fi
}
mkdir -p "$DIR"
if [ -f "$DIR/$NAME" ] && sha256_ok "$DIR/$NAME" "$SHA"; then echo "MicroPython image already in $DIR ($NAME)"; exit 0; fi
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL --retry 5 --retry-all-errors --retry-delay 5 "$URL" -o "$tmp/$NAME"
sha256_ok "$tmp/$NAME" "$SHA" || { echo "$URL: SHA-256 mismatch" >&2; exit 1; }
mv "$tmp/$NAME" "$DIR/$NAME"
echo "MicroPython image fetched into $DIR ($NAME)"
