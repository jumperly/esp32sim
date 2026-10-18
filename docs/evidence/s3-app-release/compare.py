#!/usr/bin/env python3
"""Compare public firmware work/output; no CPU measurement.
Usage: compare.py BASE_CLI CANDIDATE_CLI OUTPUT_DIRECTORY
"""
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

base, candidate, out = map(Path, sys.argv[1:])
out.mkdir(parents=True, exist_ok=True)
repo = Path(__file__).resolve().parents[3]
rows, inputs = {}, {}
for name, chip, board, prefix, app in [
    ('s3-hello', 'esp32s3', 'none', 'hello', 'hello_world'),
    ('pocket-tank-startup', 'esp32s3', 'waveshare-amoled18-v2', 'pocket-tank', 'pocket-tank'),
    ('c3-hello', 'esp32c3', 'none', 'c3-hello', 'c3-hello_world'),
    ('c6-hello', 'esp32c6', 'none', 'c6-hello', 'c6-hello_world'),
]:
    rom = {'esp32s3': 'esp32s3_rev0_rom.elf', 'esp32c3': 'esp32c3_rev3_rom.elf', 'esp32c6': 'esp32c6_rev0_rom.elf'}[chip]
    args = ['--chip', chip, '--board', board, '--boot', 'rom', '--rom', 'web/wasm/fw/' + rom,
            '--bootloader', 'web/wasm/fw/public/' + prefix + '-bootloader.bin',
            '--ptable', 'web/wasm/fw/public/' + prefix + '-ptable.bin',
            '--app', 'web/wasm/fw/public/' + app + '.bin', '--max-seconds', '3', '--no-dump']
    if name == 'pocket-tank-startup':
        args += ['--flash-mb', '16', '--psram-mb', '8']
    pair = {}
    for label, binary in [('base', base), ('candidate', candidate)]:
        run = subprocess.run(['nice', '-n', '10', str(binary)] + args, cwd=repo, capture_output=True, check=True)
        (out / (name + '-' + label + '.stdout')).write_bytes(run.stdout)
        (out / (name + '-' + label + '.stderr')).write_bytes(run.stderr)
        lines = run.stderr.decode().splitlines()
        stop = next(l for l in lines if l.startswith('[emu] stop:'))
        work = re.search(r'core0 (\d+)(?: \+ core1 (\d+))?', stop)
        if work:
            instructions = [int(x) for x in work.groups() if x]
        else:
            work = re.search(r'— (\d+) insns', stop)
            assert work, stop
            instructions = [int(work.group(1))]
        pair[label] = dict(instructions=instructions,
                           console_sha256=hashlib.sha256(run.stdout).hexdigest(),
                           events=stop.split('); ')[1],
                           irq_report=[l for l in lines if l.startswith(('[irq]', '  core'))])
    assert pair['base'] == pair['candidate'], (name, pair)
    rows[name] = dict(args=args, results=pair)
    for arg in args:
        if arg.endswith(('.bin', '.elf')):
            inputs[arg] = hashlib.sha256((repo / arg).read_bytes()).hexdigest()
    print(name, json.dumps(pair), flush=True)
(out / 'fixtures.json').write_text(json.dumps(rows, indent=2) + '\n')
(out / 'inputs.json').write_text(json.dumps(inputs, indent=2) + '\n')
