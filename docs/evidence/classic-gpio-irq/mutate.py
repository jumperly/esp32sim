#!/usr/bin/env python3
"""Assertion-based qualification; run only in an owned, otherwise idle worktree."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess

p = argparse.ArgumentParser()
p.add_argument('--workspace', type=Path, required=True)
p.add_argument('--target', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--only', help='run one named mutation')
a = p.parse_args()
f = a.workspace / 'esp32/src/periph.rs'
original = f.read_text()
a.output.mkdir(parents=True, exist_ok=True)
mutations = []
def add(name, before, after):
    assert original.count(before) == 1, (name, original.count(before))
    mutations.append((name, original.replace(before, after)))

add('stale-level-cache', 'self.level_status = status;', 'self.level_status = 0;')
add('ignore-level-mask', 'let mut pins = self.level_pins;', 'let mut pins = 0u64;')
add('keep-removed-level-pin', '(self.level_pins & !bit)', '(self.level_pins | bit)')
add('drop-low-level-type', '4 | 5) { bit }', '5) { bit }')
add('keep-removed-route', '(*mask & !bit)', '(*mask | bit)')
add('lose-software-pending-type-zero', 'if cfg & (1 << (13 + route)) != 0', 'if cfg & (1 << (13 + route)) != 0 && (cfg >> 7) & 7 != 0')
add('omit-level-status', '(self.gpio.status & ((1 << 40) - 1)) | self.level_status', 'self.gpio.status & ((1 << 40) - 1)')
add('omit-sticky-status', '(self.gpio.status & ((1 << 40) - 1)) | self.level_status', 'self.level_status')
add('ignore-route-mask', 'self.raw_status() & self.irq_routes[route]', 'self.raw_status() & (self.irq_routes[route] | ((1 << 40) - 1))')
add('swap-pro-app', '(0, false) => 2,', '(0, false) => 0,')
add('swap-nmi', '(0, true) => 3,', '(0, true) => 2,')
add('miss-pin39-config', '(0x74..=0x110).contains(&translated)', '(0x74..0x110).contains(&translated)')
add('miss-pin0-config', '(0x74..=0x110).contains(&translated)', '(0x78..=0x110).contains(&translated)')
add('refresh-unchanged-output', 'if changed {\n            self.refresh_level_status();', 'if true {\n            let _ = changed; self.refresh_level_status();')
add('never-refresh-output', 'if changed {\n            self.refresh_level_status();', 'if changed && false {\n            self.refresh_level_status();')
add('drop-other-device-sources', 'let all = Dispatch::source_status(self);\n        for (core, lines)', 'let all = [0; 4];\n        for (core, lines)')
add('reuse-core-zero-sources', 'self.with_gpio_sources(core, &all));', 'self.with_gpio_sources(0, &all));')
# Every event invalidation, independently (including output, IO_MUX, PINn, RTC).
for i, match in enumerate(re.finditer(r'self(?:\.gpio)?\.refresh_level_status\(\);', original)):
    mutations.append((f'invalidation-{i}', original[:match.start()] + '/* invalidation removed */' + original[match.end():]))

env = dict(os.environ, CARGO_TARGET_DIR=str(a.target))
results = []
try:
    for name, text in mutations:
        if a.only and name != a.only:
            continue
        f.write_text(text)
        r = subprocess.run(['nice', '-n', '10', 'cargo', '+1.99.0', 'test', '--release', '-p', 'esp32', 'gpio_irq', '--', '--nocapture'], cwd=a.workspace, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        log = r.stdout.decode(errors='replace')
        (a.output / (name + '.log')).write_text(log)
        killed = r.returncode != 0 and 'panicked at' in log and 'assertion' in log and 'could not compile' not in log
        tests = re.findall(r'test (\S+) \.\.\. FAILED', log)
        results.append(dict(name=name, killed=killed, returncode=r.returncode, tests=tests))
        (a.output / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
        print(name, 'KILLED' if killed else 'NOT KILLED', flush=True)
finally:
    f.write_text(original)
assert results and all(r['killed'] for r in results), 'surviving or invalid mutation'
