#!/usr/bin/env python3
"""Run classic TX semantic mutations serially; restore source after every run.

Usage: CARGO_TARGET_DIR=/path/to/build python3 docs/evidence/classic-wifi-auth/mutate.py /path/to/results
Each killed mutation must compile, run the named test, and fail at runtime.
"""
import json
import os
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[3]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
source = root / 'esp32/src/wifi.rs'
original = source.read_text()
positive = 'authentication_tx_reads_full_length_not_buffer_capacity'
negative = 'invalid_tx_descriptors_never_publish_completion'
mutations = [
    ('reject-length-over-size', 'if !d.owner_dma { continue; }', 'if !d.owner_dma || d.length > d.size { continue; }', positive),
    ('read-capacity-not-length', 'dma_range(d.buf, d.length as usize)', 'dma_range(d.buf, d.size as usize)', positive),
    ('ignore-owner', 'if !d.owner_dma { continue; }', 'if false { continue; }', negative),
    ('bound-capacity-not-length', 'dma_range(d.buf, d.length as usize)', 'dma_range(d.buf, d.size as usize)', negative),
    ('skip-descriptor-extent', 'if dma_range(desc, 12).is_none()', 'if false', negative),
    ('publish-completion-before-validation', '            let Some(range) = dma_range(d.buf, d.length as usize) else {', '            self.periph.wifi.tx_done(queue);\n            let Some(range) = dma_range(d.buf, d.length as usize) else {', negative),
    ('omit-station-delivery', 'self.periph.wifi.link.station_tx(frame, now_us);', 'let _ = frame;', positive),
    ('omit-tx-completion', 'self.periph.wifi.tx_done(queue);', 'let _ = queue;', positive),
]
results = []
for name, old, new, test in mutations:
    assert original.count(old) == 1, (name, original.count(old))
    command = ['nice', '-n', '10', 'cargo', '+1.99.0', 'test', '--release', '-j2', '-p', 'esp32', '--lib', 'wifi::tests::' + test, '--', '--exact']
    try:
        source.write_text(original.replace(old, new))
        result = subprocess.run(command, cwd=root, env=os.environ.copy(), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    finally:
        source.write_text(original)
    (out / (name + '.log')).write_text(result.stdout)
    killed = result.returncode != 0 and 'test result: FAILED' in result.stdout and 'panicked at' in result.stdout and 'could not compile' not in result.stdout
    results.append(dict(name=name, test=test, command=command, exit_code=result.returncode, killed=killed))
    (out / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
    print(name, 'KILLED' if killed else 'SURVIVED', flush=True)
sys.exit(0 if all(row['killed'] for row in results) else 1)
