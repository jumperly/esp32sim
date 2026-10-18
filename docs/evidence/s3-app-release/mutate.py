#!/usr/bin/env python3
"""Run from repository root; CARGO_TARGET_DIR and optional CARGO are caller supplied."""
import json
import os
from pathlib import Path
import subprocess
import sys

out = Path(sys.argv[1])
out.mkdir(parents=True, exist_ok=True)
mutations = [
    ('bus-invalidation', 'esp32s3/src/soc.rs', '        bus.irq_dirty = true;', ''),
    ('routing-invalidation', 'esp32s3/src/soc.rs', '        bus.periph.intmatrix_dirty = true;', ''),
    ('unmodeled-release', 'esp-soc/src/machine.rs', 'S::reset_core_with_bus(&mut self.cores[i], &mut self.bus, i)', 'S::reset_core(&mut self.cores[i], i)'),
    ('modeled-release', 'esp-soc/src/machine/modeled.rs', 'S::reset_core_with_bus(&mut self.cores[i], &mut self.bus, i)', 'S::reset_core(&mut self.cores[i], i)'),
]
rows = []
for name, filename, old, new in mutations:
    p = Path(filename)
    original = p.read_text()
    assert original.count(old) == 1, (name, original.count(old))
    try:
        p.write_text(original.replace(old, new))
        cmd = ['nice', '-n', '10', os.environ.get('CARGO', 'cargo'), '+1.99.0', 'test', '--release', '-p', 'esp32s3', '--test', 'irq_release', '--', '--nocapture']
        run = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=180)
        (out / (name + '.log')).write_text(run.stdout)
        killed = run.returncode == 101 and 'assertion `left == right` failed' in run.stdout and 'test result: FAILED' in run.stdout
        rows.append({'mutation': name, 'killed': killed, 'exit': run.returncode,
                     'failing_tests': [line.strip() for line in run.stdout.splitlines() if line.startswith('test ') and ' ... FAILED' in line]})
        print(json.dumps(rows[-1]), flush=True)
    finally:
        p.write_text(original)
(out / 'mutations.json').write_text(json.dumps(rows, indent=2) + '\n')
assert all(row['killed'] for row in rows)
