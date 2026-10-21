#!/usr/bin/env python3
"""Explain zero-page-offset literal loads without weakening the hot-path comparator."""
import argparse
import json
import pathlib
import re
import struct
import types

p = argparse.ArgumentParser()
for name in ['main-dis', 'candidate-dis', 'main-bin', 'candidate-bin', 'out']:
    p.add_argument('--' + name, required=True, type=pathlib.Path)
a = p.parse_args()
source = pathlib.Path(__file__).resolve().parents[3] / 'docs/evidence/classic-core/asmcmp.py'
m = types.ModuleType('asmcmp')
exec(source.read_text().split('a, b = funcs(sys.argv[1])')[0], m.__dict__)


def data_at(path, address):
    raw = path.read_bytes()
    header = struct.unpack_from('<8I', raw)
    assert header[0] == 0xfeedfacf, 'requires little-endian Mach-O 64'
    offset = 32
    for _ in range(header[4]):
        command, size = struct.unpack_from('<II', raw, offset)
        if command == 0x19:  # LC_SEGMENT_64
            vm, _, file_offset, file_size = struct.unpack_from('<QQQQ', raw, offset + 24)
            if vm <= address and address + 16 <= vm + file_size:
                start = file_offset + address - vm
                return raw[start:start + 16]
        offset += size
    raise AssertionError('literal is not in a file-backed segment')


rows = []
for chip, vector, register in [('esp32c3', 'q6', 'x9'), ('esp32c6', 'q28', 'x8')]:
    name = f'<{chip}::bus::SocBus as esp_soc::soc::SocBus>::refresh_irq'
    normalized = [m.funcs(path)[name] for path in [a.main_dis, a.candidate_dis]]
    differences = [(x, y) for x, y in zip(*normalized) if x != y]
    assert len(normalized[0]) == len(normalized[1])
    assert differences == [(f'ldr\t{vector}, [{register}, #PAGEOFF]', f'ldr\t{vector}, [{register}]')]
    samples = []
    for dis, binary in [(a.main_dis, a.main_bin), (a.candidate_dis, a.candidate_bin)]:
        text = dis.read_text().split('<' + name + '>:', 1)[1].split('\n\n', 1)[0]
        lines = text.splitlines()
        loads = [i for i, line in enumerate(lines) if f'ldr\t{vector}, [{register}' in line]
        assert len(loads) == 1
        i = loads[0]
        page = re.search(r'adrp\t' + register + r', (0x[0-9a-f]+)', lines[i - 1])
        assert page
        offset = re.search(r', #(0x[0-9a-f]+)\]', lines[i])
        address = int(page[1], 16) + (int(offset[1], 16) if offset else 0)
        samples.append(dict(address=hex(address), bytes=data_at(binary, address).hex(),
                            instructions=[line.split(':', 1)[1].strip() for line in lines[i - 1:i + 1]]))
    assert samples[0]['bytes'] == samples[1]['bytes']
    rows.append(dict(function=name, instructions=len(normalized[0]),
                     normalized_difference_count=len(differences), main=samples[0], candidate=samples[1],
                     conclusion='Only the literal pool offset changes to zero; loaded 16 bytes are identical.'))
a.out.write_text(json.dumps(rows, indent=2) + '\n')
print(json.dumps(rows, indent=2))
