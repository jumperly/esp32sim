#!/usr/bin/env python3
"""Run semantic mutations in an isolated clean worktree, restoring source on exit.

Caller supplies CARGO_TARGET_DIR and a free serialized build slot. No timing.
"""
import json
import os
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parents[3]
if not os.environ.get('CARGO_TARGET_DIR'):
    raise SystemExit('Set a dedicated CARGO_TARGET_DIR before running')
route = root / 'esp32/src/periph.rs'
control = root / 'esp-periph/src/i2c.rs'
originals = {p: p.read_text() for p in (route, control)}
mutations = [
    ('omit-routes', route, 'self.i2c[bus].set_pins(pins);', 'let _ = pins;', 'classic_i2c_routes_both_ports_and_aliases'),
    ('reverse-wires', route, 'routes.i2c_pin(sda).zip(routes.i2c_pin(scl))', 'routes.i2c_pin(scl).zip(routes.i2c_pin(sda))', 'classic_i2c_routes_both_ports_and_aliases'),
    ('wrong-port-signals', route, 'let (scl, sda) = I2C_SIGNALS[bus];', 'let (scl, sda) = I2C_SIGNALS[0];', 'classic_i2c_routes_both_ports_and_aliases'),
    ('classic-reset', control, 'let reset = v & reset_mask != 0;', 'let reset = v & (1 << 10) != 0;', 'classic_i2c_bit10_retains_end_recipient'),
    ('modern-no-reset', control, 'let reset_mask = if $n == 16 { 0 } else { 1 << 10 };', 'let reset_mask = 0;', 'modern_i2c_bit10_cancels_pending_transfer'),
]
rows = []
try:
    for name, path, old, new, test in mutations:
        for p, text in originals.items():
            p.write_text(text)
        text = originals[path]
        if old not in text:
            raise RuntimeError('Mutation anchor missing: ' + name)
        # wrong-port changes both occurrences to exercise controller isolation.
        path.write_text(text.replace(old, new))
        cmd = ['nice', '-n', '10', 'cargo', '+1.99.0', 'test', '-j2', '--release', '-p', 'esp32sim', '--test', 'classic_i2c_routes', test, '--', '--exact']
        p = subprocess.run(cmd, cwd=root, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        killed = p.returncode == 101 and ('test ' + test + ' ... FAILED') in p.stdout
        rows.append({'mutation': name, 'test': test, 'exit': p.returncode, 'killed': killed})
        print(json.dumps(rows[-1]), flush=True)
        if not killed:
            print(p.stdout)
            raise RuntimeError('Mutation survived or failed to compile: ' + name)
finally:
    for p, text in originals.items():
        p.write_text(text)
print(json.dumps({'mutations': rows}, indent=2))
