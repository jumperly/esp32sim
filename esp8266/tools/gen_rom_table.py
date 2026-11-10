"""Generate esp8266/src/rom/table.rs from the Arduino ESP8266 core's eagle.rom.addr.v6.ld.

Usage: python esp8266/tools/gen_rom_table.py <core 3.1.2>/tools/sdk/ld/eagle.rom.addr.v6.ld esp8266/src/rom/table.rs

Every PROVIDE(name = 0x...) outside a /* */ comment becomes one entry, sorted by address (then name).
"""
import re
import sys

src, out = sys.argv[1], sys.argv[2]
text = re.sub(r'/\*.*?\*/', '', open(src, encoding='utf-8').read(), flags=re.S)
entries = [(int(a, 16), n) for n, a in re.findall(r'PROVIDE\s*\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(0x[0-9a-fA-F]+)\s*\)', text)]
entries.sort()
lines = [
    "//! Every `PROVIDE` of `tools/sdk/ld/eagle.rom.addr.v6.ld` in the Arduino ESP8266 core 3.1.2",
    "//! (the ESP8266 mask ROM's symbols; the commented-out ones left out), sorted by address.",
    "//! Generated from that file by `esp8266/tools/gen_rom_table.py`; do not edit by hand. To",
    "//! regenerate: `python esp8266/tools/gen_rom_table.py <core>/tools/sdk/ld/eagle.rom.addr.v6.ld",
    "//! esp8266/src/rom/table.rs`.",
    "use super::RomEntry;",
    "",
    f"/// {len(entries)} symbols: ROM code at 0x4000_0000..0x4001_0000 and the ROM's data in DRAM.",
    "pub const ENTRIES: &[RomEntry] = &[",
]
for a, n in entries:
    lines.append(f'    RomEntry {{ addr: {a:#010x}, name: "{n}" }},')
lines.append("];")
with open(out, 'w', encoding='utf-8', newline='\n') as f:
    f.write('\n'.join(lines) + '\n')
print(len(entries))
